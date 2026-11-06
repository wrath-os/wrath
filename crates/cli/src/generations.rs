//! Building systems and moving between generations through the NixOS backend.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use distro_backend_nixos::{self as backend, Target};
use distro_model::Model;

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
pub fn apply(root: &Path, model: &Model) -> Result<ExitCode, String> {
    let hash = model.hash();
    if backend::current_model_hash().as_deref() == Some(hash.as_str()) {
        println!("the running system already has model {}", &hash[..16]);
        return Ok(ExitCode::SUCCESS);
    }
    let system = build(root, model, BuildTarget::System)?;
    activate(&system)
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
