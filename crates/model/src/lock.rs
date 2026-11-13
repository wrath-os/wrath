//! The lock: the version of each source a configuration uses, wrath's own among them. Wrath's
//! pin decides every package, since recipes live in its repository. The lock sits next to the
//! configuration as `wrath.lock` and is read like it, so the model and its hash cover exactly
//! what will run.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde::Deserialize;

pub use wrath_eval::{Ids, LOCK_FILE, WRATH_SOURCE, package_source};

/// The lock file format's version.
const FORMAT: i64 = 4;

/// The format before the lock kept uids and gids.
const FORMAT_WITHOUT_IDS: i64 = 3;

/// The format before wrath built its own packages, which also pinned each package. Those
/// entries are ignored.
const FORMAT_WITH_PACKAGES: i64 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lock {
    /// Each source by name, pinned.
    pub sources: BTreeMap<String, Source>,
    /// The uids and gids users and groups were given, kept for those since removed too.
    pub ids: Ids,
}

/// The lock file as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: i64,
    #[serde(default)]
    sources: BTreeMap<String, Source>,
    #[serde(default)]
    packages: Option<toml::Table>,
    /// Read by [`Ids::from_lock`].
    #[serde(default, rename = "ids")]
    _ids: Option<toml::Table>,
}

/// Where packages or files come from, such as a repository at a commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// What `wrath update` follows, such as `github:johnae/wrath`.
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

impl Lock {
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = toml::from_str(text).map_err(|e| e.message().to_owned())?;
        match (file.version, file.packages) {
            (FORMAT | FORMAT_WITHOUT_IDS, None) | (FORMAT_WITH_PACKAGES, _) => Ok(Self {
                sources: file.sources,
                ids: Ids::from_lock(text)?,
            }),
            (FORMAT, Some(_)) => Err("unknown field `packages`".into()),
            (other, _) => Err(format!(
                "lock format version {other} isn't supported, only {FORMAT}"
            )),
        }
    }

    /// The lock file's text, one source per table.
    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# The versions of the sources this configuration uses, written by `wrath check` and\n\
             # moved to newer ones by `wrath update`.\n",
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
        for (table, ids) in [("users", &self.ids.users), ("groups", &self.ids.groups)] {
            if ids.is_empty() {
                continue;
            }
            let _ = writeln!(out, "\n[ids.{table}]");
            for (name, id) in ids {
                let _ = writeln!(out, "{} = {id}", key(name));
            }
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
        Lock {
            sources: BTreeMap::from([
                (
                    "wrath".to_owned(),
                    Source {
                        url: "github:johnae/wrath".into(),
                        rev: Some("1d06008".into()),
                        hash: "sha256-abc".into(),
                        date: Some("2026-10-30".into()),
                    },
                ),
                (
                    "acme".to_owned(),
                    Source {
                        url: "https://example.org/acme.tar.gz".into(),
                        rev: None,
                        hash: "sha256-def".into(),
                        date: None,
                    },
                ),
            ]),
            ids: Ids {
                users: BTreeMap::from([("alice".to_owned(), 1000), ("ops.bot".to_owned(), 999)]),
                groups: BTreeMap::from([("video".to_owned(), 400)]),
            },
        }
    }

    #[test]
    fn locks_are_written_one_source_per_table_and_read_back() {
        let text = lock().to_toml();

        insta::assert_snapshot!(text, @r#"
        # The versions of the sources this configuration uses, written by `wrath check` and
        # moved to newer ones by `wrath update`.
        version = 4

        [sources.acme]
        url = "https://example.org/acme.tar.gz"
        hash = "sha256-def"

        [sources.wrath]
        url = "github:johnae/wrath"
        rev = "1d06008"
        hash = "sha256-abc"
        date = "2026-10-30"

        [ids.users]
        alice = 1000
        "ops.bot" = 999

        [ids.groups]
        video = 400
        "#);
        assert_eq!(Lock::parse(&text).unwrap(), lock());
    }

    #[test]
    fn a_lock_from_before_ids_reads_with_none() {
        let old = "version = 3\n\n[sources.wrath]\nurl = \"github:johnae/wrath\"\nhash = \"x\"\n";
        let lock = Lock::parse(old).unwrap();
        assert!(lock.ids.is_empty());
        assert_eq!(lock.sources["wrath"].hash, "x");
    }

    #[test]
    fn an_older_lock_reads_without_its_packages() {
        let old = "version = 2\n\n[sources.wrath]\nurl = \"github:johnae/wrath\"\n\
                   hash = \"sha256-abc\"\n\n[packages]\n\
                   git = { version = \"2.51.0\", id = \"/x/aaa-git\" }\n";
        let lock = Lock::parse(old).unwrap();
        assert_eq!(lock.sources["wrath"].hash, "sha256-abc");
    }

    #[test]
    fn locks_of_another_format_or_shape_are_refused() {
        let other = lock().to_toml().replace("version = 4", "version = 5");
        assert_eq!(
            Lock::parse(&other).unwrap_err(),
            "lock format version 5 isn't supported, only 4"
        );
        let typo = lock().to_toml().replace("hash =", "hsah =");
        assert!(Lock::parse(&typo).unwrap_err().contains("hsah"));
        let packages = format!("{}\n[packages]\n", lock().to_toml());
        assert!(Lock::parse(&packages).unwrap_err().contains("packages"));
    }

    #[test]
    fn package_names_say_their_source_or_come_from_wrath() {
        assert_eq!(package_source("acme#git"), ("acme", "git"));
        assert_eq!(package_source("git"), ("wrath", "git"));
    }
}
