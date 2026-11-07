//! Projects: finding the one a command runs in, and what its shell sets on top of the home.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use distro_eval::{ConfigFs, DirFs, Value};
use distro_model::{DEFAULT_SOURCE, Lock, Model};

const PROJECT: &str = "distro.toml";
const PROJECT_LOCAL: &str = "distro.local.toml";
/// The lock of what only `distro.local.toml` adds.
pub const LOCAL_LOCK: &str = "distro.local.lock";

/// The nearest directory from `start` upwards with a `distro.toml` or `distro.local.toml`.
pub fn find(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(PROJECT).is_file() || dir.join(PROJECT_LOCAL).is_file())
        .map(Path::to_path_buf)
}

/// The first line of a command's output in the project at `dir`: its name, and which file it
/// starts from when that isn't `distro.toml`.
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

/// The version in a store path such as `/nix/store/<hash>-jq-1.8.2-bin/bin/jq`: `1.8.2`.
fn store_version(path: &Path) -> Option<String> {
    let entry = path.strip_prefix("/nix/store").ok()?.components().next()?;
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

/// A project as everyone else has it: without `distro.local.toml`.
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

    fn display(&self, path: &str) -> String {
        self.0.display(path)
    }
}

/// Whether the project at `dir` has a local file, whose packages get a lock of their own.
pub fn has_local(dir: &Path) -> bool {
    dir.join(PROJECT_LOCAL).is_file()
}

/// Splits `lock` into what `shared`, the project without its local file, uses, and the rest,
/// which only the local file adds.
pub fn split_lock(lock: Lock, shared: &Model) -> (Lock, Lock) {
    let packages: BTreeSet<&str> = shared.all_packages().collect();
    let (ours, local) = lock
        .packages
        .into_iter()
        .partition(|(name, _)| packages.contains(name.as_str()));
    let (sources, local_sources) = lock
        .sources
        .into_iter()
        .partition(|(name, _)| shared.sources.contains_key(name) || name == DEFAULT_SOURCE);
    (
        Lock {
            sources,
            packages: ours,
        },
        Lock {
            sources: local_sources,
            packages: local,
        },
    )
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
            "project myapp: distro.local.toml over distro.toml"
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
            version("/nix/store/abc-jq-1.8.2-bin/bin/jq").as_deref(),
            Some("1.8.2")
        );
        assert_eq!(
            version("/nix/store/abc-ripgrep-15.2.0/bin/rg").as_deref(),
            Some("15.2.0")
        );
        assert_eq!(
            version("/nix/store/abc-python3-3.13.1/bin/python").as_deref(),
            Some("3.13.1")
        );
        assert_eq!(version("/nix/store/abc-hello/bin/hello"), None);
        assert_eq!(version("/usr/bin/jq"), None);
    }

    #[test]
    fn what_only_the_local_file_adds_is_locked_apart() {
        let locked = |version: &str| distro_model::Locked {
            version: version.into(),
            id: format!("/nix/store/{version}"),
        };
        let mut lock = Lock::default();
        for name in ["nixpkgs", "acme", "mine"] {
            lock.sources
                .insert(name.into(), distro_model::Source::follow(name));
        }
        lock.packages.insert("jq".into(), locked("1.8"));
        lock.packages.insert("bacon".into(), locked("3.0"));
        let mut shared = Model::default();
        shared.packages.insert("jq".into());
        shared
            .sources
            .insert("acme".into(), distro_model::Source::follow("acme"));

        let (ours, local) = split_lock(lock, &shared);
        assert_eq!(ours.packages.keys().collect::<Vec<_>>(), ["jq"]);
        assert_eq!(ours.sources.keys().collect::<Vec<_>>(), ["acme", "nixpkgs"]);
        assert_eq!(local.packages.keys().collect::<Vec<_>>(), ["bacon"]);
        assert_eq!(local.sources.keys().collect::<Vec<_>>(), ["mine"]);
    }
}
