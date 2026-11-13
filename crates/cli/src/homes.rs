//! Users' files on the machine. A system gives each user files, their units among them, under
//! `users/NAME/files`, and each one is linked from the user's home through `/run/system`, so a
//! switch moves all of them at once. Links an earlier system made that this one doesn't have are
//! removed. A file of the user's own where a link would go is left alone, and so is one a program
//! put in place of a link, which is said whenever it's seen.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{lchown, symlink};
use std::path::{Path, PathBuf};

use crate::trees::at;

/// Where distro keeps the list of what it linked into each home.
pub const HOME_RECORDS: &str = "/var/lib/distro/homes";

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

/// What's in the home where one of the system's files goes.
enum Found {
    Missing,
    /// A link of the system's.
    Ours,
    /// A file in place of a link the system made, as the system has it.
    Same,
    /// A file in place of a link the system made, which differs from the system's: a program
    /// saved its own over the link.
    Replaced,
    /// The user's own, there before the system linked anything.
    Own,
}

fn found(
    path: &Path,
    relative: &Path,
    files: &Files<'_>,
    linked_before: &BTreeSet<PathBuf>,
) -> Found {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Found::Missing;
    };
    if ours(path, files.through) {
        return Found::Ours;
    }
    if !linked_before.contains(relative) {
        return Found::Own;
    }
    let system = fs::read(files.source.join(relative));
    if meta.is_file() && system.is_ok_and(|system| fs::read(path).is_ok_and(|own| own == system)) {
        Found::Same
    } else {
        Found::Replaced
    }
}

fn read_record(record: &Path) -> BTreeSet<PathBuf> {
    fs::read_to_string(record)
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .collect()
}

/// Links `files` into the home under `root`, keeping what it linked in `record`, and returns
/// notes about files it left alone.
pub fn link(root: &Path, files: &Files<'_>, record: &Path) -> Result<Vec<String>, String> {
    let home = under(root, files.home);
    let wanted = crate::activate::files_in(files.source).unwrap_or_default();
    let before = read_record(record);
    for gone in before.difference(&wanted) {
        let path = home.join(gone);
        if ours(&path, files.through) {
            fs::remove_file(&path).map_err(at(&path))?;
        }
    }
    let mut notes = Vec::new();
    let mut linked = BTreeSet::new();
    for relative in &wanted {
        let path = home.join(relative);
        let target = files.through.join(relative);
        if let Some(parent) = path.parent() {
            crate::state::make_dirs(&home, parent, files.owner)?;
        }
        match found(&path, relative, files, &before) {
            Found::Missing => {}
            Found::Ours if fs::read_link(&path).is_ok_and(|now| now == target) => {
                linked.insert(relative);
                continue;
            }
            Found::Ours | Found::Same => fs::remove_file(&path).map_err(at(&path))?,
            Found::Replaced => {
                notes.push(replaced_note(files, relative));
                // Still the system's, so it's said again until it's settled.
                linked.insert(relative);
                continue;
            }
            Found::Own => {
                notes.push(format!(
                    "{}'s ~/{} is their own, so the system's is left out",
                    files.user,
                    relative.display()
                ));
                continue;
            }
        }
        symlink(&target, &path).map_err(at(&path))?;
        if let Some((uid, gid)) = files.owner {
            lchown(&path, Some(uid), Some(gid)).map_err(at(&path))?;
        }
        linked.insert(relative);
    }
    if let Some(parent) = record.parent() {
        fs::create_dir_all(parent).map_err(at(parent))?;
    }
    let listed: String = linked
        .iter()
        .map(|relative| format!("{}\n", relative.display()))
        .collect();
    fs::write(record, listed).map_err(at(record))?;
    Ok(notes)
}

/// The system's files in the home under `root` that a program saved its own over, as notes on
/// how to settle each.
pub fn replaced(root: &Path, files: &Files<'_>, record: &Path) -> Vec<String> {
    let home = under(root, files.home);
    let before = read_record(record);
    before
        .iter()
        .filter(|relative| files.source.join(relative).exists())
        .filter(|relative| {
            matches!(
                found(&home.join(relative), relative, files, &before),
                Found::Replaced
            )
        })
        .map(|relative| replaced_note(files, relative))
        .collect()
}

/// [`replaced`] for every home the running system under `root` linked files into.
pub fn replaced_in_homes(root: &Path) -> Vec<String> {
    let records = under(root, Path::new(HOME_RECORDS));
    let passwd = fs::read_to_string(root.join("etc/passwd")).unwrap_or_default();
    let mut notes = Vec::new();
    for line in passwd.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        let (Some(user), Some(home)) = (fields.first(), fields.get(5)) else {
            continue;
        };
        let record = records.join(user);
        if !record.exists() {
            continue;
        }
        let through = through_running(user);
        let source = under(root, &through);
        let files = Files {
            user,
            home: Path::new(home),
            owner: None,
            source: &source,
            through: &through,
        };
        notes.extend(replaced(root, &files, &record));
    }
    notes
}

fn replaced_note(files: &Files<'_>, relative: &Path) -> String {
    let path = relative.display();
    format!(
        "{}'s ~/{path} was replaced by a file of its own, likely by the program that reads it, \
         so the system's isn't used. `diff ~/{path} {}` shows how they differ; take what's \
         wanted into the configuration, then `mv ~/{path} ~/{path}.old` and apply again",
        files.user,
        files.through.join(relative).display()
    )
}

/// Where links to `user`'s files point: their files in the running system.
pub fn through_running(user: &str) -> PathBuf {
    Path::new("/")
        .join(crate::activate::RUNNING)
        .join("users")
        .join(user)
        .join("files")
}

/// The absolute `path` as it is under `root`.
fn under(root: &Path, path: &Path) -> PathBuf {
    root.join(path.strip_prefix("/").unwrap_or(path))
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

    /// A home with the system's `.config/app/config` linked, and what linking it needs.
    struct Linked {
        _dir: tempfile::TempDir,
        root: PathBuf,
        source: PathBuf,
        record: PathBuf,
    }

    impl Linked {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("root");
            let source = root.join("run/system/users/john/files");
            write(&source.join(".config/app/config"), "the system's\n");
            write(
                &root.join("etc/passwd"),
                "john:x:1000:100::/home/john:/bin/sh\n",
            );
            fs::create_dir_all(root.join("home/john")).unwrap();
            let record = root.join("var/lib/distro/homes/john");
            let linked = Linked {
                _dir: dir,
                root,
                source,
                record,
            };
            assert!(linked.link().is_empty());
            linked
        }

        fn files(&self) -> Files<'_> {
            Files {
                user: "john",
                home: Path::new("/home/john"),
                owner: None,
                source: &self.source,
                through: Path::new("/run/system/users/john/files"),
            }
        }

        fn link(&self) -> Vec<String> {
            link(&self.root, &self.files(), &self.record).unwrap()
        }

        fn file(&self) -> PathBuf {
            self.root.join("home/john/.config/app/config")
        }

        /// What a program that saves by renaming over its file does.
        fn save_over(&self, text: &str) {
            let new = self.file().with_extension("new");
            fs::write(&new, text).unwrap();
            fs::rename(&new, self.file()).unwrap();
        }
    }

    #[test]
    fn a_link_a_program_saved_over_is_left_and_said_until_it_is_settled() {
        let home = Linked::new();
        home.save_over("the program's\n");

        let notes = home.link();

        assert_eq!(fs::read_to_string(home.file()).unwrap(), "the program's\n");
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].starts_with("john's ~/.config/app/config was replaced"),
            "{notes:?}"
        );
        assert!(notes[0].contains("mv ~/.config/app/config ~/.config/app/config.old"));
        assert_eq!(home.link(), notes, "said again at the next switch");
        assert_eq!(replaced_in_homes(&home.root), notes, "and by status");

        fs::rename(home.file(), home.file().with_extension("old")).unwrap();
        assert!(home.link().is_empty());
        assert!(
            fs::read_link(home.file()).is_ok(),
            "the system's is linked again"
        );
        assert!(replaced_in_homes(&home.root).is_empty());
    }

    #[test]
    fn a_link_a_program_saved_over_with_the_systems_text_is_linked_again() {
        let home = Linked::new();
        home.save_over("the system's\n");

        assert!(home.link().is_empty());
        assert!(fs::read_link(home.file()).is_ok());
    }

    #[test]
    fn a_users_own_file_is_not_taken_for_one_a_program_replaced() {
        let home = Linked::new();
        fs::remove_file(home.file()).unwrap();
        fs::remove_file(&home.record).unwrap();
        write(&home.file(), "mine\n");

        let notes = home.link();

        assert_eq!(
            notes,
            ["john's ~/.config/app/config is their own, so the system's is left out"]
        );
        assert!(replaced_in_homes(&home.root).is_empty());
        assert_eq!(
            fs::read_to_string(&home.record).unwrap(),
            "",
            "only links are recorded"
        );
    }

    #[test]
    fn a_replaced_file_the_system_no_longer_has_is_left_and_not_said() {
        let home = Linked::new();
        home.save_over("the program's\n");
        fs::remove_file(home.source.join(".config/app/config")).unwrap();

        assert!(home.link().is_empty());
        assert_eq!(fs::read_to_string(home.file()).unwrap(), "the program's\n");
    }
}
