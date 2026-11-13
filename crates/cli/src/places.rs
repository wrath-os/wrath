//! Where wrath finds grapes, the set it builds from (its recipes, styles, pins and modules), and
//! where it builds: the store and its records, which are the tool's (0036).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The version of the host API, what modules and styles call, that this wrath provides. A set
/// says the one it needs in its `set.toml`.
pub const HOST_API: u32 = 1;

/// The file a set says what it is in.
const SET_FILE: &str = "set.toml";

/// The variable naming a checkout of grapes to build from when the command gives none.
pub const GRAPES_VARIABLE: &str = "WRATH_GRAPES";

struct Places {
    grapes: Result<PathBuf, String>,
    work: PathBuf,
}

static PLACES: OnceLock<Places> = OnceLock::new();

/// Decides, once per command, where grapes and the work are for wrath at `root`, with `grapes`
/// given on the command line as `--source grapes=DIR`, for the configuration at `config`.
pub fn init(root: &Path, grapes: Option<PathBuf>, config: Option<&Path>) {
    let cwd = std::env::current_dir().unwrap_or_default();
    // A checkout of grapes beside wrath's, for working on both and for their tests, which devenv
    // names in `WRATH_GRAPES`; `--source` still decides for one command.
    let grapes = grapes.or_else(|| std::env::var_os(GRAPES_VARIABLE).map(PathBuf::from));
    let grapes = match grapes {
        Some(dir) => find(root, Some(dir), &cwd),
        None => config
            .and_then(pinned)
            .unwrap_or_else(|| find(root, None, &cwd)),
    };
    let grapes = grapes.and_then(|dir| check(&dir).map(|()| dir));
    if let Ok(dir) = &grapes {
        wrath_build::seed::set_pin(dir.join("seed.pin"));
    }
    let places = Places {
        grapes,
        work: root.join("build/work"),
    };
    // Tests may decide more than once; the first decision stands.
    let _ = PLACES.set(places);
}

/// Where grapes is, the set this command builds from.
pub fn grapes() -> Result<PathBuf, String> {
    PLACES
        .get()
        .ok_or_else(|| "where grapes is hasn't been decided".to_owned())?
        .grapes
        .clone()
}

/// Where this command builds: the store and the builder's records.
pub fn work() -> PathBuf {
    PLACES
        .get()
        .map_or_else(|| PathBuf::from("build/work"), |places| places.work.clone())
}

/// Grapes as the configuration at `config` declares it, `[sources.grapes]`, fetched at the
/// version its lock pins, as includes are. `None` when it declares none.
fn pinned(config: &Path) -> Option<Result<PathBuf, String>> {
    let url = wrath_eval::declared_source(&wrath_eval::DirFs::new(config), None, "grapes")?;
    let tree = wrath_eval::ConfigFs::source(&crate::fetch::Sourced::dir(config), "grapes", &url);
    Some(
        tree.map_err(|e| format!("cannot fetch grapes: {e}"))
            .and_then(|tree| {
                tree.dir()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| "grapes isn't on disk".to_owned())
            }),
    )
}

/// Grapes as given, or a checkout of it: wrath's own `build/` while grapes still lives there,
/// or the directory the command runs in.
fn find(root: &Path, given: Option<PathBuf>, cwd: &Path) -> Result<PathBuf, String> {
    if let Some(dir) = given {
        return if dir.join("recipes").is_dir() {
            Ok(dir)
        } else {
            Err(format!(
                "{} has no recipes, so it isn't grapes",
                dir.display()
            ))
        };
    }
    let own = root.join("build");
    if own.join("recipes").is_dir() {
        return Ok(own);
    }
    if cwd.join(SET_FILE).is_file() && cwd.join("recipes").is_dir() {
        return Ok(cwd.to_path_buf());
    }
    Err(
        "there's no grapes here to build from; give one with `--source grapes=DIR`, such as a \
         checkout of it, or run from inside one"
            .to_owned(),
    )
}

/// That the set at `dir` needs the host API this wrath has. One without a `set.toml` is wrath's
/// own `build/` from before the split, which goes with this wrath.
fn check(dir: &Path) -> Result<(), String> {
    let Ok(text) = std::fs::read_to_string(dir.join(SET_FILE)) else {
        return Ok(());
    };
    let api = needed_api(&text).map_err(|e| format!("{}: {e}", dir.join(SET_FILE).display()))?;
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

/// The modules a configuration builds on when its platform names none of its own: grapes', or
/// wrath's own `modules/` while they still live there.
pub fn modules(root: &Path) -> PathBuf {
    let dir = match grapes() {
        Ok(grapes) if grapes.join("modules").is_dir() => grapes.join("modules"),
        _ => root.join("modules"),
    };
    dir.strip_prefix(".").map(Path::to_path_buf).unwrap_or(dir)
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

    #[test]
    fn grapes_is_what_the_command_gives_or_a_checkout_it_runs_in() {
        let dir = tempfile::tempdir().unwrap();
        let (root, grapes) = (dir.path().join("wrath"), dir.path().join("grapes"));
        std::fs::create_dir_all(&root).unwrap();
        set(&grapes, "api = 1\n");
        assert_eq!(find(&root, Some(grapes.clone()), &root).unwrap(), grapes);
        assert_eq!(find(&root, None, &grapes).unwrap(), grapes);
        let error = find(&root, None, &root).unwrap_err();
        assert!(error.contains("--source grapes=DIR"), "{error}");
        let error = find(&root, Some(root.clone()), &root).unwrap_err();
        assert!(error.contains("has no recipes"), "{error}");
    }

    #[test]
    fn a_checkout_with_grapes_in_its_build_directory_builds_from_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("build/recipes")).unwrap();
        assert_eq!(
            find(dir.path(), None, dir.path()).unwrap(),
            dir.path().join("build")
        );
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
    }
}
