//! The records a builder keeps beside its store about the paths in it (decision 0021): which build
//! made each path, in `fixed/`, what each path refers to, in `refs/`, which build each recipe was
//! last found as, in `hints/` (0041), what each of a system's actions made, in `actions/` with
//! the files in `e2e/objects/`, and which builds each system was made from, in `builds/`. A
//! path's name is the hash of what's in it, and a package's key is worked out from the paths of
//! what it's built with, so a machine needs the records of a system's whole build, including
//! tools it never has in its store, to tell it already has what a key asks for. Whatever copies a
//! system into another store, such as an image, `install` or `apply --host`, brings them along.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::store::{Lab, StorePath, with_path};

/// Where builds are recorded, under the work directory: a file per build named by its key and
/// the rules that fixed it up, listing its outputs as `out=PATH lib=PATH`, or empty for a build
/// that stays at its key's path.
const FIXED: &str = "fixed";

/// Where what each path refers to is recorded, under the work directory.
const REFS: &str = "refs";

/// Where each unresolved key's build is named, under the work directory: a file named by the key
/// holding the resolved key it was last worked out as. Only a record for that resolved key makes
/// it a build this store has, so a hint for one that never finished misleads nobody.
const HINTS: &str = "hints";

/// Where what each action of a system made is kept, under the work directory: a file named by
/// its resolved key, holding the files it made, as the action database has them. A machine
/// installed from an image has no database yet, and without these it would run every action again,
/// reading packages, such as the initrd's, it has no other use for.
const ACTIONS: &str = "actions";

/// Where the files actions made are kept, under the work directory, each named by its hash.
const OBJECTS: &str = "e2e/objects";

/// Where each system's builds are listed, under the work directory: a file named by the system,
/// with the `fixed/` record and the hint of every package its build reached, and what each of its
/// actions made with the files that names, one a line. It lives beside the
/// system, not in it, so tools rebuilt the same don't change the system.
const BUILDS: &str = "builds";

/// Says that the build with the `unresolved` key was worked out as `resolved`. A store that can't
/// be written to, as on a machine for a plan without root, only misses the shortcut next time.
pub fn hint(lab: &Lab, unresolved: &str, resolved: &str) {
    let path = lab.work.join(HINTS).join(unresolved);
    if fs::read_to_string(&path).is_ok_and(|known| known == resolved) {
        return;
    }
    let _ = fs::create_dir_all(lab.work.join(HINTS)).and_then(|()| replace(&path, resolved));
}

/// Writes `text` to `path` by way of a file beside it, so no reader sees it half written.
fn replace(path: &Path, text: &str) -> std::io::Result<()> {
    let partial = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&partial, text)?;
    fs::rename(&partial, path)
}

/// The resolved key the build with the `unresolved` key was last worked out as, if any.
pub fn hinted(lab: &Lab, unresolved: &str) -> Option<String> {
    let text = fs::read_to_string(lab.work.join(HINTS).join(unresolved)).ok()?;
    let resolved = text.trim();
    (!resolved.is_empty() && !resolved.contains('/')).then(|| resolved.to_owned())
}

/// What the action with the `resolved` key made, as kept beside the store, if it was.
pub fn action(lab: &Lab, resolved: &str) -> Option<String> {
    if resolved.contains('/') {
        return None;
    }
    fs::read_to_string(lab.work.join(ACTIONS).join(resolved)).ok()
}

/// Keeps which builds `system` was made from: the packages with these unresolved and resolved
/// keys, whose records are there, with their hints; and what each of its `actions`, by resolved
/// key, made, as their records and the files they name.
pub fn keep<'a>(
    lab: &Lab,
    system: &StorePath,
    keys: impl IntoIterator<Item = (&'a str, &'a str)>,
    actions: &BTreeMap<String, String>,
) -> Result<(), String> {
    let moved = &crate::fixup::rules()[..16];
    let mut records: Vec<String> = keys
        .into_iter()
        .flat_map(|(unresolved, resolved)| {
            [
                format!("{FIXED}/{resolved}-{moved}"),
                format!("{FIXED}/{resolved}-built"),
                format!("{HINTS}/{unresolved}"),
            ]
        })
        .filter(|record| lab.work.join(record).is_file())
        .map(|record| record + "\n")
        .collect();
    let dir = lab.work.join(ACTIONS);
    fs::create_dir_all(&dir).map_err(with_path(&dir))?;
    for (key, made) in actions {
        let path = dir.join(key);
        if !path.is_file() {
            replace(&path, made).map_err(with_path(&path))?;
        }
        records.push(format!("{ACTIONS}/{key}\n"));
        records.extend(
            objects(made)
                .into_iter()
                .map(|object| format!("{OBJECTS}/{object}\n"))
                .filter(|record| lab.work.join(record.trim_end()).is_file()),
        );
    }
    records.sort();
    records.dedup();
    let dir = lab.work.join(BUILDS);
    fs::create_dir_all(&dir).map_err(with_path(&dir))?;
    let path = dir.join(system.hash());
    replace(&path, &records.concat()).map_err(with_path(&path))
}

/// The records `lab` keeps of `system`, whose runtime closure is `closure`, relative to its work
/// directory: the list of its builds and each record there, and what each path in it refers to.
pub fn of(
    lab: &Lab,
    system: &StorePath,
    closure: &BTreeSet<StorePath>,
) -> Result<Vec<PathBuf>, String> {
    let list = Path::new(BUILDS).join(system.hash());
    let mut found = Vec::new();
    match fs::read_to_string(lab.work.join(&list)) {
        Ok(text) => {
            found.extend(text.lines().map(PathBuf::from).filter(|record| {
                [FIXED, HINTS, ACTIONS, OBJECTS]
                    .iter()
                    .any(|kind| record.starts_with(kind))
                    && lab.work.join(record).is_file()
            }));
            found.push(list);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(with_path(&lab.work.join(&list))(e)),
    }
    found.extend(
        closure
            .iter()
            .map(|path| Path::new(REFS).join(path.hash()))
            .filter(|record| lab.work.join(record).is_file()),
    );
    Ok(found)
}

/// The files an action's output, `made`, names: each file's object, by its hash.
fn objects(made: &str) -> Vec<String> {
    let Ok(serde_json::Value::Object(entries)) = serde_json::from_str(made) else {
        return Vec::new();
    };
    entries
        .values()
        .filter_map(|entry| entry["object"].as_str())
        .filter(|object| object.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
        .collect()
}

/// Copies `records`, relative to `lab`'s work directory, into the work directory `to`. One `to`
/// has already is left as it is.
pub fn copy(lab: &Lab, records: &[PathBuf], to: &Path) -> Result<(), String> {
    for record in records {
        let target = to.join(record);
        if target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(with_path(parent))?;
        }
        fs::copy(lab.work.join(record), &target).map_err(with_path(&target))?;
    }
    Ok(())
}

/// Copies the records `lab` keeps of `system`, whose runtime closure is `closure`, into the work
/// directory `to`.
pub fn copy_system(
    lab: &Lab,
    system: &StorePath,
    closure: &BTreeSet<StorePath>,
    to: &Path,
) -> Result<(), String> {
    copy(lab, &of(lab, system, closure)?, to)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZOXIDE_KEY: &str = "009d86b3c61ed545eda3da087b88052052561eec-zoxide-0.10.0";
    const ZOXIDE: &str = "de45825161bdcbd9afbc1c5a058404a301880397-zoxide-0.10.0";
    const GCC: &str = "078b6eca5534f4cdc032f30b40d85a818ac91bb3-gcc-stage1-15.2.0";
    const UNUSED: &str = "5555555555555555555555555555555555555555-jq-1.8";
    const SYSTEM: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-system";

    fn moved() -> &'static str {
        &crate::fixup::rules()[..16]
    }

    /// A work directory with records of zoxide, moved to its content's path, gcc, which stays at
    /// its key's, and a package no system uses.
    fn work() -> (tempfile::TempDir, Lab) {
        let dir = tempfile::tempdir().unwrap();
        let write = |record: &str, text: &str| {
            let path = dir.path().join(record);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(
            &format!("fixed/{ZOXIDE_KEY}-{}", moved()),
            &format!("out={ZOXIDE}\n"),
        );
        write(&format!("fixed/{GCC}-built"), "");
        write(&format!("fixed/{UNUSED}-built"), "");
        write(&format!("refs/{ZOXIDE}"), "");
        let lab = Lab::new(dir.path());
        (dir, lab)
    }

    fn system() -> StorePath {
        StorePath::from_hash(SYSTEM)
    }

    #[test]
    fn a_system_brings_its_whole_build_even_tools_it_does_not_carry() {
        let (_dir, lab) = work();
        hint(&lab, "u-zoxide", ZOXIDE_KEY);
        // gcc built zoxide; only zoxide is in the system.
        let gone = "0000000000000000000000000000000000000000-gone";
        keep(
            &lab,
            &system(),
            [("u-zoxide", ZOXIDE_KEY), ("u-gcc", GCC), ("u-gone", gone)],
            &BTreeMap::new(),
        )
        .unwrap();
        let closure = BTreeSet::from([StorePath::from_hash(ZOXIDE)]);
        assert_eq!(
            of(&lab, &system(), &closure).unwrap(),
            [
                PathBuf::from(format!("fixed/{ZOXIDE_KEY}-{}", moved())),
                PathBuf::from(format!("fixed/{GCC}-built")),
                PathBuf::from("hints/u-zoxide"),
                PathBuf::from(format!("builds/{SYSTEM}")),
                PathBuf::from(format!("refs/{ZOXIDE}")),
            ]
        );
    }

    #[test]
    fn a_system_built_before_its_builds_were_kept_brings_only_its_refs() {
        let (_dir, lab) = work();
        let closure = BTreeSet::from([StorePath::from_hash(ZOXIDE)]);
        assert_eq!(
            of(&lab, &system(), &closure).unwrap(),
            [PathBuf::from(format!("refs/{ZOXIDE}"))]
        );
    }

    #[test]
    fn copying_brings_what_the_other_store_lacks_and_leaves_what_it_has() {
        let (_dir, lab) = work();
        let keys = [("u-zoxide", ZOXIDE_KEY), ("u-gcc", GCC)];
        keep(&lab, &system(), keys, &BTreeMap::new()).unwrap();
        let to = tempfile::tempdir().unwrap();
        let mine = to.path().join(format!("refs/{ZOXIDE}"));
        fs::create_dir_all(mine.parent().unwrap()).unwrap();
        fs::write(&mine, "kept\n").unwrap();
        let closure = BTreeSet::from([StorePath::from_hash(ZOXIDE)]);
        copy_system(&lab, &system(), &closure, to.path()).unwrap();
        assert_eq!(fs::read_to_string(&mine).unwrap(), "kept\n");
        assert!(to.path().join(format!("fixed/{GCC}-built")).is_file());
        assert!(!to.path().join(format!("fixed/{UNUSED}-built")).exists());
        // The other store can pass them on in turn.
        let other = Lab::new(to.path());
        assert_eq!(of(&other, &system(), &closure).unwrap().len(), 4);
    }

    #[test]
    fn what_a_systems_actions_made_comes_along_with_the_files_it_names() {
        let (dir, lab) = work();
        let object = "abcdef0123456789abcdef0123456789abcdef01";
        let objects = dir.path().join("e2e/objects");
        fs::create_dir_all(&objects).unwrap();
        fs::write(objects.join(object), "[Unit]\n").unwrap();
        let made = format!(
            r#"{{"etc/systemd/system/a.service":{{"kind":"file","mode":420,"object":"{object}"}},"bin":{{"kind":"link","target":"/wrath/store/x"}}}}"#
        );
        let actions = BTreeMap::from([("action-key".to_owned(), made.clone())]);
        keep(&lab, &system(), [], &actions).unwrap();
        assert_eq!(action(&lab, "action-key").as_deref(), Some(made.as_str()));
        assert_eq!(action(&lab, "../builds"), None);
        assert_eq!(
            of(&lab, &system(), &BTreeSet::new()).unwrap(),
            [
                PathBuf::from("actions/action-key"),
                PathBuf::from(format!("e2e/objects/{object}")),
                PathBuf::from(format!("builds/{SYSTEM}")),
            ]
        );
    }

    #[test]
    fn a_hint_names_the_last_build_and_nothing_outside_the_store() {
        let (dir, lab) = work();
        assert_eq!(hinted(&lab, "u-zoxide"), None);
        hint(&lab, "u-zoxide", GCC);
        hint(&lab, "u-zoxide", ZOXIDE_KEY);
        assert_eq!(hinted(&lab, "u-zoxide").as_deref(), Some(ZOXIDE_KEY));
        fs::write(dir.path().join("hints/u-bad"), "../../etc/passwd").unwrap();
        assert_eq!(hinted(&lab, "u-bad"), None);
        // A store that can't be written to only goes without.
        hint(&Lab::new("/nonexistent/wrath-work"), "u-zoxide", ZOXIDE_KEY);
    }
}
