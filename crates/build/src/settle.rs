//! What's done to a package right after its build, before anything else: modes settled so
//! nothing is setuid, libraries checked to be found by name, and its runtime dependencies written
//! in. A package that stays at its build's path, as boot packages do, has [`rules`] in its key,
//! since its dependents couldn't otherwise tell this changed. Its tests are in `fixup.rs`, so
//! changing them leaves [`rules`] as it is.

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::chain::{Built, Package, Prepared};
use crate::fixup::files_under;
use crate::runpath;
use crate::store::{Lab, StorePath, with_path};

/// A digest of this module's code, which changes whenever what it does to a package might.
pub(crate) fn rules() -> &'static str {
    static RULES: OnceLock<String> = OnceLock::new();
    RULES.get_or_init(|| crate::store::hex(&Sha256::digest(include_str!("settle.rs"))))
}

/// Where a package keeps the store paths of its `runtime_deps`.
const RUNTIME_DEPS: &str = "lib/wrath/runtime-deps";

/// The quick part of [`fix`], which needs what's built: modes settled, libraries checked and
/// the package's runtime dependencies written in.
pub(crate) fn settle(
    lab: &Lab,
    package: &Package,
    prepared: &Prepared,
    built: &Built,
) -> Result<(), String> {
    let root = lab.on_host(&prepared.out);
    settle_modes(&root).map_err(|e| format!("`{}` installed {e}", package.id))?;
    if let Some((file, library)) = linked_by_path(&root)? {
        return Err(format!(
            "`{}`'s {} needs `{library}` by its path, which only the build has: the library has \
             no SONAME, so its recipe should give it one",
            package.id,
            file.display()
        ));
    }
    write_runtime_deps(lab, package, &prepared.out, built)
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
