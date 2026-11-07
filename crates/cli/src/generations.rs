//! Building systems and moving between generations through the NixOS backend.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use distro_backend_nixos::{self as backend, Generation, Target};
use distro_model::{DEFAULT_SOURCE, LOCK_FILE, Lock, Model, Source, Tier};

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum BuildTarget {
    /// The system, to switch to
    System,
    /// The system with the test harness's agent, for VM scenarios
    TestSystem,
    /// A bootable disk image of the test system
    TestImage,
    /// The test system for VMs that boot from the host's store
    TestVmSystem,
    /// A VM that boots the test system from the host's store, without a disk image
    TestVm,
    /// The test system as `distro install` puts it on a VM's disk
    TestInstalledSystem,
}

impl From<BuildTarget> for Target {
    fn from(target: BuildTarget) -> Self {
        match target {
            BuildTarget::System => Self::System,
            BuildTarget::TestSystem => Self::TestSystem,
            BuildTarget::TestImage => Self::TestImage,
            BuildTarget::TestVmSystem => Self::TestVmSystem,
            BuildTarget::TestVm => Self::TestVm,
            BuildTarget::TestInstalledSystem => Self::TestInstalledSystem,
        }
    }
}

/// Builds `target` from `model` with the factory under `root` and returns its store path. The
/// backend's warnings go to stderr.
pub fn build(root: &Path, model: &Model, target: impl Into<Target>) -> Result<PathBuf, String> {
    let work = tempfile::tempdir().map_err(|e| format!("cannot create a work directory: {e}"))?;
    let built = backend::build(model, &factory(root)?, target.into(), work.path())?;
    for warning in &built.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(built.path)
}

/// How to switch to a system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switch {
    /// Make it the current generation.
    Apply,
    /// Run it for this many seconds, then switch back unless it's kept.
    Try(u64),
}

/// Builds the system for `model` and switches to it, unless the running system already has it.
/// Packages the lock next to `config` doesn't have yet are locked first, so the lock always
/// describes the running system. `record` is what this machine records about itself before
/// switching; when it changes, the machine switches again even to the same model, so files are
/// filled in with the new values.
pub fn apply(
    root: &Path,
    config: &Path,
    model: Model,
    how: Switch,
    yes: bool,
    record: Option<crate::machine::Record>,
) -> Result<ExitCode, String> {
    let model = locked(root, config, model)?;
    let hash = model.hash();
    if record.is_none() && backend::current_model_hash().as_deref() == Some(hash.as_str()) {
        println!("the running system already has model {}", &hash[..16]);
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(record) = &record {
        println!("{record}");
    }
    let running = backend::current_model().and_then(|path| crate::read_model(Path::new(path)).ok());
    let plan = distro_model::plan(running.as_ref().unwrap_or(&Model::default()), &model);
    match &running {
        Some(_) => print!("{plan}"),
        None => println!(
            "distro didn't build the running system, so all of it is new: {}",
            plan.summary(5)
        ),
    }
    if !confirmed(yes, "switch to it?")? {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    }
    let system = build(root, &model, BuildTarget::System)?;
    if let Some(record) = record {
        record.write()?;
    }
    let code = switch_to(&system, how)?;
    if let Some(note) = running.and(plan.tier()).and_then(full_effect) {
        println!("{note}");
    }
    Ok(code)
}

/// `model` with its packages locked: locks what the lock next to `config` doesn't have yet, so
/// the lock always describes what runs.
pub(crate) fn locked(root: &Path, config: &Path, model: Model) -> Result<Model, String> {
    if model.is_locked() {
        crate::fetch::record(config)?;
        return Ok(model);
    }
    let mut lock = resolve(root, &model)?;
    lock.sources.extend(crate::fetch::fetched());
    write_lock(config, &lock)?;
    Ok(model.with_lock(&lock))
}

/// Whether to go ahead after showing the plan: yes with `-y`, else the person at the terminal
/// says. Without a terminal there's nobody to ask, so it's an error instead of a guess.
pub(crate) fn confirmed(yes: bool, question: &str) -> Result<bool, String> {
    use std::io::{IsTerminal, Write};
    match ask(yes, std::io::stdin().is_terminal()) {
        Ask::No => Ok(true),
        Ask::Refuse => Err(format!(
            "nobody to answer `{question}`; pass -y to go ahead without asking"
        )),
        Ask::Yes => {
            eprint!("{question} [y/N] ");
            let _ = std::io::stderr().flush();
            let mut answer = String::new();
            std::io::stdin()
                .read_line(&mut answer)
                .map_err(|e| format!("reading the answer: {e}"))?;
            Ok(matches!(answer.trim(), "y" | "Y" | "yes"))
        }
    }
}

/// Whether to ask before switching.
#[derive(Debug, PartialEq, Eq)]
enum Ask {
    Yes,
    No,
    /// There's nobody to ask.
    Refuse,
}

fn ask(yes: bool, terminal: bool) -> Ask {
    match (yes, terminal) {
        (true, _) => Ask::No,
        (false, true) => Ask::Yes,
        (false, false) => Ask::Refuse,
    }
}

/// When a switch to changes of `tier` takes full effect, if it doesn't right away.
fn full_effect(tier: Tier) -> Option<&'static str> {
    match tier {
        Tier::Live => None,
        Tier::NextLogin => Some("some changes take effect when users log in again"),
        Tier::SoftReboot => {
            Some("some changes take full effect after a soft reboot: `systemctl soft-reboot`")
        }
        Tier::Reboot => Some("some changes take effect after a reboot"),
    }
}

/// `status` as data: the running generation, whether a system is being tried, the running and
/// configured models' hashes, and how many packages aren't locked yet.
pub fn status_json(model: &Model) -> serde_json::Value {
    let current = backend::history()
        .ok()
        .and_then(|generations| generations.into_iter().find(|g| g.current));
    let running = backend::current_model_hash();
    let hash = model.hash();
    let unlocked = model
        .all_packages()
        .filter(|name| !model.locked.contains_key(*name))
        .count();
    serde_json::json!({
        "generation": current.map(|g| g.number),
        "trying": backend::trying(),
        "matches": running.as_deref() == Some(hash.as_str()),
        "running": running,
        "model": hash,
        "unlocked": unlocked,
    })
}

/// `history` as data: each generation, with a summary of what changed from the one before.
pub fn history_json() -> Result<serde_json::Value, String> {
    let mut before: Option<Model> = None;
    let mut generations = Vec::new();
    for generation in backend::history()? {
        let model = generation_model(generation.number).ok();
        let changes = before
            .as_ref()
            .zip(model.as_ref())
            .map(|(before, after)| distro_model::plan(before, after).summary(usize::MAX));
        generations.push(serde_json::json!({
            "number": generation.number,
            "created": generation.created,
            "current": generation.current,
            "model": generation.model_hash,
            "changes": changes,
        }));
        before = model;
    }
    Ok(serde_json::Value::Array(generations))
}

/// Says which generation is running and whether it's the configuration's `model`.
pub fn status(model: &Model) -> Result<ExitCode, String> {
    let current = backend::history()
        .ok()
        .and_then(|generations| generations.into_iter().find(|g| g.current));
    if backend::trying() {
        println!(
            "trying a system that isn't a generation yet; it switches back on its own, \
             `distro keep` keeps it and `distro rollback` switches back now"
        );
    } else if let Some(generation) = &current {
        println!(
            "running generation {}, from {}",
            generation.number, generation.created
        );
    }
    println!(
        "{}",
        matching(backend::current_model_hash().as_deref(), model)
    );
    let unlocked = model
        .all_packages()
        .filter(|name| !model.locked.contains_key(*name))
        .count();
    if unlocked > 0 {
        println!("{unlocked} packages aren't locked yet; `distro apply` locks them first");
    }
    Ok(ExitCode::SUCCESS)
}

/// Says which filesystems the machine runs with a disk missing. It's about the machine, so it's
/// said before anything about its configuration.
pub fn warn_degraded() {
    for note in degraded(Path::new(DEGRADED)) {
        println!("{note}");
    }
}

/// Where the boot leaves a note for each filesystem it mounted with a disk missing.
const DEGRADED: &str = "/run/distro";

/// What the notes in `dir` say: each filesystem running without some of its disks, and what to do
/// about it.
fn degraded(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut notes: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let filesystem = name.strip_prefix("degraded-")?;
            let missing = std::fs::read_to_string(entry.path()).ok()?;
            Some(format!(
                "warning: {filesystem} is running without {}; once the disk is back or \
                 replaced, rebalance it with `btrfs balance start -dconvert=raid1 \
                 -mconvert=raid1 /`",
                missing.split_whitespace().collect::<Vec<_>>().join(", ")
            ))
        })
        .collect();
    notes.sort();
    notes
}

/// Whether the running system, whose model hash is `running`, is `model`.
pub(crate) fn matching(running: Option<&str>, model: &Model) -> &'static str {
    match running {
        Some(hash) if hash == model.hash() => "it matches the configuration",
        Some(_) => "the configuration differs from it; `distro plan` shows how",
        None => "distro didn't build it; `distro plan` shows the whole configuration as new",
    }
}

/// Switches to `system`, already built, as `how` says.
pub fn switch_to(system: &Path, how: Switch) -> Result<ExitCode, String> {
    match how {
        Switch::Apply => activate(system),
        Switch::Try(seconds) => {
            backend::try_system(system, seconds)?;
            println!("trying {}", system.display());
            println!(
                "it switches back in {} unless you run `distro keep`; `distro rollback` switches \
                 back now",
                duration(seconds)
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Makes the system being tried the current generation.
pub fn keep() -> Result<ExitCode, String> {
    backend::keep_trial()?;
    let generation = backend::history()?.into_iter().find(|g| g.current);
    match generation {
        Some(generation) => println!("kept it as generation {}", generation.number),
        None => println!("kept it"),
    }
    Ok(ExitCode::SUCCESS)
}

/// Seconds from `5m`, `90s`, `1h` or `90`.
pub fn parse_duration(text: &str) -> Result<u64, String> {
    let (number, unit) = match text.find(|c: char| !c.is_ascii_digit()) {
        Some(at) => text.split_at(at),
        None => (text, "s"),
    };
    let invalid = || format!("`{text}` isn't a duration such as 90s, 5m or 1h");
    let scale = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        _ => return Err(invalid()),
    };
    match number.parse::<u64>() {
        Ok(n) if n > 0 => Ok(n * scale),
        _ => Err(invalid()),
    }
}

/// Seconds as people say them: `5 minutes`, `90 seconds`.
fn duration(seconds: u64) -> String {
    let (n, unit) = match seconds {
        s if s % 3600 == 0 => (s / 3600, "hour"),
        s if s % 60 == 0 => (s / 60, "minute"),
        s => (s, "second"),
    };
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// Removes old generations, keeping the current one and the `keep` before it, and what only
/// they used, after saying which and asking.
pub fn clean(keep: usize, yes: bool) -> Result<ExitCode, String> {
    let generations = backend::history()?;
    let old = to_remove(&generations, keep);
    if old.is_empty() {
        println!("no generations to remove; collecting what nothing uses");
    } else {
        let listed: Vec<String> = old.iter().map(u32::to_string).collect();
        println!(
            "removes generations {}, keeping the current one and the {keep} before it",
            listed.join(", ")
        );
        if !confirmed(yes, "remove them?")? {
            println!("nothing changed");
            return Ok(ExitCode::SUCCESS);
        }
    }
    let freed = backend::clean(&old)?;
    println!("{freed}");
    Ok(ExitCode::SUCCESS)
}

/// The generations to remove: all but the current one and the `keep` newest others.
fn to_remove(generations: &[Generation], keep: usize) -> Vec<u32> {
    let mut others: Vec<u32> = generations
        .iter()
        .filter(|generation| !generation.current)
        .map(|generation| generation.number)
        .collect();
    others.sort_unstable();
    others.truncate(others.len().saturating_sub(keep));
    others
}

pub fn activate(system: &Path) -> Result<ExitCode, String> {
    backend::activate(system)?;
    println!("switched to {}", system.display());
    Ok(ExitCode::SUCCESS)
}

/// Lists the generations, each with what changed from the one before it.
pub fn history() -> Result<ExitCode, String> {
    let mut before: Option<Model> = None;
    for generation in backend::history()? {
        println!("{generation}");
        let model = generation_model(generation.number).ok();
        if let (Some(before), Some(after)) = (&before, &model) {
            println!("      {}", distro_model::plan(before, after).summary(4));
        }
        before = model;
    }
    Ok(ExitCode::SUCCESS)
}

/// Shows what changed going from generation `from` to `to`, or to the current one.
pub fn diff(from: u32, to: Option<u32>) -> Result<ExitCode, String> {
    let to = match to {
        Some(to) => to,
        None => {
            let current = backend::history()?.into_iter().find(|g| g.current);
            current.ok_or("there's no current generation")?.number
        }
    };
    let (before, after) = (generation_model(from)?, generation_model(to)?);
    println!("generation {from} -> {to}");
    print!("{}", distro_model::plan(&before, &after));
    Ok(ExitCode::SUCCESS)
}

fn generation_model(number: u32) -> Result<Model, String> {
    let path = backend::generation_model(number);
    if !path.exists() {
        return Err(format!(
            "generation {number} has no model; it doesn't exist, or distro didn't build it"
        ));
    }
    crate::read_model(&path)
}

pub fn rollback() -> Result<ExitCode, String> {
    if backend::trying() {
        let generation = backend::end_trial()?;
        println!("stopped trying; back to generation {}", generation.number);
        return Ok(ExitCode::SUCCESS);
    }
    let generation = backend::rollback()?;
    println!("rolled back to generation {}", generation.number);
    Ok(ExitCode::SUCCESS)
}

/// Resolves the packages of every role in `models` and writes the one lock next to `config`,
/// then prints how each role's packages changed. The first role pins the sources and the others
/// keep those pins, so every role runs the same software. With `update`, those sources move to
/// their newest version first, or every source when it names none. A configuration of one
/// machine has a single role, `None`. A project with a local file passes `shared`, the project
/// without it: what that uses goes in `distro.lock`, and the rest in `distro.local.lock`.
pub fn lock(
    root: &Path,
    config: &Path,
    models: &[(Option<String>, Model)],
    update: Option<&[String]>,
    shared: Option<&Model>,
) -> Result<ExitCode, String> {
    let models = match update {
        Some(names) => follow_newest(models, names)?,
        None => models.to_vec(),
    };
    let mut lock: Option<Lock> = None;
    for (_, model) in &models {
        let wanted = match &lock {
            Some(lock) => model.clone().with_lock(lock),
            None => model.clone(),
        };
        let resolved = resolve(root, &wanted)?;
        match &mut lock {
            Some(lock) => {
                lock.sources.extend(resolved.sources);
                lock.packages.extend(resolved.packages);
            }
            None => lock = Some(resolved),
        }
    }
    let mut lock = lock.ok_or("there's nothing to lock")?;
    // Files were read from these versions, whatever resolving found since.
    lock.sources.extend(crate::fetch::fetched());
    match shared {
        Some(shared) => {
            let (ours, local) = crate::project::split_lock(lock.clone(), shared);
            write_lock(config, &ours)?;
            write_lock_file(&config.join(crate::project::LOCAL_LOCK), &local)?;
        }
        None => write_lock(config, &lock)?,
    }
    let count = lock.packages.len();
    let noun = if count == 1 { "package" } else { "packages" };
    let sources: Vec<&str> = lock.sources.keys().map(String::as_str).collect();
    println!("locked {count} {noun} from {}", sources.join(", "));
    for (role, model) in &models {
        let plan = distro_model::plan(model, &model.clone().with_lock(&lock));
        if plan.is_empty() {
            continue;
        }
        if let Some(role) = role {
            println!("role {role}");
        }
        print!("{plan}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The models with the sources `names` unpinned, or all of them when it names none, so resolving
/// takes their newest versions. A name has to be a source of at least one of them.
fn follow_newest(
    models: &[(Option<String>, Model)],
    names: &[String],
) -> Result<Vec<(Option<String>, Model)>, String> {
    let mut models = models.to_vec();
    for (_, model) in &mut models {
        model
            .sources
            .entry(DEFAULT_SOURCE.to_owned())
            .or_insert_with(|| Source::follow(DEFAULT_URL));
    }
    let known: BTreeSet<&str> = models
        .iter()
        .flat_map(|(_, model)| model.sources.keys().map(String::as_str))
        .collect();
    if let Some(unknown) = names.iter().find(|name| !known.contains(name.as_str())) {
        let known: Vec<&str> = known.into_iter().collect();
        return Err(format!(
            "there's no source `{unknown}`; the configuration has {}",
            known.join(", ")
        ));
    }
    for (_, model) in &mut models {
        for (name, source) in &mut model.sources {
            if names.is_empty() || names.contains(name) {
                *source = Source::follow(&source.url);
            }
        }
    }
    Ok(models)
}

/// What the default source follows when nothing says otherwise. The factory pins it.
const DEFAULT_URL: &str = "github:NixOS/nixpkgs/nixos-unstable";

fn resolve(root: &Path, model: &Model) -> Result<Lock, String> {
    let work = tempfile::tempdir().map_err(|e| format!("cannot create a work directory: {e}"))?;
    backend::resolve(model, &factory(root)?, work.path())
}

pub(crate) fn write_lock(config: &Path, lock: &Lock) -> Result<(), String> {
    write_lock_file(&config.join(LOCK_FILE), lock)
}

fn write_lock_file(path: &Path, lock: &Lock) -> Result<(), String> {
    std::fs::write(path, lock.to_toml()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Prints each source: what it follows, where it's pinned and how old that is.
pub fn sources(model: &Model) -> Result<ExitCode, String> {
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs() / 86_400) as i64;
    print!("{}", sources_table(model, today));
    Ok(ExitCode::SUCCESS)
}

/// `sources` as data: each source by name, with what it follows and where it's pinned.
pub fn sources_json(model: &Model) -> serde_json::Value {
    model
        .sources
        .iter()
        .map(|(name, source)| {
            let entry = serde_json::json!({
                "url": source.url,
                "locked": source.is_locked(),
                "rev": source.rev,
                "date": source.date,
            });
            (name.clone(), entry)
        })
        .collect::<serde_json::Map<_, _>>()
        .into()
}

/// The table `sources` prints, with ages counted to `today`, in days since 1970.
fn sources_table(model: &Model, today: i64) -> String {
    let mut rows: Vec<[String; 3]> = model
        .sources
        .iter()
        .map(|(name, source)| [name.clone(), source.url.clone(), pin(source, today)])
        .collect();
    if !model.sources.contains_key(DEFAULT_SOURCE) {
        let factory = [
            DEFAULT_SOURCE.to_owned(),
            "the factory's".to_owned(),
            String::new(),
        ];
        rows.insert(0, factory);
    }
    let width = |column: usize| rows.iter().map(|row| row[column].len()).max().unwrap_or(0);
    let (names, urls) = (width(0), width(1));
    rows.iter()
        .map(|[name, url, pin]| {
            format!("{name:names$}  {url:urls$}  {pin}")
                .trim_end()
                .to_owned()
                + "\n"
        })
        .collect()
}

/// Where a source is pinned, as `1d06008, 2026-10-30, 7 days old`.
fn pin(source: &Source, today: i64) -> String {
    if !source.is_locked() {
        return "not locked yet".to_owned();
    }
    let rev = source
        .rev
        .as_deref()
        .map_or("pinned by hash", |rev| &rev[..rev.len().min(7)]);
    match source.date.as_deref() {
        Some(date) => match days_since_epoch(date) {
            Some(days) => format!("{rev}, {date}, {}", age(today - days)),
            None => format!("{rev}, {date}"),
        },
        None => rev.to_owned(),
    }
}

fn age(days: i64) -> String {
    match days {
        ..=0 => "from today".to_owned(),
        1 => "1 day old".to_owned(),
        days => format!("{days} days old"),
    }
}

/// Days from 1970-01-01 to a date written `2026-10-30`, in the proleptic Gregorian calendar.
fn days_since_epoch(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Howard Hinnant's days_from_civil.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// The factory under `root`, the flake that builds systems.
fn factory(root: &Path) -> Result<PathBuf, String> {
    let factory = root.join("factory/nix");
    if factory.join("flake.nix").is_file() {
        Ok(factory)
    } else {
        Err(format!(
            "can't find distro's factory in {}; run distro from its repository, or set \
             DISTRO_ROOT to where `factory/nix` is",
            root.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_says_when_a_change_takes_full_effect() {
        assert_eq!(full_effect(Tier::Live), None);
        assert!(
            full_effect(Tier::NextLogin)
                .unwrap()
                .contains("log in again")
        );
        assert!(
            full_effect(Tier::SoftReboot)
                .unwrap()
                .contains("systemctl soft-reboot")
        );
        assert!(
            full_effect(Tier::Reboot)
                .unwrap()
                .contains("after a reboot")
        );
    }

    #[test]
    fn status_says_whether_the_running_system_is_the_configuration() {
        let model = Model::default();
        assert_eq!(
            matching(Some(&model.hash()), &model),
            "it matches the configuration"
        );
        assert!(matching(Some("0123"), &model).contains("differs"));
        assert!(matching(None, &model).contains("didn't build it"));
    }

    #[test]
    fn sources_show_their_pin_and_its_age() {
        assert_eq!(days_since_epoch("1970-01-01"), Some(0));
        assert_eq!(days_since_epoch("2000-03-01"), Some(11_017));
        assert_eq!(days_since_epoch("2026-13-01"), None);
        let today = days_since_epoch("2026-11-06").unwrap();

        let mut model = Model::default();
        model.sources.insert(
            "stable".into(),
            Source {
                url: "github:NixOS/nixpkgs/nixos-25.11".into(),
                rev: Some("b6018f87da91".into()),
                hash: "sha256-abc".into(),
                date: Some("2026-10-30".into()),
            },
        );
        model
            .sources
            .insert("jq".into(), Source::follow("github:jqlang/jq"));

        insta::assert_snapshot!(sources_table(&model, today), @"
        nixpkgs  the factory's
        jq       github:jqlang/jq                  not locked yet
        stable   github:NixOS/nixpkgs/nixos-25.11  b6018f8, 2026-10-30, 7 days old
        ");
    }

    #[test]
    fn apply_asks_unless_told_not_to_and_never_guesses() {
        assert_eq!(ask(true, true), Ask::No);
        assert_eq!(ask(true, false), Ask::No);
        assert_eq!(ask(false, true), Ask::Yes);
        assert_eq!(ask(false, false), Ask::Refuse);
    }

    #[test]
    fn clean_keeps_the_current_generation_and_the_newest_before_it() {
        let generation = |number, current| Generation {
            number,
            created: String::new(),
            current,
            model_hash: None,
        };
        let generations = [
            generation(1, false),
            generation(2, false),
            generation(3, true),
            generation(4, false),
        ];
        assert_eq!(to_remove(&generations, 1), [1, 2]);
        assert_eq!(to_remove(&generations, 0), [1, 2, 4]);
        assert_eq!(to_remove(&generations, 5), Vec::<u32>::new());
    }

    #[test]
    fn tries_last_as_long_as_asked() {
        assert_eq!(parse_duration("90"), Ok(90));
        assert_eq!(parse_duration("90s"), Ok(90));
        assert_eq!(parse_duration("5m"), Ok(300));
        assert_eq!(parse_duration("1h"), Ok(3600));
        for bad in ["", "0", "5d", "m", "-5m", "1.5h"] {
            assert!(parse_duration(bad).is_err(), "{bad}");
        }
        assert_eq!(duration(300), "5 minutes");
        assert_eq!(duration(3600), "1 hour");
        assert_eq!(duration(90), "90 seconds");
    }

    #[test]
    fn update_unpins_the_sources_it_names_or_all_of_them() {
        let pinned = |url: &str| Source {
            url: url.into(),
            rev: Some("1d06008".into()),
            hash: "sha256-abc".into(),
            date: None,
        };
        let mut model = Model::default();
        model.sources.insert("nixpkgs".into(), pinned("github:a/b"));
        model.sources.insert("stable".into(), pinned("github:c/d"));

        let mut other = Model::default();
        other.sources.insert("edge".into(), pinned("github:e/f"));
        let models = [(None, model), (Some("other".to_owned()), other)];
        let follow = |names: &[String]| follow_newest(&models, names);

        let one = follow(&["stable".into()]).unwrap();
        assert!(one[0].1.sources["nixpkgs"].is_locked());
        assert_eq!(one[0].1.sources["stable"], Source::follow("github:c/d"));
        assert!(one[1].1.sources["edge"].is_locked());

        let edge = follow(&["edge".into()]).unwrap();
        assert_eq!(edge[1].1.sources["edge"], Source::follow("github:e/f"));

        let all = follow(&[]).unwrap();
        assert!(!all[0].1.sources.values().any(Source::is_locked));
        assert!(!all[1].1.sources.values().any(Source::is_locked));

        let unknown = follow(&["stabel".into()]).unwrap_err();
        assert_eq!(
            unknown,
            "there's no source `stabel`; the configuration has edge, nixpkgs, stable"
        );

        let fresh = follow_newest(&[(None, Model::default())], &[]).unwrap();
        assert_eq!(fresh[0].1.sources["nixpkgs"], Source::follow(DEFAULT_URL));
    }

    #[test]
    fn status_says_which_filesystems_run_with_a_disk_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(degraded(&dir.path().join("none")).is_empty());
        std::fs::write(
            dir.path().join("degraded-root"),
            " /dev/mapper/nvme1-system\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("other"), "").unwrap();

        assert_eq!(
            degraded(dir.path()),
            [
                "warning: root is running without /dev/mapper/nvme1-system; once the disk is back \
              or replaced, rebalance it with `btrfs balance start -dconvert=raid1 \
              -mconvert=raid1 /`"
            ]
        );
    }
}
