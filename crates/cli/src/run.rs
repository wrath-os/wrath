//! `distro run` and `distro shell`: packages without installing them. A package comes from where
//! the configuration would get it, so `jq` is the jq its lock pins, and nothing is written.

use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use distro_backend_nixos::Target;
use distro_model::{DEFAULT_SOURCE, Model, Source, package_source};
use serde_json::Value;

use crate::{generations, project};

/// Runs `package`'s program with `args`, in place of this process.
pub fn run(root: &Path, model: Model, package: &str, args: &[String]) -> Result<ExitCode, String> {
    let environment = Environment::build(root, model, &[package.to_owned()])?;
    let (name, picked) = &environment.packages[0];
    let program = environment.path.join("bin").join(&picked.program);
    if !program.exists() {
        return Err(format!(
            "`{name}` has no program `{}`; it has: {}",
            picked.program,
            environment.programs().join(", ")
        ));
    }
    Err(exec(Command::new(program).args(args)))
}

/// Starts the user's shell with `packages` first on the `PATH`, in place of this process. In the
/// project at `project`, the shell has the project's packages and variables too, and says what
/// they hide from the home.
pub fn shell(
    root: &Path,
    model: Model,
    packages: &[String],
    project: Option<&Path>,
) -> Result<ExitCode, String> {
    let lookup = |name: &str| std::env::var(name).ok();
    let (mut wanted, variables): (Vec<String>, _) = match project {
        Some(_) => (
            model.packages.iter().cloned().collect(),
            project::variables(&model, lookup),
        ),
        None => (Vec::new(), Vec::new()),
    };
    wanted.extend(
        packages
            .iter()
            .filter(|p| !wanted.contains(p))
            .cloned()
            .collect::<Vec<_>>(),
    );
    if wanted.is_empty() {
        return Err("name the packages the shell should have".into());
    }
    let environment = Environment::build(root, model, &wanted)?;
    let inherited = inherited_path();
    if let Some(dir) = project {
        keep(&environment.path, &dir.join(".distro/shell"));
        let shadowed = project::shadows(
            &environment.path.join("bin"),
            &environment.programs(),
            &variables,
            &inherited,
            lookup,
        );
        if !shadowed.is_empty() {
            eprintln!("  hides {} from your home", shadowed.join(", "));
        }
    }
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "bash".into());
    let path = path_with(&environment.path.join("bin"), &inherited)?;
    let names: Vec<&str> = environment
        .packages
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    eprintln!("a shell with {}; exit to leave", names.join(", "));
    Err(exec(
        Command::new(shell)
            .envs(variables)
            .env("PATH", path)
            .env("DISTRO_SHELL", names.join(" ")),
    ))
}

/// Runs the services of the project at `project` with process-compose, in place of this process,
/// with the project's packages and variables, until you stop them.
pub fn up(root: &Path, model: Model, project: &Path) -> Result<ExitCode, String> {
    let compose = project::compose(&model)
        .ok_or("the project has no services; add them under `[services]`")?;
    let state = project.join(".distro");
    std::fs::create_dir_all(&state).map_err(|e| format!("{}: {e}", state.display()))?;
    let file = state.join("process-compose.yaml");
    std::fs::write(&file, compose).map_err(|e| format!("{}: {e}", file.display()))?;
    let lookup = |name: &str| std::env::var(name).ok();
    let variables = project::variables(&model, lookup);
    let mut wanted: Vec<String> = model.packages.iter().cloned().collect();
    wanted.push(PROCESS_COMPOSE.to_owned());
    let environment = Environment::build(root, model, &wanted)?;
    keep(&environment.path, &state.join("up"));
    let bin = environment.path.join("bin");
    let path = path_with(&bin, &inherited_path())?;
    Err(exec(
        Command::new(bin.join(PROCESS_COMPOSE))
            .args(["up", "--config"])
            .arg(&file)
            .envs(variables)
            .env("PATH", path),
    ))
}

/// What runs a project's services.
const PROCESS_COMPOSE: &str = "process-compose";

/// The `PATH` this process inherited.
fn inherited_path() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .collect()
}

/// A `PATH` with `dir` first, then `inherited`.
fn path_with(dir: &Path, inherited: &[PathBuf]) -> Result<OsString, String> {
    std::env::join_paths(std::iter::once(dir).chain(inherited.iter().map(PathBuf::as_path)))
        .map_err(|e| format!("cannot set PATH: {e}"))
}

fn exec(command: &mut Command) -> String {
    let program = command.get_program().to_string_lossy().into_owned();
    format!("cannot run {program}: {}", command.exec())
}

/// The packages, built into one environment.
struct Environment {
    path: PathBuf,
    /// Each package by the name the model gives it, in the order asked for.
    packages: Vec<(String, Picked)>,
}

/// What a package turned out to be.
#[derive(Debug, PartialEq, Eq)]
struct Picked {
    version: String,
    /// The program `run` starts.
    program: String,
}

impl Environment {
    /// Builds `references` from the sources `model` has, and says on stderr what each one is.
    fn build(root: &Path, mut model: Model, references: &[String]) -> Result<Self, String> {
        let names = references
            .iter()
            .map(|reference| package_name(&mut model, reference))
            .collect::<Result<Vec<_>, _>>()?;
        model.packages = names.iter().cloned().collect();
        let path = generations::build(root, &model, Target::Environment)?;
        if let Some(link) = gc_root(&names) {
            keep(&path, &link);
        }
        let file = path.join("share/distro/packages.json");
        let described: Value = std::fs::read_to_string(&file)
            .map_err(|e| e.to_string())
            .and_then(|json| serde_json::from_str(&json).map_err(|e| e.to_string()))
            .map_err(|e| format!("{}: {e}", file.display()))?;
        let packages = names
            .into_iter()
            .map(|name| {
                let picked = picked(&described, &name)
                    .ok_or_else(|| format!("{}: nothing about `{name}`", file.display()))?;
                eprintln!("{}", describe(&model, &described, &name, &picked));
                Ok((name, picked))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { path, packages })
    }

    fn programs(&self) -> Vec<String> {
        std::fs::read_dir(self.path.join("bin"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn picked(described: &Value, name: &str) -> Option<Picked> {
    let package = &described["packages"][name];
    Some(Picked {
        version: package["version"].as_str()?.to_owned(),
        program: package["program"].as_str()?.to_owned(),
    })
}

/// What a package is and where it came from, as
/// `jq 1.8.1 from nixpkgs (1d06008, 2026-10-30, locked)`.
fn describe(model: &Model, described: &Value, name: &str, picked: &Picked) -> String {
    let (source, attribute) = package_source(name);
    let pinned = &described["sources"][source];
    let how = match model.sources.get(source) {
        Some(declared) if declared.is_locked() => "locked",
        Some(_) => "its newest version",
        None => "the factory's",
    };
    let details: Vec<&str> = [
        pinned["rev"].as_str().map(|rev| &rev[..rev.len().min(7)]),
        pinned["date"].as_str(),
        Some(how),
    ]
    .into_iter()
    .flatten()
    .collect();
    format!(
        "{attribute} {} from {source} ({})",
        picked.version,
        details.join(", ")
    )
}

/// Where the environment for `names` is kept from garbage collection, under the user's cache.
/// Each set of packages has one, so running a newer build lets the older one be collected.
fn gc_root(names: &[String]) -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))?;
    let mut sorted: Vec<&str> = names.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    Some(
        cache
            .join("distro/environments")
            .join(root_name(&sorted.join(" "))),
    )
}

/// A file name for a set of package names: readable when they're plain, and with a short hash so
/// different sets never share one.
fn root_name(names: &str) -> String {
    let readable: String = names
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    // FNV-1a, stable across Rust versions, unlike the standard hasher.
    let hash = names.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{readable}-{:08x}", hash as u32)
}

/// Keeps the environment while it's in use. Failing to doesn't stop it running.
fn keep(path: &Path, link: &Path) {
    let kept = link
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .map_err(|e| e.to_string())
        .and_then(|()| distro_backend_nixos::keep(path, link));
    if let Err(message) = kept {
        eprintln!(
            "warning: {} may be garbage collected while it runs: {message}",
            path.display()
        );
    }
}

/// The name the model uses for a package the command line names. A source given as a URL or a
/// path is added to `model` under that URL, so `github:jqlang/jq#jq` needs nothing declared.
fn package_name(model: &mut Model, reference: &str) -> Result<String, String> {
    let (source, attribute) = package_source(reference);
    refuse_version(reference, attribute)?;
    if !reference.contains('#') || model.sources.contains_key(source) {
        return Ok(reference.to_owned());
    }
    let url = if source.starts_with('.') || source.starts_with('/') {
        let path = Path::new(source)
            .canonicalize()
            .map_err(|e| format!("{source}: {e}"))?;
        format!("path:{}", path.display())
    } else if source.contains(':') {
        source.to_owned()
    } else {
        let known = model.sources.keys().map(String::as_str);
        return Err(no_such_source(source, attribute, known));
    };
    model
        .sources
        .entry(url.clone())
        .or_insert_with(|| Source::follow(&url));
    Ok(format!("{url}#{attribute}"))
}

/// Refuses a package that picks a version, such as `jq@1.7`.
pub(crate) fn refuse_version(reference: &str, attribute: &str) -> Result<(), String> {
    if attribute.contains('@') {
        return Err(format!(
            "`{reference}`: picking a version needs the version index, which doesn't exist yet"
        ));
    }
    Ok(())
}

/// The error for a package from `source`, which isn't one of the `declared` sources or a URL.
pub(crate) fn no_such_source<'a>(
    source: &str,
    attribute: &str,
    declared: impl Iterator<Item = &'a str>,
) -> String {
    let mut known: Vec<&str> = declared.collect();
    if !known.contains(&DEFAULT_SOURCE) {
        known.insert(0, DEFAULT_SOURCE);
    }
    format!(
        "there's no source `{source}`; the configuration has {}, or give a URL such as \
         `github:NixOS/nixpkgs/nixos-25.11#{attribute}`",
        known.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_stable() -> Model {
        let mut model = Model::default();
        model.sources.insert(
            "stable".into(),
            Source::follow("github:NixOS/nixpkgs/nixos-25.11"),
        );
        model
    }

    #[test]
    fn packages_come_from_declared_sources_or_urls() {
        let mut model = with_stable();
        assert_eq!(package_name(&mut model, "jq").unwrap(), "jq");
        assert_eq!(package_name(&mut model, "stable#jq").unwrap(), "stable#jq");
        assert_eq!(
            package_name(&mut model, "github:jqlang/jq#jq").unwrap(),
            "github:jqlang/jq#jq"
        );
        assert_eq!(
            model.sources["github:jqlang/jq"],
            Source::follow("github:jqlang/jq"),
            "a URL becomes a source of its own, followed to its newest version"
        );

        let dir = tempfile::tempdir().unwrap();
        let local = format!("{}#tool", dir.path().display());
        let path = package_name(&mut model, &local).unwrap();
        assert!(
            path.starts_with("path:/") && path.ends_with("#tool"),
            "{path}"
        );
    }

    #[test]
    fn unknown_sources_and_versions_are_refused() {
        let mut model = with_stable();
        assert_eq!(
            package_name(&mut model, "stabel#jq").unwrap_err(),
            "there's no source `stabel`; the configuration has nixpkgs, stable, or give a URL \
             such as `github:NixOS/nixpkgs/nixos-25.11#jq`"
        );
        assert!(
            package_name(&mut model, "jq@1.7")
                .unwrap_err()
                .contains("version index")
        );
        assert_eq!(
            package_name(&mut model, "git+ssh://git@example.com/x#jq").unwrap(),
            "git+ssh://git@example.com/x#jq",
            "an @ in the source is part of its URL"
        );
    }

    #[test]
    fn each_set_of_packages_has_its_own_root() {
        assert_eq!(root_name("jq ripgrep"), root_name("jq ripgrep"));
        assert_ne!(root_name("jq ripgrep"), root_name("jq_ripgrep"));
        assert!(root_name("jq ripgrep").starts_with("jq_ripgrep-"));
        assert!(root_name("github:jqlang/jq#jq").starts_with("github_jqlang_jq_jq-"));
    }

    #[test]
    fn what_was_picked_says_where_it_came_from() {
        let mut model = with_stable();
        model.sources.insert(
            "nixpkgs".into(),
            Source {
                url: "github:NixOS/nixpkgs/nixos-unstable".into(),
                rev: Some("1d060086d60c".into()),
                hash: "sha256-abc".into(),
                date: None,
            },
        );
        let described: Value = serde_json::json!({
            "packages": { "jq": { "version": "1.8.1", "program": "jq" } },
            "sources": {
                "nixpkgs": { "rev": "1d060086d60c", "date": "2026-10-30" },
                "stable": { "rev": "b6018f87da91" },
            },
        });
        let jq = picked(&described, "jq").unwrap();
        assert_eq!(jq.program, "jq");
        assert_eq!(
            describe(&model, &described, "jq", &jq),
            "jq 1.8.1 from nixpkgs (1d06008, 2026-10-30, locked)"
        );
        assert_eq!(
            describe(&model, &described, "stable#jq", &jq),
            "jq 1.8.1 from stable (b6018f8, its newest version)"
        );
        assert_eq!(
            describe(&Model::default(), &described, "jq", &jq),
            "jq 1.8.1 from nixpkgs (1d06008, 2026-10-30, the factory's)"
        );
        assert_eq!(picked(&described, "rg"), None);
    }
}
