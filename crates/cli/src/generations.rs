//! Building systems and moving between generations through the NixOS backend.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use distro_backend_nixos::{self as backend, Target};
use distro_model::{LOCK_FILE, Lock, Model, Source, Tier};

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
pub fn build(root: &Path, model: &Model, target: BuildTarget) -> Result<PathBuf, String> {
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
    let model = if is_locked(&model) {
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
/// packages changed. With `update`, the lock's package set moves to its newest version first.
pub fn lock(root: &Path, config: &Path, model: &Model, update: bool) -> Result<ExitCode, String> {
    let mut wanted = model.clone();
    if update {
        // A source without a hash is followed to its newest version.
        let source = wanted.source.get_or_insert_with(|| Source {
            url: DEFAULT_SOURCE.into(),
            ..Source::default()
        });
        source.rev = None;
        source.hash.clear();
    }
    let lock = resolve(root, &wanted)?;
    write_lock(config, &lock)?;
    let plan = distro_model::plan(model, &model.clone().with_lock(&lock));
    let count = lock.packages.len();
    let noun = if count == 1 { "package" } else { "packages" };
    println!("locked {count} {noun} from {}", lock.source.url);
    if !plan.is_empty() {
        print!("{plan}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The package set a configuration follows when it has no lock yet. The factory pins it.
const DEFAULT_SOURCE: &str = "github:NixOS/nixpkgs/nixos-unstable";

/// Whether the model has a lock with every package it uses.
fn is_locked(model: &Model) -> bool {
    model.source.is_some()
        && model
            .all_packages()
            .all(|name| model.locked.contains_key(name))
}

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
    use distro_model::Locked;

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
    fn a_model_is_locked_when_its_lock_has_every_package() {
        let mut model = Model::default();
        model.packages.insert("nginx".into());
        model
            .homes
            .entry("john".into())
            .or_default()
            .packages
            .insert("git".into());
        let lock = |names: &[&str]| {
            let packages = names
                .iter()
                .map(|name| ((*name).to_owned(), Locked::default()))
                .collect();
            Lock::new(Source::default(), packages)
        };

        assert!(!is_locked(&model));
        assert!(!is_locked(&model.clone().with_lock(&lock(&["nginx"]))));
        assert!(is_locked(
            &model.clone().with_lock(&lock(&["nginx", "git"]))
        ));
    }
}
