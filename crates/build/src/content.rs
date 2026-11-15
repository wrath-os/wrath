//! Names a built package by what it contains.
//!
//! Changing this changes [`crate::fixup::rules`], so every package is fixed up again.
//!
//! A package builds at a temporary store path. Its address is a hash of its contents with that
//! path masked, so the same contents get the same address wherever they were built. Then every
//! mention of the temporary path, in contents, links and file names, becomes the final one, which
//! has the same length.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use memchr::memmem;
use sha2::{Digest, Sha256};

use crate::chain;
use crate::fixup::{files_under, replace_contents};
use crate::store::{HASH_LENGTH, Lab, StorePath, hex, with_path};

/// Replaces every `from` with `to`, and returns where they were.
fn replace(bytes: &mut [u8], from: &[u8], to: &[u8]) -> Vec<usize> {
    let found: Vec<usize> = memmem::find_iter(bytes, from).collect();
    for &at in &found {
        bytes[at..at + from.len()].copy_from_slice(to);
    }
    found
}

/// The hash at the start of `path`, when a name follows it.
fn digest_of(path: &str) -> Option<&str> {
    path.get(..HASH_LENGTH).filter(|_| path.len() > HASH_LENGTH)
}

/// Replaces every `own` with `new`, then every hash of `own` left, where a compiler cut the name
/// after it apart: the name is the same in both, so the hash alone is what changes. Returns
/// where each was.
fn replace_own(bytes: &mut [u8], own: &str, new: &str) -> (Vec<usize>, Vec<usize>) {
    let whole = replace(bytes, own.as_bytes(), new.as_bytes());
    let bare = match (digest_of(own), digest_of(new)) {
        (Some(from), Some(to)) => replace(bytes, from.as_bytes(), to.as_bytes()),
        _ => Vec::new(),
    };
    (whole, bare)
}

/// The places a package's own path was masked. Without them in the hash, a file holding the path
/// and a file holding the mask would get the same address.
fn offsets(found: &[usize]) -> Vec<u8> {
    found
        .iter()
        .flat_map(|at| (*at as u64).to_le_bytes())
        .collect()
}

fn relative(root: &Path, file: &Path) -> Result<String, String> {
    file.strip_prefix(root)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|e| format!("{}: {e}", file.display()))
}

/// Every file, link and directory under `root`, sorted.
fn sorted_entries(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).map_err(with_path(&dir))? {
            let entry = entry.map_err(with_path(&dir))?;
            let path = entry.path();
            if entry.file_type().map_err(with_path(&path))?.is_dir() {
                pending.push(path.clone());
            }
            entries.push(path);
        }
    }
    entries.sort();
    Ok(entries)
}

/// The address of the tree at `root`, built at temporary path `own`.
fn content_hash(root: &Path, own: &str) -> Result<StorePath, String> {
    let zeros = "0".repeat(own.len());
    let name = own.get(HASH_LENGTH + 1..);
    let mut hasher = Sha256::new();
    let mut add = |bytes: &[u8]| {
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    add(name.unwrap_or_default().as_bytes());
    for file in sorted_entries(root)? {
        let mut name = relative(root, &file)?.into_bytes();
        let in_name = replace(&mut name, own.as_bytes(), zeros.as_bytes());
        add(&name);
        add(&offsets(&in_name));
        let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
        let (kind, mut bytes): (&[u8], Vec<u8>) = if meta.is_symlink() {
            let target = fs::read_link(&file).map_err(with_path(&file))?;
            (b"link", target.to_string_lossy().into_owned().into_bytes())
        } else if meta.is_dir() {
            (b"dir", Vec::new())
        } else {
            (b"file", fs::read(&file).map_err(with_path(&file))?)
        };
        if !meta.is_symlink() {
            add(&(meta.permissions().mode() & 0o777).to_le_bytes());
        }
        let (in_file, cut) = replace_own(&mut bytes, own, &zeros);
        add(kind);
        add(&bytes);
        add(&offsets(&in_file));
        // Only where there are any, so a package without them keeps its address.
        if !cut.is_empty() {
            add(b"cut");
            add(&offsets(&cut));
        }
    }
    let address = StorePath::from_hash(&hex(&hasher.finalize())[..HASH_LENGTH]);
    Ok(match name {
        Some(name) => address.named(name),
        None => address,
    })
}

/// Where in `bytes` `own` is written whole, and where only its hash is: the name after it cut
/// apart, as copies in overlapping 16-byte pieces leave it.
fn occurrences(bytes: &[u8], own: &str) -> (Vec<usize>, Vec<usize>) {
    let whole: Vec<usize> = memmem::find_iter(bytes, own.as_bytes()).collect();
    let cut = digest_of(own).map_or_else(Vec::new, |digest| {
        memmem::find_iter(bytes, digest.as_bytes())
            .filter(|at| whole.binary_search(at).is_err())
            .collect()
    });
    (whole, cut)
}

/// The files under `root` holding `own`'s hash with the name after it cut apart. Rewriting the
/// hash alone gives them the final path, but which part of a split package they name can't be
/// told, so the package stays whole.
pub fn cut_names(root: &Path, own: &str) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    for file in regular_files(root)? {
        let bytes = fs::read(&file).map_err(with_path(&file))?;
        if !occurrences(&bytes, own).1.is_empty() {
            found.push(file);
        }
    }
    Ok(found)
}

/// The files under `root` holding a piece of `own`'s hash, eight bytes or more, that isn't part of
/// the hash written whole: where a compiler turned the path into pieces, such as the immediates
/// of a `memcmp` it inlined. Nothing can rewrite those, so they keep naming the path the build
/// saw.
pub fn split_pieces(root: &Path, own: &str) -> Result<Vec<PathBuf>, String> {
    let digest = own.get(..HASH_LENGTH).unwrap_or(own).as_bytes();
    let windows: Vec<&[u8]> = digest.windows(8).collect();
    let mut found = Vec::new();
    for file in regular_files(root)? {
        let bytes = fs::read(&file).map_err(with_path(&file))?;
        let (whole, cut) = occurrences(&bytes, own);
        let inside = |at: usize| {
            whole
                .iter()
                .chain(&cut)
                .any(|&start| at >= start && at < start + digest.len())
        };
        let split = windows
            .iter()
            .any(|window| memmem::find_iter(&bytes, window).any(|at| !inside(at)));
        if split {
            found.push(file);
        }
    }
    Ok(found)
}

/// The regular files under `root`, sorted.
fn regular_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for file in sorted_entries(root)? {
        if fs::symlink_metadata(&file)
            .map_err(with_path(&file))?
            .is_file()
        {
            files.push(file);
        }
    }
    Ok(files)
}

pub(crate) fn rewrite_contents(root: &Path, own: &str, new: &str) -> Result<(), String> {
    for file in files_under(root)? {
        let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
        if meta.is_symlink() {
            let target = fs::read_link(&file).map_err(with_path(&file))?;
            let mut text = target.to_string_lossy().into_owned().into_bytes();
            if replace_own(&mut text, own, new) != (Vec::new(), Vec::new()) {
                fs::remove_file(&file).map_err(with_path(&file))?;
                symlink(String::from_utf8_lossy(&text).as_ref(), &file)
                    .map_err(with_path(&file))?;
            }
            continue;
        }
        let mut bytes = fs::read(&file).map_err(with_path(&file))?;
        if replace_own(&mut bytes, own, new) != (Vec::new(), Vec::new()) {
            replace_contents(&file, &bytes)?;
        }
    }
    Ok(())
}

/// Renames anything under `root` with `own` in its name, shallowest first.
pub(crate) fn rewrite_names(root: &Path, own: &str, new: &str) -> Result<(), String> {
    loop {
        let mut target = None;
        for file in files_under(root)? {
            let rest = relative(root, &file)?;
            if let Some(at) = rest.find(own) {
                let end = rest[at..].find('/').map_or(rest.len(), |slash| at + slash);
                target = Some(rest[..end].to_owned());
                break;
            }
        }
        let Some(name) = target else {
            return Ok(());
        };
        let from = root.join(&name);
        fs::rename(&from, root.join(name.replace(own, new))).map_err(with_path(&from))?;
    }
}

/// Moves the package at `out` to the path its contents give it, and returns that path.
pub fn address(lab: &Lab, out: &StorePath) -> Result<StorePath, String> {
    let root = lab.on_host(out);
    let address = content_hash(&root, out.hash())?;
    if address == *out {
        return Ok(address);
    }
    let destination = lab.on_host(&address);
    if destination.exists() {
        chain::remove(&root)?;
        return Ok(address);
    }
    rewrite_contents(&root, out.hash(), address.hash())?;
    rewrite_names(&root, out.hash(), address.hash())?;
    fs::rename(&root, &destination).map_err(with_path(&root))?;
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_of_a_packages_own_path_a_compiler_split_apart_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let own = format!(
            "{}-glibc-2.42",
            "0123456789abcdef".repeat(3)[..40].to_owned()
        );
        let whole = format!("/wrath/store/{own}/lib");
        let digest = &own[..40];
        // As `movabs` immediates: eight bytes of the path at a time, with code in between.
        let mut ld = format!("{whole}\0").into_bytes();
        ld.extend_from_slice(&[0x48, 0xb8]);
        ld.extend_from_slice(format!("/wrath/store/{}", &digest[..1]).as_bytes());
        ld.extend_from_slice(&[0x49, 0x33, 0x48, 0xba]);
        ld.extend_from_slice(&digest.as_bytes()[1..9]);
        fs::write(dir.path().join("ld.so"), ld).unwrap();
        fs::write(dir.path().join("libc.so"), format!("{whole}\0{whole}\0")).unwrap();

        let found = split_pieces(dir.path(), &own).unwrap();

        assert_eq!(found, [dir.path().join("ld.so")]);
    }

    /// A file naming `out` as glib's charset path did: copied in 16-byte pieces whose last one
    /// overlaps the one before, so the hash is whole but the name after it is cut apart.
    fn cut_apart(out: &StorePath) -> Vec<u8> {
        let path = format!("{}/lib", out.inside());
        let (head, tail) = path.split_at(path.len() - 16);
        format!("{head}{}{tail}", &head[head.len() - 1..]).into_bytes()
    }

    #[test]
    fn a_path_whose_name_was_cut_apart_is_found_and_its_hash_rewritten() {
        let (_dir, lab) = lab();
        let out = StorePath::derive("test", &[b"cut"]).named("glib-2.84.4");
        let root = lab.on_host(&out);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("libglib.so"), cut_apart(&out)).unwrap();
        fs::write(root.join("whole"), out.inside()).unwrap();
        assert_eq!(
            cut_names(&root, out.hash()).unwrap(),
            [root.join("libglib.so")]
        );
        assert!(split_pieces(&root, out.hash()).unwrap().is_empty());

        let address = address(&lab, &out).unwrap();
        let moved = lab.on_host(&address);
        let bytes = fs::read(moved.join("libglib.so")).unwrap();
        let digest = |path: &StorePath| path.hash()[..HASH_LENGTH].to_owned();
        assert!(memmem::find(&bytes, digest(&out).as_bytes()).is_none());
        assert!(memmem::find(&bytes, digest(&address).as_bytes()).is_some());
        assert_eq!(
            fs::read_to_string(moved.join("whole")).unwrap(),
            address.inside()
        );
    }

    #[test]
    fn a_cut_apart_path_gets_the_same_address_wherever_it_was_built() {
        let (_dir, lab) = lab();
        let place = |seed: &[u8]| {
            let out = StorePath::derive("test", &[seed]).named("glib-2.84.4");
            let root = lab.on_host(&out);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("libglib.so"), cut_apart(&out)).unwrap();
            address(&lab, &out).unwrap()
        };
        assert_eq!(place(b"first"), place(b"second"));
    }

    fn built_at(lab: &Lab, seed: &[u8], text: &str) -> StorePath {
        let out = StorePath::derive("test", &[seed]);
        let root = lab.on_host(&out);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(
            root.join("bin/tool"),
            format!("{}/lib {text}", out.inside()),
        )
        .unwrap();
        symlink(format!("{}/bin/tool", out.inside()), root.join("bin/link")).unwrap();
        fs::create_dir_all(root.join(format!("share/{}", out.hash()))).unwrap();
        fs::write(root.join(format!("share/{}/note", out.hash())), "note").unwrap();
        out
    }

    fn lab() -> (tempfile::TempDir, Lab) {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        (dir, lab)
    }

    #[test]
    fn the_same_contents_get_the_same_address_wherever_they_were_built() {
        let (_dir, lab) = lab();
        let one = built_at(&lab, b"first", "same");
        let two = built_at(&lab, b"second", "same");
        assert_ne!(one, two);
        let (a, b) = (address(&lab, &one).unwrap(), address(&lab, &two).unwrap());
        assert_eq!(a, b);
        assert!(lab.on_host(&a).exists());
        assert!(!lab.on_host(&one).exists());
        assert!(!lab.on_host(&two).exists());
    }

    #[test]
    fn a_file_holding_the_mask_is_not_a_file_holding_the_path() {
        let (_dir, lab) = lab();
        let own = StorePath::derive("test", &[b"own"]);
        let root = lab.on_host(&own);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("f"), own.hash()).unwrap();
        let with_path = content_hash(&root, own.hash()).unwrap();
        fs::write(root.join("f"), "0".repeat(HASH_LENGTH)).unwrap();
        let with_mask = content_hash(&root, own.hash()).unwrap();
        assert_ne!(with_path, with_mask);
    }

    #[test]
    fn an_empty_directory_and_a_files_permissions_change_the_address() {
        let (_dir, lab) = lab();
        let own = StorePath::derive("test", &[b"own"]);
        let root = lab.on_host(&own);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("key"), "k").unwrap();
        let hash = || content_hash(&root, own.hash()).unwrap();
        let plain = hash();
        fs::create_dir(root.join("empty")).unwrap();
        let with_directory = hash();
        assert_ne!(with_directory, plain);
        fs::set_permissions(root.join("key"), fs::Permissions::from_mode(0o600)).unwrap();
        assert_ne!(hash(), with_directory);
    }

    #[test]
    fn different_contents_get_different_addresses() {
        let (_dir, lab) = lab();
        let one = built_at(&lab, b"first", "one");
        let two = built_at(&lab, b"second", "two");
        assert_ne!(address(&lab, &one).unwrap(), address(&lab, &two).unwrap());
    }

    #[test]
    fn nothing_mentions_the_temporary_path_afterwards() {
        let (_dir, lab) = lab();
        let out = built_at(&lab, b"first", "text");
        let final_path = address(&lab, &out).unwrap();
        let root = lab.on_host(&final_path);
        let text = fs::read_to_string(root.join("bin/tool")).unwrap();
        assert_eq!(text, format!("{}/lib text", final_path.inside()));
        assert_eq!(
            fs::read_link(root.join("bin/link")).unwrap(),
            PathBuf::from(format!("{}/bin/tool", final_path.inside()))
        );
        assert!(
            root.join(format!("share/{}/note", final_path.hash()))
                .exists()
        );
        assert!(!root.join(format!("share/{}", out.hash())).exists());
    }

    #[test]
    fn a_named_package_keeps_its_name_and_its_address_needs_the_same_contents() {
        let (_dir, lab) = lab();
        let built = |seed: &[u8], name: &str| {
            let out = StorePath::derive("test", &[seed]).named(name);
            let root = lab.on_host(&out);
            fs::create_dir_all(root.join("bin")).unwrap();
            fs::write(root.join("bin/tool"), format!("{}/lib", out.inside())).unwrap();
            out
        };
        let one = address(&lab, &built(b"first", "jq-1.8.1")).unwrap();
        let two = address(&lab, &built(b"second", "jq-1.8.1")).unwrap();
        assert_eq!(one, two, "where it was built doesn't matter");
        assert_eq!(one.name(), Some("jq-1.8.1"));
        assert_eq!(one.hash().len(), HASH_LENGTH + "-jq-1.8.1".len());
        let text = fs::read_to_string(lab.on_host(&one).join("bin/tool")).unwrap();
        assert_eq!(text, format!("{}/lib", one.inside()));
        let other = address(&lab, &built(b"third", "jq-1.8.2")).unwrap();
        assert_ne!(
            one.digest(),
            other.digest(),
            "the name is part of what's hashed"
        );
    }

    #[test]
    fn the_address_does_not_change_on_moving() {
        let (_dir, lab) = lab();
        let out = built_at(&lab, b"first", "text");
        let final_path = address(&lab, &out).unwrap();
        assert_eq!(address(&lab, &final_path).unwrap(), final_path);
    }
}
