use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use distro_backend_nixos as backend;
use distro_check::{Outcome, RunContext, SIGNOFF_DIR, Scenario, Suite};
use distro_eval::{Evaluation, OptionPath};
use distro_model::{LOCK_FILE, Lock, Model};

mod bench;
mod checks;
mod edit;
mod generations;
mod run;
mod secret;
mod system;
mod templates;

/// How many times `dev test --determinism` evaluates each configuration.
const DETERMINISM_RUNS: u64 = 5;

/// The system's configuration, which commands act on unless told otherwise.
const SYSTEM_CONFIG: &str = "/etc/distro";

#[derive(Parser)]
#[command(name = "distro", version, about = "Declarative system configuration")]
struct Cli {
    /// The configuration to act on [default: the system's, in /etc/distro]
    #[arg(short = 'C', long = "config", global = true, value_name = "DIR")]
    config: Option<PathBuf>,
    /// Directory of modules [default: modules under the root]
    #[arg(long, global = true, hide = true)]
    modules: Option<PathBuf>,
    /// Repository root containing `acceptance/` and `factory/` [default: found, see `find_root`]
    #[arg(long, global = true, hide = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show what's running and whether it matches the configuration
    Status,
    /// Show what would change going to the configuration from the running system
    Plan {
        /// A configuration, or a `model.json`, to compare against instead of the running system
        #[arg(long, conflicts_with = "fresh")]
        against: Option<PathBuf>,
        /// Compare against nothing, so everything is new
        #[arg(long)]
        fresh: bool,
    },
    /// Switch to the configuration, locking its packages first, and say when it takes effect
    Apply {
        /// A system that is already built, such as one `build` made for this machine elsewhere
        system: Option<PathBuf>,
    },
    /// Build the system without switching to it, and print where it is
    Build {
        #[arg(long, value_enum, default_value = "system")]
        target: generations::BuildTarget,
    },
    /// Show the system's generations
    History,
    /// Switch back to the previous generation
    Rollback,
    /// Run a package's program without installing it
    Run {
        /// The package, such as `jq`, `stable#jq` or `github:jqlang/jq#jq`
        package: String,
        /// Arguments for the program, after `--`
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Start a shell with packages, without installing them
    Shell {
        /// The packages, such as `jq`, `stable#jq` or `github:jqlang/jq#jq`
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Add packages to the configuration and lock them
    Add {
        /// The packages, such as `jq`, `stable#jq` or `github:jqlang/jq#jq`
        #[arg(required = true)]
        packages: Vec<String>,
        /// Add them to this user's packages instead of the system's
        #[arg(long, value_name = "USER")]
        home: Option<String>,
    },
    /// Remove packages from the configuration
    Remove {
        /// The packages, as the configuration names them
        #[arg(required = true)]
        packages: Vec<String>,
        /// Remove them from this user's packages instead of the system's
        #[arg(long, value_name = "USER")]
        home: Option<String>,
    },
    /// Lock what isn't locked yet, moving nothing that is
    Lock,
    /// Move sources to their newest versions and lock them
    Update {
        /// Sources to update, such as `nixpkgs` [default: all of them]
        sources: Vec<String>,
    },
    /// Show each source: what it follows, where it's pinned and how old that is
    Sources,
    /// Check the configuration for errors, without building it
    Check,
    /// Show where option values come from
    Why {
        /// Explain every option
        #[arg(long, conflicts_with = "option", required_unless_present = "option")]
        all: bool,
        /// The option, or table of options, to explain
        option: Option<String>,
    },
    /// Edit secrets and encrypt them to the machine and the people who manage it
    Secret {
        #[command(subcommand)]
        command: secret::SecretCommand,
    },
    /// Commands for working on distro itself
    Dev {
        #[command(subcommand)]
        command: DevCommand,
    },
    /// Fill in files with secrets during activation
    #[command(hide = true)]
    FillTemplates {
        /// The files to fill in, as JSON: each name to its parts
        spec: PathBuf,
        /// Where the decrypted secrets are
        secrets: PathBuf,
        /// The directory to write the files to
        out: PathBuf,
    },
}

#[derive(Subcommand)]
enum DevCommand {
    /// Run scenario files, or compare bad configs with their golden errors
    Test {
        /// Scenario files to run, or directories of bad configs to compare with their golden errors
        #[arg(required_unless_present = "determinism")]
        files: Vec<PathBuf>,
        #[command(flatten)]
        test: TestOptions,
    },
    /// Run the acceptance scenarios and show which pass
    Status {
        /// Only run the scenarios of this milestone, for example `m2`
        #[arg(long)]
        milestone: Option<String>,
    },
    /// Measure how long `plan` takes with many synthetic modules
    Bench {
        /// How many synthetic service modules to add
        #[arg(long, default_value_t = 150)]
        synthetic: usize,
        /// How many times to measure
        #[arg(long, default_value_t = 5)]
        runs: usize,
        /// The most the median plan may take, in milliseconds
        #[arg(long, default_value_t = 500)]
        budget_ms: u64,
    },
}

#[derive(clap::Args)]
struct TestOptions {
    /// Check that the modules in this directory render the fixture configs byte for byte the
    /// same across fresh interpreters and module orders
    #[arg(long, conflicts_with = "files")]
    determinism: Option<PathBuf>,
    /// Configurations for --determinism
    #[arg(long, default_value = "fixtures/configs")]
    configs: PathBuf,
    /// Write the actual errors of bad configs as their expected errors
    #[arg(long)]
    bless: bool,
    /// Run VM scenarios against this built image instead of building one
    #[arg(long)]
    image: Option<PathBuf>,
    /// After a failure, run the printed reproduction and report whether it fails the same way
    #[arg(long)]
    run_reproduction: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = find_root(cli.root, std::env::var_os("DISTRO_ROOT").map(PathBuf::from));
    let modules = modules_dir(&root, cli.modules);
    let root = root.as_path();
    let config = || config_dir(cli.config.as_deref());
    let result = match cli.command {
        Command::Status => config().and_then(|config| match model(&config, &modules) {
            Some(model) => generations::status(&model),
            None => Ok(ExitCode::FAILURE),
        }),
        Command::Plan { against, fresh } => {
            let against = match (against, fresh) {
                (_, true) => Against::Nothing,
                (Some(path), false) => Against::Path(path),
                (None, false) => Against::Running,
            };
            config().and_then(|config| plan(&config, against, &modules))
        }
        Command::Apply {
            system: Some(system),
        } => generations::activate(&system),
        Command::Apply { system: None } => {
            config().and_then(|config| match model(&config, &modules) {
                Some(model) => generations::apply(root, &config, model),
                None => Ok(ExitCode::FAILURE),
            })
        }
        Command::Build { target } => config().and_then(|config| match model(&config, &modules) {
            Some(model) => generations::build(root, &model, target).map(|path| {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }),
            None => Ok(ExitCode::FAILURE),
        }),
        Command::History => generations::history(),
        Command::Rollback => generations::rollback(),
        Command::Run { package, args } => package_model(cli.config.as_deref(), &modules)
            .and_then(|model| run::run(root, model, &package, &args)),
        Command::Shell { packages } => package_model(cli.config.as_deref(), &modules)
            .and_then(|model| run::shell(root, model, &packages)),
        Command::Add { packages, home } => config()
            .and_then(|config| edit::add(root, &config, &modules, home.as_deref(), &packages)),
        Command::Remove { packages, home } => config()
            .and_then(|config| edit::remove(root, &config, &modules, home.as_deref(), &packages)),
        Command::Lock => config().and_then(|config| lock(root, &config, &modules, None)),
        Command::Update { sources } => {
            config().and_then(|config| lock(root, &config, &modules, Some(&sources)))
        }
        Command::Sources => config().and_then(|config| match model(&config, &modules) {
            Some(model) => generations::sources(&model),
            None => Ok(ExitCode::FAILURE),
        }),
        Command::Check => config().and_then(|config| check_config(&config, &modules)),
        Command::Why { all: _, option } => {
            config().and_then(|config| why(&config, &modules, option.as_deref()))
        }
        Command::Secret { command } => {
            config().and_then(|config| secret::run(command, &config, &modules))
        }
        Command::Dev { command } => match command {
            DevCommand::Test { files, test } => dev_test(root, &files, test, &modules),
            DevCommand::Status { milestone } => status(root, milestone.as_deref()),
            DevCommand::Bench {
                synthetic,
                runs,
                budget_ms,
            } => bench::plan_time(&bench::Options {
                modules: &modules,
                synthetic,
                runs: runs.max(1),
                budget: std::time::Duration::from_millis(budget_ms),
            })
            .map(exit_code),
        },
        Command::FillTemplates { spec, secrets, out } => templates::fill(&spec, &secrets, &out),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The configuration a command acts on: the one given, or the system's. The context goes to
/// stderr first, so output always says which configuration it's about.
fn config_dir(given: Option<&Path>) -> Result<PathBuf, String> {
    let config = given.unwrap_or(Path::new(SYSTEM_CONFIG));
    if !config.is_dir() {
        return Err(match given {
            Some(_) => format!("{}: no such configuration directory", config.display()),
            None => {
                format!("there's no system configuration in {SYSTEM_CONFIG}; give one with -C DIR")
            }
        });
    }
    let kind = if given.is_some() { "config" } else { "system" };
    eprintln!("{kind} {}", config.display());
    Ok(config.to_path_buf())
}

/// The model packages come from for `run` and `shell`: the configuration's, or with none, an
/// empty one, whose packages come from the factory's nixpkgs.
fn package_model(given: Option<&Path>, modules: &Path) -> Result<Model, String> {
    if given.is_none() && !Path::new(SYSTEM_CONFIG).is_dir() {
        return Ok(Model::default());
    }
    let config = config_dir(given)?;
    model(&config, modules).ok_or_else(|| "the configuration has errors".to_owned())
}

/// Evaluates the configuration and says whether it has errors, without building anything.
fn check_config(config: &Path, modules: &Path) -> Result<ExitCode, String> {
    let Some(model) = model(config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    let packages = model.all_packages().count();
    let locked = if model.is_locked() {
        "locked"
    } else {
        "not fully locked"
    };
    println!(
        "no errors: {} options, {packages} packages ({locked}), model {}",
        model.options.len(),
        &model.hash()[..16]
    );
    Ok(ExitCode::SUCCESS)
}

fn dev_test(
    root: &Path,
    files: &[PathBuf],
    test: TestOptions,
    modules: &Path,
) -> Result<ExitCode, String> {
    match test.determinism {
        Some(dir) => checks::determinism(
            &under_root(root, &dir),
            &under_root(root, &test.configs),
            DETERMINISM_RUNS,
        )
        .map(exit_code),
        None => check(
            root,
            files,
            modules,
            test.bless,
            test.image,
            test.run_reproduction,
        ),
    }
}

fn context(root: &Path, image_override: Option<PathBuf>) -> Result<RunContext, String> {
    Ok(RunContext {
        workdir: root.to_path_buf(),
        distro_exe: std::env::current_exe()
            .map_err(|e| format!("cannot locate the distro binary: {e}"))?,
        signoff_dir: root.join("acceptance").join(SIGNOFF_DIR),
        run_root: root.join("target/distro-check/runs"),
        image_override,
    })
}

fn status(root: &Path, milestone: Option<&str>) -> Result<ExitCode, String> {
    let suite = Suite::load(&root.join("acceptance")).map_err(|e| e.to_string())?;

    if let Some(id) = milestone
        && !suite.milestones.iter().any(|m| m.id == id)
    {
        let known: Vec<_> = suite.milestones.iter().map(|m| m.id.as_str()).collect();
        return Err(format!(
            "unknown milestone `{id}`; known milestones: {}",
            known.join(", ")
        ));
    }

    let ctx = context(root, None)?;
    let results: Vec<_> = suite
        .scenarios
        .iter()
        .filter(|s| milestone.is_none_or(|id| s.milestone == id))
        .map(|s| (s, distro_check::run(s, &ctx)))
        .collect();

    print!("{}", distro_check::render(&suite.milestones, &results));
    Ok(ExitCode::SUCCESS)
}

fn check(
    root: &Path,
    files: &[PathBuf],
    modules: &Path,
    bless: bool,
    image: Option<PathBuf>,
    run_reproduction: bool,
) -> Result<ExitCode, String> {
    let (dirs, files): (Vec<&PathBuf>, Vec<&PathBuf>) = files
        .iter()
        .partition(|file| under_root(root, file).is_dir());
    let scenarios = files
        .iter()
        .map(|file| load_scenario(root, file))
        .collect::<Result<Vec<_>, _>>()?;

    let mut all_passed = true;
    for dir in dirs {
        all_passed &= checks::bad_configs(&under_root(root, dir), modules, bless)?;
    }
    let ctx = context(root, image)?;
    for scenario in &scenarios {
        let outcome = distro_check::run(scenario, &ctx);
        all_passed &= outcome.is_pass();
        print!(
            "{}",
            distro_check::render_list(&[(scenario, outcome.clone())])
        );
        if run_reproduction && let Outcome::Fail(failure) = &outcome {
            println!("        {}", reproduce(scenario, failure, &ctx));
        }
    }
    Ok(exit_code(all_passed))
}

fn exit_code(passed: bool) -> ExitCode {
    if passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// A path relative to the root, without a leading `./` so locations in messages stay short.
fn under_root(root: &Path, path: &Path) -> PathBuf {
    let joined = root.join(path);
    joined
        .strip_prefix(".")
        .map(Path::to_path_buf)
        .unwrap_or(joined)
}

/// Reruns a failed scenario against the image of the failed run and reports whether it fails
/// with the same reason.
fn reproduce(scenario: &Scenario, failure: &distro_check::Failure, ctx: &RunContext) -> String {
    let Some(reproduction) = &failure.reproduction else {
        return "reproduced: no reproduction available for this scenario".into();
    };
    let ctx = RunContext {
        image_override: Some(reproduction.image.clone()),
        ..ctx.clone()
    };
    match distro_check::run(scenario, &ctx) {
        Outcome::Fail(again) if again.reason == failure.reason => "reproduced: yes".into(),
        Outcome::Fail(again) => format!("reproduced: no, it failed differently: {}", again.reason),
        Outcome::Pass => "reproduced: no, it passed".into(),
        Outcome::Pending(reason) => format!("reproduced: no, it is pending: {reason}"),
    }
}

/// Loads a scenario file that is not part of the acceptance suite. Its ID is the file name.
fn load_scenario(root: &Path, file: &Path) -> Result<Scenario, String> {
    let path = root.join(file);
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let id = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    distro_check::parse_scenario(&path, &id, "", &text).map_err(|e| e.to_string())
}

fn modules_dir(root: &Path, modules: Option<PathBuf>) -> PathBuf {
    modules.unwrap_or_else(|| under_root(root, Path::new("modules")))
}

/// Evaluates a configuration, or prints why it can't and returns `None`.
fn evaluate(config: &Path, modules: &Path) -> Option<Evaluation> {
    system::evaluate_dir(config, modules, system::ModuleOrder::Sorted)
        .inspect_err(|errors| eprint!("{errors}"))
        .ok()
}

/// The configuration's model, with its lock when it has one, or prints why it can't and
/// returns `None`.
fn model(config: &Path, modules: &Path) -> Option<Model> {
    let model = Model::from_evaluation(&evaluate(config, modules)?);
    match read_lock(config) {
        Ok(Some(lock)) => Some(model.with_lock(&lock)),
        Ok(None) => Some(model),
        Err(message) => {
            eprintln!("error: {message}");
            None
        }
    }
}

/// The lock next to `config`, if there is one.
fn read_lock(config: &Path) -> Result<Option<Lock>, String> {
    let path = config.join(LOCK_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => Lock::parse(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

fn lock(
    root: &Path,
    config: &Path,
    modules: &Path,
    update: Option<&[String]>,
) -> Result<ExitCode, String> {
    match model(config, modules) {
        Some(model) => generations::lock(root, config, &model, update),
        None => Ok(ExitCode::FAILURE),
    }
}

/// What a plan compares a configuration against.
enum Against {
    Nothing,
    /// A configuration directory, or a model as `model.json`.
    Path(PathBuf),
    /// The running system's model, or nothing when distro didn't build it.
    Running,
}

fn plan(config: &Path, against: Against, modules: &Path) -> Result<ExitCode, String> {
    let Some(after) = model(config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    let (before, compared) = match against {
        Against::Nothing => (Model::default(), None),
        Against::Path(path) if path.is_file() => {
            (read_model(&path)?, Some(path.display().to_string()))
        }
        Against::Path(path) => match model(&path, modules) {
            Some(before) => (before, None),
            None => return Ok(ExitCode::FAILURE),
        },
        Against::Running => match backend::current_model() {
            Some(path) => (
                read_model(Path::new(path))
                    .map_err(|e| format!("{e}; plan with --against or --fresh instead"))?,
                Some("the running system".to_owned()),
            ),
            None => (Model::default(), None),
        },
    };
    println!("model {}", &after.hash()[..16]);
    if let Some(compared) = compared {
        println!("against {compared} (model {})", &before.hash()[..16]);
    }
    print!("{}", distro_model::plan(&before, &after));
    Ok(ExitCode::SUCCESS)
}

fn read_model(path: &Path) -> Result<Model, String> {
    std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|json| Model::from_json(&json))
        .map_err(|e| format!("{}: {e}", path.display()))
}

fn why(config: &Path, modules: &Path, option: Option<&str>) -> Result<ExitCode, String> {
    let prefix = option.map(OptionPath::parse).transpose()?;
    let Some(evaluation) = evaluate(config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    print!("{}", distro_eval::explain(&evaluation, prefix.as_ref())?);
    Ok(ExitCode::SUCCESS)
}

/// Where distro's own files are: `factory/nix` and `modules`, and in a checkout `acceptance` too.
/// It's `--root`, else `$DISTRO_ROOT`, else the checkout the running binary was built in, else
/// the current directory. Inside that checkout it's `.`, so paths in messages stay short.
fn find_root(given: Option<PathBuf>, from_env: Option<PathBuf>) -> PathBuf {
    if let Some(root) = given.or(from_env) {
        return root;
    }
    let Some(checkout) = std::env::current_exe().ok().and_then(|exe| {
        exe.ancestors()
            .find(|dir| dir.join("factory/nix/flake.nix").is_file())
            .map(Path::to_path_buf)
    }) else {
        return PathBuf::from(".");
    };
    let here = std::env::current_dir()
        .and_then(|dir| dir.canonicalize())
        .ok();
    if here.is_some_and(|here| checkout.canonicalize().is_ok_and(|c| c == here)) {
        PathBuf::from(".")
    } else {
        checkout
    }
}
