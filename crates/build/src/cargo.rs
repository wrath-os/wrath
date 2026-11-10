//! Reads a `Cargo.lock` into what a recipe needs: one source per crate, pinned by the lock's hash.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::store::with_path;

#[derive(Deserialize)]
struct Lock {
    package: Vec<LockedPackage>,
}

#[derive(Deserialize)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// A crate from the registry.
#[derive(Debug, PartialEq, Eq)]
pub struct Crate {
    pub name: String,
    pub version: String,
    pub checksum: String,
}

impl Crate {
    fn url(&self) -> String {
        format!(
            "https://static.crates.io/crates/{0}/{0}-{1}.crate",
            self.name, self.version
        )
    }
}

/// The registry crates in the text of a `Cargo.lock`. The workspace's own have no source.
pub fn crates(lock: &str) -> Result<Vec<Crate>, String> {
    let lock: Lock = toml::from_str(lock).map_err(|e| format!("Cargo.lock: {e}"))?;
    let mut crates = Vec::new();
    for package in lock.package {
        match package.source.as_deref() {
            None => {}
            Some(REGISTRY) => crates.push(Crate {
                checksum: package
                    .checksum
                    .ok_or_else(|| format!("`{}` has no checksum", package.name))?,
                name: package.name,
                version: package.version,
            }),
            Some(other) => return Err(format!("`{}` comes from {other}", package.name)),
        }
    }
    Ok(crates)
}

/// The recipe's `sources` entries, each unpacked under `vendor/`.
pub fn source_lines(crates: &[Crate]) -> String {
    crates
        .iter()
        .map(|c| {
            format!(
                "    {{ url = \"{}\", into = \"vendor/{}-{}\" }},\n",
                c.url(),
                c.name,
                c.version
            )
        })
        .collect()
}

/// Adds the lock's hashes to `sources.lock`, keeping what's there, so fetching checks each crate
/// against what the lock says.
pub fn pin(crates: &[Crate], file: &Path) -> Result<(), String> {
    let text = fs::read_to_string(file).map_err(with_path(file))?;
    let mut pins: BTreeMap<String, String> = text
        .lines()
        .filter_map(|line| line.split_once("  "))
        .map(|(hash, url)| (url.to_owned(), hash.to_owned()))
        .collect();
    for item in crates {
        pins.insert(item.url(), item.checksum.clone());
    }
    let joined: String = pins
        .iter()
        .map(|(url, hash)| format!("{hash}  {url}\n"))
        .collect();
    fs::write(file, joined).map_err(with_path(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"
version = 3

[[package]]
name = "aho-corasick"
version = "1.1.3"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc123"

[[package]]
name = "ripgrep"
version = "14.1.1"
"#;

    #[test]
    fn registry_crates_are_listed_and_the_workspace_is_not() {
        let found = crates(LOCK).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "aho-corasick");
        assert_eq!(found[0].checksum, "abc123");
    }

    #[test]
    fn each_crate_becomes_a_source_under_vendor() {
        let lines = source_lines(&crates(LOCK).unwrap());
        assert!(
            lines.contains("crates/aho-corasick/aho-corasick-1.1.3.crate"),
            "{lines}"
        );
        assert!(
            lines.contains("into = \"vendor/aho-corasick-1.1.3\""),
            "{lines}"
        );
    }

    #[test]
    fn a_crate_from_elsewhere_is_an_error() {
        let lock = LOCK.replace(
            "registry+https://github.com/rust-lang/crates.io-index",
            "git+x",
        );
        assert!(crates(&lock).err().unwrap().contains("git+x"));
    }

    #[test]
    fn pinning_keeps_what_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sources.lock");
        fs::write(&file, "aaa  https://example.com/x\n").unwrap();
        pin(&crates(LOCK).unwrap(), &file).unwrap();
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("aaa  https://example.com/x\n"), "{text}");
        assert!(text.contains("abc123  https://static.crates.io"), "{text}");
    }
}
