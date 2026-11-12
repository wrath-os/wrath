//! Building systems and moving between generations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use distro_model::{DISTRO_SOURCE, LOCK_FILE, Lock, Model, Source, Tier};

use crate::switching::{self as here, Generation};

/// Prints where each of `built`, by name, is, and links it at `link` when given: a directory gets
/// one link per name in it, any other path one link, which replaces a link already there.
pub fn show_built(built: &[(String, PathBuf)], link: Option<&Path>) -> Result<ExitCode, String> {
    for (_, path) in built {
        println!("{}", path.display());
    }
    if let Some(link) = link {
        for (at, target) in link_places(built, link)? {
            if at.symlink_metadata().is_ok_and(|meta| !meta.is_symlink()) {
                return Err(format!(
                    "{} is there already and isn't a link",
                    at.display()
                ));
            }
            let _ = std::fs::remove_file(&at);
            std::os::unix::fs::symlink(&target, &at)
                .map_err(|e| format!("{}: {e}", at.display()))?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Where each link to `built` goes for `link`, with what it points at.
fn link_places(
    built: &[(String, PathBuf)],
    link: &Path,
) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    if link.is_dir() {
        return Ok(built
            .iter()
            .map(|(name, path)| (link.join(name), path.clone()))
            .collect());
    }
    match built {
        [(_, path)] => Ok(vec![(link.to_path_buf(), path.clone())]),
        _ => Err(format!(
            "{} isn't a directory, so it can only link one of what was built",
            link.display()
        )),
    }
}

/// Each secret's encrypted file, by name, read from where the configuration has it and checked
/// against the hash the model was made with, so the system holds what was planned.
fn encrypted_secrets(model: &Model) -> Result<BTreeMap<String, Vec<u8>>, String> {
    model
        .secrets
        .iter()
        .map(|(name, secret)| {
            let bytes = std::fs::read(&secret.source)
                .map_err(|e| format!("cannot read secret `{name}` from {}: {e}", secret.source))?;
            if blake3::hash(&bytes).to_hex().as_str() != secret.hash {
                return Err(format!(
                    "secret `{name}` changed in {} since the configuration was read",
                    secret.source
                ));
            }
            Ok((name.clone(), bytes))
        })
        .collect()
}

/// Builds `model` into a generation with distro's own builder, from the recipes in `build/` under
/// `root`, and returns where it is. Packages without a recipe are left out, with a warning.
pub fn build_generation(root: &Path, model: &Model, test: bool) -> Result<PathBuf, String> {
    let repository = root
        .join("build")
        .canonicalize()
        .map_err(|e| format!("cannot find the recipes in {}: {e}", root.display()))?;
    let kept = distro_build::graph::Kept {
        text: model.canonical(),
        hash: model.hash(),
        secrets: encrypted_secrets(model)?,
    };
    let mut model = serde_json::from_str(&kept.text).map_err(|e| e.to_string())?;
    if test {
        add_test_layer(&mut model);
    }
    let system = distro_build::graph::system(&repository, &model, &kept)?;
    match missing(&system.skipped, &system.unfound) {
        Some(message) => Err(message),
        None => Ok(system.path),
    }
}

/// What a configuration asks for that nothing builds: packages without a recipe, and units no
/// package provides. A system that silently went without them wouldn't be the one described.
fn missing(packages: &[String], units: &[String]) -> Option<String> {
    let mut lines: Vec<String> = packages
        .iter()
        .map(|package| format!("there's no recipe for the package `{package}`"))
        .collect();
    lines.extend(
        units
            .iter()
            .map(|unit| format!("`{unit}` is enabled, but no package the system has provides it")),
    );
    (!lines.is_empty()).then(|| lines.join("\n"))
}

/// What a test system adds so the VM harness can drive it: the agent on vsock, the harness's
/// share and the host's store over 9p, which the kernel has built in, and the console on the
/// serial port. Only the system has these; the model it keeps is the configuration's own, so
/// plans and switches don't see them.
fn add_test_layer(model: &mut serde_json::Value) {
    use serde_json::json;
    model["enabled"]["distro-agent.service"] = json!(["multi-user.target"]);
    for param in ["console=ttyS0", "systemd.journald.forward_to_console=1"] {
        if let Some(params) = model["base"]["boot"]["kernel_params"].as_array_mut() {
            params.push(json!(param));
        } else {
            model["base"]["boot"]["kernel_params"] = json!([param]);
        }
    }
    for (unit, tag, at) in [
        (
            "run-distro\\x2dshare.mount",
            "distro-share",
            "/run/distro-share",
        ),
        (
            "run-distro\\x2dstore.mount",
            "distro-store",
            "/run/distro-store",
        ),
    ] {
        model["units"][unit] = json!({"sections": [
            {"name": "Unit", "entries": [
                ["Description", format!("The harness's {tag}")],
                ["DefaultDependencies", "no"],
                ["Before", "local-fs.target"],
            ]},
            {"name": "Mount", "entries": [
                ["What", tag],
                ["Where", at],
                ["Type", "9p"],
                ["Options", "trans=virtio,version=9p2000.L,msize=1048576,ro,nofail"],
            ]},
            {"name": "Install", "entries": [["WantedBy", "local-fs.target"]]},
        ]});
    }
}

/// The kernel options an image gets when it's given none: a shell when the initrd can't find the
/// root, and the console on the screen with the serial port as well.
const IMAGE_OPTIONS: &str = "distro.rescue console=ttyS0,115200 console=tty0";

/// Builds `model` into a generation and a disk image that boots it, with `options` added to the
/// kernel's own and, when given, `config`, the configuration it's from, as the system's own.
/// Returns where the image is: in `build/work/images`, named by the generation.
pub fn build_image(
    root: &Path,
    config: Option<&Path>,
    model: &Model,
    options: &[String],
    test: bool,
) -> Result<PathBuf, String> {
    let generation = build_generation(root, model, test)?;
    let name = generation
        .file_name()
        .ok_or("a generation has a name")?
        .to_string_lossy()
        .into_owned();
    let work = std::path::absolute(root.join("build/work")).map_err(|e| e.to_string())?;
    let images = work.join("images");
    std::fs::create_dir_all(&images).map_err(|e| format!("{}: {e}", images.display()))?;
    let out = images.join(format!("{name}.img"));
    // A test system has its console on the serial port, where the harness reads it.
    let options = if options.is_empty() && !test {
        IMAGE_OPTIONS.to_owned()
    } else {
        options.join(" ")
    };
    distro_build::image::image(
        &distro_build::store::Lab::new(&work),
        &distro_build::store::StorePath::from_hash(&name),
        config,
        &options,
        &work.join("image"),
        &out,
    )?;
    Ok(out)
}

/// What today's recipes rebuild in a system built from `built_from`, the text of its
/// [`distro_build::graph::PACKAGE_KEYS`], as changes for a plan. `None`, with a note saying why,
/// when that can't be told: the system doesn't say what it was built from, or there are no
/// recipes here to compare with.
pub(crate) fn rebuilt(
    root: &Path,
    model: &Model,
    built_from: Option<&str>,
) -> Option<Vec<distro_model::Change>> {
    let Some(before) = built_from.and_then(|text| serde_json::from_str(text).ok()) else {
        crate::style::note(
            "what runs doesn't say which packages it was built from, so the plan can't show \
             what a newer distro rebuilds",
        );
        return None;
    };
    let repository = root.join("build").canonicalize().ok()?;
    let value = serde_json::from_str(&model.canonical()).ok()?;
    match distro_build::graph::package_keys(&repository, &value) {
        Ok(after) => Some(distro_model::rebuilt(model, &before, &after)),
        Err(message) => {
            crate::style::warning(&format!(
                "cannot tell what a newer distro rebuilds: {message}"
            ));
            None
        }
    }
}

/// `plan` with the packages `rebuilt` lists, leaving out those the plan adds or removes.
pub(crate) fn with_rebuilt(
    mut plan: distro_model::Plan,
    rebuilt: Option<Vec<distro_model::Change>>,
) -> distro_model::Plan {
    for change in rebuilt.into_iter().flatten() {
        if !plan.packages.iter().any(|known| known.name == change.name) {
            plan.packages.push(change);
        }
    }
    plan
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
    let rebuilt = rebuilt(root, &model, here::running_built_from().as_deref());
    let current = here::history()
        .ok()
        .and_then(|all| all.into_iter().find(|generation| generation.current))
        .and_then(|generation| generation.model_hash);
    let same = already_there(
        &hash,
        here::running_model_hash().as_deref(),
        current.as_deref(),
        here::trying(),
    );
    let unfinished = here::unfinished();
    if let Some(unfinished) = &unfinished {
        println!("{}", unfinished_note(&unfinished.error));
    }
    if record.is_none()
        && same
        && unfinished.is_none()
        && rebuilt.as_ref().is_some_and(Vec::is_empty)
    {
        println!("the running system already has model {}", &hash[..16]);
        return Ok(ExitCode::SUCCESS);
    }
    if let Some(record) = &record {
        println!("{record}");
    }
    let Some(tier) = planned(&model, rebuilt, yes)? else {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    };
    let system = build_generation(root, &model, false)?;
    if let Some(record) = record {
        record.write()?;
    }
    switched(&system, how, tier)
}

/// What to say about a switch to the running system that failed partway with `error`.
pub(crate) fn unfinished_note(error: &str) -> String {
    format!("switching to what runs failed partway ({error}); switching again finishes it")
}

/// Whether a machine already has model `hash`: running it, as the generation it boots, and not in
/// the middle of trying another. After trying or a boot that fell back, the running system can
/// have it while the generation the machine boots doesn't, or the other way round.
fn already_there(hash: &str, running: Option<&str>, current: Option<&str>, trying: bool) -> bool {
    !trying && running == Some(hash) && current.is_none_or(|current| current == hash)
}

/// Switches to `system`, built before, after showing the plan from the running system to it and
/// asking, as `apply` does for a configuration. With `yes` it switches without a plan: whoever
/// built it, such as `apply --host`, showed the whole plan already, including what a newer distro
/// rebuilt, which the machine can't tell.
pub fn apply_built(system: &Path, how: Switch, yes: bool) -> Result<ExitCode, String> {
    let model = crate::read_model(&system.join("etc/distro/model.json"))
        .map_err(|e| format!("{} isn't a system distro built: {e}", system.display()))?;
    if yes {
        return switched(system, how, None);
    }
    let Some(tier) = planned(&model, None, false)? else {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    };
    switched(system, how, tier)
}

/// Shows the plan from the running system to `model` and asks whether to switch. Gives the tier
/// to say when the switch takes full effect, or `None` when the answer is no.
fn planned(
    model: &Model,
    rebuilt: Option<Vec<distro_model::Change>>,
    yes: bool,
) -> Result<Option<Option<Tier>>, String> {
    let running = here::switching_from_model().and_then(|path| crate::read_model(&path).ok());
    let plan = distro_model::plan(running.as_ref().unwrap_or(&Model::default()), model);
    let plan = with_rebuilt(plan, rebuilt);
    match &running {
        Some(_) => crate::print_plan(&plan),
        None => println!(
            "distro didn't build the running system, so all of it is new: {}",
            plan.summary(5)
        ),
    }
    if !confirmed(yes, "switch to it?")? {
        return Ok(None);
    }
    Ok(Some(running.and(plan.tier())))
}

fn switched(system: &Path, how: Switch, tier: Option<Tier>) -> Result<ExitCode, String> {
    let code = switch_to(system, how)?;
    if let Some(note) = tier.and_then(full_effect) {
        println!("{note}");
    }
    Ok(code)
}

/// Whether the lock next to `config` lacks something `model` needs pinned: a source, or a uid or
/// gid. Distro itself is only pinned by a distro that knows where it came from, which one built
/// from a checkout doesn't.
pub(crate) fn needs_locking(root: &Path, config: &Path, model: &Model) -> Result<bool, String> {
    let kept = crate::read_lock(config)?.unwrap_or_default().ids;
    let distro_unpinned =
        !model.sources.contains_key(DISTRO_SOURCE) && crate::pinned::running(root).is_some();
    let unpinned = model.sources.values().any(|source| !source.is_locked());
    Ok(distro_unpinned || unpinned || kept.merged(&model.ids())? != kept)
}

/// `model` with its sources pinned: pins what the lock next to `config` doesn't have yet, so the
/// lock always describes what runs.
pub(crate) fn locked(root: &Path, config: &Path, model: Model) -> Result<Model, String> {
    let kept = crate::read_lock(config)?.unwrap_or_default().ids;
    let ids = kept.merged(&model.ids())?;
    if model.is_locked() && ids == kept {
        crate::fetch::record(config)?;
        return Ok(model);
    }
    let distro = crate::pinned::for_lock(
        crate::pinned::running(root),
        model.sources.get(DISTRO_SOURCE),
        false,
    );
    let mut lock = Lock {
        sources: pins(&model)?,
        ids,
    };
    lock.sources
        .extend(distro.map(|d| (DISTRO_SOURCE.to_owned(), d)));
    write_lock(config, &lock)?;
    Ok(model.with_lock(&lock))
}

/// The pin of each source `model` names besides distro: the version files were read from, the one
/// it has, or the newest for a source nothing was read from yet.
fn pins(model: &Model) -> Result<BTreeMap<String, Source>, String> {
    let fetched = crate::fetch::fetched();
    model
        .sources
        .iter()
        .filter(|(name, _)| name.as_str() != DISTRO_SOURCE)
        .map(|(name, source)| {
            let pinned = match fetched.get(name) {
                Some(read) => read.clone(),
                None if source.is_locked() => source.clone(),
                None => {
                    crate::trees::follow(&source.url)
                        .map_err(|message| format!("source `{name}`: {message}"))?
                        .0
                }
            };
            Ok((name.clone(), pinned))
        })
        .collect()
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
pub(crate) fn full_effect(tier: Tier) -> Option<&'static str> {
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
/// configured models' hashes, and whether the lock pins every source.
pub fn status_json(model: &Model) -> serde_json::Value {
    let current = here::history()
        .ok()
        .and_then(|generations| generations.into_iter().find(|g| g.current));
    let running = here::running_model_hash();
    let hash = model.hash();
    serde_json::json!({
        "generation": current.map(|g| g.number),
        "trying": here::trying(),
        "matches": running.as_deref() == Some(hash.as_str()),
        "running": running,
        "model": hash,
        "locked": model.is_locked(),
    })
}

/// `history` as data: each generation, with a summary of what changed from the one before.
pub fn history_json() -> Result<serde_json::Value, String> {
    let mut before: Option<Model> = None;
    let mut generations = Vec::new();
    for generation in here::history()? {
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
    let current = here::history()
        .ok()
        .and_then(|generations| generations.into_iter().find(|g| g.current));
    if here::trying() {
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
    println!("{}", matching(here::running_model_hash().as_deref(), model));
    if let Some(unfinished) = here::unfinished() {
        println!("{}", unfinished_note(&unfinished.error));
    }
    if !model.is_locked() {
        println!("the lock doesn't pin every source yet; `distro apply` pins them first");
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
fn matching(running: Option<&str>, model: &Model) -> &'static str {
    match running {
        Some(hash) if hash == model.hash() => "it matches the configuration",
        Some(_) => "the configuration differs from it; `distro plan` shows how",
        None => "distro didn't build it; `distro plan` shows the whole configuration as new",
    }
}

/// Switches to `system`, already built, as `how` says.
fn switch_to(system: &Path, how: Switch) -> Result<ExitCode, String> {
    match how {
        Switch::Apply => activate(system),
        Switch::Try(seconds) => {
            here::try_system(system, seconds)?;
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
    let number = here::keep_trial()?;
    println!("kept it as generation {number}");
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
    let generations = here::history()?;
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
    let freed = here::clean(&old)?;
    println!("{freed}");
    Ok(ExitCode::SUCCESS)
}

/// The generations to remove: all but the current one, the running and booted ones, and the `keep`
/// newest others.
fn to_remove(generations: &[Generation], keep: usize) -> Vec<u32> {
    let mut others: Vec<u32> = generations
        .iter()
        .filter(|generation| !generation.current && !generation.in_use)
        .map(|generation| generation.number)
        .collect();
    others.sort_unstable();
    others.truncate(others.len().saturating_sub(keep));
    others
}

pub fn activate(system: &Path) -> Result<ExitCode, String> {
    let switched = here::activate(system);
    if distro_build::progress::verbose() {
        let steps = Path::new("/").join(crate::activate::SWITCH_STEPS);
        for line in std::fs::read_to_string(steps).unwrap_or_default().lines() {
            eprintln!("  {line}");
        }
    }
    switched?;
    println!("switched to {}", system.display());
    Ok(ExitCode::SUCCESS)
}

/// Lists the generations, each with what changed from the one before it.
pub fn history() -> Result<ExitCode, String> {
    let mut before: Option<Model> = None;
    for generation in here::history()? {
        println!("{generation}");
        let model = generation_model(generation.number).ok();
        if let (Some(before), Some(after)) = (&before, &model) {
            println!("      {}", distro_model::plan(before, after).summary(4));
        }
        before = model;
    }
    Ok(ExitCode::SUCCESS)
}

/// Shows what changed in generation `number`, from the one before it.
pub fn changes_in(number: u32) -> Result<ExitCode, String> {
    let before = here::history()?
        .into_iter()
        .map(|generation| generation.number)
        .filter(|earlier| *earlier < number)
        .max()
        .ok_or(format!("nothing came before generation {number}"))?;
    diff(before, number)
}

/// Shows what changed going from generation `from` to `to`.
pub fn diff(from: u32, to: u32) -> Result<ExitCode, String> {
    let (before, after) = (generation_model(from)?, generation_model(to)?);
    println!("generation {from} -> {to}");
    print!("{}", distro_model::plan(&before, &after));
    Ok(ExitCode::SUCCESS)
}

fn generation_model(number: u32) -> Result<Model, String> {
    let path = here::generation_model(number)?;
    if !path.exists() {
        return Err(format!(
            "generation {number} has no model; it doesn't exist, or distro didn't build it"
        ));
    }
    crate::read_model(&path)
}

pub fn rollback(to: Option<u32>) -> Result<ExitCode, String> {
    if here::trying() {
        if to.is_some() {
            return Err("a system is being tried; `distro rollback` stops trying first".into());
        }
        let number = here::end_trial()?;
        println!("stopped trying; back to generation {number}");
        return Ok(ExitCode::SUCCESS);
    }
    let number = here::rollback(to)?;
    println!("rolled back to generation {number}");
    Ok(ExitCode::SUCCESS)
}

/// Pins the sources of every role in `models` and writes the one lock next to `config`, then
/// prints how each role changed. The first role pins the sources and the others keep those pins,
/// so every role runs the same software. With `update`, those sources move to their newest
/// version first, or every source when it names none. A configuration of one machine has a
/// single role, `None`. A project with a local file passes `shared`, the project without it:
/// what that uses goes in `distro.lock`, and the rest in `distro.local.lock`.
pub fn lock(
    root: &Path,
    config: &Path,
    models: &[(Option<String>, Model)],
    update: Option<&[String]>,
    shared: Option<&Model>,
) -> Result<ExitCode, String> {
    let distro = crate::pinned::for_lock(
        crate::pinned::running(root),
        models
            .first()
            .and_then(|(_, model)| model.sources.get(DISTRO_SOURCE)),
        update.is_some_and(|names| names.is_empty() || names.iter().any(|n| n == DISTRO_SOURCE)),
    );
    let models = match update {
        Some(names) => follow_newest(models, names)?,
        None => models.to_vec(),
    };
    let mut lock = Lock {
        ids: crate::read_lock(config)?.unwrap_or_default().ids,
        ..Lock::default()
    };
    for (_, model) in &models {
        lock.sources.extend(pins(&model.clone().with_lock(&lock))?);
        lock.ids = lock.ids.merged(&model.ids())?;
    }
    lock.sources
        .extend(distro.map(|d| (DISTRO_SOURCE.to_owned(), d)));
    match shared {
        Some(shared) => {
            let (ours, local) = crate::project::split_lock(lock.clone(), shared);
            write_lock(config, &ours)?;
            write_lock_file(&config.join(crate::project::LOCAL_LOCK), &local)?;
        }
        None => write_lock(config, &lock)?,
    }
    let sources: Vec<&str> = lock.sources.keys().map(String::as_str).collect();
    match sources.as_slice() {
        [] => println!("there's nothing to pin: this distro was built from a checkout"),
        _ => println!("pinned {}", sources.join(", ")),
    }
    for (role, model) in &models {
        let plan = distro_model::plan(model, &model.clone().with_lock(&lock));
        if plan.is_empty() {
            continue;
        }
        if let Some(role) = role {
            println!("role {role}");
        }
        crate::print_plan(&plan);
    }
    Ok(ExitCode::SUCCESS)
}

/// The models with the sources `names` unpinned, or all of them when it names none, so locking
/// takes their newest versions. A name has to be a source of at least one of them.
fn follow_newest(
    models: &[(Option<String>, Model)],
    names: &[String],
) -> Result<Vec<(Option<String>, Model)>, String> {
    let mut models = models.to_vec();
    let known: BTreeSet<&str> = models
        .iter()
        .flat_map(|(_, model)| model.sources.keys().map(String::as_str))
        .chain([DISTRO_SOURCE])
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
    if !model.sources.contains_key(DISTRO_SOURCE) {
        let running = [
            DISTRO_SOURCE.to_owned(),
            "the running distro's".to_owned(),
            String::new(),
        ];
        rows.insert(0, running);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_machine_has_a_model_only_when_it_runs_and_boots_it_and_isnt_trying_another() {
        assert!(already_there("m", Some("m"), Some("m"), false));
        // A machine that was never installed by distro has no generations to compare.
        assert!(already_there("m", Some("m"), None, false));
        assert!(!already_there("m", Some("m"), Some("m"), true), "trying it");
        assert!(
            !already_there("m", Some("m"), Some("old"), false),
            "it booted another"
        );
        assert!(
            !already_there("m", Some("old"), Some("m"), false),
            "it fell back"
        );
        assert!(!already_there("m", None, None, false));
    }

    #[test]
    fn a_build_is_linked_where_asked_or_by_name_in_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let built = |names: &[&str]| -> Vec<(String, PathBuf)> {
            names
                .iter()
                .map(|name| {
                    (
                        (*name).to_owned(),
                        PathBuf::from(format!("/store/x-{name}")),
                    )
                })
                .collect()
        };
        let at = dir.path().join("jq-link");
        assert_eq!(
            link_places(&built(&["jq"]), &at).unwrap(),
            [(at.clone(), PathBuf::from("/store/x-jq"))]
        );
        assert_eq!(
            link_places(&built(&["jq", "ripgrep"]), dir.path()).unwrap(),
            [
                (dir.path().join("jq"), PathBuf::from("/store/x-jq")),
                (
                    dir.path().join("ripgrep"),
                    PathBuf::from("/store/x-ripgrep")
                ),
            ]
        );
        assert!(link_places(&built(&["jq", "ripgrep"]), &at).is_err());

        show_built(&built(&["jq"]), Some(&at)).unwrap();
        show_built(&built(&["ripgrep"]), Some(&at)).unwrap();
        assert_eq!(
            std::fs::read_link(&at).unwrap(),
            PathBuf::from("/store/x-ripgrep")
        );
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(show_built(&built(&["jq"]), Some(&file)).is_err());
    }

    #[test]
    fn a_system_missing_a_package_or_a_unit_isnt_built() {
        assert_eq!(missing(&[], &[]), None);
        let message = missing(&["iputils".into()], &["polkit-agent-helper.socket".into()]).unwrap();
        assert_eq!(
            message,
            "there's no recipe for the package `iputils`\n\
             `polkit-agent-helper.socket` is enabled, but no package the system has provides it"
        );
    }

    #[test]
    fn a_test_system_adds_the_agent_the_shares_and_a_serial_console() {
        let mut model = serde_json::json!({
            "base": {"boot": {"kernel_params": ["quiet"]}},
            "enabled": {"sshd.service": []},
            "files": {},
            "units": {},
        });
        add_test_layer(&mut model);
        assert_eq!(
            model["enabled"]["distro-agent.service"],
            serde_json::json!(["multi-user.target"])
        );
        assert_eq!(model["enabled"]["sshd.service"], serde_json::json!([]));
        let params = &model["base"]["boot"]["kernel_params"];
        assert_eq!(params[0], "quiet");
        assert!(
            params
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("console=ttyS0"))
        );
        for (unit, tag, at) in [
            (
                "run-distro\\x2dshare.mount",
                "distro-share",
                "/run/distro-share",
            ),
            (
                "run-distro\\x2dstore.mount",
                "distro-store",
                "/run/distro-store",
            ),
        ] {
            let text = model["units"][unit].to_string();
            assert!(text.contains(&format!("\"What\",\"{tag}\"")), "{text}");
            assert!(text.contains(&format!("\"Where\",\"{at}\"")), "{text}");
            assert!(text.contains("\"WantedBy\",\"local-fs.target\""), "{text}");
        }
    }

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
        distro  the running distro's
        jq      github:jqlang/jq                  not locked yet
        stable  github:NixOS/nixpkgs/nixos-25.11  b6018f8, 2026-10-30, 7 days old
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
    fn clean_keeps_the_current_running_and_booted_generations_and_the_newest_before_them() {
        let generation = |number, current, in_use| Generation {
            number,
            created: String::new(),
            current,
            in_use,
            model_hash: None,
        };
        let generations = [
            generation(1, false, false),
            generation(2, false, false),
            generation(3, true, false),
            generation(4, false, false),
        ];
        assert_eq!(to_remove(&generations, 1), [1, 2]);
        assert_eq!(to_remove(&generations, 0), [1, 2, 4]);
        assert_eq!(to_remove(&generations, 5), Vec::<u32>::new());
        // After a boot fell back to 1, or while 4 is being tried, neither goes.
        let generations = [
            generation(1, false, true),
            generation(2, false, false),
            generation(3, true, false),
            generation(4, false, true),
        ];
        assert_eq!(to_remove(&generations, 0), [2]);
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
        model.sources.insert("acme".into(), pinned("github:a/b"));
        model.sources.insert("stable".into(), pinned("github:c/d"));

        let mut other = Model::default();
        other.sources.insert("edge".into(), pinned("github:e/f"));
        let models = [(None, model), (Some("other".to_owned()), other)];
        let follow = |names: &[String]| follow_newest(&models, names);

        let one = follow(&["stable".into()]).unwrap();
        assert!(one[0].1.sources["acme"].is_locked());
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
            "there's no source `stabel`; the configuration has acme, distro, edge, stable"
        );
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
