//! A user's home on its own: generations in a profile under their home, switched to without root.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Generation, command, parse_generations};

/// The profile whose generations are the user's home, under `home`.
pub fn profile(home: &Path) -> PathBuf {
    home.join(".local/state/distro/profiles/home")
}

/// Makes `built`, a home already built, the current generation of `profile` and links its files
/// into the home.
pub fn activate(profile: &Path, built: &Path) -> Result<(), String> {
    if let Some(dir) = profile.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    command(
        Command::new("nix-env")
            .arg("--profile")
            .arg(profile)
            .arg("--set")
            .arg(built),
    )?;
    link(profile)
}

fn link(profile: &Path) -> Result<(), String> {
    command(&mut Command::new(profile.join("activate"))).map(drop)
}

/// Switches back to the previous generation of `profile` and returns it.
pub fn rollback(profile: &Path) -> Result<Generation, String> {
    command(
        Command::new("nix-env")
            .arg("--profile")
            .arg(profile)
            .arg("--rollback"),
    )?;
    link(profile)?;
    history(profile)?
        .into_iter()
        .find(|generation| generation.current)
        .ok_or_else(|| "there's no current generation".to_owned())
}

/// Every generation of `profile`, oldest first. None before the first `apply`.
pub fn history(profile: &Path) -> Result<Vec<Generation>, String> {
    if !profile.exists() {
        return Ok(Vec::new());
    }
    let listing = command(
        Command::new("nix-env")
            .arg("--profile")
            .arg(profile)
            .arg("--list-generations"),
    )?;
    let mut generations = parse_generations(&listing)?;
    for generation in &mut generations {
        let hash = generation_dir(profile, generation.number).join("distro/model-hash");
        generation.model_hash = std::fs::read_to_string(hash).ok();
    }
    Ok(generations)
}

fn generation_dir(profile: &Path, number: u32) -> PathBuf {
    let name = profile.file_name().unwrap_or_default().to_string_lossy();
    profile.with_file_name(format!("{name}-{number}-link"))
}

/// The model the current home was built from, after its first `apply`.
pub fn current_model(profile: &Path) -> Option<PathBuf> {
    Some(profile.join("distro/model.json")).filter(|path| path.exists())
}

/// The hash of the model the current home was built from.
pub fn current_model_hash(profile: &Path) -> Option<String> {
    std::fs::read_to_string(profile.join("distro/model-hash")).ok()
}

/// Where generation `number` of `profile` keeps the model it was built from.
pub fn generation_model(profile: &Path, number: u32) -> PathBuf {
    generation_dir(profile, number).join("distro/model.json")
}
