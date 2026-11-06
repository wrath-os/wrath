//! The NixOS backend (Phase A): the model is lowered to JSON, a fixed shim in `factory/nix` maps
//! it onto NixOS options, and NixOS builds and switches the system. Generations are NixOS's own
//! system profile generations.

mod generations;
mod lower;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use distro_model::{Lock, Locked, Model, Source};
use serde::Deserialize;

pub use generations::Generation;
use generations::parse_generations;
pub use lower::lower;

/// The system profile whose generations `history` and `rollback` work with.
pub const PROFILE: &str = "/nix/var/nix/profiles/system";

/// Where the running system records the hash of the model it was built from.
pub const CURRENT_MODEL_HASH: &str = "/run/current-system/etc/distro/model-hash";

/// Where the running system keeps the model it was built from.
pub const CURRENT_MODEL: &str = "/run/current-system/etc/distro/model.json";

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
    /// The model's packages in one environment, for running them without installing them: their
    /// programs under `bin`, and `share/distro/packages.json` with their versions and sources.
    Environment,
}

impl Target {
    fn function(self) -> &'static str {
        match self {
            Self::System => "toplevel",
            Self::TestSystem => "testToplevel",
            Self::TestImage => "testImage",
            Self::TestVmSystem => "testVmToplevel",
            Self::TestVm => "testVm",
            Self::Environment => "environment",
        }
    }
}

/// A built store path and the warnings evaluating it printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub path: PathBuf,
    pub warnings: Vec<String>,
}

/// Builds `target` from `model` with the flake in `factory` (normally `factory/nix`), writing
/// the shim's input to `work`. Packages come from the model's lock when it has one.
pub fn build(model: &Model, factory: &Path, target: Target, work: &Path) -> Result<Built, String> {
    let (stdout, stderr) = nix(
        &["build", "--no-link", "--print-out-paths"],
        model,
        factory,
        target.function(),
        work,
    )?;
    Ok(Built {
        path: PathBuf::from(stdout.trim()),
        warnings: warnings(&stderr),
    })
}

/// Resolves `model`'s packages to a lock: from each source as its lock pins it, or the latest
/// of a source the lock doesn't pin. Without a `nixpkgs` source, the factory's own. A system that
/// extends a NixOS configuration takes packages from that configuration, so it is the source.
pub fn resolve(model: &Model, factory: &Path, work: &Path) -> Result<Lock, String> {
    let (stdout, _) = nix(&["eval", "--json"], model, factory, "resolve", work)?;
    parse_resolved(&stdout)
}

#[derive(Deserialize)]
struct Resolved {
    sources: BTreeMap<String, Source>,
    packages: BTreeMap<String, Locked>,
}

fn parse_resolved(json: &str) -> Result<Lock, String> {
    let resolved: Resolved = serde_json::from_str(json)
        .map_err(|e| format!("cannot read the packages Nix resolved: {e}"))?;
    Ok(Lock {
        sources: resolved.sources,
        packages: resolved.packages,
    })
}

/// Runs `nix` with `args` on the factory's `function` applied to `model`'s lowered input.
fn nix(
    args: &[&str],
    model: &Model,
    factory: &Path,
    function: &str,
    work: &Path,
) -> Result<(String, String), String> {
    let input = work.join("nixos.json");
    std::fs::write(&input, lower(model)).map_err(|e| format!("{}: {e}", input.display()))?;
    let canonical = |path: &Path| {
        path.canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    let (factory, input) = (canonical(factory)?, canonical(&input)?);
    run(Command::new("nix")
        .args(args)
        .args(["--impure", "--expr"])
        .arg(expression(&factory, function, &input)))
}

/// The warnings in what Nix printed, such as those NixOS modules raise. Nix indents the further
/// lines of a warning to line up with its first.
fn warnings(stderr: &str) -> Vec<String> {
    const PREFIXES: [&str; 2] = ["evaluation warning: ", "trace: warning: "];
    let mut warnings: Vec<String> = Vec::new();
    let mut indent = None;
    for line in stderr.lines() {
        if let Some((prefix, message)) = PREFIXES
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix).map(|message| (prefix, message)))
        {
            warnings.push(message.to_owned());
            indent = Some(" ".repeat(prefix.len()));
        } else if let (Some(last), Some(rest)) = (
            warnings.last_mut(),
            indent
                .as_deref()
                .and_then(|indent| line.strip_prefix(indent)),
        ) {
            last.push('\n');
            last.push_str(rest);
        } else {
            indent = None;
        }
    }
    warnings
}

/// The Nix expression applying the factory's `function` to `input`. The flake is read through git,
/// so files jj tracks are seen and build outputs in the repository are not.
fn expression(factory: &Path, function: &str, input: &Path) -> String {
    let (repo, dir) = git_root(factory);
    format!(
        "(builtins.getFlake {flake:?}).lib.${{builtins.currentSystem}}.{function} (/. + {input:?})",
        flake = format!("git+file://{}?dir={}", repo.display(), dir.display()),
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

/// The running system's model file, when distro built the running system.
pub fn current_model() -> Option<&'static str> {
    Path::new(CURRENT_MODEL).exists().then_some(CURRENT_MODEL)
}

fn switch(system: &Path) -> Result<(), String> {
    command(Command::new(system.join("bin/switch-to-configuration")).arg("switch")).map(drop)
}

/// Runs a command to completion, returning its stdout, or its last lines of stderr on failure.
fn command(command: &mut Command) -> Result<String, String> {
    run(command).map(|(stdout, _)| stdout)
}

/// Like [`command`], returning stderr as well on success.
fn run(command: &mut Command) -> Result<(String, String), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if output.status.success() {
        return Ok((String::from_utf8_lossy(&output.stdout).into_owned(), stderr));
    }
    if let Some(message) = factory_message(&stderr) {
        return Err(message);
    }
    let lines: Vec<&str> = stderr.lines().collect();
    let tail = lines[lines.len().saturating_sub(10)..].join("\n");
    Err(format!("{program} failed ({}):\n{tail}", output.status))
}

/// A message the factory meant for the user, thrown as `distro: ...`, in what Nix printed.
fn factory_message(stderr: &str) -> Option<String> {
    const MARK: &str = "distro: ";
    let line = stderr.lines().rev().find(|line| line.contains(MARK))?;
    let message = &line[line.find(MARK)? + MARK.len()..];
    Some(message.trim_end_matches('\'').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_the_factory_throws_are_shown_as_they_are() {
        let stderr = "error:\n       … while evaluating\n\n       error: distro: there's no package `jqq` in nixpkgs\n";
        assert_eq!(
            factory_message(stderr).as_deref(),
            Some("there's no package `jqq` in nixpkgs")
        );
        let aborted = "error: evaluation aborted with the following error message: 'distro: x'";
        assert_eq!(factory_message(aborted).as_deref(), Some("x"));
        assert_eq!(factory_message("error: attribute 'x' missing"), None);
    }
    #[test]
    fn warnings_are_picked_from_what_nix_prints() {
        let stderr = [
            "copying path '/nix/store/abc-x' from 'https://cache.nixos.org'...",
            "evaluation warning: the TOML sets time zone Europe/Stockholm, NixOS uses UTC",
            "evaluation warning: The option `a` is obsolete. Use",
            "                    `b` instead.",
            "trace: warning: the NixOS firewall runs as well",
            "building '/nix/store/def-y.drv'...",
        ]
        .join("\n");

        assert_eq!(
            warnings(&stderr),
            [
                "the TOML sets time zone Europe/Stockholm, NixOS uses UTC",
                "The option `a` is obsolete. Use\n`b` instead.",
                "the NixOS firewall runs as well"
            ]
        );
    }

    #[test]
    fn the_expression_reads_the_flake_through_git() {
        let expression = expression(
            Path::new("/nonexistent/factory/nix"),
            Target::TestImage.function(),
            Path::new("/tmp/run/nixos.json"),
        );

        assert_eq!(
            expression,
            r#"(builtins.getFlake "git+file:///nonexistent/factory/nix?dir=").lib.${builtins.currentSystem}.testImage (/. + "/tmp/run/nixos.json")"#
        );
    }

    #[test]
    fn resolved_packages_become_a_lock() {
        let json = r#"{
          "sources": {
            "nixpkgs": { "url": "github:NixOS/nixpkgs/nixos-unstable", "rev": "1d06008", "hash": "sha256-abc" },
            "etc": { "url": "git+file:///etc/nixos", "rev": null, "hash": "sha256-x" }
          },
          "packages": { "git": { "version": "2.56.0", "id": "/nix/store/xgw-git-2.56.0" } }
        }"#;

        let lock = parse_resolved(json).unwrap();

        assert_eq!(lock.sources["nixpkgs"].rev.as_deref(), Some("1d06008"));
        assert_eq!(lock.sources["etc"].rev, None);
        assert_eq!(lock.packages["git"].version, "2.56.0");
        assert!(parse_resolved("{}").is_err());
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
