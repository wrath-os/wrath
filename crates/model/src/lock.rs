//! The lock: which build of each package a configuration uses, and the package sets they come
//! from. It sits next to the configuration as `distro.lock` and is read like it, so the model
//! and its hash cover exactly what will run. Resolving it takes the backend (`distro lock`);
//! reading it doesn't.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::Deserialize;

pub use distro_eval::{DEFAULT_SOURCE, DISTRO_SOURCE, package_source};

/// The lock file's name in a configuration directory.
pub const LOCK_FILE: &str = "distro.lock";

/// The lock file format's version.
const FORMAT: i64 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lock {
    /// Each package source by name, pinned.
    pub sources: BTreeMap<String, Source>,
    pub packages: BTreeMap<String, Locked>,
}

/// The lock file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: i64,
    #[serde(default)]
    sources: BTreeMap<String, Source>,
    #[serde(default)]
    packages: BTreeMap<String, Locked>,
}

/// Where packages come from, such as a nixpkgs revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// What `distro update` follows, such as `github:NixOS/nixpkgs/nixos-unstable`.
    pub url: String,
    /// The revision it was at, when it has one.
    pub rev: Option<String>,
    /// The hash of its contents, which pins it. Empty until the source is locked.
    #[serde(default)]
    pub hash: String,
    /// When the revision was made, as `2026-10-30`, when the source says.
    pub date: Option<String>,
}

impl Source {
    /// A source that isn't locked yet, so it's taken at its newest version.
    pub fn follow(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            ..Self::default()
        }
    }

    pub fn is_locked(&self) -> bool {
        !self.hash.is_empty()
    }
}

/// One package as resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Locked {
    pub version: String,
    /// What the backend builds, such as a store path. Changes when anything in the build does.
    pub id: String,
}

impl Lock {
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = toml::from_str(text).map_err(|e| e.message().to_owned())?;
        if file.version != FORMAT {
            return Err(format!(
                "lock format version {} isn't supported, only {FORMAT}",
                file.version
            ));
        }
        Ok(Self {
            sources: file.sources,
            packages: file.packages,
        })
    }

    /// The lock file's text, one package per line.
    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# The packages this configuration uses, written by `distro lock` and moved to newer\n\
             # versions of their sources by `distro update`.\n",
        );
        // Writing to a String cannot fail.
        let _ = writeln!(out, "version = {FORMAT}");
        for (name, source) in &self.sources {
            let _ = writeln!(out, "\n[sources.{}]", key(name));
            let _ = writeln!(out, "url = {}", quote(&source.url));
            if let Some(rev) = &source.rev {
                let _ = writeln!(out, "rev = {}", quote(rev));
            }
            let _ = writeln!(out, "hash = {}", quote(&source.hash));
            if let Some(date) = &source.date {
                let _ = writeln!(out, "date = {}", quote(date));
            }
        }
        out.push_str("\n[packages]\n");
        for (name, locked) in &self.packages {
            let _ = writeln!(
                out,
                "{} = {{ version = {}, id = {} }}",
                key(name),
                quote(&locked.version),
                quote(&locked.id)
            );
        }
        out
    }
}

fn quote(text: &str) -> String {
    toml::Value::String(text.to_owned()).to_string()
}

fn key(name: &str) -> String {
    let bare = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if bare { name.to_owned() } else { quote(name) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked(version: &str, id: &str) -> Locked {
        Locked {
            version: version.into(),
            id: id.into(),
        }
    }

    fn lock() -> Lock {
        Lock {
            sources: BTreeMap::from([
                (
                    "nixpkgs".to_owned(),
                    Source {
                        url: "github:NixOS/nixpkgs/nixos-unstable".into(),
                        rev: Some("1d06008".into()),
                        hash: "sha256-abc".into(),
                        date: Some("2026-10-30".into()),
                    },
                ),
                (
                    "stable".to_owned(),
                    Source {
                        url: "github:NixOS/nixpkgs/nixos-25.11".into(),
                        rev: Some("9f2c1aa".into()),
                        hash: "sha256-def".into(),
                        date: None,
                    },
                ),
            ]),
            packages: BTreeMap::from([
                (
                    "git".to_owned(),
                    locked("2.51.0", "/nix/store/aaa-git-2.51.0"),
                ),
                (
                    "python3Packages.rich".to_owned(),
                    locked("14.0", "/nix/store/bbb-rich"),
                ),
                (
                    "stable#git".to_owned(),
                    locked("2.50.1", "/nix/store/ccc-git-2.50.1"),
                ),
            ]),
        }
    }

    #[test]
    fn locks_are_written_one_source_and_package_per_entry_and_read_back() {
        let text = lock().to_toml();

        insta::assert_snapshot!(text, @r#"
        # The packages this configuration uses, written by `distro lock` and moved to newer
        # versions of their sources by `distro update`.
        version = 2

        [sources.nixpkgs]
        url = "github:NixOS/nixpkgs/nixos-unstable"
        rev = "1d06008"
        hash = "sha256-abc"
        date = "2026-10-30"

        [sources.stable]
        url = "github:NixOS/nixpkgs/nixos-25.11"
        rev = "9f2c1aa"
        hash = "sha256-def"

        [packages]
        git = { version = "2.51.0", id = "/nix/store/aaa-git-2.51.0" }
        "python3Packages.rich" = { version = "14.0", id = "/nix/store/bbb-rich" }
        "stable#git" = { version = "2.50.1", id = "/nix/store/ccc-git-2.50.1" }
        "#);
        assert_eq!(Lock::parse(&text).unwrap(), lock());
    }

    #[test]
    fn locks_of_another_format_or_shape_are_refused() {
        let other = lock().to_toml().replace("version = 2", "version = 3");
        assert_eq!(
            Lock::parse(&other).unwrap_err(),
            "lock format version 3 isn't supported, only 2"
        );
        let typo = lock().to_toml().replace("hash =", "hsah =");
        assert!(Lock::parse(&typo).unwrap_err().contains("hsah"));
    }

    #[test]
    fn package_names_say_their_source_or_come_from_nixpkgs() {
        assert_eq!(package_source("stable#git"), ("stable", "git"));
        assert_eq!(
            package_source("python3Packages.rich"),
            ("nixpkgs", "python3Packages.rich")
        );
    }
}
