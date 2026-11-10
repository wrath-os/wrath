//! Reads a `Cargo.lock` into what a recipe needs: one source per crate, pinned by the lock's hash.

use std::collections::{BTreeMap, BTreeSet};
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
const GIT: &str = "git+";

/// A crate from the registry.
#[derive(Debug, PartialEq, Eq)]
pub struct Crate {
    pub name: String,
    pub version: String,
    pub checksum: String,
}

impl Crate {
    pub(crate) fn url(&self) -> String {
        format!(
            "https://static.crates.io/crates/{0}/{0}-{1}.crate",
            self.name, self.version
        )
    }
}

/// The registry crates in the text of a `Cargo.lock`. The workspace's own have no source, and
/// those from git come with their repositories, from [`git_repositories`].
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
            Some(git) if git.starts_with(GIT) => {}
            Some(other) => return Err(format!("`{}` comes from {other}", package.name)),
        }
    }
    Ok(crates)
}

/// A git repository a `Cargo.lock` takes crates from, at the commit it pins.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GitRepository {
    url: String,
    commit: String,
}

impl GitRepository {
    /// An archive of the commit, which GitHub serves for any commit of a repository.
    pub(crate) fn archive(&self) -> Result<String, String> {
        let path = self
            .url
            .strip_prefix("https://github.com/")
            .ok_or_else(|| {
                format!(
                    "crates from {} can't be fetched, only from GitHub",
                    self.url
                )
            })?;
        let path = path.strip_suffix(".git").unwrap_or(path);
        Ok(format!(
            "https://github.com/{path}/archive/{}.tar.gz",
            self.commit
        ))
    }

    /// Where the archive is unpacked: `git/` and the repository's name.
    pub(crate) fn dir(&self) -> String {
        let name = self.url.rsplit('/').next().unwrap_or(&self.url);
        format!("git/{}", name.strip_suffix(".git").unwrap_or(name))
    }
}

/// Each git repository the lock takes crates from, once. A source looks like
/// `git+https://github.com/o/r.git?rev=abc#abc`, with the commit after the `#`.
pub fn git_repositories(lock: &str) -> Result<Vec<GitRepository>, String> {
    let lock: Lock = toml::from_str(lock).map_err(|e| format!("Cargo.lock: {e}"))?;
    let repositories: BTreeSet<GitRepository> = lock
        .package
        .iter()
        .filter_map(|package| package.source.as_deref()?.strip_prefix(GIT))
        .map(|source| {
            let (url, commit) = source
                .split_once('#')
                .ok_or_else(|| format!("{GIT}{source} names no commit"))?;
            let url = url.split_once('?').map_or(url, |(url, _)| url);
            Ok(GitRepository {
                url: url.to_owned(),
                commit: commit.to_owned(),
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(repositories.into_iter().collect())
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

    const GIT: &str = r#"
[[package]]
name = "smithay"
version = "0.7.0"
source = "git+https://github.com/Smithay/smithay.git?rev=ff5fa7d#ff5fa7d"

[[package]]
name = "smithay-drm-extras"
version = "0.1.0"
source = "git+https://github.com/Smithay/smithay.git?rev=ff5fa7d#ff5fa7d"
"#;

    #[test]
    fn a_crate_from_elsewhere_is_an_error() {
        let lock = LOCK.replace(
            "registry+https://github.com/rust-lang/crates.io-index",
            "sparse+x",
        );
        assert!(crates(&lock).err().unwrap().contains("sparse+x"));
    }

    #[test]
    fn crates_from_git_are_left_out_of_the_registry_crates() {
        let found = crates(&format!("{LOCK}{GIT}")).unwrap();
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn a_git_repository_is_one_archive_of_its_commit_however_many_crates_it_has() {
        let repositories = git_repositories(&format!("{LOCK}{GIT}")).unwrap();
        let sources: Vec<(String, String)> = repositories
            .iter()
            .map(|r| (r.archive().unwrap(), r.dir()))
            .collect();
        assert_eq!(
            sources,
            [(
                "https://github.com/Smithay/smithay/archive/ff5fa7d.tar.gz".to_owned(),
                "git/smithay".to_owned()
            )]
        );
    }

    #[test]
    fn a_git_repository_outside_github_is_an_error() {
        let lock = GIT.replace("https://github.com/Smithay", "https://example.com/Smithay");
        let repositories = git_repositories(&lock).unwrap();
        let error = repositories[0].archive().unwrap_err();
        assert!(error.contains("example.com"), "{error}");
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
