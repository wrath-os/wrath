//! The lock: which build of each package a configuration uses, and the package set they come
//! from. It sits next to the configuration as `distro.lock` and is read like it, so the model
//! and its hash cover exactly what will run. Resolving it takes the backend (`distro lock`);
//! reading it doesn't.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::Deserialize;

/// The lock file's name in a configuration directory.
pub const LOCK_FILE: &str = "distro.lock";

/// The lock file format's version.
const FORMAT: i64 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    version: i64,
    pub source: Source,
    #[serde(default)]
    pub packages: BTreeMap<String, Locked>,
}

/// Where packages come from, such as a nixpkgs revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// What `distro update` follows, such as `github:NixOS/nixpkgs/nixos-unstable`.
    pub url: String,
    /// The revision it was at, when it has one.
    pub rev: Option<String>,
    /// The hash of its contents, which pins it.
    pub hash: String,
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
    pub fn new(source: Source, packages: BTreeMap<String, Locked>) -> Self {
        Self {
            version: FORMAT,
            source,
            packages,
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let lock: Self = toml::from_str(text).map_err(|e| e.message().to_owned())?;
        if lock.version != FORMAT {
            return Err(format!(
                "lock format version {} isn't supported, only {FORMAT}",
                lock.version
            ));
        }
        Ok(lock)
    }

    /// The lock file's text, one package per line.
    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# The packages this configuration uses, written by `distro lock` and moved to a newer\n\
             # package set by `distro update`.\n",
        );
        // Writing to a String cannot fail.
        let _ = writeln!(out, "version = {FORMAT}\n\n[source]");
        let _ = writeln!(out, "url = {}", quote(&self.source.url));
        if let Some(rev) = &self.source.rev {
            let _ = writeln!(out, "rev = {}", quote(rev));
        }
        let _ = writeln!(out, "hash = {}\n\n[packages]", quote(&self.source.hash));
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

    fn lock() -> Lock {
        Lock::new(
            Source {
                url: "github:NixOS/nixpkgs/nixos-unstable".into(),
                rev: Some("1d06008".into()),
                hash: "sha256-abc".into(),
            },
            BTreeMap::from([
                (
                    "git".to_owned(),
                    Locked {
                        version: "2.51.0".into(),
                        id: "/nix/store/aaa-git-2.51.0".into(),
                    },
                ),
                (
                    "python3Packages.rich".to_owned(),
                    Locked {
                        version: "14.0".into(),
                        id: "/nix/store/bbb-rich".into(),
                    },
                ),
            ]),
        )
    }

    #[test]
    fn locks_are_written_one_package_per_line_and_read_back() {
        let text = lock().to_toml();

        insta::assert_snapshot!(text, @r#"
        # The packages this configuration uses, written by `distro lock` and moved to a newer
        # package set by `distro update`.
        version = 1

        [source]
        url = "github:NixOS/nixpkgs/nixos-unstable"
        rev = "1d06008"
        hash = "sha256-abc"

        [packages]
        git = { version = "2.51.0", id = "/nix/store/aaa-git-2.51.0" }
        "python3Packages.rich" = { version = "14.0", id = "/nix/store/bbb-rich" }
        "#);
        assert_eq!(Lock::parse(&text).unwrap(), lock());
    }

    #[test]
    fn locks_of_another_format_or_shape_are_refused() {
        let other = lock().to_toml().replace("version = 1", "version = 2");
        assert_eq!(
            Lock::parse(&other).unwrap_err(),
            "lock format version 2 isn't supported, only 1"
        );
        let typo = lock().to_toml().replace("hash =", "hsah =");
        assert!(Lock::parse(&typo).unwrap_err().contains("hsah"));
    }
}
