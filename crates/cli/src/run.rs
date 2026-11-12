//! `distro run` and `distro shell`: packages without installing them. A package is built from the
//! recipes of the distro that runs, into its store, and nothing in the configuration is written.

use std::ffi::{OsStr, OsString};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use distro_model::{DISTRO_SOURCE, Model, package_source};

use crate::project;

/// Runs `package`'s program with `args`, in place of this process: the one named like the
/// package, or its only one.
pub fn run(root: &Path, model: Model, package: &str, args: &[String]) -> Result<ExitCode, String> {
    let environment = Environment::build(root, &model, &[package.to_owned()])?;
    let name = &environment.packages[0];
    let programs = environment.programs();
    let program = if programs.contains(name) {
        name
    } else if let [only] = programs.as_slice() {
        only
    } else {
        return Err(format!(
            "`{name}` has no program `{name}`; it has: {}",
            programs.join(", ")
        ));
    };
    let program = environment.bin().join(program);
    Err(exec(environment.command(&program).args(args)))
}

/// Starts the user's shell with `packages` first on the `PATH`, in place of this process. In the
/// project at `project`, the shell has the project's packages and variables too, and says what
/// they hide from the home.
pub fn shell(
    root: &Path,
    model: Model,
    packages: &[String],
    project: Option<&Path>,
) -> Result<ExitCode, String> {
    let lookup = |name: &str| std::env::var(name).ok();
    let (mut wanted, variables): (Vec<String>, _) = match project {
        Some(_) => (
            model.packages.iter().cloned().collect(),
            project::variables(&model, lookup),
        ),
        None => (Vec::new(), Vec::new()),
    };
    wanted.extend(
        packages
            .iter()
            .filter(|p| !wanted.contains(p))
            .cloned()
            .collect::<Vec<_>>(),
    );
    if wanted.is_empty() {
        return Err("name the packages the shell should have".into());
    }
    let environment = Environment::build(root, &model, &wanted)?;
    let inherited = inherited_path();
    if let Some(dir) = project {
        keep(&environment.path, &dir.join(".distro/shell"));
        let shadowed = project::shadows(
            &environment.bin(),
            &environment.programs(),
            &variables,
            &inherited,
            lookup,
        );
        if !shadowed.is_empty() {
            eprintln!("  hides {} from your home", shadowed.join(", "));
        }
    }
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "bash".into());
    let path = path_with(&environment.bin(), &inherited)?;
    let names = environment.packages.join(" ");
    eprintln!(
        "a shell with {}; exit to leave",
        environment.packages.join(", ")
    );
    Err(exec(
        environment
            .command(Path::new(&shell))
            .envs(variables)
            .env("PATH", path)
            .env("DISTRO_SHELL", names),
    ))
}

/// Runs the services of the project at `project` with process-compose, in place of this process,
/// with the project's packages and variables, until you stop them.
pub fn up(root: &Path, model: Model, project: &Path) -> Result<ExitCode, String> {
    let compose = project::compose(&model)
        .ok_or("the project has no services; add them under `[services]`")?;
    let state = project.join(".distro");
    std::fs::create_dir_all(&state).map_err(|e| format!("{}: {e}", state.display()))?;
    let file = state.join("process-compose.yaml");
    std::fs::write(&file, compose).map_err(|e| format!("{}: {e}", file.display()))?;
    let lookup = |name: &str| std::env::var(name).ok();
    let variables = project::variables(&model, lookup);
    let mut wanted: Vec<String> = model.packages.iter().cloned().collect();
    wanted.push(PROCESS_COMPOSE.to_owned());
    let environment = Environment::build(root, &model, &wanted)?;
    keep(&environment.path, &state.join("up"));
    let bin = environment.bin();
    let path = path_with(&bin, &inherited_path())?;
    Err(exec(
        environment
            .command(&bin.join(PROCESS_COMPOSE))
            .args(["up", "--config"])
            .arg(&file)
            .envs(variables)
            .env("PATH", path),
    ))
}

/// What runs a project's services.
const PROCESS_COMPOSE: &str = "process-compose";

/// The `PATH` this process inherited.
fn inherited_path() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .collect()
}

/// A `PATH` with `dir` first, then `inherited`.
fn path_with(dir: &Path, inherited: &[PathBuf]) -> Result<OsString, String> {
    std::env::join_paths(std::iter::once(dir).chain(inherited.iter().map(PathBuf::as_path)))
        .map_err(|e| format!("cannot set PATH: {e}"))
}

fn exec(command: &mut Command) -> String {
    let program = command.get_program().to_string_lossy().into_owned();
    format!("cannot run {program}: {}", command.exec())
}

/// Packages built from distro's recipes, as named, with where each is.
pub struct Built {
    /// Each package's name, in the order asked for.
    pub packages: Vec<String>,
    /// Each package's main output, in the same order.
    pub paths: Vec<distro_build::store::StorePath>,
    /// The store they were built into, on this machine.
    pub store: PathBuf,
}

/// Builds `references`, such as `jq`, from the recipes of the distro under `root`.
pub fn built(root: &Path, model: &Model, references: &[String]) -> Result<Built, String> {
    let packages = references
        .iter()
        .map(|reference| package_name(model, reference))
        .collect::<Result<Vec<_>, _>>()?;
    let repository = root
        .join("build")
        .canonicalize()
        .map_err(|e| format!("cannot find the recipes in {}: {e}", root.display()))?;
    let names: Vec<&str> = packages.iter().map(String::as_str).collect();
    let paths = distro_build::graph::packages(&repository, &names)?;
    Ok(Built {
        packages,
        paths,
        store: repository.join("work/store"),
    })
}

/// Where packages are when programs run: their links point into it.
const STORE: &str = "/store";

/// The packages' programs, linked into one directory.
struct Environment {
    path: PathBuf,
    /// The store the packages were built into, on this machine.
    store: PathBuf,
    /// Each package's name, in the order asked for.
    packages: Vec<String>,
}

impl Environment {
    /// Builds `references` from distro's recipes and links their programs into a directory under
    /// the user's cache, one for each set of packages.
    fn build(root: &Path, model: &Model, references: &[String]) -> Result<Self, String> {
        let Built {
            packages,
            paths,
            store,
        } = built(root, model, references)?;
        let names: Vec<&str> = packages.iter().map(String::as_str).collect();
        let path = environment_dir(&packages)?;
        let bin = path.join("bin");
        if path.exists() {
            std::fs::remove_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        std::fs::create_dir_all(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
        let from = describe(model);
        for (name, built) in names.iter().zip(&paths) {
            for dir in ["bin", "sbin"] {
                let Ok(entries) = std::fs::read_dir(store.join(built.hash()).join(dir)) else {
                    continue;
                };
                for program in entries.flatten().map(|entry| entry.file_name()) {
                    let link = bin.join(&program);
                    if link.symlink_metadata().is_ok() {
                        continue;
                    }
                    let target = Path::new(STORE).join(built.hash()).join(dir).join(&program);
                    std::os::unix::fs::symlink(&target, &link)
                        .map_err(|e| format!("{}: {e}", link.display()))?;
                }
            }
            eprintln!("{name} from {from}");
        }
        Ok(Self {
            path,
            store,
            packages,
        })
    }

    fn bin(&self) -> PathBuf {
        self.path.join("bin")
    }

    fn programs(&self) -> Vec<String> {
        let mut programs: Vec<String> = std::fs::read_dir(self.bin())
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        programs.sort();
        programs
    }

    /// A command that runs `program` with the store at `/store`. Where the packages were built
    /// into another directory, as in a checkout, it runs in a mount namespace with that directory
    /// at `/store`, the way builds see it.
    fn command(&self, program: &Path) -> Command {
        let here = Path::new(STORE).canonicalize().ok();
        if here.is_some() && here == self.store.canonicalize().ok() {
            return Command::new(program);
        }
        let mut command = Command::new("bwrap");
        command
            .args(namespace_args(&self.store))
            .arg("--")
            .arg(program);
        command
    }
}

/// What to pass `bwrap` to run with this machine's directories and `store` at `/store`. `/`
/// can't take a new directory, so the root is built from its entries.
fn namespace_args(store: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir("/")
        .map(|entries| entries.flatten().collect())
        .unwrap_or_default();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        if path == Path::new(STORE) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                args.extend(["--symlink".into(), target.into(), path.into()]);
            }
        } else if kind.is_dir() {
            args.extend(["--dev-bind".into(), path.clone().into(), path.into()]);
        }
    }
    args.extend([
        OsStr::new("--dev-bind").into(),
        store.into(),
        OsStr::new(STORE).into(),
        OsStr::new("--die-with-parent").into(),
    ]);
    args
}

/// Where packages come from, as `distro 1d06008, 2026-10-30` or `this distro`.
fn describe(model: &Model) -> String {
    match model.sources.get(DISTRO_SOURCE) {
        Some(source) if source.is_locked() => {
            let rev = source.rev.as_deref().map(|rev| &rev[..rev.len().min(7)]);
            let details: Vec<&str> = [rev, source.date.as_deref()]
                .into_iter()
                .flatten()
                .collect();
            match details.as_slice() {
                [] => "distro, as the lock pins it".to_owned(),
                _ => format!("distro {}", details.join(", ")),
            }
        }
        _ => "this distro".to_owned(),
    }
}

/// The directory for the environment of `names`, under the user's cache.
fn environment_dir(names: &[String]) -> Result<PathBuf, String> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))
        .ok_or("neither XDG_CACHE_HOME nor HOME is set, so there's nowhere to keep it")?;
    let mut sorted: Vec<&str> = names.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    Ok(cache
        .join("distro/environments")
        .join(root_name(&sorted.join(" "))))
}

/// A file name for a set of package names: readable when they're plain, and with a short hash so
/// different sets never share one.
fn root_name(names: &str) -> String {
    let readable: String = names
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    // FNV-1a, stable across Rust versions, unlike the standard hasher.
    let hash = names.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{readable}-{:08x}", hash as u32)
}

/// Links `link` to the environment at `path`, so a project shows what its shell has. Failing to
/// doesn't stop it running.
fn keep(path: &Path, link: &Path) {
    let kept = link
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| match std::fs::remove_file(link) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => std::os::unix::fs::symlink(path, link),
        });
    if let Err(message) = kept {
        crate::style::warning(&format!("cannot link {}: {message}", link.display()));
    }
}

/// The recipe a package the command line names comes from. Packages come from distro's own
/// recipes; other sources of packages come later.
fn package_name(model: &Model, reference: &str) -> Result<String, String> {
    let (source, attribute) = package_source(reference);
    refuse_version(reference, attribute)?;
    if source == DISTRO_SOURCE {
        return Ok(attribute.to_owned());
    }
    if reference.contains('#') && !model.sources.contains_key(source) && !source.contains(':') {
        let known = model.sources.keys().map(String::as_str);
        return Err(no_such_source(source, attribute, known));
    }
    Err(format!(
        "`{reference}`: packages come from distro's recipes for now, and other sources of \
         packages come later"
    ))
}

/// Refuses a package that picks a version, such as `jq@1.7`.
pub(crate) fn refuse_version(reference: &str, attribute: &str) -> Result<(), String> {
    if attribute.contains('@') {
        return Err(format!(
            "`{reference}`: picking a version needs the version index, which doesn't exist yet"
        ));
    }
    Ok(())
}

/// The error for a package from `source`, which isn't one of the `declared` sources or a URL.
pub(crate) fn no_such_source<'a>(
    source: &str,
    attribute: &str,
    declared: impl Iterator<Item = &'a str>,
) -> String {
    let mut known: Vec<&str> = declared.collect();
    if !known.contains(&DISTRO_SOURCE) {
        known.insert(0, DISTRO_SOURCE);
    }
    format!(
        "there's no source `{source}`; the configuration has {}, or give a URL such as \
         `github:owner/repo#{attribute}`",
        known.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use distro_model::Source;

    fn with_acme() -> Model {
        let mut model = Model::default();
        model
            .sources
            .insert("acme".into(), Source::follow("github:acme/recipes"));
        model
    }

    #[test]
    fn packages_come_from_distros_recipes() {
        let model = with_acme();
        assert_eq!(package_name(&model, "jq").unwrap(), "jq");
        assert_eq!(package_name(&model, "distro#jq").unwrap(), "jq");
        for other in ["acme#jq", "github:jqlang/jq#jq"] {
            assert!(
                package_name(&model, other)
                    .unwrap_err()
                    .contains("other sources of packages come later"),
                "{other}"
            );
        }
    }

    #[test]
    fn unknown_sources_and_versions_are_refused() {
        let model = with_acme();
        assert_eq!(
            package_name(&model, "acem#jq").unwrap_err(),
            "there's no source `acem`; the configuration has distro, acme, or give a URL such \
             as `github:owner/repo#jq`"
        );
        assert!(
            package_name(&model, "jq@1.7")
                .unwrap_err()
                .contains("version index")
        );
    }

    #[test]
    fn each_set_of_packages_has_its_own_directory() {
        assert_eq!(root_name("jq ripgrep"), root_name("jq ripgrep"));
        assert_ne!(root_name("jq ripgrep"), root_name("jq_ripgrep"));
        assert!(root_name("jq ripgrep").starts_with("jq_ripgrep-"));
    }

    #[test]
    fn packages_say_which_distro_they_come_from() {
        let mut model = Model::default();
        assert_eq!(describe(&model), "this distro");
        model.sources.insert(
            DISTRO_SOURCE.into(),
            Source {
                url: "github:johnae/distro".into(),
                rev: Some("1d060086d60c".into()),
                hash: "sha256-abc".into(),
                date: Some("2026-10-30".into()),
            },
        );
        assert_eq!(describe(&model), "distro 1d06008, 2026-10-30");
    }

    #[test]
    fn the_store_is_mounted_where_programs_expect_it() {
        let args: Vec<String> = namespace_args(Path::new("/srv/work/store"))
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let at = args
            .iter()
            .position(|arg| arg == "/srv/work/store")
            .unwrap();
        assert_eq!(args[at - 1], "--dev-bind");
        assert_eq!(args[at + 1], "/store");
        assert!(args.windows(3).any(|w| w == ["--dev-bind", "/tmp", "/tmp"]));
    }
}
