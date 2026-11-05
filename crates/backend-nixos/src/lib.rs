//! The NixOS backend (Phase A): the model is lowered to JSON, a fixed shim in `factory/nix` maps
//! it onto NixOS options, and NixOS builds and switches the system. Generations are NixOS's own
//! system profile generations.

mod generations;
mod lower;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use distro_model::Model;

pub use generations::Generation;
use generations::parse_generations;
pub use lower::lower;

/// The system profile whose generations `history` and `rollback` work with.
pub const PROFILE: &str = "/nix/var/nix/profiles/system";

/// Where the running system records the hash of the model it was built from.
pub const CURRENT_MODEL_HASH: &str = "/run/current-system/etc/distro/model-hash";

/// What to build from a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The system, to switch to.
    System,
    /// The system with the test harness's agent and share, for VM scenarios.
    TestSystem,
    /// A bootable disk image of the test system, as a directory containing `disk.qcow2`.
    TestImage,
    /// The test system for VMs that mount the host's store.
    TestVmSystem,
    /// A VM that boots the test system from the host's store, as a directory containing
    /// `kernel`, `initrd` and `kernel-params`.
    TestVm,
}

impl Target {
    fn function(self) -> &'static str {
        match self {
            Self::System => "toplevel",
            Self::TestSystem => "testToplevel",
            Self::TestImage => "testImage",
            Self::TestVmSystem => "testVmToplevel",
            Self::TestVm => "testVm",
        }
    }
}

/// Builds `target` from `model` with the flake in `factory` (normally `factory/nix`), writing
/// the shim's input to `work`. Returns the built store path.
pub fn build(
    model: &Model,
    factory: &Path,
    target: Target,
    work: &Path,
) -> Result<PathBuf, String> {
    let input = work.join("nixos.json");
    std::fs::write(&input, lower(model)).map_err(|e| format!("{}: {e}", input.display()))?;
    let canonical = |path: &Path| {
        path.canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    let factory = canonical(factory)?;
    let input = canonical(&input)?;
    let output = command(
        Command::new("nix")
            .args([
                "build",
                "--impure",
                "--no-link",
                "--print-out-paths",
                "--expr",
            ])
            .arg(build_expression(&factory, target, &input)),
    )?;
    Ok(PathBuf::from(output.trim()))
}

/// The Nix expression that builds `target`. The flake is read through git, so files jj tracks
/// are seen and build outputs in the repository are not.
fn build_expression(factory: &Path, target: Target, input: &Path) -> String {
    let (repo, dir) = git_root(factory);
    format!(
        "(builtins.getFlake {flake:?}).lib.${{builtins.currentSystem}}.{function} (/. + {input:?})",
        flake = format!("git+file://{}?dir={}", repo.display(), dir.display()),
        function = target.function(),
        input = input.display().to_string(),
    )
}

/// The enclosing repository of `dir`, and `dir` relative to it.
fn git_root(dir: &Path) -> (PathBuf, PathBuf) {
    let repo = dir
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .unwrap_or(dir);
    let relative = dir.strip_prefix(repo).unwrap_or(Path::new(""));
    (repo.to_path_buf(), relative.to_path_buf())
}

/// Makes `system`, an already built and present system, the current generation and switches to
/// it. Needs root.
pub fn activate(system: &Path) -> Result<(), String> {
    command(
        Command::new("nix-env")
            .args(["--profile", PROFILE, "--set"])
            .arg(system),
    )?;
    switch(system)
}

/// Switches back to the previous generation and returns it.
pub fn rollback() -> Result<Generation, String> {
    command(Command::new("nix-env").args(["--profile", PROFILE, "--rollback"]))?;
    switch(Path::new(PROFILE))?;
    history()?
        .into_iter()
        .find(|generation| generation.current)
        .ok_or_else(|| "no current generation after rolling back".to_owned())
}

/// Every generation of the system profile, oldest first.
pub fn history() -> Result<Vec<Generation>, String> {
    let listing =
        command(Command::new("nix-env").args(["--profile", PROFILE, "--list-generations"]))?;
    let mut generations = parse_generations(&listing)?;
    for generation in &mut generations {
        let hash = Path::new(&format!("{PROFILE}-{}-link", generation.number))
            .join("etc/distro/model-hash");
        generation.model_hash = std::fs::read_to_string(hash).ok();
    }
    Ok(generations)
}

/// The hash of the model the running system was built from, if it was built by `distro`.
pub fn current_model_hash() -> Option<String> {
    std::fs::read_to_string(CURRENT_MODEL_HASH).ok()
}

fn switch(system: &Path) -> Result<(), String> {
    command(Command::new(system.join("bin/switch-to-configuration")).arg("switch")).map(drop)
}

/// Runs a command to completion, returning its stdout, or its last lines of stderr on failure.
fn command(command: &mut Command) -> Result<String, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    let tail = lines[lines.len().saturating_sub(10)..].join("\n");
    Err(format!("{program} failed ({}):\n{tail}", output.status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_expression_reads_the_flake_through_git() {
        let expression = build_expression(
            Path::new("/nonexistent/factory/nix"),
            Target::TestImage,
            Path::new("/tmp/run/nixos.json"),
        );

        assert_eq!(
            expression,
            r#"(builtins.getFlake "git+file:///nonexistent/factory/nix?dir=").lib.${builtins.currentSystem}.testImage (/. + "/tmp/run/nixos.json")"#
        );
    }

    #[test]
    fn the_repository_is_the_nearest_directory_with_git() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap();

        let (root, dir) = git_root(&repo.join("factory/nix"));

        assert_eq!(root, repo);
        assert_eq!(dir, Path::new("factory/nix"));
    }
}
