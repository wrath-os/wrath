//! A machine's generations in distro's store: each one a link in `/store/.system/generations`
//! named by its number, `/store/.system/current` naming the current one, and the systems
//! themselves in `/store`. The records live with the store, so a root erased at every boot keeps
//! them.
//! Making one current and switching to it, trying one for a while, rolling back, and collecting
//! what no generation uses any more.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use distro_build::store::{Lab, StorePath, is_entry};

use crate::activate::{BOOTED, CURRENT, GENERATIONS, RUNNING, generations};

/// The root of the machine distro runs on.
const ROOT: &str = "/";

/// Whether this machine's generations are in distro's store, which its first `apply` starts.
fn here_in_store() -> bool {
    Path::new(ROOT).join(GENERATIONS).is_dir()
}

/// Refuses to change generations on a machine that has none yet.
fn require_generations() -> Result<(), String> {
    if here_in_store() {
        Ok(())
    } else {
        Err(
            "this machine has no generations in distro's store yet; `distro apply` makes the first"
                .into(),
        )
    }
}

/// A system being tried, kept from collection while no generation has it.
const TRYING: &str = "store/.system/trying";

/// Held while the generations change, so two changes never take the same number and a clean never
/// runs in the middle of a switch.
const LOCK: &str = "store/.system/lock";

/// How long a store path is kept after it arrives though nothing uses it yet, such as a system
/// copied in for a switch that hasn't made it a generation.
const GRACE: Duration = Duration::from_secs(3600);

/// The timer that switches back from a system being tried.
const TRY_TIMER: &str = "distro-try";

/// Waits for any other change to the generations of the machine at `root`, then holds them until
/// the returned file is dropped.
fn lock(root: &Path) -> Result<fs::File, String> {
    let path = root.join(LOCK);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if file.try_lock().is_err() {
        eprintln!("waiting for another change to this machine's generations to finish");
        file.lock()
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(file)
}

/// One of the machine's generations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    pub number: u32,
    /// When it was made, as `2026-11-10 14:03:22` in UTC.
    pub created: String,
    pub current: bool,
    /// Whether it's the system running or the one that booted, which can differ from the current
    /// one after a boot fell back to an older generation.
    pub in_use: bool,
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

/// The hash of the running system's model, when distro built the running system.
pub fn running_model_hash() -> Option<String> {
    fs::read_to_string(Path::new(ROOT).join(RUNNING).join(MODEL_HASH)).ok()
}

/// The running system's model file, when distro built the running system.
pub fn running_model() -> Option<PathBuf> {
    let path = Path::new(ROOT).join(RUNNING).join(MODEL);
    path.exists().then_some(path)
}

/// What the running system says it was built from, when it says.
pub fn running_built_from() -> Option<String> {
    let keys = distro_build::graph::PACKAGE_KEYS;
    fs::read_to_string(Path::new(ROOT).join(RUNNING).join(keys)).ok()
}

/// Where a system keeps its model and the model's hash.
const MODEL: &str = "etc/distro/model.json";
const MODEL_HASH: &str = "etc/distro/model-hash";

/// Every generation of this machine, oldest first.
pub fn history() -> Result<Vec<Generation>, String> {
    if !here_in_store() {
        return Ok(Vec::new());
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
            let target = fs::read_link(&link).ok();
            let in_use = [RUNNING, BOOTED]
                .iter()
                .any(|name| fs::read_link(root.join(name)).ok() == target);
            Ok(Generation {
                number,
                created: utc(seconds),
                current: number == current,
                in_use,
                model_hash: fs::read_to_string(system.join(MODEL_HASH)).ok(),
            })
        })
        .collect()
}

/// Where generation `number` of this machine keeps its model.
pub fn generation_model(number: u32) -> Result<PathBuf, String> {
    require_generations()?;
    let root = Path::new(ROOT);
    let link = root.join(GENERATIONS).join(number.to_string());
    Ok(system_of(root, &link)?.join(MODEL))
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

/// The generation a rollback goes to: `to`, which may be the current one, or else the one before.
fn rollback_target(root: &Path, to: Option<u32>) -> Result<u32, String> {
    let Some(to) = to else {
        return previous(root);
    };
    let (listed, _) = generations(root)?;
    if listed.iter().any(|(number, _)| *number == to) {
        Ok(to)
    } else {
        Err(format!(
            "there's no generation {to}; `distro history` lists them"
        ))
    }
}

/// What to say when switching to generation `number` failed after it became current and the
/// boot default. `previous` is where a plain rollback goes, when it's worth saying.
fn switch_failed(number: u32, previous: Option<u32>, error: String) -> String {
    let next = match previous {
        Some(previous) => format!("`distro rollback` goes back to {previous}"),
        None => format!("`distro rollback {number}` tries it again"),
    };
    format!(
        "{error}\ngeneration {number} is current and boots next, but switching to it failed \
         partway; {next}"
    )
}

/// Makes `system` the current generation and switches to it. Needs root.
pub fn activate(system: &Path) -> Result<(), String> {
    let root = Path::new(ROOT);
    let _held = lock(root)?;
    let number = add(root, system)?;
    switch(system, "switch").map_err(|e| switch_failed(number, previous(root).ok(), e))
}

/// Goes back to generation `to`, or the one before the current one, and switches to it. Needs
/// root.
pub fn rollback(to: Option<u32>) -> Result<u32, String> {
    require_generations()?;
    let root = Path::new(ROOT);
    let _held = lock(root)?;
    let number = rollback_target(root, to)?;
    make_current(root, number)?;
    switch(&root.join(CURRENT), "switch").map_err(|e| switch_failed(number, None, e))?;
    Ok(number)
}

/// Switches to `system` without making it a generation, so a timer switching back after
/// `seconds`, `rollback`, or a reboot all return to the current generation. Needs root.
pub fn try_system(system: &Path, seconds: u64) -> Result<(), String> {
    require_generations()?;
    let _held = lock(Path::new(ROOT))?;
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
    if !trying() {
        return Err("no system is being tried".into());
    }
    let _held = lock(Path::new(ROOT))?;
    let root = Path::new(ROOT);
    // The timer stops only once the tried system is a generation: anything failing before that
    // leaves it running, and it switches back as it would have.
    let tried = fs::read_link(root.join(TRYING)).map_err(|e| format!("{TRYING}: {e}"))?;
    let number = add(root, &tried)?;
    stop_trial();
    switch(&tried, "boot")?;
    let _ = fs::remove_file(root.join(TRYING));
    Ok(number)
}

/// Switches back from the system being tried to the current generation. Returns its number.
pub fn end_trial() -> Result<u32, String> {
    require_generations()?;
    let _held = lock(Path::new(ROOT))?;
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
pub fn remove(root: &Path, numbers: &[u32], grace: Duration) -> Result<usize, String> {
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
        .chain([TRYING, RUNNING, BOOTED].map(|link| root.join(link)));
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
        if !is_entry(&name) || used.contains(&StorePath::from_hash(&name)) {
            continue;
        }
        let path = entry.path();
        if arrived_within(&path, grace) {
            continue;
        }
        make_writable(&path);
        fs::remove_dir_all(&path)
            .or_else(|_| fs::remove_file(&path))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

/// Whether `path` came into the store less than `grace` ago. Its status-change time says when: a
/// copy keeps the time a file was last modified, but never this one.
fn arrived_within(path: &Path, grace: Duration) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    let arrived = UNIX_EPOCH + Duration::from_secs(u64::try_from(meta.ctime()).unwrap_or(0));
    SystemTime::now()
        .duration_since(arrived)
        .is_ok_and(|age| age < grace)
}

/// The store path a link's target is in, such as `/store/<hash>/sw`.
fn store_path(target: &Path) -> Option<StorePath> {
    let mut parts = target.strip_prefix("/store").ok()?.components();
    let hash = parts.next()?.as_os_str().to_str()?;
    is_entry(hash).then(|| StorePath::from_hash(hash))
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
    require_generations()?;
    let root = Path::new(ROOT);
    let _held = lock(root)?;
    let removed = remove(root, numbers, GRACE)?;
    if !numbers.is_empty() {
        switch(&root.join(CURRENT), "boot")?;
    }
    Ok(format!("{removed} store paths removed"))
}

/// Installs `system`, already built and in this machine's store, onto the filesystems mounted at
/// `target`: copies what it needs into their store, makes it their first generation, and has its
/// own distro put the boot loader and its entries in place. Needs root.
pub fn install(system: &Path, target: &Path) -> Result<(), String> {
    if !system.starts_with("/store") {
        return Err(format!(
            "{} isn't in /store; build it with distro first",
            system.display()
        ));
    }
    copy_onto(Path::new(ROOT), system, target)?;
    run(Command::new(system.join("bin/distro"))
        .args(["activate", "--mode", "install", "--root"])
        .arg(target)
        .arg(system))
}

/// Copies `system` and every store path it refers to from the store under `from` into the one
/// under `target`, and makes it generation 1 there. Paths already there are left alone.
fn copy_onto(from: &Path, system: &Path, target: &Path) -> Result<(), String> {
    let generation = store_path(system)
        .ok_or_else(|| format!("{} isn't a system in /store", system.display()))?;
    let store = target.join("store");
    fs::create_dir_all(&store).map_err(|e| format!("{}: {e}", store.display()))?;
    let lab = Lab::new(from);
    for path in distro_build::chain::runtime_closure(&lab, &generation)? {
        if store.join(path.hash()).exists() {
            continue;
        }
        run(Command::new("cp")
            .args(["-a", "--reflink=auto"])
            .arg(lab.on_host(&path))
            .arg(&store))?;
    }
    let generations = target.join(GENERATIONS);
    fs::create_dir_all(&generations).map_err(|e| format!("{}: {e}", generations.display()))?;
    let first = generations.join("1");
    let _ = fs::remove_file(&first);
    symlink(generation.inside(), &first).map_err(|e| format!("{}: {e}", first.display()))?;
    make_current(target, 1)
}

/// Runs `system`'s switch in `mode`: `switch`, `test` or `boot`. What it says goes straight to stderr, as it says it, with
/// its own notes and errors, so a failure here only says that it failed.
fn switch(system: &Path, mode: &str) -> Result<(), String> {
    let status = Command::new(system.join("bin/switch-to-configuration"))
        .arg(mode)
        .stdout(Stdio::null())
        .status()
        .map_err(|e| format!("could not switch to {}: {e}", system.display()))?;
    if status.success() {
        return Ok(());
    }
    Err(format!("switching failed ({status})"))
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
    fn a_rollback_goes_to_the_generation_named_or_the_one_before() {
        let dir = machine(&[&hash('a'), &hash('b'), &hash('c')]);
        assert_eq!(rollback_target(dir.path(), None).unwrap(), 2);
        assert_eq!(rollback_target(dir.path(), Some(1)).unwrap(), 1);
        // The current one again, which is how a switch that failed is tried again.
        assert_eq!(rollback_target(dir.path(), Some(3)).unwrap(), 3);
        let error = rollback_target(dir.path(), Some(7)).unwrap_err();
        assert!(error.contains("no generation 7"), "{error}");
    }

    #[test]
    fn a_failed_switch_says_what_is_current_and_how_to_go_on() {
        let after_apply = switch_failed(4, Some(3), "nginx.service failed".into());
        assert!(
            after_apply.contains("generation 4 is current"),
            "{after_apply}"
        );
        assert!(
            after_apply.contains("`distro rollback` goes back to 3"),
            "{after_apply}"
        );
        let after_rollback = switch_failed(2, None, "nginx.service failed".into());
        assert!(
            after_rollback.contains("`distro rollback 2`"),
            "{after_rollback}"
        );
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
        assert_eq!(remove(root, &[1], Duration::ZERO).unwrap(), 3);
        let left: BTreeSet<String> = fs::read_dir(root.join("store"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            BTreeSet::from([hash('b'), hash('c'), ".system".to_owned()])
        );
        assert!(
            remove(root, &[2], Duration::ZERO)
                .unwrap_err()
                .contains("current")
        );
    }

    #[test]
    fn a_store_path_that_just_arrived_survives_a_clean() {
        let dir = machine(&[&hash('a')]);
        let root = dir.path();
        // Copied in for a switch that hasn't made it a generation yet.
        fs::create_dir_all(root.join("store").join(hash('f'))).unwrap();
        assert_eq!(remove(root, &[], Duration::from_secs(3600)).unwrap(), 0);
        assert!(root.join("store").join(hash('f')).exists());
        assert_eq!(remove(root, &[], Duration::ZERO).unwrap(), 1);
    }

    #[test]
    fn changes_to_the_generations_wait_for_each_other() {
        let dir = machine(&[&hash('a')]);
        let held = lock(dir.path()).unwrap();
        assert!(
            fs::File::open(dir.path().join(LOCK))
                .unwrap()
                .try_lock()
                .is_err()
        );
        drop(held);
        assert!(
            fs::File::open(dir.path().join(LOCK))
                .unwrap()
                .try_lock()
                .is_ok()
        );
    }

    #[test]
    fn installing_copies_what_the_system_needs_and_makes_it_the_first_generation() {
        let dir = machine(&[&hash('a')]);
        let from = dir.path();
        let system = from.join("store").join(hash('a'));
        fs::write(
            system.join("sw"),
            format!("/store/{}/bin/tool\n", hash('b')),
        )
        .unwrap();
        for name in [hash('b'), hash('c')] {
            fs::create_dir_all(from.join("store").join(name)).unwrap();
        }
        let target = tempfile::tempdir().unwrap();
        let inside = PathBuf::from(format!("/store/{}", hash('a')));
        copy_onto(from, &inside, target.path()).unwrap();
        let store = target.path().join("store");
        assert!(
            store
                .join(hash('a'))
                .join("etc/distro/model-hash")
                .is_file()
        );
        assert!(store.join(hash('b')).is_dir());
        assert!(!store.join(hash('c')).exists(), "nothing uses c");
        let (listed, current) = generations(target.path()).unwrap();
        assert_eq!((listed.len(), current), (1, 1));
        assert_eq!(fs::read_link(&listed[0].1).unwrap(), inside);
    }

    #[test]
    fn seconds_read_as_a_utc_date() {
        assert_eq!(utc(0), "1970-01-01 00:00:00");
        assert_eq!(utc(1_794_319_402), "2026-11-10 14:03:22");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00:00");
    }
}
