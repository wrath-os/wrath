//! Projects: finding the one a command runs in, and what its shell sets on top of the home.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use wrath_build::store::STORE_DIR;
use wrath_eval::{ConfigFs, DirFs, Value};
use wrath_model::{Lock, Model, WRATH_SOURCE};

const PROJECT: &str = "wrath.toml";
const PROJECT_LOCAL: &str = "wrath.local.toml";
/// The lock of what only `wrath.local.toml` adds.
pub const LOCAL_LOCK: &str = "wrath.local.lock";

/// The nearest directory from `start` upwards with a `wrath.toml` or `wrath.local.toml`.
pub fn find(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(PROJECT).is_file() || dir.join(PROJECT_LOCAL).is_file())
        .map(Path::to_path_buf)
}

/// The first line of a command's output in the project at `dir`: its name, and which file it
/// starts from when that isn't `wrath.toml`.
pub fn header(dir: &Path) -> String {
    let name = dir.file_name().map_or_else(
        || dir.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    if has_local(dir) {
        format!("project {name}: {PROJECT_LOCAL} over {PROJECT}")
    } else {
        format!("project {name}")
    }
}

/// The file the project at `dir` starts from, which `add` and `remove` write to.
pub fn starting_file(dir: &Path) -> PathBuf {
    if has_local(dir) {
        dir.join(PROJECT_LOCAL)
    } else {
        dir.join(PROJECT)
    }
}

/// The variables a project's shell sets, with `$NAME` and `${NAME}` taken from `lookup`, which
/// gives the variables the shell starts with.
pub fn variables(model: &Model, lookup: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    model
        .options
        .iter()
        .filter_map(
            |(path, value)| match (path.strip_prefix("environment."), value) {
                (Some(name), Value::Str(text)) => Some((name.to_owned(), expand(text, &lookup))),
                _ => None,
            },
        )
        .collect()
}

/// `text` with `$NAME` and `${NAME}` replaced by what `lookup` gives, or nothing.
fn expand(text: &str, lookup: &impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let after = &rest[at + 1..];
        let (name, next) = match after.strip_prefix('{') {
            Some(braced) => match braced.find('}') {
                Some(end) => (&braced[..end], &braced[end + 1..]),
                None => ("", after),
            },
            None => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                (&after[..end], &after[end..])
            }
        };
        if name.is_empty() {
            out.push('$');
        } else {
            out.push_str(&lookup(name).unwrap_or_default());
        }
        rest = next;
    }
    out.push_str(rest);
    out
}

/// What the project's shell hides from the home: programs in `bin` that `path` has a different
/// one of, with both versions when the store says, and variables `lookup` has with a different
/// value.
pub fn shadows(
    bin: &Path,
    programs: &[String],
    variables: &[(String, String)],
    path: &[PathBuf],
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut shadowed: Vec<String> = programs
        .iter()
        .filter_map(|program| {
            let home = path
                .iter()
                .map(|dir| dir.join(program.as_str()))
                .find(|candidate| candidate.exists())?;
            let (home, ours) = (resolved(&home), resolved(&bin.join(program.as_str())));
            if home == ours {
                return None;
            }
            Some(match (store_version(&home), store_version(&ours)) {
                (Some(old), Some(new)) if old != new => format!("{program} {old} with {new}"),
                _ => program.clone(),
            })
        })
        .collect();
    shadowed.sort();
    shadowed.extend(
        variables
            .iter()
            .filter(|(name, value)| lookup(name).is_some_and(|old| old != *value))
            .map(|(name, _)| format!("${name}")),
    );
    shadowed
}

fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The version in a store path such as `/wrath/store/<hash>-jq-1.8.2-bin/bin/jq`: `1.8.2`, once
/// store paths carry names.
fn store_version(path: &Path) -> Option<String> {
    let entry = path.strip_prefix(STORE_DIR).ok()?.components().next()?;
    let entry = entry.as_os_str().to_str()?;
    let (_, name) = entry.split_once('-')?;
    let (dash, _) = name
        .match_indices('-')
        .find(|(at, _)| name[at + 1..].starts_with(|c: char| c.is_ascii_digit()))?;
    let version = &name[dash + 1..];
    let outputs = ["-bin", "-out", "-lib", "-dev", "-man"];
    let version = outputs
        .iter()
        .find_map(|output| version.strip_suffix(output))
        .unwrap_or(version);
    Some(version.to_owned())
}

/// A project as everyone else has it: without `wrath.local.toml`.
pub struct Shared(pub DirFs);

impl ConfigFs for Shared {
    fn read(&self, path: &str) -> io::Result<String> {
        if path == PROJECT_LOCAL {
            return Err(io::ErrorKind::NotFound.into());
        }
        self.0.read(path)
    }

    fn read_bytes(&self, path: &str) -> io::Result<Vec<u8>> {
        self.0.read_bytes(path)
    }

    fn toml_files(&self, dir: &str) -> io::Result<Vec<String>> {
        self.0.toml_files(dir)
    }

    fn subdirs(&self, dir: &str) -> io::Result<Vec<String>> {
        self.0.subdirs(dir)
    }

    fn display(&self, path: &str) -> String {
        self.0.display(path)
    }
}

/// Whether the project at `dir` has a local file, whose packages get a lock of their own.
pub fn has_local(dir: &Path) -> bool {
    dir.join(PROJECT_LOCAL).is_file()
}

/// Splits `lock` into the sources `shared`, the project without its local file, uses, and the
/// rest, which only the local file adds.
pub fn split_lock(lock: Lock, shared: &Model) -> (Lock, Lock) {
    let (sources, local) = lock
        .sources
        .into_iter()
        .partition(|(name, _)| shared.sources.contains_key(name) || name == WRATH_SOURCE);
    (
        Lock {
            sources,
            ids: lock.ids,
        },
        Lock {
            sources: local,
            ..Lock::default()
        },
    )
}

/// process-compose's configuration for the project's services in `model`, which `wrath up`
/// runs, or `None` when it has none. Commands run as written, without a shell, and every string
/// is YAML single-quoted, the one form process-compose reads without changing backslashes.
pub fn compose(model: &Model) -> Option<String> {
    let services = services(model);
    if services.is_empty() {
        return None;
    }
    let mut out = String::from("version: '0.5'\nprocesses:\n");
    for (name, service) in &services {
        let command: Vec<String> = service.command.iter().map(|arg| quoted(arg)).collect();
        let restart = match service.restart.as_str() {
            "always" => "always",
            "never" => "no",
            _ => "on_failure",
        };
        out.push_str(&format!("  {}:\n", quoted(name)));
        out.push_str(&format!("    entrypoint: [{}]\n", command.join(", ")));
        out.push_str(&format!("    availability: {{restart: '{restart}'}}\n"));
        out.push_str(&format!("    disabled: {}\n", !service.autostart));
        if !service.environment.is_empty() {
            let pairs: Vec<String> = service
                .environment
                .iter()
                .map(|(key, value)| quoted(&format!("{key}={value}")))
                .collect();
            out.push_str(&format!("    environment: [{}]\n", pairs.join(", ")));
        }
        if let Some(port) = service.ready.strip_prefix("tcp:") {
            let probe = format!("bash -c \"(exec 3<>/dev/tcp/127.0.0.1/{port})\"");
            out.push_str(&format!(
                "    readiness_probe: {{exec: {{command: {}}}, period_seconds: 1}}\n",
                quoted(&probe)
            ));
        }
        let needs: Vec<String> = service
            .needs
            .iter()
            .filter_map(|other| {
                let condition = match services.get(other)?.ready.as_str() {
                    "exit" => "process_completed_successfully",
                    "started" => "process_started",
                    _ => "process_healthy",
                };
                Some(format!("{}: {{condition: {condition}}}", quoted(other)))
            })
            .collect();
        if !needs.is_empty() {
            out.push_str(&format!("    depends_on: {{{}}}\n", needs.join(", ")));
        }
    }
    Some(out)
}

/// `text` as a YAML single-quoted string.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// A project's service, as its options say.
#[derive(Debug, Default)]
struct Service {
    command: Vec<String>,
    environment: BTreeMap<String, String>,
    restart: String,
    needs: Vec<String>,
    ready: String,
    autostart: bool,
}

/// The project's services, from the options under `services`.
fn services(model: &Model) -> BTreeMap<String, Service> {
    let mut services: BTreeMap<String, Service> = BTreeMap::new();
    let strings = |value: &Value| match value {
        Value::List(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Str(text) => Some(text.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    for (path, value) in &model.options {
        let Some(rest) = path.strip_prefix("services.") else {
            continue;
        };
        let Some((name, field)) = rest.split_once('.') else {
            continue;
        };
        let service = services.entry(name.to_owned()).or_default();
        match (field, value) {
            ("command", value) => service.command = strings(value),
            ("needs", value) => service.needs = strings(value),
            ("restart", Value::Str(text)) => service.restart = text.clone(),
            ("ready", Value::Str(text)) => service.ready = text.clone(),
            ("autostart", Value::Bool(on)) => service.autostart = *on,
            (field, Value::Str(text)) => {
                if let Some(name) = field.strip_prefix("environment.") {
                    service.environment.insert(name.to_owned(), text.clone());
                }
            }
            _ => {}
        }
    }
    services
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name {
            "HOME" => Some("/home/john".into()),
            "EDITOR" => Some("hx".into()),
            _ => None,
        }
    }

    #[test]
    fn a_project_is_found_upwards_from_where_a_command_runs() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("myapp");
        std::fs::create_dir_all(project.join("src/deep")).unwrap();
        assert_eq!(find(&project.join("src/deep")), None);

        std::fs::write(project.join(PROJECT), "").unwrap();
        assert_eq!(find(&project.join("src/deep")), Some(project.clone()));
        assert_eq!(header(&project), "project myapp");

        std::fs::write(project.join(PROJECT_LOCAL), "").unwrap();
        assert_eq!(
            header(&project),
            "project myapp: wrath.local.toml over wrath.toml"
        );
    }

    #[test]
    fn variables_refer_to_the_ones_the_shell_starts_with() {
        let mut model = Model::default();
        let set = [
            ("CACHE", "$HOME/.cache/${PROJECT}x"),
            ("EDITOR", "nvim"),
            ("PRICE", "5$ or ${"),
        ];
        for (name, value) in set {
            let path = format!("environment.{name}");
            model.options.insert(path, Value::Str(value.into()));
        }
        model
            .options
            .insert("packages".into(), Value::List(Vec::new()));

        assert_eq!(
            variables(&model, env),
            [
                ("CACHE".to_owned(), "/home/john/.cache/x".to_owned()),
                ("EDITOR".to_owned(), "nvim".to_owned()),
                ("PRICE".to_owned(), "5$ or ${".to_owned()),
            ]
        );
    }

    #[test]
    fn the_shell_says_what_it_hides_from_the_home() {
        let (home, ours) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for program in ["jq", "hx"] {
            std::fs::write(home.path().join(program), "").unwrap();
            std::fs::write(ours.path().join(program), "").unwrap();
        }
        // The same program as the home's hides nothing.
        std::fs::remove_file(ours.path().join("hx")).unwrap();
        std::os::unix::fs::symlink(home.path().join("hx"), ours.path().join("hx")).unwrap();
        let programs = ["rg".to_owned(), "jq".to_owned(), "hx".to_owned()];
        let variables = [
            ("EDITOR".to_owned(), "nvim".to_owned()),
            ("HOME".to_owned(), "/home/john".to_owned()),
            ("RUST_LOG".to_owned(), "info".to_owned()),
        ];

        let path = [home.path().to_path_buf()];
        let shadowed = shadows(ours.path(), &programs, &variables, &path, env);
        assert_eq!(shadowed, ["jq", "$EDITOR"]);
    }

    #[test]
    fn versions_come_from_store_paths() {
        let version = |path: &str| store_version(Path::new(path));
        assert_eq!(
            version("/wrath/store/abc-jq-1.8.2-bin/bin/jq").as_deref(),
            Some("1.8.2")
        );
        assert_eq!(
            version("/wrath/store/abc-ripgrep-15.2.0/bin/rg").as_deref(),
            Some("15.2.0")
        );
        assert_eq!(
            version("/wrath/store/abc-python3-3.13.1/bin/python").as_deref(),
            Some("3.13.1")
        );
        assert_eq!(version("/wrath/store/abc-hello/bin/hello"), None);
        assert_eq!(version("/usr/bin/jq"), None);
    }

    #[test]
    fn what_only_the_local_file_adds_is_locked_apart() {
        let mut lock = Lock::default();
        for name in ["wrath", "acme", "mine"] {
            lock.sources
                .insert(name.into(), wrath_model::Source::follow(name));
        }
        let mut shared = Model::default();
        shared.packages.insert("jq".into());
        shared
            .sources
            .insert("acme".into(), wrath_model::Source::follow("acme"));

        let (ours, local) = split_lock(lock, &shared);
        assert_eq!(ours.sources.keys().collect::<Vec<_>>(), ["acme", "wrath"]);
        assert_eq!(local.sources.keys().collect::<Vec<_>>(), ["mine"]);
    }

    #[test]
    fn services_run_with_process_compose_as_written() {
        let mut model = Model::default();
        let set = |model: &mut Model, path: &str, value: Value| {
            model.options.insert(path.to_owned(), value);
        };
        let list =
            |items: &[&str]| Value::List(items.iter().map(|s| Value::Str((*s).into())).collect());
        set(
            &mut model,
            "services.db.command",
            list(&["postgres", "-D", "it's \\here"]),
        );
        set(
            &mut model,
            "services.db.ready",
            Value::Str("tcp:5432".into()),
        );
        set(
            &mut model,
            "services.db.restart",
            Value::Str("always".into()),
        );
        set(&mut model, "services.db.autostart", Value::Bool(true));
        set(
            &mut model,
            "services.db.environment.PGDATA",
            Value::Str("/srv/db".into()),
        );
        set(&mut model, "services.migrate.command", list(&["migrate"]));
        set(
            &mut model,
            "services.migrate.ready",
            Value::Str("exit".into()),
        );
        set(&mut model, "services.migrate.needs", list(&["db"]));
        set(&mut model, "services.migrate.autostart", Value::Bool(false));

        assert_eq!(compose(&Model::default()), None);
        insta::assert_snapshot!(compose(&model).unwrap(), @r#"
        version: '0.5'
        processes:
          'db':
            entrypoint: ['postgres', '-D', 'it''s \here']
            availability: {restart: 'always'}
            disabled: false
            environment: ['PGDATA=/srv/db']
            readiness_probe: {exec: {command: 'bash -c "(exec 3<>/dev/tcp/127.0.0.1/5432)"'}, period_seconds: 1}
          'migrate':
            entrypoint: ['migrate']
            availability: {restart: 'on_failure'}
            disabled: true
            depends_on: {'db': {condition: process_healthy}}
        "#);
    }
}
