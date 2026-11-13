//! Keeping chosen paths across boots on a machine whose root is memory. Each kept directory or
//! file lives under the state path and is mounted where it belongs. A kept file that doesn't
//! exist yet is a link there, so whatever makes it writes it to the right place. Something that
//! can't be kept says so and the others carry on, so one bad path can't stop a machine booting.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown, lchown, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use wrath_eval::Value;
use wrath_model::Model;

/// Under the state path: the users and groups the machine has, since /etc starts empty.
const ACCOUNTS: &str = "var/lib/wrath/accounts";
const ACCOUNT_FILES: [&str; 3] = ["passwd", "group", "shadow"];
const MACHINE_ID: &str = "/etc/machine-id";

/// What a system keeps, as its `state.star` module wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// Where things are kept, as an absolute path.
    path: String,
    directories: Vec<String>,
    files: Vec<String>,
    homes: Vec<Home>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Home {
    user: String,
    group: String,
    home: String,
    directories: Vec<String>,
    files: Vec<String>,
}

/// Who owns what's made for a kept path, when it isn't root.
type Owner = Option<(u32, u32)>;

fn entry<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Table(entries) => entries.get(key),
        _ => None,
    }
}

fn string(value: &Value) -> Option<String> {
    match value {
        Value::Str(text) => Some(text.clone()),
        _ => None,
    }
}

fn text(value: &Value, key: &str) -> Option<String> {
    entry(value, key).and_then(string)
}

fn texts(value: &Value, key: &str) -> Vec<String> {
    match entry(value, key) {
        Some(Value::List(items)) => items.iter().filter_map(string).collect(),
        _ => Vec::new(),
    }
}

fn under(path: &str) -> &str {
    path.trim_start_matches('/')
}

/// The mount points in `mountinfo`, which writes a space in a path as `\040`.
fn mount_points(mountinfo: &str) -> BTreeSet<PathBuf> {
    mountinfo
        .lines()
        .filter_map(|line| line.split(' ').nth(4))
        .map(|path| PathBuf::from(path.replace("\\040", " ")))
        .collect()
}

fn mounted() -> BTreeSet<PathBuf> {
    mount_points(&fs::read_to_string("/proc/self/mountinfo").unwrap_or_default())
}

/// Makes `dir` and the directories above it down from `from`, giving new ones `owner`.
pub(crate) fn make_dirs(from: &Path, dir: &Path, owner: Owner) -> Result<(), String> {
    let mut missing = Vec::new();
    let mut at = dir;
    while at.starts_with(from) && at != from && !at.exists() {
        missing.push(at.to_path_buf());
        at = at.parent().unwrap_or(from);
    }
    for path in missing.iter().rev() {
        fs::create_dir(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Some((uid, gid)) = owner {
            chown(path, Some(uid), Some(gid)).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        // Explicit, as there's no umask yet at boot. systemd refuses a private state directory
        // that others can look into.
        let mode = if path.ends_with("var/lib/private") {
            0o700
        } else {
            0o755
        };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

fn say(result: Result<(), String>) {
    if let Err(message) = result {
        eprintln!("wrath: keeping state: {message}");
    }
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", to.display()))
}

fn ok(command: &mut Command, what: &str) -> Result<(), String> {
    match command.output() {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!(
            "{what}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(e) => Err(format!("{what}: {e}")),
    }
}

/// Where kept things are mounted from and to, on one machine's root.
struct Places<'a> {
    /// The root being set up, which is `/` except in tests.
    root: &'a Path,
    /// The state path under that root.
    keep: PathBuf,
    /// The state path as the running machine sees it, which links point to.
    keep_absolute: &'a str,
    mount: PathBuf,
    /// At boot nothing is on the path yet, so programs are named by where the system has them.
    cp: PathBuf,
    mounted: BTreeSet<PathBuf>,
}

impl Places<'_> {
    fn bind(&self, source: &Path, target: &Path) -> Result<(), String> {
        if self.mounted.contains(target) {
            return Ok(());
        }
        ok(
            Command::new(&self.mount)
                .arg("--bind")
                .arg(source)
                .arg(target),
            &format!("mounting {}", target.display()),
        )
    }

    /// Keeps the directory at `path`, relative to the root.
    fn directory(&self, path: &str, owner: Owner) -> Result<(), String> {
        let source = self.keep.join(path);
        let target = self.root.join(path);
        let new = !source.exists();
        make_dirs(&self.keep, &source, owner)?;
        make_dirs(self.root, &target, owner)?;
        if new {
            // Whatever the directory already holds, when it's only now being kept.
            ok(
                Command::new(&self.cp)
                    .arg("-a")
                    .arg(target.join("."))
                    .arg(&source),
                &format!("copying {}", target.display()),
            )?;
            let metadata =
                fs::metadata(&target).map_err(|e| format!("{}: {e}", target.display()))?;
            chown(&source, Some(metadata.uid()), Some(metadata.gid()))
                .map_err(|e| format!("{}: {e}", source.display()))?;
            fs::set_permissions(&source, metadata.permissions())
                .map_err(|e| format!("{}: {e}", source.display()))?;
        }
        self.bind(&source, &target)
    }

    /// Keeps the file at `path`, relative to the root.
    fn file(&self, path: &str, owner: Owner) -> Result<(), String> {
        let source = self.keep.join(path);
        let target = self.root.join(path);
        make_dirs(&self.keep, source.parent().unwrap_or(&self.keep), owner)?;
        make_dirs(self.root, target.parent().unwrap_or(self.root), owner)?;
        let at_target = fs::symlink_metadata(&target).ok();
        if !source.exists() {
            match &at_target {
                Some(metadata) if metadata.is_file() => {
                    copy(&target, &source)?;
                    if let Some((uid, gid)) = owner {
                        chown(&source, Some(uid), Some(gid))
                            .map_err(|e| format!("{}: {e}", source.display()))?;
                    }
                }
                Some(_) => return Ok(()),
                None => {
                    let link = Path::new(self.keep_absolute).join(path);
                    symlink(link, &target).map_err(|e| format!("{}: {e}", target.display()))?;
                    if let Some((uid, gid)) = owner {
                        lchown(&target, Some(uid), Some(gid))
                            .map_err(|e| format!("{}: {e}", target.display()))?;
                    }
                    return Ok(());
                }
            }
        }
        // A link from before the file existed, or nothing yet.
        if at_target.is_some_and(|metadata| metadata.is_symlink()) {
            fs::remove_file(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        }
        if !target.exists() {
            fs::File::create(&target).map_err(|e| format!("{}: {e}", target.display()))?;
        }
        self.bind(&source, &target)
    }
}

/// Gives a machine that keeps its id one at its first boot. systemd won't write through a link
/// to a file that isn't there yet, and makes up a new id each boot instead.
fn first_machine_id(keep: &Path) -> Result<(), String> {
    let file = keep.join(under(MACHINE_ID));
    if file.exists() {
        return Ok(());
    }
    make_dirs(keep, file.parent().expect("the id is in /etc"), None)?;
    let uuid = fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map_err(|e| format!("making a machine id: {e}"))?;
    fs::write(&file, format!("{}\n", uuid.trim().replace('-', "")))
        .and_then(|()| fs::set_permissions(&file, fs::Permissions::from_mode(0o444)))
        .map_err(|e| format!("{}: {e}", file.display()))
}

impl State {
    /// What `model` keeps, if its root is erased at every boot.
    pub fn of(model: &Model) -> Option<Self> {
        let state = model.base.get("state")?;
        let homes = match entry(state, "homes") {
            Some(Value::List(homes)) => homes
                .iter()
                .filter_map(|home| {
                    Some(Home {
                        user: text(home, "user")?,
                        group: text(home, "group")?,
                        home: text(home, "home")?,
                        directories: texts(home, "directories"),
                        files: texts(home, "files"),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        Some(Self {
            path: text(state, "path")?,
            directories: texts(state, "directories"),
            files: texts(state, "files"),
            homes,
        })
    }

    fn places<'a>(&'a self, root: &'a Path, system: &Path) -> Places<'a> {
        let programs = system.join("bin");
        Places {
            root,
            keep: root.join(under(&self.path)),
            keep_absolute: &self.path,
            mount: programs.join("mount"),
            cp: programs.join("cp"),
            mounted: mounted(),
        }
    }

    /// Puts back the users and groups the machine had, so ids stay the same across boots.
    pub fn restore_accounts(&self, root: &Path) {
        let saved = root.join(under(&self.path)).join(ACCOUNTS);
        for name in ACCOUNT_FILES {
            let (from, to) = (saved.join(name), root.join("etc").join(name));
            if from.exists() && !to.exists() {
                say(copy(&from, &to));
            }
        }
    }

    /// Saves the machine's users and groups for its next boot.
    pub fn save_accounts(&self, root: &Path) {
        let keep = root.join(under(&self.path));
        let saved = keep.join(ACCOUNTS);
        say(make_dirs(&keep, &saved, None));
        for name in ACCOUNT_FILES {
            let (from, to) = (root.join("etc").join(name), saved.join(name));
            say(copy(&from, &to));
        }
    }

    /// Mounts what the system keeps, and stops keeping what `before` kept and this doesn't.
    /// What was kept stays under the state path.
    pub fn keep_system(&self, root: &Path, system: &Path, before: Option<&State>) {
        let places = self.places(root, system);
        for path in &self.directories {
            say(places.directory(under(path), None));
        }
        if self.files.iter().any(|path| path == MACHINE_ID) {
            say(first_machine_id(&places.keep));
        }
        for path in &self.files {
            say(places.file(under(path), None));
        }
        let Some(before) = before else {
            return;
        };
        let umount = system.join("bin/umount");
        for path in before.directories.iter().chain(&before.files) {
            let target = root.join(under(path));
            let dropped = !self.directories.contains(path) && !self.files.contains(path);
            if dropped && places.mounted.contains(&target) {
                say(ok(
                    Command::new(&umount).arg("--lazy").arg(&target),
                    &format!("unmounting {}", target.display()),
                ));
            }
        }
    }

    /// Mounts what users keep in their homes, which have to exist by now.
    pub fn keep_homes(&self, system: &Path) {
        let places = self.places(Path::new("/"), system);
        let users = crate::activate::ids(Path::new("/etc/passwd"));
        let groups = crate::activate::ids(Path::new("/etc/group"));
        for home in &self.homes {
            let (Some(uid), Some(gid)) = (users.get(&home.user), groups.get(&home.group)) else {
                eprintln!("wrath: keeping state: there's no user `{}`", home.user);
                continue;
            };
            let owner = Some((*uid, *gid));
            let at = |path: &str| format!("{}/{path}", under(&home.home));
            let kept_home = places.keep.join(under(&home.home));
            say(make_dirs(&places.keep, &kept_home, owner));
            // A home is private, and so is what's kept from it.
            say(
                fs::set_permissions(&kept_home, fs::Permissions::from_mode(0o700))
                    .map_err(|e| format!("{}: {e}", kept_home.display())),
            );
            for path in &home.directories {
                say(places.directory(&at(path), owner));
            }
            for path in &home.files {
                say(places.file(&at(path), owner));
            }
        }
    }

    /// Moves what an install wrote to a root that's about to vanish to where it's kept.
    pub fn move_into_keep(&self, target: &Path) -> Result<(), String> {
        let keep = target.join(under(&self.path));
        for path in self.directories.iter().chain(&self.files) {
            let from = target.join(under(path));
            if !from.exists() {
                continue;
            }
            let to = keep.join(under(path));
            let parent = to.parent().expect("a kept path is below the state path");
            make_dirs(&keep, parent, None)?;
            ok(
                Command::new("cp").arg("-aT").arg(&from).arg(&to),
                &format!("keeping {}", from.display()),
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points_are_read_with_their_spaces() {
        let info = "36 35 98:0 /a /keep rw - ext3 /dev/root rw\n\
                    37 35 98:0 /b /home/j/My\\040Games rw - ext3 /dev/root rw\n";

        assert_eq!(
            mount_points(info),
            BTreeSet::from([PathBuf::from("/keep"), PathBuf::from("/home/j/My Games")])
        );
    }

    #[test]
    fn a_machine_id_is_made_once() {
        let dir = tempfile::tempdir().unwrap();

        first_machine_id(dir.path()).unwrap();
        let id = fs::read_to_string(dir.path().join("etc/machine-id")).unwrap();
        first_machine_id(dir.path()).unwrap();

        assert_eq!(id.trim().len(), 32);
        assert_eq!(
            fs::read_to_string(dir.path().join("etc/machine-id")).unwrap(),
            id
        );
    }

    #[test]
    fn only_a_model_with_a_state_path_keeps_anything() {
        assert_eq!(State::of(&Model::default()), None);
    }

    #[test]
    fn dirs_are_made_only_below_where_they_start() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("var/lib/private/x");

        make_dirs(dir.path(), &deep, None).unwrap();

        assert!(deep.is_dir());
        let shared = fs::metadata(dir.path().join("var")).unwrap().mode();
        assert_eq!(shared & 0o777, 0o755);
        let mode = fs::metadata(dir.path().join("var/lib/private"))
            .unwrap()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    fn places(root: &Path) -> Places<'_> {
        Places {
            root,
            keep: root.join("keep"),
            keep_absolute: "/keep",
            mount: PathBuf::from("/does/not/run"),
            cp: PathBuf::from("cp"),
            // Mounting is done already, so nothing runs in these tests.
            mounted: BTreeSet::from([root.join("var/lib/a"), root.join("etc/key")]),
        }
    }

    #[test]
    fn a_directory_being_kept_takes_what_it_already_holds() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("keep")).unwrap();
        fs::create_dir_all(dir.path().join("var/lib/a")).unwrap();
        fs::write(dir.path().join("var/lib/a/old"), "old").unwrap();

        places(dir.path()).directory("var/lib/a", None).unwrap();

        let kept = dir.path().join("keep/var/lib/a/old");
        assert_eq!(fs::read_to_string(kept).unwrap(), "old");
    }

    #[test]
    fn a_file_that_doesnt_exist_yet_is_a_link_to_where_its_kept() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("keep")).unwrap();

        places(dir.path()).file("etc/key", None).unwrap();

        let link = fs::read_link(dir.path().join("etc/key")).unwrap();
        assert_eq!(link, Path::new("/keep/etc/key"));
    }

    #[test]
    fn a_file_kept_earlier_replaces_its_link() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("keep/etc")).unwrap();
        fs::create_dir_all(dir.path().join("etc")).unwrap();
        fs::write(dir.path().join("keep/etc/key"), "secret").unwrap();
        symlink("/keep/etc/key", dir.path().join("etc/key")).unwrap();

        places(dir.path()).file("etc/key", None).unwrap();

        let metadata = fs::symlink_metadata(dir.path().join("etc/key")).unwrap();
        assert!(metadata.is_file());
    }
}
