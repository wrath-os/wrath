//! Fetching the sources a configuration includes files from, at the versions its lock pins. A
//! command that writes the lock also fetches what the lock doesn't have yet; the others only use
//! what's pinned, so they don't reach the network once a source is fetched.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use distro_backend_nixos as backend;
use distro_eval::{ConfigFs, DirFs};
use distro_model::{Lock, Source};

/// Which sources a command may fetch at their newest version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetching {
    /// None: only what the lock pins.
    Pinned,
    /// Those the lock doesn't pin yet, as `lock` and `apply` do.
    Missing,
    /// Those named, or all with none named, as `update` does, and those the lock doesn't pin.
    Newest(Vec<String>),
}

static FETCHING: OnceLock<Fetching> = OnceLock::new();

/// The sources fetched while this command ran, at the version each file was read from.
static FETCHED: Mutex<BTreeMap<String, Source>> = Mutex::new(BTreeMap::new());

/// Says which sources this command may fetch. Set once, when the command line is read.
pub fn set(fetching: Fetching) {
    FETCHING.set(fetching).expect("what to fetch is set once");
}

/// The sources files were included from, at the versions they were read.
pub fn fetched() -> BTreeMap<String, Source> {
    FETCHED
        .lock()
        .expect("the fetched sources are readable")
        .clone()
}

/// A configuration directory whose sources come from its lock.
pub struct Sourced<F> {
    inner: F,
    lock: Option<Lock>,
}

impl Sourced<DirFs> {
    pub fn dir(config: &Path) -> Self {
        Self {
            inner: DirFs::new(config),
            lock: crate::read_lock(config).ok().flatten(),
        }
    }
}

impl<F> Sourced<F> {
    /// Where to read `name`, following `url`: the lock's version, or the newest when this
    /// command may fetch it.
    fn tree(&self, name: &str, url: &str) -> Result<(Source, PathBuf), String> {
        let fetching = FETCHING.get().unwrap_or(&Fetching::Pinned);
        match choose(self.lock.as_ref(), fetching, name, url)? {
            Some(source) => backend::fetch_source(source)
                .map(|path| (source.clone(), path))
                .map_err(|message| format!("{message}; `distro update {name}` takes its newest")),
            None => backend::follow_source(url),
        }
    }
}

/// The version of the source `name`, following `url`, to read: the one `lock` pins, or `None`
/// for its newest.
fn choose<'l>(
    lock: Option<&'l Lock>,
    fetching: &Fetching,
    name: &str,
    url: &str,
) -> Result<Option<&'l Source>, String> {
    let newest = match fetching {
        Fetching::Newest(names) => names.is_empty() || names.iter().any(|n| n == name),
        Fetching::Pinned | Fetching::Missing => false,
    };
    let pinned = lock
        .and_then(|lock| lock.sources.get(name))
        .filter(|source| source.url == url && source.is_locked());
    if newest {
        return Ok(None);
    }
    match (pinned, fetching) {
        (Some(source), _) => Ok(Some(source)),
        (None, Fetching::Pinned) => Err("it isn't locked yet; `distro lock` fetches it".to_owned()),
        (None, Fetching::Missing | Fetching::Newest(_)) => Ok(None),
    }
}

impl<F: ConfigFs> ConfigFs for Sourced<F> {
    fn read(&self, path: &str) -> io::Result<String> {
        self.inner.read(path)
    }

    fn read_bytes(&self, path: &str) -> io::Result<Vec<u8>> {
        self.inner.read_bytes(path)
    }

    fn toml_files(&self, dir: &str) -> io::Result<Vec<String>> {
        self.inner.toml_files(dir)
    }

    fn display(&self, path: &str) -> String {
        self.inner.display(path)
    }

    fn source(&self, name: &str, url: &str) -> Result<Box<dyn ConfigFs>, String> {
        let (source, path) = self.tree(name, url)?;
        FETCHED
            .lock()
            .expect("the fetched sources are writable")
            .insert(name.to_owned(), source);
        Ok(Box::new(DirFs::new(path)))
    }
}

/// Adds the sources fetched for files to the lock next to `config`, when it doesn't have them at
/// those versions yet.
pub fn record(config: &Path) -> Result<(), String> {
    let fetched = fetched();
    let mut lock = crate::read_lock(config)?.unwrap_or_default();
    if fetched
        .iter()
        .all(|(name, source)| lock.sources.get(name) == Some(source))
    {
        return Ok(());
    }
    lock.sources.extend(fetched);
    crate::generations::write_lock(config, &lock)
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "github:acme/templates";

    fn lock(url: &str) -> Lock {
        let mut lock = Lock::default();
        let source = Source {
            hash: "sha256-h".into(),
            ..Source::follow(url)
        };
        lock.sources.insert("acme".into(), source);
        lock
    }

    #[test]
    fn the_lock_decides_unless_the_command_moves_the_source() {
        let locked = lock(URL);
        let pinned = Some(&locked.sources["acme"]);
        let choose = |lock, fetching: &Fetching| choose(lock, fetching, "acme", URL);

        assert_eq!(choose(Some(&locked), &Fetching::Pinned), Ok(pinned));
        assert_eq!(choose(Some(&locked), &Fetching::Missing), Ok(pinned));
        assert_eq!(
            choose(Some(&locked), &Fetching::Newest(vec!["other".into()])),
            Ok(pinned)
        );
        assert_eq!(
            choose(Some(&locked), &Fetching::Newest(vec!["acme".into()])),
            Ok(None)
        );
        assert_eq!(choose(Some(&locked), &Fetching::Newest(vec![])), Ok(None));
    }

    #[test]
    fn a_source_the_lock_doesnt_pin_is_fetched_only_by_commands_that_lock() {
        let moved = lock("github:acme/elsewhere");
        let choose = |lock, fetching: &Fetching| choose(lock, fetching, "acme", URL);

        assert_eq!(choose(None, &Fetching::Missing), Ok(None));
        assert_eq!(choose(Some(&moved), &Fetching::Missing), Ok(None));
        assert_eq!(
            choose(Some(&moved), &Fetching::Pinned),
            Err("it isn't locked yet; `distro lock` fetches it".to_owned())
        );
        assert!(choose(None, &Fetching::Pinned).is_err());
    }
}
