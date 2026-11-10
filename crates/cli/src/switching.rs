//! A machine's generations in distro's store: each one a link in `/store/.system/generations`
//! named by its number, `/store/.system/current` naming the current one, and the systems
//! themselves in `/store`. The records live with the store, so a root erased at every boot keeps
//! them.
//! Making one current and switching to it, trying one for a while, rolling back, and collecting
//! what no generation uses any more.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use distro_build::store::{Lab, StorePath};

use distro_backend as backend;

use crate::activate::{CURRENT, GENERATIONS, generations};

/// The root of the machine distro runs on.
const ROOT: &str = "/";

/// Whether this machine's generations are in distro's store. Machines from the Nix factory keep
/// them as a Nix profile until that's gone.
fn here_in_store() -> bool {
    in_store(Path::new(ROOT))
}

/// A system being tried, kept from collection while no generation has it.
const TRYING: &str = "store/.system/trying";

/// The timer that switches back from a system being tried.
const TRY_TIMER: &str = "distro-try";

/// One of the machine's generations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub number: u32,
    /// When it was made, as `2026-11-10 14:03:22` in UTC.
    pub created: String,
    pub current: bool,
    /// The hash of the model it was built from.
    pub model_hash: Option<String>,
}

impl std::fmt::Display for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let hash = self
            .model_hash
            .as_deref()
            .map_or("not built by distro", |hash| &hash[..hash.len().min(12)]);
        write!(f, "{:>4}  {}  model {hash}", self.number, self.created)?;
        if self.current {
            write!(f, "  (current)")?;
        }
        Ok(())
    }
}

/// Whether the machine at `root` keeps its generations in distro's store.
pub fn in_store(root: &Path) -> bool {
    root.join(GENERATIONS).is_dir()
}

/// Every generation of this machine, oldest first.
pub fn history() -> Result<Vec<Generation>, String> {
    if !here_in_store() {
        let listed = backend::history()?;
        return Ok(listed
            .into_iter()
            .map(|g| Generation {
                number: g.number,
                created: g.created,
                current: g.current,
                model_hash: g.model_hash,
            })
            .collect());
    }
    history_at(Path::new(ROOT))
}

/// Every generation of the machine at `root`, oldest first.
fn history_at(root: &Path) -> Result<Vec<Generation>, String> {
    let (listed, current) = generations(root)?;
    listed
        .into_iter()
        .map(|(number, link)| {
            let created = fs::symlink_metadata(&link)
                .and_then(|metadata| metadata.modified())
                .map_err(|e| format!("{}: {e}", link.display()))?;
            let seconds = created
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs());
            let system = system_of(root, &link)?;
            Ok(Generation {
                number,
                created: utc(seconds),
                current: number == current,
                model_hash: fs::read_to_string(system.join("etc/distro/model-hash")).ok(),
            })
        })
        .collect()
}

/// Where generation `number` of this machine keeps its model.
pub fn generation_model(number: u32) -> Result<PathBuf, String> {
    if !here_in_store() {
        return Ok(backend::generation_model(number));
    }
    let root = Path::new(ROOT);
    let link = root.join(GENERATIONS).join(number.to_string());
    Ok(system_of(root, &link)?.join("etc/distro/model.json"))
}

/// The system `link` names, under `root`.
fn system_of(root: &Path, link: &Path) -> Result<PathBuf, String> {
    let target = fs::read_link(link).map_err(|e| format!("{}: {e}", link.display()))?;
    Ok(root.join(target.strip_prefix("/").unwrap_or(&target)))
}

/// Adds `system`, a path in `/store`, as the newest generation and makes it current. Returns its
/// number. A system that is the current generation already stays that one.
pub fn add(root: &Path, system: &Path) -> Result<u32, String> {
    if !system.starts_with("/store") {
        return Err(format!(
            "{} isn't in /store; copy it in first",
            system.display()
        ));
    }
    let (listed, current) = generations(root)?;
    let current_link = root.join(GENERATIONS).join(current.to_string());
    if fs::read_link(&current_link).is_ok_and(|target| target == system) {
        return Ok(current);
    }
    let number = listed.last().map_or(1, |(last, _)| last + 1);
    symlink(system, root.join(GENERATIONS).join(number.to_string()))
        .map_err(|e| format!("generation {number}: {e}"))?;
    make_current(root, number)?;
    Ok(number)
}

/// Makes generation `number` the current one, in one step.
fn make_current(root: &Path, number: u32) -> Result<(), String> {
    let current = root.join(CURRENT);
    let next = current.with_extension("next");
    let _ = fs::remove_file(&next);
    symlink(format!("generations/{number}"), &next)
        .and_then(|()| fs::rename(&next, &current))
        .map_err(|e| format!("{}: {e}", current.display()))
}

/// The generation before the current one.
fn previous(root: &Path) -> Result<u32, String> {
    let (listed, current) = generations(root)?;
    listed
        .iter()
        .map(|(number, _)| *number)
        .filter(|number| *number < current)
        .max()
        .ok_or_else(|| format!("generation {current} is the oldest there is"))
}

/// Makes `system` the current generation and switches to it. Needs root.
pub fn activate(system: &Path) -> Result<(), String> {
    if !here_in_store() {
        return backend::activate(system);
    }
    add(Path::new(ROOT), system)?;
    switch(system, "switch")
}

/// Goes back to the generation before the current one and switches to it. Needs root.
pub fn rollback() -> Result<u32, String> {
    if !here_in_store() {
        return backend::rollback().map(|g| g.number);
    }
    let root = Path::new(ROOT);
    let number = previous(root)?;
    make_current(root, number)?;
    switch(&root.join(CURRENT), "switch")?;
    Ok(number)
}

/// Switches to `system` without making it a generation, so a timer switching back after
/// `seconds`, `rollback`, or a reboot all return to the current generation. Needs root.
pub fn try_system(system: &Path, seconds: u64) -> Result<(), String> {
    if !here_in_store() {
        return backend::try_system(system, seconds);
    }
    if trying() {
        return Err("a system is already being tried; keep it or roll back first".into());
    }
    let root = Path::new(ROOT);
    let trying_link = root.join(TRYING);
    let _ = fs::remove_file(&trying_link);
    symlink(system, &trying_link).map_err(|e| format!("{}: {e}", trying_link.display()))?;
    if let Err(message) = switch(system, "test") {
        let _ = fs::remove_file(&trying_link);
        let _ = switch(&root.join(CURRENT), "switch");
        return Err(message);
    }
    run(Command::new("systemd-run")
        .arg(format!("--unit={TRY_TIMER}"))
        .arg(format!("--on-active={seconds}s"))
        .args([
            "--timer-property=AccuracySec=1s",
            "--description=distro: end a try",
        ])
        .arg(root.join(CURRENT).join("bin/switch-to-configuration"))
        .arg("switch"))
}

/// Whether a system is being tried, with a timer waiting to switch back from it.
pub fn trying() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet"])
        .arg(format!("{TRY_TIMER}.timer"))
        .status()
        .is_ok_and(|status| status.success())
}

/// Makes the system being tried the current generation and the boot default. Returns its number.
pub fn keep_trial() -> Result<u32, String> {
    if !here_in_store() {
        backend::keep_trial()?;
        let listed = history()?;
        return listed
            .into_iter()
            .find(|g| g.current)
            .map(|g| g.number)
            .ok_or_else(|| "there's no current generation".to_owned());
    }
    if !trying() {
        return Err("no system is being tried".into());
    }
    stop_trial();
    let root = Path::new(ROOT);
    let tried = fs::read_link(root.join(TRYING)).map_err(|e| format!("{TRYING}: {e}"))?;
    let number = add(root, &tried)?;
    switch(&tried, "boot")?;
    let _ = fs::remove_file(root.join(TRYING));
    Ok(number)
}

/// Switches back from the system being tried to the current generation. Returns its number.
pub fn end_trial() -> Result<u32, String> {
    if !here_in_store() {
        return backend::end_trial().map(|g| g.number);
    }
    stop_trial();
    let root = Path::new(ROOT);
    switch(&root.join(CURRENT), "switch")?;
    let _ = fs::remove_file(root.join(TRYING));
    Ok(generations(root)?.1)
}

fn stop_trial() {
    let _ = run(Command::new("systemctl")
        .arg("stop")
        .arg(format!("{TRY_TIMER}.timer")));
}

/// Removes generations `numbers` from the machine at `root`, and every path in its store that no
/// generation left, the running system or one being tried uses. Returns how many paths went.
/// The current generation can't be removed.
pub fn remove(root: &Path, numbers: &[u32]) -> Result<usize, String> {
    let (_, current) = generations(root)?;
    if numbers.contains(&current) {
        return Err(format!("generation {current} is the current one"));
    }
    for number in numbers {
        let link = root.join(GENERATIONS).join(number.to_string());
        fs::remove_file(&link).map_err(|e| format!("{}: {e}", link.display()))?;
    }
    let (left, _) = generations(root)?;
    let starts = left
        .into_iter()
        .map(|(_, link)| link)
        .chain([TRYING, "run/current-system", "run/booted-system"].map(|link| root.join(link)));
    let lab = Lab::new(root);
    let mut used = BTreeSet::new();
    for link in starts {
        let Ok(target) = fs::read_link(&link) else {
            continue;
        };
        if let Some(path) = store_path(&target) {
            used.extend(distro_build::chain::runtime_closure(&lab, &path)?);
        }
    }
    let store = root.join("store");
    let mut removed = 0;
    for entry in fs::read_dir(&store).map_err(|e| format!("{}: {e}", store.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", store.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        // Only store paths are collected; the machine's records in `.system` stay.
        if !is_hash(&name) || used.contains(&StorePath::from_hash(&name)) {
            continue;
        }
        let path = entry.path();
        make_writable(&path);
        fs::remove_dir_all(&path)
            .or_else(|_| fs::remove_file(&path))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

/// The store path a link's target is in, such as `/store/<hash>/sw`.
fn store_path(target: &Path) -> Option<StorePath> {
    let mut parts = target.strip_prefix("/store").ok()?.components();
    let hash = parts.next()?.as_os_str().to_str()?;
    is_hash(hash).then(|| StorePath::from_hash(hash))
}

/// Whether `name` is a store path's, 40 hex digits.
fn is_hash(name: &str) -> bool {
    name.len() == 40 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Store paths are read-only, so their directories are made writable before they're removed.
fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
        for entry in fs::read_dir(path).into_iter().flatten().flatten() {
            make_writable(&entry.path());
        }
    }
}

/// Removes generations `numbers` from this machine, writes the boot entries again and collects
/// what nothing uses. Needs root.
pub fn clean(numbers: &[u32]) -> Result<String, String> {
    if !here_in_store() {
        return backend::clean(numbers);
    }
    let root = Path::new(ROOT);
    let removed = remove(root, numbers)?;
    if !numbers.is_empty() {
        switch(&root.join(CURRENT), "boot")?;
    }
    Ok(format!("{removed} store paths removed"))
}

/// Runs `system`'s switch as `mode` says: `switch`, `test` or `boot`.
fn switch(system: &Path, mode: &str) -> Result<(), String> {
    run(Command::new(system.join("bin/switch-to-configuration")).arg(mode))
}

fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{program} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// Seconds since 1970 as `2026-11-10 14:03:22`, in UTC.
fn utc(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Days to a civil date, after Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine with a generation for each of `systems`, by store name, the last current.
    fn machine(systems: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join(GENERATIONS)).unwrap();
        for (n, name) in systems.iter().enumerate() {
            let system = root.join("store").join(name);
            fs::create_dir_all(system.join("etc/distro")).unwrap();
            fs::write(system.join("etc/distro/model-hash"), format!("hash-{name}")).unwrap();
            let link = root.join(GENERATIONS).join((n + 1).to_string());
            symlink(format!("/store/{name}"), link).unwrap();
        }
        symlink(format!("generations/{}", systems.len()), root.join(CURRENT)).unwrap();
        dir
    }

    fn hash(tag: char) -> String {
        tag.to_string().repeat(40)
    }

    #[test]
    fn history_lists_each_generation_with_its_model_and_the_current_one() {
        let dir = machine(&[&hash('a'), &hash('b')]);
        let listed = history_at(dir.path()).unwrap();
        let summary: Vec<(u32, bool, Option<&str>)> = listed
            .iter()
            .map(|g| (g.number, g.current, g.model_hash.as_deref()))
            .collect();
        let (a, b) = (format!("hash-{}", hash('a')), format!("hash-{}", hash('b')));
        assert_eq!(
            summary,
            [(1, false, Some(a.as_str())), (2, true, Some(b.as_str()))]
        );
        assert_eq!(listed[0].created.len(), "2026-11-10 14:03:22".len());
    }

    #[test]
    fn adding_a_system_makes_it_the_newest_generation_and_current() {
        let dir = machine(&[&hash('a')]);
        let system = format!("/store/{}", hash('b'));
        fs::create_dir_all(dir.path().join(&system[1..])).unwrap();
        assert_eq!(add(dir.path(), Path::new(&system)).unwrap(), 2);
        let (_, current) = generations(dir.path()).unwrap();
        assert_eq!(current, 2);
        // Adding the current system again changes nothing.
        assert_eq!(add(dir.path(), Path::new(&system)).unwrap(), 2);
        assert_eq!(generations(dir.path()).unwrap().0.len(), 2);
    }

    #[test]
    fn only_a_system_in_the_store_can_become_a_generation() {
        let dir = machine(&[&hash('a')]);
        let error = add(dir.path(), Path::new("/home/me/system")).unwrap_err();
        assert!(error.contains("isn't in /store"), "{error}");
    }

    #[test]
    fn the_previous_generation_is_the_newest_before_the_current_one() {
        let dir = machine(&[&hash('a'), &hash('b'), &hash('c')]);
        make_current(dir.path(), 2).unwrap();
        assert_eq!(previous(dir.path()).unwrap(), 1);
        make_current(dir.path(), 1).unwrap();
        let error = previous(dir.path()).unwrap_err();
        assert!(error.contains("oldest"), "{error}");
    }

    #[test]
    fn removing_generations_collects_only_what_nothing_else_uses() {
        let dir = machine(&[&hash('a'), &hash('b')]);
        let root = dir.path();
        // `b` uses `c`; `a` uses `d`; `e` nothing uses.
        for (system, uses) in [(hash('b'), hash('c')), (hash('a'), hash('d'))] {
            let path = root.join("store").join(&system).join("sw");
            fs::write(path, format!("/store/{uses}/bin/tool\n")).unwrap();
        }
        for name in [hash('c'), hash('d'), hash('e')] {
            fs::create_dir_all(root.join("store").join(name)).unwrap();
        }
        assert_eq!(remove(root, &[1]).unwrap(), 3);
        let left: BTreeSet<String> = fs::read_dir(root.join("store"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            BTreeSet::from([hash('b'), hash('c'), ".system".to_owned()])
        );
        assert!(remove(root, &[2]).unwrap_err().contains("current"));
    }

    #[test]
    fn seconds_read_as_a_utc_date() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(1_794_319_402), "2026-11-10 14:03:22");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00");
    }
}
