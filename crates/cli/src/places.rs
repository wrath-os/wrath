//! Where wrath finds grapes, the set it builds from (its recipes, styles, pins and modules), and
//! where it builds: the store and its records, which are the tool's (0036).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use wrath_model::{Lock, Source};

/// The version of the host API, what modules and styles call, that this wrath provides. A set
/// says the one it needs in its `set.toml`.
pub const HOST_API: u32 = 1;

/// The file a set says what it is in.
const SET_FILE: &str = "set.toml";

/// The variable naming a checkout of grapes to build from when the command gives none.
pub const GRAPES_VARIABLE: &str = "WRATH_GRAPES";

/// Where a configuration's grapes comes from when it declares no `[sources.grapes]`. Its lock
/// pins it the first time a command reads it, as it does any source.
pub const GRAPES_URL: &str = "github:wrath-os/grapes";

/// The name grapes has among a configuration's sources and in its lock.
pub const GRAPES_SOURCE: &str = "grapes";

/// The system running on this machine, with the grapes it was built from at [`GRAPES_LINK`] and
/// its model at [`RUNNING_MODEL`].
const RUNNING_SYSTEM: &str = "/run/system";
const GRAPES_LINK: &str = "grapes";
const RUNNING_MODEL: &str = "etc/wrath/model.json";

struct Places {
    given: Option<PathBuf>,
    config: Option<PathBuf>,
    /// Decided the first time a command needs grapes, so commands that don't, such as a
    /// rollback, never fetch it.
    grapes: OnceLock<Result<PathBuf, String>>,
    work: PathBuf,
}

static PLACES: OnceLock<Places> = OnceLock::new();

/// Says, once per command, where grapes and the work are for wrath at `root`: `grapes` given on
/// the command line as `--source grapes=DIR`, or in `WRATH_GRAPES`, or else the one the
/// configuration at `config` builds on.
pub fn init(root: &Path, grapes: Option<PathBuf>, config: Option<&Path>, named: bool) {
    let cwd = std::env::current_dir().unwrap_or_default();
    let places = Places {
        given: grapes.or_else(|| std::env::var_os(GRAPES_VARIABLE).map(PathBuf::from)),
        config: building_for(config, named, &cwd),
        grapes: OnceLock::new(),
        work: root.join("build/work"),
    };
    // Tests may decide more than once; the first decision stands.
    let _ = PLACES.set(places);
}

/// The configuration whose grapes a command builds from: `config`, unless the command runs in a
/// checkout of grapes and wasn't told a configuration (`named`), as when working on grapes on a
/// machine whose own configuration is found anyway.
fn building_for(config: Option<&Path>, named: bool, cwd: &Path) -> Option<PathBuf> {
    config
        .filter(|_| named || here(cwd).is_err())
        .map(Path::to_path_buf)
}

/// Where grapes is, the set this command builds from.
pub fn grapes() -> Result<PathBuf, String> {
    let places = PLACES
        .get()
        .ok_or_else(|| "where grapes is hasn't been decided".to_owned())?;
    places
        .grapes
        .get_or_init(|| resolve(places.given.as_deref(), places.config.as_deref()))
        .clone()
}

/// Whether this command builds from a directory given for it, in place of the pinned grapes.
pub fn given() -> bool {
    PLACES.get().is_some_and(|places| places.given.is_some())
}

/// Where this command builds: the store and the builder's records.
pub fn work() -> PathBuf {
    PLACES
        .get()
        .map_or_else(|| PathBuf::from("build/work"), |places| places.work.clone())
}

/// Where grapes comes from for one command.
#[derive(Debug, PartialEq, Eq)]
enum Origin {
    /// A directory given for this command.
    Given(PathBuf),
    /// The configuration's grapes, followed at this URL, at the version its lock pins.
    Configured(String),
    /// The checkout of grapes the command runs in, when there's no configuration.
    Here,
}

fn origin(given: Option<&Path>, config: Option<&Path>) -> Origin {
    match (given, config) {
        (Some(dir), _) => Origin::Given(dir.to_path_buf()),
        (None, Some(config)) => Origin::Configured(
            wrath_eval::declared_source(&wrath_eval::DirFs::new(config), None, GRAPES_SOURCE)
                .unwrap_or_else(|| GRAPES_URL.to_owned()),
        ),
        (None, None) => Origin::Here,
    }
}

fn resolve(given: Option<&Path>, config: Option<&Path>) -> Result<PathBuf, String> {
    let dir = match origin(given, config) {
        Origin::Given(dir) => {
            if config.is_some_and(pins_grapes) {
                crate::style::note(&format!(
                    "grapes from {}, not the version the lock pins",
                    dir.display()
                ));
            }
            given_dir(dir)?
        }
        Origin::Configured(url) => {
            fetched(config.expect("a configured grapes has a config"), &url)?
        }
        Origin::Here => here(&std::env::current_dir().unwrap_or_default())?,
    };
    check(&dir)?;
    wrath_build::seed::set_pin(dir.join("seed.pin"));
    Ok(dir)
}

/// Whether the lock of the configuration at `config` pins a version of grapes, which a given
/// directory then builds in place of.
fn pins_grapes(config: &Path) -> bool {
    crate::read_lock(config)
        .ok()
        .flatten()
        .is_some_and(|lock| lock.sources.contains_key(GRAPES_SOURCE))
}

/// Keeps grapes' pin in `sources`, a lock being written from what the configuration names. One
/// that doesn't declare grapes still builds on it, so its pin is the one this command read, or
/// else the one the lock had, `previous`, as wrath's own is kept.
pub fn keep_pin(sources: &mut BTreeMap<String, Source>, previous: Option<&Lock>) {
    let read = crate::fetch::fetched().remove(GRAPES_SOURCE);
    let had = previous.and_then(|lock| lock.sources.get(GRAPES_SOURCE).cloned());
    if let Some(pin) = read.or(had) {
        sources.entry(GRAPES_SOURCE.to_owned()).or_insert(pin);
    }
}

/// Grapes at `url` for the configuration at `config`, at the version its lock pins, or its
/// newest when this command may fetch it, as includes are.
fn fetched(config: &Path, url: &str) -> Result<PathBuf, String> {
    let running = Path::new(RUNNING_SYSTEM);
    let pinned = !crate::fetch::follows_newest(GRAPES_SOURCE);
    if let Some((dir, pin)) = running_copy(config, url, running).filter(|_| pinned) {
        crate::fetch::note(GRAPES_SOURCE, pin);
        return Ok(dir);
    }
    let tree =
        wrath_eval::ConfigFs::source(&crate::fetch::Sourced::dir(config), GRAPES_SOURCE, url)
            .map_err(|e| {
                let mut message = format!("cannot fetch grapes from {url}: {e}");
                if pinned && whole_set(&running.join(GRAPES_LINK)) {
                    message.push_str(&format!(
                        "; to use the grapes the running system was built with instead, give \
                         `--source grapes={}`",
                        running.join(GRAPES_LINK).display()
                    ));
                } else {
                    message
                        .push_str("; to build from a checkout, give it with `--source grapes=DIR`");
                }
                message
            })?;
    tree.dir()
        .map(Path::to_path_buf)
        .ok_or_else(|| "grapes isn't on disk".to_owned())
}

/// The grapes the system at `running` carries, when it's the version the lock next to `config`
/// pins for `url`: the one it was built with, as its model records, so nothing needs fetching.
/// Any other version is never used in its place.
fn running_copy(config: &Path, url: &str, running: &Path) -> Option<(PathBuf, Source)> {
    let lock = crate::read_lock(config).ok()??;
    let pin = lock.sources.get(GRAPES_SOURCE)?;
    let model = crate::read_model(&running.join(RUNNING_MODEL)).ok()?;
    let built_with = model.sources.get(GRAPES_SOURCE)?;
    let dir = running.join(GRAPES_LINK);
    let same = pin.url == url && pin.is_locked() && built_with.url == pin.url;
    (same && built_with.hash == pin.hash && whole_set(&dir)).then(|| (dir, pin.clone()))
}

/// Whether `dir` has everything of a set a command can build with: a copy missing part of it
/// would fail later with errors about what's missing rather than about grapes.
fn whole_set(dir: &Path) -> bool {
    [SET_FILE, "seed.pin", "sources.lock"]
        .iter()
        .all(|file| dir.join(file).is_file())
        && ["recipes", "styles", "modules"]
            .iter()
            .all(|sub| dir.join(sub).is_dir())
}

fn given_dir(dir: PathBuf) -> Result<PathBuf, String> {
    if dir.join("recipes").is_dir() {
        Ok(dir)
    } else {
        Err(format!(
            "{} has no recipes, so it isn't grapes",
            dir.display()
        ))
    }
}

fn here(cwd: &Path) -> Result<PathBuf, String> {
    if cwd.join(SET_FILE).is_file() && cwd.join("recipes").is_dir() {
        return Ok(cwd.to_path_buf());
    }
    Err(
        "there's no grapes here to build from; give one with `--source grapes=DIR`, such as a \
         checkout of it, or run from inside one"
            .to_owned(),
    )
}

/// That the set at `dir` says it needs the host API this wrath has.
fn check(dir: &Path) -> Result<(), String> {
    let file = dir.join(SET_FILE);
    let text = std::fs::read_to_string(&file).map_err(|e| {
        format!(
            "{}: {e}; a set says the version of wrath's host API it needs there",
            file.display()
        )
    })?;
    let api = needed_api(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    if api == HOST_API {
        Ok(())
    } else {
        Err(format!(
            "{} needs version {api} of wrath's host API, and this wrath has version {HOST_API}; \
             use a wrath that has it, or a version of the set made for this one",
            dir.display()
        ))
    }
}

fn needed_api(text: &str) -> Result<u32, String> {
    let doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    match doc.get("api") {
        Some(item) => item
            .as_integer()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| "`api` is a number, such as `api = 1`".to_owned()),
        None => Err("it says no `api`, the version of wrath's host API it needs".into()),
    }
}

/// The stand-in for grapes' modules when there's no grapes, which no directory is.
pub const NO_MODULES: &str = "<no grapes>/modules";

/// The modules a configuration builds on when its platform names none of its own: grapes'.
/// Without grapes, a directory that isn't there, so loading them says why with [`grapes`]'s
/// error: no other version is used in its place.
pub fn modules() -> PathBuf {
    grapes().map_or_else(|_| PathBuf::from(NO_MODULES), |dir| dir.join("modules"))
}

/// A checkout of grapes for tests that run its modules: `WRATH_GRAPES`, or one beside wrath's.
#[cfg(test)]
pub(crate) fn checkout_for_tests() -> PathBuf {
    std::env::var_os(GRAPES_VARIABLE).map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../grapes"),
        PathBuf::from,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(dir: &Path, api: &str) {
        std::fs::create_dir_all(dir.join("recipes")).unwrap();
        std::fs::write(dir.join(SET_FILE), api).unwrap();
    }

    fn config(system: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("system.toml"), system).unwrap();
        dir
    }

    #[test]
    fn a_directory_given_for_the_command_wins_over_the_configuration() {
        let config = config("[sources.grapes]\nurl = \"github:acme/grapes\"\n");
        let given = Path::new("/checkouts/grapes");
        assert_eq!(
            origin(Some(given), Some(config.path())),
            Origin::Given(given.to_path_buf())
        );
    }

    #[test]
    fn a_configuration_builds_on_the_grapes_it_declares() {
        let config = config("[sources.grapes]\nurl = \"github:acme/grapes\"\n");
        assert_eq!(
            origin(None, Some(config.path())),
            Origin::Configured("github:acme/grapes".into())
        );
    }

    #[test]
    fn a_configuration_declaring_no_grapes_builds_on_wraths() {
        let config = config("[system]\nhostname = \"a\"\n");
        assert_eq!(
            origin(None, Some(config.path())),
            Origin::Configured(GRAPES_URL.into())
        );
    }

    #[test]
    fn without_a_configuration_grapes_is_the_checkout_the_command_runs_in() {
        assert_eq!(origin(None, None), Origin::Here);
        let dir = tempfile::tempdir().unwrap();
        set(dir.path(), "api = 1\n");
        assert_eq!(here(dir.path()).unwrap(), dir.path());
        let elsewhere = tempfile::tempdir().unwrap();
        let error = here(elsewhere.path()).unwrap_err();
        assert!(error.contains("--source grapes=DIR"), "{error}");
    }

    #[test]
    fn a_given_directory_is_noted_only_where_the_lock_pins_another_grapes() {
        let config = config("[system]\nhostname = \"a\"\n");
        assert!(!pins_grapes(config.path()));
        let mut lock = wrath_model::Lock::default();
        lock.sources.insert(
            "grapes".into(),
            wrath_model::Source {
                url: GRAPES_URL.into(),
                rev: Some("0123456".into()),
                hash: "sha256-x".into(),
                date: None,
            },
        );
        std::fs::write(config.path().join("wrath.lock"), lock.to_toml()).unwrap();
        assert!(pins_grapes(config.path()));
    }

    fn pin(url: &str, rev: &str) -> Source {
        Source {
            url: url.into(),
            rev: Some(rev.into()),
            hash: format!("sha256-{rev}"),
            date: None,
        }
    }

    #[test]
    fn a_lock_written_again_keeps_grapes_pin_unless_the_configuration_declares_one() {
        let mut previous = Lock::default();
        previous
            .sources
            .insert(GRAPES_SOURCE.into(), pin(GRAPES_URL, "aaaaaaa"));

        let mut sources = BTreeMap::new();
        keep_pin(&mut sources, Some(&previous));
        assert_eq!(sources[GRAPES_SOURCE], pin(GRAPES_URL, "aaaaaaa"));

        let declared = pin("github:acme/grapes", "bbbbbbb");
        let mut sources = BTreeMap::from([(GRAPES_SOURCE.to_owned(), declared.clone())]);
        keep_pin(&mut sources, Some(&previous));
        assert_eq!(sources[GRAPES_SOURCE], declared);

        let mut sources = BTreeMap::new();
        keep_pin(&mut sources, None);
        assert!(sources.is_empty());
    }

    #[test]
    fn a_grapes_checkout_the_command_runs_in_wins_unless_a_configuration_is_named() {
        let checkout = tempfile::tempdir().unwrap();
        set(checkout.path(), "api = 1\n");
        let elsewhere = tempfile::tempdir().unwrap();
        let config = Path::new("/etc/wrath");
        assert_eq!(building_for(Some(config), false, checkout.path()), None);
        assert_eq!(
            building_for(Some(config), true, checkout.path()).as_deref(),
            Some(config)
        );
        assert_eq!(
            building_for(Some(config), false, elsewhere.path()).as_deref(),
            Some(config)
        );
    }

    /// A running system built with grapes at `built_with`, or from a checkout with `None`, and a
    /// configuration whose lock pins `pinned`.
    fn machine(
        built_with: Option<Source>,
        pinned: Source,
    ) -> (tempfile::TempDir, tempfile::TempDir) {
        let running = tempfile::tempdir().unwrap();
        let mut model = wrath_model::Model::default();
        model
            .sources
            .extend(built_with.map(|s| (GRAPES_SOURCE.to_owned(), s)));
        let file = running.path().join(RUNNING_MODEL);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, model.canonical()).unwrap();
        let copy = running.path().join(GRAPES_LINK);
        set(&copy, "api = 1\n");
        for sub in ["styles", "modules"] {
            std::fs::create_dir_all(copy.join(sub)).unwrap();
        }
        for file in ["seed.pin", "sources.lock"] {
            std::fs::write(copy.join(file), "").unwrap();
        }
        let config = config("[system]\nhostname = \"a\"\n");
        let mut lock = Lock::default();
        lock.sources.insert(GRAPES_SOURCE.into(), pinned);
        std::fs::write(config.path().join("wrath.lock"), lock.to_toml()).unwrap();
        (running, config)
    }

    #[test]
    fn the_grapes_a_system_carries_is_used_only_where_the_lock_pins_that_version() {
        let pin = pin(GRAPES_URL, "aaaaaaa");
        let (running, config) = machine(Some(pin.clone()), pin.clone());
        let (dir, used) = running_copy(config.path(), GRAPES_URL, running.path()).unwrap();
        assert_eq!(dir, running.path().join(GRAPES_LINK));
        assert_eq!(used, pin);

        let (running, config) = machine(Some(pin.clone()), self::pin(GRAPES_URL, "bbbbbbb"));
        assert_eq!(
            running_copy(config.path(), GRAPES_URL, running.path()),
            None
        );

        let (running, config) = machine(None, pin.clone());
        assert_eq!(
            running_copy(config.path(), GRAPES_URL, running.path()),
            None
        );

        let (running, config) = machine(Some(pin.clone()), pin.clone());
        assert_eq!(
            running_copy(config.path(), "github:acme/grapes", running.path()),
            None
        );

        let (running, config) = machine(Some(pin.clone()), pin);
        std::fs::remove_dir(running.path().join(GRAPES_LINK).join("modules")).unwrap();
        assert_eq!(
            running_copy(config.path(), GRAPES_URL, running.path()),
            None
        );
    }

    #[test]
    fn a_given_directory_without_recipes_isnt_grapes() {
        let dir = tempfile::tempdir().unwrap();
        let error = given_dir(dir.path().to_path_buf()).unwrap_err();
        assert!(error.contains("has no recipes"), "{error}");
    }

    #[test]
    fn a_set_needing_another_host_api_says_so_before_anything_runs() {
        let dir = tempfile::tempdir().unwrap();
        set(dir.path(), &format!("api = {}\n", HOST_API + 1));
        let error = check(dir.path()).unwrap_err();
        assert!(
            error.contains(&format!("needs version {}", HOST_API + 1)),
            "{error}"
        );
        set(dir.path(), &format!("api = {HOST_API}\n"));
        assert!(check(dir.path()).is_ok());
        set(dir.path(), "name = \"grapes\"\n");
        assert!(check(dir.path()).unwrap_err().contains("says no `api`"));
        std::fs::remove_file(dir.path().join(SET_FILE)).unwrap();
        assert!(check(dir.path()).unwrap_err().contains("host API it needs"));
    }
}
