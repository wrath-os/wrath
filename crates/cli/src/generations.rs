//! Building systems and moving between generations through the NixOS backend.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use distro_backend_nixos::{self as backend, Target};
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
}

impl From<BuildTarget> for Target {
    fn from(target: BuildTarget) -> Self {
        match target {
            BuildTarget::System => Self::System,
            BuildTarget::TestSystem => Self::TestSystem,
            BuildTarget::TestImage => Self::TestImage,
            BuildTarget::TestVmSystem => Self::TestVmSystem,
            BuildTarget::TestVm => Self::TestVm,
        }
    }
}

/// Builds `target` from `model` with the factory under `root` and returns its store path. The
/// backend's warnings go to stderr.
pub fn build(root: &Path, model: &Model, target: impl Into<Target>) -> Result<PathBuf, String> {
    let work = tempfile::tempdir().map_err(|e| format!("cannot create a work directory: {e}"))?;
    let built = backend::build(model, &root.join("factory/nix"), target.into(), work.path())?;
    for warning in &built.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(built.path)
}

/// Builds the system for `model` and switches to it, unless the running system already has it.
/// Packages the lock next to `config` doesn't have yet are locked first, so the lock always
/// describes the running system.
pub fn apply(root: &Path, config: &Path, model: Model) -> Result<ExitCode, String> {
    let model = if model.is_locked() {
        model
    } else {
        let lock = resolve(root, &model)?;
        write_lock(config, &lock)?;
        model.with_lock(&lock)
    };
    let hash = model.hash();
    if backend::current_model_hash().as_deref() == Some(hash.as_str()) {
        println!("the running system already has model {}", &hash[..16]);
        return Ok(ExitCode::SUCCESS);
    }
    // What the running system is, to say afterwards whether the switch did everything.
    let running = backend::current_model().and_then(|path| crate::read_model(Path::new(path)).ok());
    let system = build(root, &model, BuildTarget::System)?;
    let code = activate(&system)?;
    if let Some(note) = running
        .and_then(|running| distro_model::plan(&running, &model).tier())
        .and_then(full_effect)
    {
        println!("{note}");
    }
    Ok(code)
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

/// Says which generation is running and whether it's the configuration's `model`.
pub fn status(model: &Model) -> Result<ExitCode, String> {
    let current = backend::history()
        .ok()
        .and_then(|generations| generations.into_iter().find(|g| g.current));
    if let Some(generation) = &current {
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

/// Whether the running system, whose model hash is `running`, is `model`.
fn matching(running: Option<&str>, model: &Model) -> &'static str {
    match running {
        Some(hash) if hash == model.hash() => "it matches the configuration",
        Some(_) => "the configuration differs from it; `distro plan` shows how",
        None => "distro didn't build it; `distro plan` shows the whole configuration as new",
    }
}

pub fn activate(system: &Path) -> Result<ExitCode, String> {
    backend::activate(system)?;
    println!("switched to {}", system.display());
    Ok(ExitCode::SUCCESS)
}

pub fn history() -> Result<ExitCode, String> {
    for generation in backend::history()? {
        println!("{generation}");
    }
    Ok(ExitCode::SUCCESS)
}

pub fn rollback() -> Result<ExitCode, String> {
    let generation = backend::rollback()?;
    println!("rolled back to generation {}", generation.number);
    Ok(ExitCode::SUCCESS)
}

/// Resolves `model`'s packages and writes the lock next to `config`, then prints how the
/// packages changed. With `update`, those sources move to their newest version first, or every
/// source when it names none.
pub fn lock(
    root: &Path,
    config: &Path,
    model: &Model,
    update: Option<&[String]>,
) -> Result<ExitCode, String> {
    let wanted = match update {
        Some(names) => follow_newest(model.clone(), names)?,
        None => model.clone(),
    };
    let lock = resolve(root, &wanted)?;
    write_lock(config, &lock)?;
    let plan = distro_model::plan(model, &model.clone().with_lock(&lock));
    let count = lock.packages.len();
    let noun = if count == 1 { "package" } else { "packages" };
    let sources: Vec<&str> = lock.sources.keys().map(String::as_str).collect();
    println!("locked {count} {noun} from {}", sources.join(", "));
    if !plan.is_empty() {
        print!("{plan}");
    }
    Ok(ExitCode::SUCCESS)
}

/// `model` with the sources `names` unpinned, or all of them when it names none, so resolving
/// takes their newest versions.
fn follow_newest(mut model: Model, names: &[String]) -> Result<Model, String> {
    model
        .sources
        .entry(DEFAULT_SOURCE.to_owned())
        .or_insert_with(|| Source::follow(DEFAULT_URL));
    if let Some(unknown) = names.iter().find(|name| !model.sources.contains_key(*name)) {
        let known: Vec<&str> = model.sources.keys().map(String::as_str).collect();
        return Err(format!(
            "there's no source `{unknown}`; the configuration has {}",
            known.join(", ")
        ));
    }
    for (name, source) in &mut model.sources {
        if names.is_empty() || names.contains(name) {
            *source = Source::follow(&source.url);
        }
    }
    Ok(model)
}

/// What the default source follows when nothing says otherwise. The factory pins it.
const DEFAULT_URL: &str = "github:NixOS/nixpkgs/nixos-unstable";

fn resolve(root: &Path, model: &Model) -> Result<Lock, String> {
    let work = tempfile::tempdir().map_err(|e| format!("cannot create a work directory: {e}"))?;
    backend::resolve(model, &root.join("factory/nix"), work.path())
}

fn write_lock(config: &Path, lock: &Lock) -> Result<(), String> {
    let path = config.join(LOCK_FILE);
    std::fs::write(&path, lock.to_toml()).map_err(|e| format!("{}: {e}", path.display()))
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
    fn update_unpins_the_sources_it_names_or_all_of_them() {
        let pinned = |url: &str| Source {
            url: url.into(),
            rev: Some("1d06008".into()),
            hash: "sha256-abc".into(),
        };
        let mut model = Model::default();
        model.sources.insert("nixpkgs".into(), pinned("github:a/b"));
        model.sources.insert("stable".into(), pinned("github:c/d"));

        let one = follow_newest(model.clone(), &["stable".into()]).unwrap();
        assert!(one.sources["nixpkgs"].is_locked());
        assert_eq!(one.sources["stable"], Source::follow("github:c/d"));

        let all = follow_newest(model.clone(), &[]).unwrap();
        assert!(!all.sources.values().any(Source::is_locked));

        let unknown = follow_newest(model, &["stabel".into()]).unwrap_err();
        assert_eq!(
            unknown,
            "there's no source `stabel`; the configuration has nixpkgs, stable"
        );

        let fresh = follow_newest(Model::default(), &[]).unwrap();
        assert_eq!(fresh.sources["nixpkgs"], Source::follow(DEFAULT_URL));
    }
}
