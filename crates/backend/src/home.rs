//! A user's home on its own: generations in a profile under their home, switched to without root.

use std::os::unix::fs::MetadataExt;
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
    set(profile, built)?;
    link(profile)
}

fn set(profile: &Path, built: &Path) -> Result<(), String> {
    command(
        Command::new("nix-env")
            .arg("--profile")
            .arg(profile)
            .arg("--set")
            .arg(built),
    )
    .map(drop)
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

/// The user timer that switches back from a home being tried.
const TRY_UNIT: &str = "distro-home-try";

/// Where the home being tried is kept from garbage collection, beside `profile`, since no
/// generation has it yet.
fn try_root(profile: &Path) -> PathBuf {
    profile.with_file_name("home-try")
}

/// `systemctl --user` or `systemd-run --user`, which find the user's manager even outside a
/// login session.
fn user_command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.arg("--user");
    // Outside a login session, such as under runuser, the manager still listens in the runtime
    // directory named after the user's uid, which owns this process.
    if std::env::var_os("XDG_RUNTIME_DIR").is_none()
        && let Ok(meta) = std::fs::metadata("/proc/self")
    {
        command.env("XDG_RUNTIME_DIR", format!("/run/user/{}", meta.uid()));
    }
    command
}

/// Switches the home to `built` without making it a generation: a timer switches back to the
/// current generation after `seconds`, unless [`keep_trial`] makes it one first.
pub fn try_home(profile: &Path, built: &Path, seconds: u64) -> Result<(), String> {
    if trying() {
        return Err("a home is already being tried; keep it or roll back first".into());
    }
    let root = try_root(profile);
    command(
        Command::new("nix-store")
            .arg("--add-root")
            .arg(&root)
            .arg("--realise")
            .arg(built),
    )?;
    if let Err(message) = command(&mut Command::new(built.join("activate"))) {
        let _ = std::fs::remove_file(&root);
        let _ = link(profile);
        return Err(message);
    }
    command(
        user_command("systemd-run")
            .arg(format!("--unit={TRY_UNIT}"))
            .arg(format!("--on-active={seconds}s"))
            .args([
                "--timer-property=AccuracySec=1s",
                "--description=distro: end a home's try",
            ])
            .arg(profile.join("activate")),
    )
    .map(drop)
}

/// Whether a home is being tried, with a timer waiting to switch back from it.
pub fn trying() -> bool {
    user_command("systemctl")
        .args(["is-active", "--quiet"])
        .arg(format!("{TRY_UNIT}.timer"))
        .status()
        .is_ok_and(|status| status.success())
}

fn stop_trial() {
    let _ = command(
        user_command("systemctl")
            .arg("stop")
            .arg(format!("{TRY_UNIT}.timer")),
    );
}

/// Makes the home being tried the current generation of `profile`.
pub fn keep_trial(profile: &Path) -> Result<(), String> {
    if !trying() {
        return Err("no home is being tried".into());
    }
    stop_trial();
    let root = try_root(profile);
    let tried = std::fs::read_link(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    set(profile, &tried)?;
    let _ = std::fs::remove_file(&root);
    Ok(())
}

/// Switches back from the home being tried to the current generation of `profile`.
pub fn end_trial(profile: &Path) -> Result<(), String> {
    stop_trial();
    link(profile)?;
    let _ = std::fs::remove_file(try_root(profile));
    Ok(())
}
