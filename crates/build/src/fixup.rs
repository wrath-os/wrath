//! What's done to a package once its build has run, on the host: modes settled, its runtime
//! dependencies written in, libraries checked, `.la` files removed, shebangs and run paths
//! pointed at the store, binaries stripped, and the package split and moved to its content
//! addresses (decision 0037).

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::chain::{
    Built, Package, Prepared, content_addressed, logs_dir, references_of, remember_outputs,
};
use crate::runpath;
use crate::split;
use crate::store::{Lab, StorePath, with_path};

/// Where a package keeps the store paths of its `runtime_deps`.
const RUNTIME_DEPS: &str = "lib/wrath/runtime-deps";

#[derive(Debug, Default)]
pub(crate) struct Leaks {
    unresolved_shebangs: usize,
    samples: Vec<String>,
}

impl Leaks {
    fn sample(&mut self, text: String) {
        if self.samples.len() < 20 {
            self.samples.push(text);
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Fixups {
    la_removed: usize,
    shebangs: usize,
}

pub(crate) fn files_under(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(with_path(dir))? {
        let entry = entry.map_err(with_path(dir))?;
        let kind = entry.file_type().map_err(with_path(&entry.path()))?;
        if kind.is_dir() {
            found.extend(files_under(&entry.path())?);
        } else {
            found.push(entry.path());
        }
    }
    Ok(found)
}

/// Writes `bytes` to a file that may be read-only, keeping its mode.
pub(crate) fn replace_contents(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mode = fs::metadata(path)
        .map_err(with_path(path))?
        .permissions()
        .mode();
    fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o200)).map_err(with_path(path))?;
    fs::write(path, bytes).map_err(with_path(path))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(with_path(path))
}

pub(crate) fn path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"._+/-".contains(&byte)
}

/// Where `relative`, a path in the view, really is in the store, if some dependency provides it.
fn owner_of(view: &Path, relative: &str) -> Option<String> {
    let target = fs::read_link(view.join(relative)).ok()?;
    target.to_str().map(str::to_owned)
}

/// Where a script's interpreter really is, or `None` when nothing provides it.
fn real_interpreter(
    path: &str,
    prefix: &str,
    out_host: &Path,
    out_inside: &str,
    view: &Path,
) -> Option<String> {
    let rest = path.strip_prefix(prefix)?.trim_start_matches('/');
    if let Some(real) = owner_of(view, rest) {
        return Some(real);
    }
    out_host
        .join(rest)
        .exists()
        .then(|| format!("{out_inside}/{rest}"))
}

/// Points a script's first line at the store. Returns whether it changed.
pub(crate) fn fix_shebang(
    file: &Path,
    out_host: &Path,
    out_inside: &str,
    view: &Path,
    leaks: &mut Leaks,
) -> Result<bool, String> {
    let bytes = fs::read(file).map_err(with_path(file))?;
    if !bytes.starts_with(b"#!") {
        return Ok(false);
    }
    let end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(bytes.len());
    let Ok(line) = std::str::from_utf8(&bytes[2..end]) else {
        return Ok(false);
    };
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else {
        return Ok(false);
    };
    let rest: Vec<&str> = words.collect();
    // /bin/sh is provided everywhere. Anything else in /bin is really a program of the view's.
    let moved = format!("/usr{first}");
    let first = if first.starts_with("/bin/") && first != "/bin/sh" {
        moved.as_str()
    } else {
        first
    };
    let prefix = "/usr";
    if !first.starts_with(prefix) {
        return Ok(false);
    }
    let (target, arguments): (String, Vec<&str>) = if first.ends_with("/bin/env") {
        match rest.iter().position(|word| !word.starts_with('-')) {
            Some(at) => (
                format!("{prefix}/bin/{}", rest[at]),
                rest[at + 1..].to_vec(),
            ),
            None => return Ok(false),
        }
    } else {
        (first.to_owned(), rest)
    };
    let line = match real_interpreter(&target, prefix, out_host, out_inside, view) {
        Some(real) => {
            let mut line = format!("#!{real}");
            for word in arguments {
                line.push(' ');
                line.push_str(word);
            }
            line
        }
        None => {
            leaks.unresolved_shebangs += 1;
            leaks.sample(format!("shebang {first}: {}", file.display()));
            if first.ends_with("/bin/env") {
                return Ok(false);
            }
            // The build doesn't have it, so the system's PATH finds it when the script runs,
            // through /usr/bin/env, the one other path a system makes for scripts (decision
            // 0016). The kernel passes what follows as one argument, so `-S` splits it when
            // there's more than a name.
            let name = target.rsplit('/').next().unwrap_or(&target);
            if arguments.is_empty() {
                format!("#!/usr/bin/env {name}")
            } else {
                format!("#!/usr/bin/env -S {name} {}", arguments.join(" "))
            }
        }
    };
    let mut new = line.into_bytes();
    new.extend_from_slice(&bytes[end..]);
    replace_contents(file, &new)?;
    Ok(true)
}

/// What's done to a package once it's installed, on the host.
pub(crate) fn fix_up(
    lab: &Lab,
    prepared: &Prepared,
    fixups: &mut Fixups,
    leaks: &mut Leaks,
) -> Result<(), String> {
    let out = lab.on_host(&prepared.out);
    let inside = prepared.out.temporary().inside();
    let view = &prepared.view.dir;
    for file in files_under(&out)? {
        let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
        if meta.is_symlink() {
            continue;
        }
        if file
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".la"))
        {
            fs::remove_file(&file).map_err(with_path(&file))?;
            fixups.la_removed += 1;
            continue;
        }
        if fix_shebang(&file, &out, &inside, view, leaks)? {
            fixups.shebangs += 1;
        }
    }
    runpath::shrink_files(
        lab,
        &prepared.out.temporary(),
        &files_under(&out)?,
        replace_contents,
    )?;
    split::strip(lab, &BTreeSet::from([prepared.out.clone()]))?;
    Ok(())
}

/// Writes the store paths of a package's `runtime_deps` into it, where the reference scan finds
/// them.
pub(crate) fn write_runtime_deps(
    lab: &Lab,
    package: &Package,
    out: &StorePath,
    built: &Built,
) -> Result<(), String> {
    if package.runtime_deps.is_empty() {
        return Ok(());
    }
    let text: String = package
        .runtime_deps
        .iter()
        .map(|id| format!("{}\n", built.paths[id].inside()))
        .collect();
    let file = lab.on_host(out).join(RUNTIME_DEPS);
    let dir = file.parent().unwrap_or(&file);
    fs::create_dir_all(dir).map_err(with_path(dir))?;
    fs::write(&file, text).map_err(with_path(&file))
}

/// Moves a finished package to its content addresses, so what's built next is keyed on what this
/// one really produced. Boot packages stay where they were built.
pub(crate) fn finish(
    lab: &Lab,
    package: &Package,
    out: &StorePath,
    built: &mut Built,
) -> Result<(), String> {
    if !content_addressed(package) {
        return Ok(());
    }
    let (outputs, whole) = place_outputs(lab, package, out)?;
    for path in outputs.values() {
        references_of(lab, path)?;
    }
    if let Some(reason) = whole {
        note_whole(lab, package, &reason)?;
    }
    let record = outputs
        .iter()
        .map(|(class, path)| format!("{}={}", class.name(), path.hash()))
        .collect::<Vec<_>>()
        .join(" ");
    let done = lab.work.join("done").join(out.hash());
    fs::write(&done, &record).map_err(with_path(&done))?;
    remember_outputs(built, package, &record);
    // Only advice for now: nothing here fails a build that worked.
    let noted = crate::requires::unbrought_headers(lab, built, &package.id).and_then(|lines| {
        if lines.is_empty() {
            Ok(())
        } else {
            note_unbrought(lab, package, &lines)
        }
    });
    if let Err(why) = noted {
        eprintln!(
            "warning: couldn't check what `{}`'s headers include: {why}",
            package.id
        );
    }
    Ok(())
}

/// Says which of a package's headers include ones from a dependency it doesn't bring along, and
/// records them, so whether this can be an error is decided from what it finds.
fn note_unbrought(lab: &Lab, package: &Package, lines: &[String]) -> Result<(), String> {
    eprintln!(
        "warning: `{}` doesn't bring along what its headers include, so builds using them may \
         fail; `propagates` in its recipe brings it:\n  {}",
        package.id,
        lines.join("\n  ")
    );
    let path = logs_dir(lab).join("unbrought-headers.txt");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(with_path(&path))?;
    for line in lines {
        writeln!(file, "{} {line}", package.id).map_err(with_path(&path))?;
    }
    Ok(())
}

/// Moves a finished package to its content addresses, and says why if it stayed whole.
pub(crate) fn place_outputs(
    lab: &Lab,
    package: &Package,
    out: &StorePath,
) -> Result<(split::Outputs, Option<String>), String> {
    let seen_as = out.temporary();
    let root = lab.on_host(out);
    let pieces = crate::content::split_pieces(&root, seen_as.hash())?;
    if !pieces.is_empty() {
        note_pieces(lab, package, out, &pieces)?;
    }
    let cut = crate::content::cut_names(&root, seen_as.hash())?;
    if package.whole {
        split::keep_whole(lab, out, &seen_as, "the recipe asks".to_owned())
    } else if let Some(file) = cut.first() {
        let file = file.strip_prefix(&root).unwrap_or(file).display();
        let reason = format!(
            "{file} names its path with the name after the hash cut apart, so which part it \
             means can't be told"
        );
        split::keep_whole(lab, out, &seen_as, reason)
    } else {
        split::split_package(lab, out, &seen_as)
    }
}

/// Records that a package stayed whole, and why.
fn note_whole(lab: &Lab, package: &Package, reason: &str) -> Result<(), String> {
    let path = logs_dir(lab).join("whole.txt");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(with_path(&path))?;
    writeln!(file, "{} {reason}", package.id).map_err(with_path(&path))
}

/// Says which of a package's files hold pieces of the path its build saw, which keep naming that
/// path once it's moved, and records them for the packaging findings.
fn note_pieces(
    lab: &Lab,
    package: &Package,
    out: &StorePath,
    files: &[PathBuf],
) -> Result<(), String> {
    let root = lab.on_host(out);
    let names: Vec<String> = files
        .iter()
        .map(|file| {
            file.strip_prefix(&root)
                .unwrap_or(file)
                .display()
                .to_string()
        })
        .collect();
    eprintln!(
        "warning: `{}` keeps pieces of its build's path in {}, which can't be rewritten",
        package.id,
        names.join(", ")
    );
    let path = logs_dir(lab).join("split-pieces.txt");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(with_path(&path))?;
    writeln!(file, "{} {}", package.id, names.join(" ")).map_err(with_path(&path))
}

/// The first ELF file under `dir` that needs a library by a path outside the store, such as the
/// build's `/usr/lib/libsqlite3.so`, with that path. A linker records a path when the library has
/// no SONAME, and the program then can't start anywhere else.
pub(crate) fn linked_by_path(dir: &Path) -> Result<Option<(PathBuf, String)>, String> {
    for file in files_under(dir)? {
        if file.is_symlink() {
            continue;
        }
        let mut magic = [0u8; 4];
        let is_elf = fs::File::open(&file)
            .and_then(|mut opened| opened.read_exact(&mut magic))
            .is_ok_and(|()| &magic == b"\x7fELF");
        if !is_elf {
            continue;
        }
        let bytes = fs::read(&file).map_err(with_path(&file))?;
        let Some((needed, _)) = runpath::needed_and_run_path(&bytes) else {
            continue;
        };
        if let Some(library) = needed.into_iter().find(|name| outside_store(name)) {
            let relative = file.strip_prefix(dir).unwrap_or(&file).to_path_buf();
            return Ok(Some((relative, library)));
        }
    }
    Ok(None)
}

/// Whether a needed library is named by a path outside the store.
pub(crate) fn outside_store(needed: &str) -> bool {
    needed.starts_with('/') && !needed.starts_with("/store/")
}

/// Clears setuid and setgid under `dir`, which would make a program run as root wherever the
/// store is copied as root; a system's wrappers are where privileges come from. A device, pipe
/// or socket is an error.
pub(crate) fn settle_modes(dir: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(with_path(dir))? {
        let path = entry.map_err(with_path(dir))?.path();
        let meta = fs::symlink_metadata(&path).map_err(with_path(&path))?;
        let kind = meta.file_type();
        if kind.is_symlink() {
            continue;
        }
        if !kind.is_file() && !kind.is_dir() {
            return Err(format!(
                "{}, a device, pipe or socket, but packages hold only files, directories and links",
                path.display()
            ));
        }
        let mode = meta.permissions().mode();
        if mode & 0o6000 != 0 {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode & !0o6000))
                .map_err(with_path(&path))?;
        }
        if kind.is_dir() {
            settle_modes(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn view_with(dir: &Path, relative: &str, target: &str) {
        let at = dir.join(relative);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        symlink(target, at).unwrap();
    }

    #[test]
    fn a_shebang_through_env_or_usr_points_at_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let view = dir.path().join("view");
        view_with(&view, "bin/python3", "/store/aaa/bin/python3");
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        let mut leaks = Leaks::default();
        for (before, after) in [
            (
                "#!/usr/bin/python3 -E\nprint\n",
                "#!/store/aaa/bin/python3 -E\nprint\n",
            ),
            (
                "#!/usr/bin/env python3\nprint\n",
                "#!/store/aaa/bin/python3\nprint\n",
            ),
        ] {
            let file = out.join("script");
            fs::write(&file, before).unwrap();
            assert!(fix_shebang(&file, &out, "/store/self", &view, &mut leaks).unwrap());
            assert_eq!(fs::read_to_string(&file).unwrap(), after);
        }
    }

    #[test]
    fn a_shebang_the_build_cant_resolve_finds_its_interpreter_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let view = dir.path().join("view");
        fs::create_dir_all(&view).unwrap();
        let mut leaks = Leaks::default();
        for (before, after) in [
            ("#!/bin/bash -e\nx\n", "#!/usr/bin/env -S bash -e\nx\n"),
            ("#!/usr/bin/perl\nx\n", "#!/usr/bin/env perl\nx\n"),
            (
                "#!/usr/bin/env python3.13\nx\n",
                "#!/usr/bin/env python3.13\nx\n",
            ),
        ] {
            let file = dir.path().join("script");
            fs::write(&file, before).unwrap();
            fix_shebang(&file, dir.path(), "/store/s", &view, &mut leaks).unwrap();
            assert_eq!(fs::read_to_string(&file).unwrap(), after);
        }
        assert_eq!(
            leaks.unresolved_shebangs, 3,
            "each is still counted, to find recipes to fix"
        );
        // /bin/sh is on every system.
        let file = dir.path().join("script");
        fs::write(&file, "#!/bin/sh\nx\n").unwrap();
        assert!(!fix_shebang(&file, dir.path(), "/store/s", &view, &mut leaks).unwrap());
    }
}
