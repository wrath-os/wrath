//! Users' files on the machine. A system gives each user files, their units among them, under
//! `users/NAME/files`, and each one is linked from the user's home through `/run/system`, so a
//! switch moves all of them at once. Links an earlier system made that this one doesn't have are
//! removed. A file of the user's own where a link would go is left alone.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{lchown, symlink};
use std::path::{Path, PathBuf};

use crate::trees::at;

/// One user's files to link: where they are in the system on this machine, and where links to
/// them point, which is the same place through `/run/system`.
pub struct Files<'a> {
    pub user: &'a str,
    /// The user's home, as their account names it.
    pub home: &'a Path,
    pub owner: Option<(u32, u32)>,
    /// The user's files in the system being activated.
    pub source: &'a Path,
    /// What links point at, such as `/run/system/users/john/files`.
    pub through: &'a Path,
}

/// Links `files` into the home under `root`, keeping what it linked in `record`, and returns
/// notes about files it left alone.
pub fn link(root: &Path, files: &Files<'_>, record: &Path) -> Result<Vec<String>, String> {
    let home = root.join(files.home.strip_prefix("/").unwrap_or(files.home));
    let wanted = crate::activate::files_in(files.source).unwrap_or_default();
    let before: BTreeSet<PathBuf> = fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .collect();
    for gone in before.difference(&wanted) {
        let path = home.join(gone);
        if ours(&path, files.through) {
            fs::remove_file(&path).map_err(at(&path))?;
        }
    }
    let mut notes = Vec::new();
    for relative in &wanted {
        let path = home.join(relative);
        let target = files.through.join(relative);
        if let Some(parent) = path.parent() {
            crate::state::make_dirs(&home, parent, files.owner)?;
        }
        match fs::symlink_metadata(&path) {
            Ok(_) if ours(&path, files.through) => {
                if fs::read_link(&path).is_ok_and(|now| now == target) {
                    continue;
                }
                fs::remove_file(&path).map_err(at(&path))?;
            }
            Ok(_) => {
                notes.push(format!(
                    "{}'s ~/{} is their own, so the system's is left out",
                    files.user,
                    relative.display()
                ));
                continue;
            }
            Err(_) => {}
        }
        symlink(&target, &path).map_err(at(&path))?;
        if let Some((uid, gid)) = files.owner {
            lchown(&path, Some(uid), Some(gid)).map_err(at(&path))?;
        }
    }
    if let Some(parent) = record.parent() {
        fs::create_dir_all(parent).map_err(at(parent))?;
    }
    let listed: String = wanted
        .iter()
        .map(|relative| format!("{}\n", relative.display()))
        .collect();
    fs::write(record, listed).map_err(at(record))?;
    Ok(notes)
}

/// Whether `path` is a link a system made, pointing into `through`.
fn ours(path: &Path, through: &Path) -> bool {
    fs::read_link(path).is_ok_and(|target| target.starts_with(through))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn a_users_files_are_linked_and_what_a_system_drops_goes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let source = dir.path().join("system/users/john/files");
        write(&source.join(".config/git/config"), "[user]\n");
        write(
            &source.join(".config/systemd/user/sync.service"),
            "[Service]\n",
        );
        write(&root.join("home/john/.bashrc"), "mine\n");
        write(&source.join(".bashrc"), "the system's\n");
        let files = Files {
            user: "john",
            home: Path::new("/home/john"),
            owner: None,
            source: &source,
            through: Path::new("/run/system/users/john/files"),
        };
        let record = root.join("var/lib/distro/homes/john");

        let notes = link(&root, &files, &record).unwrap();

        let home = root.join("home/john");
        assert_eq!(
            fs::read_link(home.join(".config/git/config")).unwrap(),
            Path::new("/run/system/users/john/files/.config/git/config")
        );
        assert!(fs::symlink_metadata(home.join(".config/systemd/user/sync.service")).is_ok());
        assert_eq!(fs::read_to_string(home.join(".bashrc")).unwrap(), "mine\n");
        assert_eq!(
            notes,
            ["john's ~/.bashrc is their own, so the system's is left out"]
        );

        fs::remove_file(source.join(".config/git/config")).unwrap();
        link(&root, &files, &record).unwrap();
        assert!(
            fs::symlink_metadata(home.join(".config/git/config")).is_err(),
            "a file the system no longer has is unlinked"
        );
        assert!(home.join(".bashrc").exists(), "the user's own stays");
        let listed = fs::read_to_string(&record).unwrap();
        assert!(
            listed.contains(".config/systemd/user/sync.service"),
            "{listed}"
        );
    }
}
