//! Packs the toolchain and tools we built into a seed: one tree, in the layout `seed::prepare`
//! expects, that can build everything again.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::chain::{self, Built, Sources};
use crate::experiments::all_packages;
use crate::runpath;
use crate::sandbox::Status;
use crate::seed::{self, run, sha256_of};
use crate::store::{Lab, StorePath, with_path};

const PROGRAMS: [&str; 15] = [
    "coreutils",
    "sed",
    "grep",
    "gawk",
    "findutils",
    "diffutils",
    "patch",
    "make",
    "tar",
    "gzip",
    "bzip2",
    "bash",
    "patchelf",
    "binutils",
    "gcc",
];

/// Names `seed::prepare` makes itself, as links or small scripts.
const MADE_WHEN_PREPARED: [&str; 5] = ["sh", "bunzip2", "gunzip", "egrep", "fgrep"];

/// What's under `libexec/gcc` that isn't a program, which preparing a seed would trip over.
const NOT_PROGRAMS: [&str; 2] = ["install-tools", "plugin"];

const EMPTY_ARCHIVES: [&str; 4] = ["libpthread.a", "libdl.a", "librt.a", "libutil.a"];

const HASH_LENGTH: usize = 40;

fn is_elf(path: &Path) -> bool {
    fs::read(path).is_ok_and(|bytes| bytes.starts_with(b"\x7fELF"))
}

fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = fs::read_dir(dir)
        .map_err(with_path(dir))?
        .map(|entry| entry.map(|e| e.path()).map_err(with_path(dir)))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Copies a file so the pack can change it, whatever the store's modes were.
fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(with_path(parent))?;
    }
    fs::copy(from, to).map_err(with_path(from))?;
    let executable = fs::metadata(from)
        .map_err(with_path(from))?
        .permissions()
        .mode()
        & 0o100;
    let mode = if executable == 0 { 0o644 } else { 0o755 };
    fs::set_permissions(to, fs::Permissions::from_mode(mode)).map_err(with_path(to))
}

/// Copies the programs in `from/bin` that aren't already there. Scripts stay behind, since they
/// name the store paths they were built with. Returns the links, to be made once every program
/// has been copied.
fn copy_programs(from: &Path, stage: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let bin = from.join("bin");
    let mut links = Vec::new();
    if !bin.is_dir() {
        return Ok(links);
    }
    for path in sorted_entries(&bin)? {
        let name = file_name(&path);
        let to = stage.join("bin").join(&name);
        if MADE_WHEN_PREPARED.contains(&name.as_str()) || fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(with_path(&path))?;
        if meta.is_symlink() {
            links.push((name, fs::read_link(&path).map_err(with_path(&path))?));
        } else if is_elf(&path) {
            copy_file(&path, &to)?;
        }
    }
    Ok(links)
}

/// Makes the links whose target is a program in the pack, and drops the others.
fn make_links(stage: &Path, links: &[(String, PathBuf)]) -> Result<(), String> {
    let bin = stage.join("bin");
    for (name, target) in links {
        let to = bin.join(name);
        if target.components().count() == 1
            && bin.join(target).exists()
            && fs::symlink_metadata(&to).is_err()
        {
            symlink(target, &to).map_err(with_path(&to))?;
        }
    }
    Ok(())
}

/// Copies a tree, merging it into what's there and keeping links as they are.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(to).map_err(with_path(to))?;
    for path in sorted_entries(from)? {
        let name = file_name(&path);
        if NOT_PROGRAMS.contains(&name.as_str()) {
            continue;
        }
        let target = to.join(&name);
        let meta = fs::symlink_metadata(&path).map_err(with_path(&path))?;
        if meta.is_dir() {
            copy_tree(&path, &target)?;
        } else if meta.is_symlink() {
            let link = fs::read_link(&path).map_err(with_path(&path))?;
            if fs::symlink_metadata(&target).is_err() {
                symlink(&link, &target).map_err(with_path(&target))?;
            }
        } else {
            copy_file(&path, &target)?;
        }
    }
    Ok(())
}

/// Copies what's directly in `from/lib` for which `wanted` says yes.
fn copy_libraries(from: &Path, stage: &Path, wanted: impl Fn(&str) -> bool) -> Result<(), String> {
    let lib = from.join("lib");
    if !lib.is_dir() {
        return Ok(());
    }
    for path in sorted_entries(&lib)? {
        let name = file_name(&path);
        let meta = fs::symlink_metadata(&path).map_err(with_path(&path))?;
        if meta.is_dir() || !wanted(&name) {
            continue;
        }
        let to = stage.join("lib").join(&name);
        if meta.is_symlink() {
            if let Ok(link) = fs::read_link(&path)
                && fs::symlink_metadata(&to).is_err()
            {
                fs::create_dir_all(stage.join("lib")).map_err(with_path(stage))?;
                symlink(link, &to).map_err(with_path(&to))?;
            }
        } else {
            copy_file(&path, &to)?;
        }
    }
    Ok(())
}

/// glibc's libraries without its static ones. `-lpthread` and friends still need the empty
/// archives glibc leaves now those libraries are part of libc.
fn wanted_from_glibc(name: &str) -> bool {
    !name.ends_with(".a") || name.ends_with("_nonshared.a") || EMPTY_ARCHIVES.contains(&name)
}

/// A linker script that names libraries by store path, naming them by file instead.
fn without_store_paths(script: &str) -> String {
    let mut result = String::with_capacity(script.len());
    let mut rest = script;
    while let Some(at) = rest.find("/store/") {
        result.push_str(&rest[..at]);
        let after = &rest[at + "/store/".len()..];
        let hash = after.get(..HASH_LENGTH);
        match hash.zip(after.get(HASH_LENGTH..)) {
            Some((hash, tail)) if hash.bytes().all(|b| b.is_ascii_hexdigit()) => {
                rest = tail.strip_prefix("/lib/").unwrap_or(tail);
            }
            _ => {
                result.push_str("/store/");
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

/// Rewrites the linker scripts directly in `lib`, which are small text files that aren't ELF.
fn fix_linker_scripts(stage: &Path) -> Result<(), String> {
    for path in sorted_entries(&stage.join("lib"))? {
        let meta = fs::symlink_metadata(&path).map_err(with_path(&path))?;
        if !meta.is_file() || meta.len() > 4096 || is_elf(&path) {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&path) {
            let fixed = without_store_paths(&text);
            if fixed != text {
                fs::write(&path, fixed).map_err(with_path(&path))?;
            }
        }
    }
    Ok(())
}

fn elf_files(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
    if !dir.is_dir() {
        return Ok(());
    }
    for path in sorted_entries(dir)? {
        let meta = fs::symlink_metadata(&path).map_err(with_path(&path))?;
        if meta.is_dir() {
            elf_files(&path, found)?;
        } else if meta.is_file() && is_elf(&path) {
            found.push(path);
        }
    }
    Ok(())
}

/// Where a run path directory is on the host, if it's in the store.
fn on_host(lab: &Lab, dir: &str) -> Option<PathBuf> {
    let rest = dir.strip_prefix("/store/")?;
    let hash = rest.get(..HASH_LENGTH)?;
    let tail = rest.get(HASH_LENGTH..)?.trim_start_matches('/');
    Some(lab.on_host(&StorePath::from_hash(hash)).join(tail))
}

/// Copies into `lib` every library the pack's programs need and doesn't have yet, following what
/// those libraries need in turn.
fn copy_needed(lab: &Lab, stage: &Path) -> Result<(), String> {
    let mut todo = Vec::new();
    for dir in ["bin", "libexec", "lib"] {
        elf_files(&stage.join(dir), &mut todo)?;
    }
    let mut missing = BTreeSet::new();
    while let Some(file) = todo.pop() {
        let bytes = fs::read(&file).map_err(with_path(&file))?;
        let Some((needed, runpath)) = runpath::needed_and_run_path(&bytes) else {
            continue;
        };
        for name in needed {
            let to = stage.join("lib").join(&name);
            if name.starts_with("ld-linux") || fs::symlink_metadata(&to).is_ok() {
                continue;
            }
            let source = runpath
                .split(':')
                .filter_map(|dir| on_host(lab, dir))
                .map(|dir| dir.join(&name))
                .find(|path| path.exists());
            match source {
                Some(source) => {
                    copy_file(&source, &to)?;
                    todo.push(to);
                }
                None => {
                    missing.insert(format!("{name} (for {})", file_name(&file)));
                }
            }
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        let list: Vec<String> = missing.into_iter().collect();
        Err(format!("libraries not found: {}", list.join(", ")))
    }
}

/// Fills `stage` with the seed's files from the packages' outputs.
fn fill(lab: &Lab, stage: &Path, outputs: &dyn Fn(&str) -> Vec<PathBuf>) -> Result<(), String> {
    let mut links = Vec::new();
    for package in PROGRAMS {
        for output in outputs(package) {
            links.extend(copy_programs(&output, stage)?);
        }
    }
    make_links(stage, &links)?;
    for output in outputs("gcc") {
        copy_tree(&output.join("lib/gcc"), &stage.join("lib/gcc"))?;
        copy_tree(&output.join("libexec/gcc"), &stage.join("libexec/gcc"))?;
        copy_tree(&output.join("include/c++"), &stage.join("include/c++"))?;
        copy_libraries(&output, stage, |name| {
            !name.ends_with(".la") && !name.ends_with(".py") && !name.starts_with("libcc1")
        })?;
    }
    for output in outputs("glibc") {
        copy_libraries(&output, stage, wanted_from_glibc)?;
        copy_tree(&output.join("include"), &stage.join("include-glibc"))?;
    }
    for output in outputs("linux-headers") {
        copy_tree(&output.join("include"), &stage.join("include-glibc"))?;
    }
    fix_linker_scripts(stage)?;
    copy_needed(lab, stage)
}

/// Packs a tree into a tarball that depends only on the tree, and returns the tarball's hash.
fn tarball(lab: &Lab, stage: &Path) -> Result<String, String> {
    let dir = lab.work.join("seed");
    let tar = dir.join("pack.tar");
    run(
        Command::new("tar")
            .args(["--sort=name", "--mtime=@0", "--owner=0", "--group=0"])
            .args(["--numeric-owner", "--mode=u+w", "-cf"])
            .arg(&tar)
            .arg("-C")
            .arg(stage)
            .arg("."),
        "packing the seed",
    )?;
    run(
        Command::new("xz").args(["--force", "-T1", "-9"]).arg(&tar),
        "compressing the seed",
    )?;
    let packed = dir.join("pack.tar.xz");
    let hash = sha256_of(&packed)?;
    let final_path = dir.join(format!("{hash}.tar.xz"));
    fs::rename(&packed, &final_path).map_err(with_path(&packed))?;
    Ok(hash)
}

/// What has been built from the pinned seed, or an error if anything is missing.
fn built(lab: &Lab, repository: &Path) -> Result<(Vec<String>, Built), String> {
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
    let mut built = chain::built_from_seed(&seed);
    let mut ids = Vec::new();
    for package in all_packages(lab, repository)? {
        let made = chain::build_one(lab, repository, &package, &seed, &mut sources, &mut built)?;
        if made.is_some() {
            let id = package.id;
            return Err(format!(
                "{id} isn't built yet: `distro build {id}` builds it",
            ));
        }
        ids.push(package.id);
    }
    Ok((ids, built))
}

/// Prints where each package landed, to compare what two seeds produce.
pub fn addresses(lab: &Lab, repository: &Path) -> Result<(), String> {
    let (ids, built) = built(lab, repository)?;
    for id in ids {
        let paths = built.output_paths(&id);
        let hashes: Vec<&str> = paths.iter().map(StorePath::hash).collect();
        println!("{id} {}", hashes.join(" "));
    }
    Ok(())
}

/// Packs what was built into a new seed in `work/seed`, and prints its hash for `seed.pin`.
pub fn pack(lab: &Lab, repository: &Path) -> Result<(), String> {
    let (_, built) = built(lab, repository)?;
    let stage = lab.work.join("seed/stage");
    chain::remove(&stage)?;
    fs::create_dir_all(&stage).map_err(with_path(&stage))?;
    fill(lab, &stage, &|id| {
        built
            .output_paths(id)
            .iter()
            .map(|path| lab.on_host(path))
            .collect()
    })?;
    let hash = tarball(lab, &stage)?;
    println!("{hash}");
    chain::remove(&stage)
}

/// What must be left behind once the final toolchain is built.
const BOOTSTRAP: [&str; 7] = [
    "seed",
    "wrapper",
    "m4",
    "bison",
    "python",
    "binutils-stage1",
    "gcc-stage1",
];
const FINAL: [&str; 4] = ["glibc", "binutils", "gcc", "hello"];

/// Builds the toolchain and checks it: the final compiler, C library and binutils
/// refer to nothing from the bootstrap, and a C and a C++ program run with no `/usr` at all.
pub fn check_toolchain(lab: &Lab, repository: &Path) -> Result<(), String> {
    let built = chain::build(lab, repository, "toolchain.toml")?;
    let mut failed = false;

    for name in FINAL {
        let found = chain::references(&lab.on_host(&built.paths[name]))?;
        for bootstrap in BOOTSTRAP {
            if let Some(file) = found.get(built.paths[bootstrap].hash()) {
                println!(
                    "  LEAK   {name} refers to {bootstrap}, in {}",
                    file.display()
                );
                failed = true;
            }
        }
    }

    for (program, expected) in [
        ("bin/hello", "hello from the store\n"),
        ("bin/hello-cxx", "hello from libstdc++\n"),
    ] {
        let (status, text) = chain::run_alone(
            lab,
            &built.paths["hello"],
            program,
            &program.replace('/', "-"),
        )?;
        if status == Status::Passed && text == expected {
            println!("  ran    {program} with /usr absent");
        } else {
            println!("  FAILED {program}: {status:?}: {text}");
            failed = true;
        }
    }
    if failed {
        Err("the toolchain leaks its bootstrap or doesn't run on its own".to_owned())
    } else {
        println!("the toolchain stands on its own");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELF: &[u8] = b"\x7fELF rest";

    fn write(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn glibc_keeps_shared_libraries_and_empty_archives_only() {
        assert!(wanted_from_glibc("libc.so.6"));
        assert!(wanted_from_glibc("libc_nonshared.a"));
        assert!(wanted_from_glibc("libpthread.a"));
        assert!(!wanted_from_glibc("libc.a"));
    }

    #[test]
    fn a_linker_script_stops_naming_store_paths() {
        let hash = "a".repeat(40);
        let script = format!("GROUP ( /store/{hash}/lib/libc.so.6 /store/{hash}/lib/x.a )\n");
        assert_eq!(without_store_paths(&script), "GROUP ( libc.so.6 x.a )\n");
    }

    #[test]
    fn text_that_only_looks_like_a_store_path_stays() {
        let script = "/store/short/lib/x\n";
        assert_eq!(without_store_paths(script), script);
    }

    #[test]
    fn programs_are_copied_and_scripts_and_prepared_names_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let (from, stage) = (dir.path().join("from"), dir.path().join("stage"));
        write(&from.join("bin/ls"), ELF);
        write(&from.join("bin/sh"), ELF);
        write(&from.join("bin/zgrep"), b"#!/store/x/bin/sh\n");
        copy_programs(&from, &stage).unwrap();
        assert!(stage.join("bin/ls").exists());
        assert!(!stage.join("bin/sh").exists());
        assert!(!stage.join("bin/zgrep").exists());
    }

    #[test]
    fn the_first_package_to_provide_a_program_wins() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        write(&dir.path().join("a/bin/x"), b"\x7fELF first");
        write(&dir.path().join("b/bin/x"), b"\x7fELF second");
        copy_programs(&dir.path().join("a"), &stage).unwrap();
        copy_programs(&dir.path().join("b"), &stage).unwrap();
        assert_eq!(fs::read(stage.join("bin/x")).unwrap(), b"\x7fELF first");
    }

    #[test]
    fn only_links_to_packed_programs_are_made() {
        let dir = tempfile::tempdir().unwrap();
        let stage = dir.path().join("stage");
        write(&stage.join("bin/g++"), ELF);
        let links = [
            ("c++".to_owned(), PathBuf::from("g++")),
            ("gone".to_owned(), PathBuf::from("missing")),
            ("far".to_owned(), PathBuf::from("/store/x/bin/y")),
        ];
        make_links(&stage, &links).unwrap();
        assert!(stage.join("bin/c++").exists());
        assert!(fs::symlink_metadata(stage.join("bin/gone")).is_err());
        assert!(fs::symlink_metadata(stage.join("bin/far")).is_err());
    }

    #[test]
    fn copied_files_can_be_changed_and_keep_being_executable() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("from"), dir.path().join("to/x"));
        fs::write(&from, ELF).unwrap();
        fs::set_permissions(&from, fs::Permissions::from_mode(0o555)).unwrap();
        copy_file(&from, &to).unwrap();
        assert_eq!(
            fs::metadata(&to).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
