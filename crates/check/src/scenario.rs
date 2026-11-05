//! Loading and validating acceptance scenarios.
//!
//! An acceptance directory holds `milestones.toml`, which lists milestones in order, and one
//! subdirectory per milestone containing one TOML file per scenario. A scenario's ID is its file
//! name without the extension. Each scenario declares exactly one runner table: `[command]`,
//! `[vm]`, `[nspawn]` or `[manual]`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::vm::VmSpec;

/// Directory inside the acceptance directory that holds human sign-offs, not scenarios.
pub const SIGNOFF_DIR: &str = "signoff";

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{}: {message}", path.display())]
    Invalid { path: PathBuf, message: String },
}

impl LoadError {
    fn invalid(path: &Path, message: impl Into<String>) -> Self {
        Self::Invalid {
            path: path.to_path_buf(),
            message: message.into(),
        }
    }

    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Milestone {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub id: String,
    pub milestone: String,
    pub title: String,
    pub runner: Runner,
    /// The file the scenario was loaded from.
    pub source: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Runner {
    Command(CommandSpec),
    Manual(ManualSpec),
    Vm(VmSpec),
    /// Declared, but not runnable until a later milestone adds what it needs.
    Pending {
        reason: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    /// Program and arguments. A program named `distro` resolves to the running `distro` binary.
    pub run: Vec<String>,
    /// Expected exit code.
    #[serde(default)]
    pub exit: i32,
    #[serde(default)]
    pub stdout_contains: Vec<String>,
    #[serde(default)]
    pub stderr_contains: Vec<String>,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

impl CommandSpec {
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

fn default_timeout_secs() -> u64 {
    60
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualSpec {
    /// What a human has to confirm before signing the scenario off.
    pub confirm: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScenario {
    title: String,
    /// Free-form context for humans; accepted and ignored.
    #[serde(default, rename = "notes")]
    _notes: Option<serde::de::IgnoredAny>,
    command: Option<CommandSpec>,
    manual: Option<ManualSpec>,
    vm: Option<toml::Table>,
    nspawn: Option<toml::Table>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMilestones {
    milestone: Vec<Milestone>,
}

/// Parses one scenario file's contents.
pub fn parse_scenario(
    path: &Path,
    id: &str,
    milestone: &str,
    text: &str,
) -> Result<Scenario, LoadError> {
    let raw: RawScenario =
        toml::from_str(text).map_err(|e| LoadError::invalid(path, e.to_string()))?;

    if raw.command.as_ref().is_some_and(|spec| spec.run.is_empty()) {
        return Err(LoadError::invalid(path, "`command.run` must not be empty"));
    }

    let vm = raw.vm.map(|table| vm_runner(path, table)).transpose()?;
    let mut runners = [
        raw.command.map(Runner::Command),
        raw.manual.map(Runner::Manual),
        vm,
        raw.nspawn.map(|_| Runner::Pending {
            reason: "no `nspawn` runner yet",
        }),
    ]
    .into_iter()
    .flatten();

    let runner = match (runners.next(), runners.next()) {
        (Some(runner), None) => runner,
        (None, _) => {
            return Err(LoadError::invalid(
                path,
                "no runner: add one of [command], [manual], [vm] or [nspawn]",
            ));
        }
        (Some(_), Some(_)) => {
            return Err(LoadError::invalid(
                path,
                "more than one runner: keep exactly one of [command], [manual], [vm] or [nspawn]",
            ));
        }
    };

    Ok(Scenario {
        id: id.to_owned(),
        milestone: milestone.to_owned(),
        title: raw.title,
        runner,
        source: path.to_path_buf(),
    })
}

fn vm_runner(path: &Path, table: toml::Table) -> Result<Runner, LoadError> {
    if table.contains_key("config") {
        return Ok(Runner::Pending {
            reason: "vm scenarios with `config` need the NixOS backend (m3)",
        });
    }
    toml::Value::Table(table)
        .try_into()
        .map(Runner::Vm)
        .map_err(|e: toml::de::Error| LoadError::invalid(path, format!("[vm]: {}", e.message())))
}

/// All milestones and scenarios in an acceptance directory, in milestone order.
#[derive(Debug, Clone, PartialEq)]
pub struct Suite {
    pub milestones: Vec<Milestone>,
    pub scenarios: Vec<Scenario>,
}

impl Suite {
    pub fn load(dir: &Path) -> Result<Self, LoadError> {
        let milestones_path = dir.join("milestones.toml");
        let text =
            fs::read_to_string(&milestones_path).map_err(|e| LoadError::io(&milestones_path, e))?;
        let milestones = toml::from_str::<RawMilestones>(&text)
            .map_err(|e| LoadError::invalid(&milestones_path, e.to_string()))?
            .milestone;

        let mut seen = HashSet::new();
        if let Some(duplicate) = milestones.iter().find(|m| !seen.insert(&m.id)) {
            return Err(LoadError::invalid(
                &milestones_path,
                format!("milestone `{}` is listed twice", duplicate.id),
            ));
        }

        for entry in read_dir_sorted(dir)? {
            if !entry.is_dir() {
                continue;
            }
            let name = file_name(&entry);
            if name != SIGNOFF_DIR && !milestones.iter().any(|m| m.id == name) {
                return Err(LoadError::invalid(
                    &entry,
                    "directory is not a milestone listed in milestones.toml",
                ));
            }
        }

        let mut scenarios = Vec::new();
        for milestone in &milestones {
            let milestone_dir = dir.join(&milestone.id);
            if !milestone_dir.is_dir() {
                continue;
            }
            for path in read_dir_sorted(&milestone_dir)? {
                if path.extension().is_none_or(|ext| ext != "toml") {
                    return Err(LoadError::invalid(
                        &path,
                        "only .toml scenario files belong in a milestone directory",
                    ));
                }
                let text = fs::read_to_string(&path).map_err(|e| LoadError::io(&path, e))?;
                let id = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                scenarios.push(parse_scenario(&path, &id, &milestone.id, &text)?);
            }
        }

        Ok(Self {
            milestones,
            scenarios,
        })
    }
}

fn read_dir_sorted(dir: &Path) -> Result<Vec<PathBuf>, LoadError> {
    let mut paths = fs::read_dir(dir)
        .map_err(|e| LoadError::io(dir, e))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| LoadError::io(dir, e))?;
    paths.sort();
    Ok(paths)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Scenario, LoadError> {
        parse_scenario(Path::new("m1/example.toml"), "example", "m1", text)
    }

    #[test]
    fn parses_command_scenario_with_defaults() {
        let scenario = parse(
            r#"
            title = "Runs"
            [command]
            run = ["distro", "plan"]
            "#,
        )
        .unwrap();

        assert_eq!(scenario.id, "example");
        assert_eq!(scenario.milestone, "m1");
        assert_eq!(scenario.title, "Runs");
        let Runner::Command(spec) = scenario.runner else {
            panic!("expected a command runner");
        };
        assert_eq!(spec.run, ["distro", "plan"]);
        assert_eq!(spec.exit, 0);
        assert!(spec.stdout_contains.is_empty());
        assert_eq!(spec.timeout(), Duration::from_secs(60));
    }

    #[test]
    fn parses_manual_scenario() {
        let scenario = parse(
            r#"
            title = "Signed off"
            [manual]
            confirm = "It works"
            "#,
        )
        .unwrap();

        assert_eq!(
            scenario.runner,
            Runner::Manual(ManualSpec {
                confirm: "It works".into()
            })
        );
    }

    #[test]
    fn vm_with_image_gets_the_vm_runner() {
        let scenario = parse("title = \"t\"\n[vm]\nimage = \"factory/nix#fixture\"\n").unwrap();

        let Runner::Vm(spec) = scenario.runner else {
            panic!("expected a vm runner");
        };
        assert_eq!(spec.image, "factory/nix#fixture");
        assert_eq!(scenario.source, Path::new("m1/example.toml"));
    }

    #[test]
    fn vm_with_config_and_nspawn_are_pending() {
        let vm = parse("title = \"t\"\n[vm]\nconfig = \"x\"\nsteps = []\n").unwrap();
        assert_eq!(
            vm.runner,
            Runner::Pending {
                reason: "vm scenarios with `config` need the NixOS backend (m3)"
            }
        );

        let nspawn = parse("title = \"t\"\n[nspawn]\n").unwrap();
        assert_eq!(
            nspawn.runner,
            Runner::Pending {
                reason: "no `nspawn` runner yet"
            }
        );
    }

    #[test]
    fn notes_are_allowed() {
        parse("title = \"t\"\nnotes = \"why\"\n[nspawn]\n").unwrap();
    }

    #[test]
    fn errors_are_reported_with_the_file_path() {
        let cases = [
            ("no_runner", "title = \"t\"\n"),
            (
                "two_runners",
                "title = \"t\"\n[nspawn]\n[manual]\nconfirm = \"x\"\n",
            ),
            ("empty_run", "title = \"t\"\n[command]\nrun = []\n"),
            (
                "unknown_key",
                "title = \"t\"\nowner = \"me\"\n[manual]\nconfirm = \"x\"\n",
            ),
            (
                "unknown_command_key",
                "title = \"t\"\n[command]\nrun = [\"true\"]\nstdout = \"x\"\n",
            ),
            ("missing_title", "[manual]\nconfirm = \"x\"\n"),
            (
                "vm_unknown_key",
                "title = \"t\"\n[vm]\nimage = \"x\"\nram = 4\n",
            ),
        ];
        for (name, text) in cases {
            let err = parse(text).unwrap_err();
            insta::assert_snapshot!(name, err.to_string());
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    const MILESTONES: &str = r#"
        [[milestone]]
        id = "m1"
        title = "First"

        [[milestone]]
        id = "m2"
        title = "Second"
    "#;

    #[test]
    fn loads_scenarios_in_milestone_then_file_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(&root.join("milestones.toml"), MILESTONES);
        write(&root.join("m2/a.toml"), "title = \"m2 a\"\n[nspawn]\n");
        write(&root.join("m1/b.toml"), "title = \"m1 b\"\n[nspawn]\n");
        write(&root.join("m1/a.toml"), "title = \"m1 a\"\n[nspawn]\n");
        write(&root.join("signoff/m1-a.toml"), "by = \"human\"\n");

        let suite = Suite::load(root).unwrap();

        let ids: Vec<_> = suite
            .scenarios
            .iter()
            .map(|s| format!("{}/{}", s.milestone, s.id))
            .collect();
        assert_eq!(ids, ["m1/a", "m1/b", "m2/a"]);
        assert_eq!(suite.milestones.len(), 2);
    }

    #[test]
    fn milestone_without_directory_has_no_scenarios() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("milestones.toml"), MILESTONES);

        let suite = Suite::load(dir.path()).unwrap();

        assert!(suite.scenarios.is_empty());
    }

    #[test]
    fn rejects_directory_that_is_not_a_milestone() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("milestones.toml"), MILESTONES);
        write(&dir.path().join("m9/a.toml"), "title = \"t\"\n[nspawn]\n");

        let err = Suite::load(dir.path()).unwrap_err();

        assert!(
            err.to_string()
                .ends_with("m9: directory is not a milestone listed in milestones.toml"),
            "{err}"
        );
    }

    #[test]
    fn rejects_non_toml_file_in_milestone_directory() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("milestones.toml"), MILESTONES);
        write(&dir.path().join("m1/readme.md"), "hi");

        let err = Suite::load(dir.path()).unwrap_err();

        assert!(err.to_string().contains("readme.md"), "{err}");
    }

    #[test]
    fn rejects_duplicate_milestone() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("milestones.toml"),
            "[[milestone]]\nid = \"m1\"\ntitle = \"a\"\n[[milestone]]\nid = \"m1\"\ntitle = \"b\"\n",
        );

        let err = Suite::load(dir.path()).unwrap_err();

        assert!(
            err.to_string().ends_with("milestone `m1` is listed twice"),
            "{err}"
        );
    }

    #[test]
    fn missing_milestones_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();

        let err = Suite::load(dir.path()).unwrap_err();

        assert!(matches!(err, LoadError::Io { .. }), "{err}");
    }
}
