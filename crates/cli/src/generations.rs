//! Building systems and moving between generations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wrath_build::graph::Seed;
use wrath_model::{LOCK_FILE, Lock, Model, Source, Tier, WRATH_SOURCE};

use crate::config_from::ConfigFrom;
use crate::switching::{self as here, Generation};

/// Prints where each of `built`, by name, is, or links it at `link` when given and prints the
/// link: a directory gets one link per name in it, any other path one link, which replaces a link
/// already there. A disk image is moved there instead, and that's what's printed.
pub fn show_built(built: &[(String, PathBuf)], link: Option<&Path>) -> Result<ExitCode, String> {
    let Some(link) = link else {
        for (_, path) in built {
            println!("{}", path.display());
        }
        return Ok(ExitCode::SUCCESS);
    };
    for (at, target) in link_places(built, link)? {
        // A disk image is a file of its own, made again by each build of it, so it moves to
        // what `-o` names rather than staying behind as well.
        if target
            .extension()
            .is_some_and(|extension| extension == "img")
        {
            if at.is_dir() {
                return Err(format!("{} is a directory", at.display()));
            }
            move_file(&target, &at)?;
            println!("{}", at.display());
            continue;
        }
        if at.symlink_metadata().is_ok_and(|meta| !meta.is_symlink()) {
            return Err(format!(
                "{} is there already and isn't a link",
                at.display()
            ));
        }
        let _ = std::fs::remove_file(&at);
        std::os::unix::fs::symlink(&target, &at).map_err(|e| format!("{}: {e}", at.display()))?;
        println!("{} -> {}", at.display(), target.display());
    }
    Ok(ExitCode::SUCCESS)
}

/// Moves `from` to `to`, replacing what's there, by renaming it, or by copying it when they're on
/// different filesystems.
fn move_file(from: &Path, to: &Path) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("{}: {e}", to.display());
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            std::fs::copy(from, to).map_err(failed)?;
            std::fs::remove_file(from).map_err(|e| format!("{}: {e}", from.display()))
        }
        renamed => renamed.map_err(failed),
    }
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

/// Prints what getting to `model` takes: what would be fetched from each cache and what would be
/// built. Asking the caches, which only traces answer, can't fail the plan; it says why instead.
pub fn print_takes(model: &Model) {
    if crate::cache::offline() || !crate::known::recipes_here() {
        return;
    }
    // Working it out keeps the traces it asks for, in the work directory; one the user can't write,
    // as a machine's for anyone but root, leaves the line out with a word on why.
    let work = crate::places::work();
    if std::fs::create_dir_all(&work).is_err() || !writable(&work) {
        println!(
            "to get there: only someone who can write {} can say",
            work.display()
        );
        return;
    }
    match system_takes(model) {
        Ok(takes) => {
            if let Some(line) = takes_line(&takes, crate::cache::name_of) {
                println!("{line}");
            }
            if takes.seed == Seed::Missing {
                crate::style::warning(wrath_build::seed::MISSING);
            }
        }
        Err(e) => crate::style::warning(&format!("can't tell what it would build: {e}")),
    }
}

/// What getting to `model` takes, asking caches for traces and building nothing.
fn system_takes(model: &Model) -> Result<wrath_build::graph::Takes, String> {
    let value = serde_json::from_str(&model.canonical()).map_err(|e| e.to_string())?;
    wrath_build::graph::system_takes(&crate::places::grapes()?, &crate::places::work(), &value)
}

/// Whether this user can make files in `dir`.
fn writable(dir: &Path) -> bool {
    tempfile::tempfile_in(dir).is_ok()
}

/// What `takes` says, naming each cache by `name_of` where it is, when there's anything to say.
fn takes_line(
    takes: &wrath_build::graph::Takes,
    name_of: impl Fn(&str) -> String,
) -> Option<String> {
    let fetched: Vec<String> = takes
        .fetch
        .iter()
        .enumerate()
        .map(|(n, (cache, (count, bytes)))| {
            let count = if n == 0 {
                crate::counted(*count, "path")
            } else {
                count.to_string()
            };
            let sized = if *bytes > 0 {
                format!(" ({})", size(*bytes))
            } else {
                String::new()
            };
            format!("{count} from {}{sized}", name_of(cache))
        })
        .collect();
    let mut parts = Vec::new();
    if !fetched.is_empty() {
        parts.push(format!("fetch {}", fetched.join(" and ")));
    }
    parts.extend(builds(takes));
    (!parts.is_empty()).then(|| format!("to get there: {}", parts.join(", ")))
}

/// What `takes` builds, such as `build 2 (glibc; 1 built with it)`, when it builds anything.
fn builds(takes: &wrath_build::graph::Takes) -> Option<String> {
    if takes.build.is_empty() {
        return None;
    }
    // What no cache has, by name; the rest is only built with it.
    let nowhere: Vec<&str> = takes
        .build
        .iter()
        .filter(|(_, why)| **why == wrath_build::pilot::Why::Nowhere)
        .map(|(id, _)| id.as_str())
        .collect();
    let mut named = nowhere[..nowhere.len().min(5)].join(", ");
    if nowhere.len() > 5 {
        named.push_str(&format!(" and {} more", nowhere.len() - 5));
    }
    let with = takes.build.len() - nowhere.len();
    if with > 0 {
        let them = if nowhere.len() == 1 { "it" } else { "them" };
        named.push_str(&format!("; {with} built with {them}"));
    }
    let seed = match takes.seed {
        Seed::Ready => "",
        Seed::Prepare => ", preparing the seed first",
        Seed::Missing => ", which needs a seed this machine doesn't have",
    };
    Some(format!("build {} ({named}){seed}", takes.build.len()))
}

/// `bytes` as people read sizes, such as `40 MB` or `1.2 GB`.
pub(crate) fn size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// Builds `model` into a generation with wrath's own builder, from grapes' recipes, and returns
/// where it is. Packages without a recipe are left out, with a warning.
pub fn build_generation(model: &Model, test: bool) -> Result<PathBuf, String> {
    if !crate::known::recipes_here() {
        return Err(crate::known::NO_RECIPES.to_owned());
    }
    let grapes = crate::places::grapes()?;
    let repository = grapes
        .canonicalize()
        .map_err(|e| format!("cannot find the recipes in {}: {e}", grapes.display()))?;
    let kept = wrath_build::graph::Kept {
        text: model.canonical(),
        hash: model.hash(),
        secrets: encrypted_secrets(model)?,
    };
    let mut model = serde_json::from_str(&kept.text).map_err(|e| e.to_string())?;
    if test {
        add_test_layer(&mut model);
    }
    let system = wrath_build::graph::system(&repository, &crate::places::work(), &model, &kept)?;
    crate::cache::say_fetched();
    match missing(&system.skipped, &system.unfound) {
        Some(message) => Err(message),
        None => Ok(crate::places::in_store(&system.path)),
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
    model["enabled"]["wrath-agent.service"] = json!(["multi-user.target"]);
    for param in ["console=ttyS0", "systemd.journald.forward_to_console=1"] {
        if let Some(params) = model["base"]["boot"]["kernel_params"].as_array_mut() {
            params.push(json!(param));
        } else {
            model["base"]["boot"]["kernel_params"] = json!([param]);
        }
    }
    for (unit, tag, at) in [
        (
            "run-wrath\\x2dshare.mount",
            "wrath-share",
            "/run/wrath-share",
        ),
        (
            "run-wrath\\x2dstore.mount",
            "wrath-store",
            "/run/wrath-store",
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
const IMAGE_OPTIONS: &str = "wrath.rescue console=ttyS0,115200 console=tty0";

/// Builds `model` into a generation and a disk image that boots it, with `options` added to the
/// kernel's own and, when given, `config`, the configuration it's from, as the system's own, and
/// `install`, the system `wrath install` puts on the disks, built and in its store. Returns where
/// the image is: in `build/work/images`, named by the generation.
pub fn build_image(
    config: Option<&Path>,
    model: &Model,
    install: Option<&Model>,
    options: &[String],
    test: bool,
) -> Result<PathBuf, String> {
    let generation = build_generation(model, test)?;
    let install = install
        .map(|install| build_generation(install, test))
        .transpose()?;
    let store_path = |path: &Path| {
        path.file_name()
            .map(|name| wrath_build::store::StorePath::from_hash(&name.to_string_lossy()))
            .ok_or("a generation has a name")
    };
    let generation = store_path(&generation)?;
    let install = install.as_deref().map(store_path).transpose()?;
    let work = std::path::absolute(crate::places::work()).map_err(|e| e.to_string())?;
    let images = work.join("images");
    std::fs::create_dir_all(&images).map_err(|e| format!("{}: {e}", images.display()))?;
    let out = images.join(format!("{}.img", generation.hash()));
    // A test system has its console on the serial port, where the harness reads it.
    let options = if options.is_empty() && !test {
        IMAGE_OPTIONS.to_owned()
    } else {
        options.join(" ")
    };
    wrath_build::image::image(
        &wrath_build::store::Lab::new(&work),
        &generation,
        config,
        install.as_ref(),
        &options,
        &work.join("image"),
        &out,
    )?;
    Ok(out)
}

/// What today's recipes rebuild in a system built from `built_from`, the text of its
/// [`wrath_build::graph::PACKAGE_KEYS`], as changes for a plan. `None`, with a note saying why,
/// when that can't be told: the system doesn't say what it was built from, or there are no
/// recipes here to compare with.
pub(crate) fn rebuilt(model: &Model, built_from: Option<&str>) -> Option<Vec<wrath_model::Change>> {
    let Some(before) = built_from.and_then(|text| serde_json::from_str(text).ok()) else {
        crate::style::note(
            "what runs doesn't say which packages it was built from, so the plan can't show \
             what a newer wrath rebuilds",
        );
        return None;
    };
    let repository = crate::places::grapes().ok()?.canonicalize().ok()?;
    let value = serde_json::from_str(&model.canonical()).ok()?;
    match wrath_build::graph::package_keys(&repository, &crate::places::work(), &value) {
        Ok(after) => Some(wrath_model::rebuilt(model, &before, &after)),
        Err(message) => {
            crate::style::warning(&format!(
                "cannot tell what a newer wrath rebuilds: {message}"
            ));
            None
        }
    }
}

/// `plan` with the packages `rebuilt` lists, leaving out those the plan adds or removes.
pub(crate) fn with_rebuilt(
    mut plan: wrath_model::Plan,
    rebuilt: Option<Vec<wrath_model::Change>>,
) -> wrath_model::Plan {
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
    /// Make it the current generation, the one booted next, without switching to it now.
    Boot,
    /// What the configuration's `updates.switch` says, for an automatic upgrade: [`Self::Apply`]
    /// or [`Self::Boot`], depending on when the changes take effect.
    Automatic,
}

impl Switch {
    /// What this is for `model`, whose changes take effect at `tier`.
    fn resolved(self, model: &Model, tier: Option<Tier>) -> Self {
        match self {
            Self::Automatic if crate::upgrade::switches_now(crate::upgrade::mode(model), tier) => {
                Self::Apply
            }
            Self::Automatic => Self::Boot,
            Self::Apply | Self::Try(_) | Self::Boot => self,
        }
    }
}

/// Builds the system for `model` and switches to it, unless the running system already has it.
/// Packages the lock next to `config` doesn't have yet are locked first, so the lock always
/// describes the running system. `record` is what this machine records about itself before
/// switching; when it changes, the machine switches again even to the same model, so files are
/// filled in with the new values.
pub fn apply(
    config: &Path,
    model: Model,
    how: Switch,
    yes: bool,
    record: Option<crate::machine::Record>,
) -> Result<ExitCode, String> {
    let model = locked(config, model)?;
    let hash = model.hash();
    let rebuilt = rebuilt(&model, here::running_built_from().as_deref());
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
        // It may have come from elsewhere before, such as a checkout applied by hand.
        let history = here::history().unwrap_or_default();
        if let Some(generation) = history.iter().find(|g| g.current) {
            let _ = crate::config_from::keep(Path::new("/"), generation.number);
        }
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
    if how == Switch::Automatic
        && !crate::upgrade::builds(&model)
        && let Some(builds) = builds(&system_takes(&model)?)
    {
        return Ok(crate::upgrade::wait(&builds));
    }
    let system = build_generation(&model, false)?;
    if let Some(record) = record {
        record.write()?;
    }
    switched(&system, how.resolved(&model, tier), tier)
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
/// built it, such as `apply --host`, showed the whole plan already, including what a newer wrath
/// rebuilt, which the machine can't tell.
pub fn apply_built(system: &Path, how: Switch, yes: bool) -> Result<ExitCode, String> {
    let model = crate::read_model(&system.join("etc/wrath/model.json"))
        .map_err(|e| format!("{} isn't a system wrath built: {e}", system.display()))?;
    if yes {
        return switched(system, how.resolved(&model, None), None);
    }
    let Some(tier) = planned(&model, None, false)? else {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    };
    switched(system, how.resolved(&model, tier), tier)
}

/// Shows the plan from the running system to `model` and asks whether to switch. Gives the tier
/// to say when the switch takes full effect, or `None` when the answer is no.
fn planned(
    model: &Model,
    rebuilt: Option<Vec<wrath_model::Change>>,
    yes: bool,
) -> Result<Option<Option<Tier>>, String> {
    let running = here::switching_from_model().and_then(|path| crate::read_model(&path).ok());
    let plan = wrath_model::plan(running.as_ref().unwrap_or(&Model::default()), model);
    let plan = with_rebuilt(plan, rebuilt);
    match &running {
        Some(_) => crate::print_plan(&plan),
        None => println!(
            "wrath didn't build the running system, so all of it is new: {}",
            plan.summary(5)
        ),
    }
    print_takes(model);
    if !confirmed(yes, "switch to it?")? {
        return Ok(None);
    }
    Ok(Some(running.and(plan.tier())))
}

fn switched(system: &Path, how: Switch, tier: Option<Tier>) -> Result<ExitCode, String> {
    let code = switch_to(system, how)?;
    if how == Switch::Boot {
        println!("it takes effect at the next boot");
    } else if let Some(note) = tier.and_then(full_effect) {
        println!("{note}");
    }
    Ok(code)
}

/// Whether the lock next to `config` lacks something `model` needs pinned: a source, a uid or
/// gid, or grapes as this command read it. wrath itself is pinned only by `wrath update`.
pub(crate) fn needs_locking(config: &Path, model: &Model) -> Result<bool, String> {
    let lock = crate::read_lock(config)?.unwrap_or_default();
    let unpinned = model.sources.values().any(|source| !source.is_locked());
    let unread = !crate::fetch::recorded(&lock);
    let ids_unkept = lock.ids.merged(&model.ids())? != lock.ids;
    Ok(unpinned || unread || ids_unkept)
}

/// `model` with its sources pinned: pins what the lock next to `config` doesn't have yet, so the
/// lock always describes what runs.
pub(crate) fn locked(config: &Path, model: Model) -> Result<Model, String> {
    let existing = crate::read_lock(config)?;
    let kept = existing
        .as_ref()
        .map(|lock| lock.ids.clone())
        .unwrap_or_default();
    let ids = kept.merged(&model.ids())?;
    if model.is_locked() && ids == kept {
        crate::fetch::record(config)?;
        return Ok(model);
    }
    // wrath keeps its pin, which only `wrath update` moves.
    let wrath = model
        .sources
        .get(WRATH_SOURCE)
        .filter(|source| source.is_locked())
        .cloned();
    let mut lock = Lock {
        sources: pins(&model)?,
        ids,
    };
    crate::places::keep_pin(&mut lock.sources, existing.as_ref());
    lock.sources
        .extend(wrath.map(|d| (WRATH_SOURCE.to_owned(), d)));
    // No lock is the same as one pinning nothing, which a configuration read from a URL couldn't
    // write.
    if existing.unwrap_or_default() != lock {
        write_lock(config, &lock)?;
    }
    Ok(model.with_lock(&lock))
}

/// The pin of each source `model` names besides wrath: the version files were read from, the one
/// it has, or the newest for a source nothing was read from yet.
fn pins(model: &Model) -> Result<BTreeMap<String, Source>, String> {
    let fetched = crate::fetch::fetched();
    model
        .sources
        .iter()
        .filter(|(name, _)| name.as_str() != WRATH_SOURCE)
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
    let generations = here::history().unwrap_or_default();
    let running = here::running_model_hash();
    let hash = model.hash();
    serde_json::json!({
        "generation": running_of(&generations).map(|g| g.number),
        "boots_next": generations.iter().find(|g| g.current).map(|g| g.number),
        "from": running_of(&generations).and_then(|g| g.from.as_ref()).map(ConfigFrom::to_json),
        "trying": here::trying(),
        "matches": running.as_deref() == Some(hash.as_str()),
        "running": running,
        "model": hash,
        "locked": model.is_locked(),
        "replaced_home_files": crate::homes::replaced_in_homes(Path::new("/")),
        "upgrade": crate::upgrade::last(Path::new("/")).map(|o| o.to_json()),
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
            .map(|(before, after)| wrath_model::plan(before, after).summary(usize::MAX));
        generations.push(serde_json::json!({
            "number": generation.number,
            "created": generation.created,
            "current": generation.current,
            "model": generation.model_hash,
            "from": generation.from.as_ref().map(ConfigFrom::to_json),
            "changes": changes,
        }));
        before = model;
    }
    Ok(serde_json::Value::Array(generations))
}

/// Says which generation is running and whether it's the configuration's `model`.
pub fn status(model: &Model) -> Result<ExitCode, String> {
    let generations = here::history().unwrap_or_default();
    print_running(&generations);
    println!("{}", matching(here::running_model_hash().as_deref(), model));
    print_notes(crate::upgrade::last(Path::new("/")));
    // wrath from a checkout has no version to pin, so only the configuration's sources count.
    if model.sources.values().any(|source| !source.is_locked()) {
        println!("the lock doesn't pin every source yet; `wrath apply` pins them first");
    }
    Ok(ExitCode::SUCCESS)
}

/// `status` when the configuration can't be read, which said why: what runs, at least.
pub fn status_unread() -> ExitCode {
    print_running(&here::history().unwrap_or_default());
    ExitCode::FAILURE
}

/// `status` on a machine that upgrades from `follows`, offline: what runs and where it came from.
pub fn following(follows: &str) -> Result<ExitCode, String> {
    let generations = here::history().unwrap_or_default();
    print_running(&generations);
    // The current generation is the one automatic upgrades compare, as it's the one that boots.
    let from = generations
        .iter()
        .find(|g| g.current)
        .and_then(|g| g.from.as_ref());
    if let Some(from) = from.filter(|from| !from.is(follows)) {
        println!("it came from {from}; upgrades go back to {follows}");
    } else {
        println!("upgrades from {follows}; `wrath plan` shows what its newest commit would change");
    }
    if let Some(file) = crate::config_from::running_token_file() {
        match std::fs::metadata(&file) {
            Ok(_) => println!("it reads it with the token in {}", file.display()),
            Err(e) => crate::style::warning(&format!(
                "the token it reads it with, {}, isn't there: {e}",
                file.display()
            )),
        }
    }
    print_notes(crate::upgrade::last(Path::new("/")));
    Ok(ExitCode::SUCCESS)
}

/// `following` as data.
pub fn following_json(follows: &str) -> serde_json::Value {
    let generations = here::history().unwrap_or_default();
    let current = generations.iter().find(|g| g.current);
    let from = current.and_then(|g| g.from.as_ref());
    serde_json::json!({
        "generation": running_of(&generations).map(|g| g.number),
        "boots_next": current.map(|g| g.number),
        "from": from.map(ConfigFrom::to_json),
        "follows": follows,
        "upgrade": crate::upgrade::last(Path::new("/"))
            .map(|outcome| outcome.to_json()),
        "trying": here::trying(),
        "running": here::running_model_hash(),
    })
}

pub(crate) fn current_generation() -> Option<Generation> {
    here::history()
        .ok()
        .and_then(|generations| generations.into_iter().find(|g| g.current))
}

/// The generation that runs: the current one when it runs, otherwise the newest that's the
/// running system, as after `apply --boot` or a boot that fell back.
fn running_of(generations: &[Generation]) -> Option<&Generation> {
    generations
        .iter()
        .find(|g| g.current && g.running)
        .or_else(|| generations.iter().rev().find(|g| g.running))
}

/// Says which generation runs and where its configuration came from, or that a system is being
/// tried, and which generation boots next when that's another.
fn print_running(generations: &[Generation]) {
    if here::trying() {
        println!(
            "trying a system that isn't a generation yet; it switches back on its own, \
             `wrath keep` keeps it and `wrath rollback` switches back now"
        );
        return;
    }
    let running = running_of(generations);
    if let Some(generation) = running {
        println!(
            "running generation {}, from {}",
            generation.number, generation.created
        );
        if let Some(from) = &generation.from {
            println!("its configuration came from {from}");
        }
    }
    let current = generations.iter().find(|g| g.current);
    if let Some(current) = current.filter(|c| running.is_none_or(|r| r.number != c.number)) {
        println!("generation {} boots next", current.number);
    }
}

/// A switch that didn't finish, files in homes that were replaced, and how the last automatic
/// upgrade went.
fn print_notes(upgrade: Option<crate::upgrade::Outcome>) {
    if let Some(outcome) = upgrade {
        println!("{outcome}");
    }
    if let Some(unfinished) = here::unfinished() {
        println!("{}", unfinished_note(&unfinished.error));
    }
    for note in crate::homes::replaced_in_homes(Path::new("/")) {
        println!("{note}");
    }
}

/// Says which filesystems the machine runs with a disk missing. It's about the machine, so it's
/// said before anything about its configuration.
pub fn warn_degraded() {
    for note in degraded(Path::new(DEGRADED)) {
        println!("{note}");
    }
}

/// Where the boot leaves a note for each filesystem it mounted with a disk missing.
const DEGRADED: &str = "/run/wrath";

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
        Some(_) => "the configuration differs from it; `wrath plan` shows how",
        None => {
            "this machine doesn't run a system wrath built; `wrath plan` shows the whole \
             configuration as new"
        }
    }
}

/// Switches to `system`, already built, as `how` says.
fn switch_to(system: &Path, how: Switch) -> Result<ExitCode, String> {
    match how {
        // Resolved before switching; were it not, switching now is what `apply` does.
        Switch::Apply | Switch::Automatic => activate(system),
        Switch::Boot => {
            let number = here::activate_boot(system)?;
            println!("generation {number} boots next; what runs now stays as it is until then");
            Ok(ExitCode::SUCCESS)
        }
        Switch::Try(seconds) => {
            here::try_system(system, seconds)?;
            println!("trying {}", system.display());
            println!(
                "it switches back in {} unless you run `wrath keep`; `wrath rollback` switches \
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
/// they used, after saying which and asking, then what the builder left in its work directory.
/// A machine with no generations, such as one building from a checkout, only gets the latter.
pub fn clean(keep: usize, yes: bool, raw: bool) -> Result<ExitCode, String> {
    if here::here_in_store() {
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
    }
    // What builds here made before their fix-ups goes with the last package made from it, or
    // all of it with `raw`.
    let work = crate::places::work();
    if work.is_dir() {
        let lab = wrath_build::store::Lab::new(&work);
        let forgotten = wrath_build::chain::forget_raw_outputs(&lab, raw)?;
        if forgotten > 0 {
            println!(
                "removed what {} made before fix-ups",
                crate::counted(forgotten, "build")
            );
        }
        let (images, bytes) = forget_images(&work)?;
        if images > 0 {
            println!(
                "removed {} that builds left, {:.1} GB",
                crate::counted(images, "disk image"),
                bytes as f64 / 1e9
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Removes the disk images builds left in `work`, and says how many and how many bytes. Each build
/// of an image makes it again, so none is worth keeping once it has been copied or booted.
fn forget_images(work: &Path) -> Result<(usize, u64), String> {
    let images = work.join("images");
    let Ok(entries) = std::fs::read_dir(&images) else {
        return Ok((0, 0));
    };
    let (mut count, mut bytes) = (0, 0);
    for entry in entries {
        let path = entry
            .map_err(|e| format!("{}: {e}", images.display()))?
            .path();
        if path.extension().is_none_or(|extension| extension != "img") {
            continue;
        }
        // What it takes on disk: an image is sparse, so its length says more than that.
        let size = path.metadata().map_or(0, |meta| {
            std::os::unix::fs::MetadataExt::blocks(&meta) * 512
        });
        std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        count += 1;
        bytes += size;
    }
    Ok((count, bytes))
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
    if wrath_build::progress::verbose() {
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
    let generations = here::history()?;
    if generations.is_empty() {
        println!("no generations: this machine doesn't run a system wrath built");
    }
    let mut before: Option<Model> = None;
    for generation in generations {
        println!("{generation}");
        if let Some(from) = &generation.from {
            println!("      from {from}");
        }
        let model = generation_model(generation.number).ok();
        if let (Some(before), Some(after)) = (&before, &model) {
            println!("      {}", wrath_model::plan(before, after).summary(4));
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
    print!("{}", wrath_model::plan(&before, &after));
    Ok(ExitCode::SUCCESS)
}

fn generation_model(number: u32) -> Result<Model, String> {
    let path = here::generation_model(number)?;
    if !path.exists() {
        return Err(format!(
            "generation {number} has no model; it doesn't exist, or wrath didn't build it"
        ));
    }
    crate::read_model(&path)
}

pub fn rollback(to: Option<u32>) -> Result<ExitCode, String> {
    if here::trying() {
        if to.is_some() {
            return Err("a system is being tried; `wrath rollback` stops trying first".into());
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
/// what that uses goes in `wrath.lock`, and the rest in `wrath.local.lock`.
pub fn lock(
    config: &Path,
    models: &[(Option<String>, Model)],
    update: Option<&[String]>,
    shared: Option<&Model>,
) -> Result<ExitCode, String> {
    let wrath = crate::pinned::for_lock(
        models
            .first()
            .and_then(|(_, model)| model.sources.get(WRATH_SOURCE)),
        update.is_some_and(|names| names.is_empty() || names.iter().any(|n| n == WRATH_SOURCE)),
        |url| crate::trees::follow(url).map(|(pin, _)| pin),
    )?;
    // What the lock pins is worked out from these; what changed is said against `models`, as
    // pinned.
    let followed = match update {
        Some(names) => follow_newest(models, names)?,
        None => models.to_vec(),
    };
    let mut lock = Lock {
        ids: crate::read_lock(config)?.unwrap_or_default().ids,
        ..Lock::default()
    };
    for (_, model) in &followed {
        lock.sources.extend(pins(&model.clone().with_lock(&lock))?);
        lock.ids = lock.ids.merged(&model.ids())?;
    }
    crate::places::keep_pin(&mut lock.sources, crate::read_lock(config)?.as_ref());
    lock.sources
        .extend(wrath.map(|d| (WRATH_SOURCE.to_owned(), d)));
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
        [] => println!(
            "no sources to pin: wrath and grapes come from checkouts here; the lock keeps user \
             and group ids"
        ),
        _ => println!("pinned {}", sources.join(", ")),
    }
    for (role, model) in models {
        let plan = wrath_model::plan(model, &model.clone().with_lock(&lock));
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
        .chain([WRATH_SOURCE, crate::places::GRAPES_SOURCE])
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
    if let Some(url) = crate::config_from::fetched_url() {
        return Err(crate::config_from::unpinned(url));
    }
    if crate::locked() {
        return Err(format!(
            "{} would change, and `--locked` writes no lock; the same command without it does",
            path.display()
        ));
    }
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
    if !model.sources.contains_key(WRATH_SOURCE) {
        let running = [
            WRATH_SOURCE.to_owned(),
            "the running wrath's".to_owned(),
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
    fn what_a_system_takes_says_what_comes_from_each_cache_and_what_is_built() {
        use wrath_build::graph::Takes;
        let name_of = |url: &str| {
            if url == "http://h" {
                "home".to_owned()
            } else {
                url.to_owned()
            }
        };
        let line = |takes: &Takes| takes_line(takes, name_of);
        assert_eq!(line(&Takes::default()), None);
        let mut takes = Takes {
            fetch: BTreeMap::from([
                ("http://h".to_owned(), (12, 40_400_000)),
                ("/srv/c".to_owned(), (1, 0)),
            ]),
            ..Takes::default()
        };
        assert_eq!(
            line(&takes).unwrap(),
            "to get there: fetch 1 path from /srv/c and 12 from home (40 MB)"
        );
        use wrath_build::pilot::Why;
        takes.build = (1..=7).map(|n| (format!("p{n}"), Why::Nowhere)).collect();
        takes
            .build
            .insert("app".into(), Why::BuiltWith("p1".into()));
        let built = line(&takes).unwrap();
        assert!(
            built.ends_with(", build 8 (p1, p2, p3, p4, p5 and 2 more; 1 built with them)"),
            "{built}"
        );
        takes.build = BTreeMap::from([
            ("glibc".into(), Why::Nowhere),
            ("zlib".into(), Why::BuiltWith("glibc".into())),
        ]);
        assert!(
            line(&takes)
                .unwrap()
                .ends_with(", build 2 (glibc; 1 built with it)")
        );
        takes.seed = Seed::Prepare;
        let prepared = line(&takes).unwrap();
        assert!(
            prepared.ends_with("with it), preparing the seed first"),
            "{prepared}"
        );
        takes.seed = Seed::Missing;
        let missing = line(&takes).unwrap();
        assert!(
            missing.ends_with("with it), which needs a seed this machine doesn't have"),
            "{missing}"
        );
        assert_eq!(size(999), "999 B");
        assert_eq!(size(1_250_000), "1.2 MB");
        assert_eq!(size(3_400_000_000), "3.4 GB");
    }

    #[test]
    fn every_image_a_build_left_goes_and_nothing_else() {
        let work = tempfile::tempdir().unwrap();
        assert_eq!(forget_images(work.path()), Ok((0, 0)));
        let images = work.path().join("images");
        std::fs::create_dir_all(&images).unwrap();
        std::fs::write(images.join("aaa-system.img"), "one").unwrap();
        std::fs::write(images.join("bbb-system.img"), "two").unwrap();
        std::fs::write(images.join("notes.txt"), "not an image").unwrap();
        assert_eq!(forget_images(work.path()).map(|(count, _)| count), Ok(2));
        let mut left: Vec<String> = std::fs::read_dir(&images)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, ["notes.txt"]);
    }

    #[test]
    fn building_without_recipes_says_where_to_build_from() {
        let error = build_generation(&Model::default(), false).unwrap_err();
        assert!(error.contains("wrath apply --host"), "{error}");
    }

    #[test]
    fn a_machine_has_a_model_only_when_it_runs_and_boots_it_and_isnt_trying_another() {
        assert!(already_there("m", Some("m"), Some("m"), false));
        // A machine that was never installed by wrath has no generations to compare.
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
                        PathBuf::from(format!("/wrath/store/x-{name}")),
                    )
                })
                .collect()
        };
        let at = dir.path().join("jq-link");
        assert_eq!(
            link_places(&built(&["jq"]), &at).unwrap(),
            [(at.clone(), PathBuf::from("/wrath/store/x-jq"))]
        );
        assert_eq!(
            link_places(&built(&["jq", "ripgrep"]), dir.path()).unwrap(),
            [
                (dir.path().join("jq"), PathBuf::from("/wrath/store/x-jq")),
                (
                    dir.path().join("ripgrep"),
                    PathBuf::from("/wrath/store/x-ripgrep")
                ),
            ]
        );
        assert!(link_places(&built(&["jq", "ripgrep"]), &at).is_err());

        show_built(&built(&["jq"]), Some(&at)).unwrap();
        show_built(&built(&["ripgrep"]), Some(&at)).unwrap();
        assert_eq!(
            std::fs::read_link(&at).unwrap(),
            PathBuf::from("/wrath/store/x-ripgrep")
        );
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        assert!(show_built(&built(&["jq"]), Some(&file)).is_err());

        // An image moves, over a link or an image put there before, leaving nothing behind.
        let image = dir.path().join("abc-system.img");
        let stick = dir.path().join("stick.img");
        std::os::unix::fs::symlink(&image, &stick).unwrap();
        let built = [("stick.img".to_owned(), image.clone())];
        for disk in ["one", "two"] {
            std::fs::write(&image, disk).unwrap();
            show_built(&built, Some(&stick)).unwrap();
            assert!(!image.exists());
            assert_eq!(std::fs::read_to_string(&stick).unwrap(), disk);
        }
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
            model["enabled"]["wrath-agent.service"],
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
                "run-wrath\\x2dshare.mount",
                "wrath-share",
                "/run/wrath-share",
            ),
            (
                "run-wrath\\x2dstore.mount",
                "wrath-store",
                "/run/wrath-store",
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
        assert!(matching(None, &model).contains("doesn't run a system wrath built"));
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
        wrath   the running wrath's
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
    fn the_running_generation_is_the_current_one_when_it_runs_and_else_the_newest_running() {
        let generation = |number, current, running| Generation {
            number,
            created: String::new(),
            current,
            in_use: running,
            running,
            model_hash: None,
            from: None,
        };
        let number = |generations: &[Generation]| running_of(generations).map(|g| g.number);
        // 2 and 4 are the same system; 4 is current and runs.
        let switched = [
            generation(2, false, true),
            generation(3, false, false),
            generation(4, true, true),
        ];
        assert_eq!(number(&switched), Some(4));
        // After `apply --boot`, 5 boots next while 4 still runs.
        let boot_next = [
            generation(2, false, true),
            generation(4, false, true),
            generation(5, true, false),
        ];
        assert_eq!(number(&boot_next), Some(4));
        assert_eq!(number(&[generation(1, true, false)]), None);
    }

    #[test]
    fn clean_keeps_the_current_running_and_booted_generations_and_the_newest_before_them() {
        let generation = |number, current, in_use| Generation {
            number,
            created: String::new(),
            current,
            in_use,
            running: false,
            model_hash: None,
            from: None,
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
            "there's no source `stabel`; the configuration has acme, edge, grapes, stable, wrath"
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
