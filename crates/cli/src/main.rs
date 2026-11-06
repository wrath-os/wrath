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
mod generations;
mod secret;
mod system;
mod templates;

/// How many times `check --determinism` evaluates each configuration.
const DETERMINISM_RUNS: u64 = 5;

#[derive(Parser)]
#[command(name = "distro", version, about = "Declarative system configuration")]
struct Cli {
    /// Repository root containing `acceptance/` and `factory/`
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the acceptance scenarios and show which pass
    Status {
        /// Only run the scenarios of this milestone, for example `m2`
        #[arg(long)]
        milestone: Option<String>,
    },
    /// Run scenario files and report each result
    Check {
        /// Scenario files to run, or directories of bad configs to compare with their golden errors
        #[arg(required_unless_present = "determinism")]
        files: Vec<PathBuf>,
        /// Check that the modules in this directory render the fixture configs byte for byte the
        /// same across fresh interpreters and module orders
        #[arg(long, conflicts_with = "files")]
        determinism: Option<PathBuf>,
        /// Configurations for --determinism
        #[arg(long, default_value = "fixtures/configs")]
        configs: PathBuf,
        /// Directory of modules for bad configs [default: modules under the root]
        #[arg(long)]
        modules: Option<PathBuf>,
        /// Write the actual errors of bad configs as their expected errors
        #[arg(long)]
        bless: bool,
        /// Run VM scenarios against this built image instead of building one
        #[arg(long)]
        image: Option<PathBuf>,
        /// After a failure, run the printed reproduction and report whether it fails the same way
        #[arg(long)]
        run_reproduction: bool,
    },
    /// Show what would change going to a configuration from the running system or another one
    Plan {
        /// The configuration to plan
        #[arg(long)]
        config: PathBuf,
        /// The configuration, or a `model.json`, to compare against [default: the running
        /// system's model, or nothing on a system distro didn't build]
        #[arg(long, conflicts_with = "fresh")]
        against: Option<PathBuf>,
        /// Compare against nothing, so everything is new
        #[arg(long)]
        fresh: bool,
        /// Directory of modules [default: modules under the root]
        #[arg(long)]
        modules: Option<PathBuf>,
    },
    /// Measure how long `plan` takes with many synthetic modules
    Bench {
        /// Directory of real modules to add the synthetic ones to
        #[arg(long, default_value = "modules")]
        modules: PathBuf,
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
    /// Build the system for a configuration and print its store path, without switching to it
    Build {
        /// The configuration to build
        #[arg(long)]
        config: PathBuf,
        /// Directory of modules [default: modules under the root]
        #[arg(long)]
        modules: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "system")]
        target: generations::BuildTarget,
    },
    /// Build the system for a configuration and switch to it, locking its packages first
    Apply {
        /// The configuration to apply
        #[arg(long)]
        config: PathBuf,
        /// Directory of modules [default: modules under the root]
        #[arg(long)]
        modules: Option<PathBuf>,
    },
    /// Write the configuration's lock: resolve the packages it uses from the lock's package set
    Lock {
        /// The configuration to lock
        #[arg(long)]
        config: PathBuf,
        /// Directory of modules [default: modules under the root]
        #[arg(long)]
        modules: Option<PathBuf>,
    },
    /// Move the configuration's lock to the newest version of its sources
    Update {
        /// The configuration to update
        #[arg(long)]
        config: PathBuf,
        /// Directory of modules [default: modules under the root]
        #[arg(long)]
        modules: Option<PathBuf>,
        /// Sources to update, such as `nixpkgs` [default: all of them]
        sources: Vec<String>,
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
    /// Switch to a system that is already built, such as one `build` made on another machine
    Activate {
        /// The system's store path
        system: PathBuf,
    },
    /// Show the system's generations
    History,
    /// Switch back to the previous generation
    Rollback,
    /// Edit secrets and encrypt them to the machine and the people who manage it
    Secret {
        #[command(subcommand)]
        command: secret::SecretCommand,
    },
    /// Show where option values come from
    Why {
        /// The configuration to explain
        #[arg(long)]
        config: PathBuf,
        /// Directory of modules [default: modules under the root]
        #[arg(long)]
        modules: Option<PathBuf>,
        /// Explain every option
        #[arg(long, conflicts_with = "option", required_unless_present = "option")]
        all: bool,
        /// The option, or table of options, to explain
        option: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Status { milestone } => status(&cli.root, milestone.as_deref()),
        Command::Check {
            files,
            determinism,
            configs,
            modules,
            bless,
            image,
            run_reproduction,
        } => match determinism {
            Some(modules) => checks::determinism(
                &under_root(&cli.root, &modules),
                &under_root(&cli.root, &configs),
                DETERMINISM_RUNS,
            )
            .map(exit_code),
            None => check(
                &cli.root,
                &files,
                &modules_dir(&cli.root, modules),
                bless,
                image,
                run_reproduction,
            ),
        },
        Command::Plan {
            config,
            against,
            fresh,
            modules,
        } => {
            let against = match (against, fresh) {
                (_, true) => Against::Nothing,
                (Some(path), false) => Against::Path(path),
                (None, false) => Against::Running,
            };
            plan(&config, against, &modules_dir(&cli.root, modules))
        }
        Command::Bench {
            modules,
            synthetic,
            runs,
            budget_ms,
        } => bench::plan_time(&bench::Options {
            modules: &under_root(&cli.root, &modules),
            synthetic,
            runs: runs.max(1),
            budget: std::time::Duration::from_millis(budget_ms),
        })
        .map(exit_code),
        Command::Secret { command } => {
            secret::run(command, |modules| modules_dir(&cli.root, modules))
        }
        Command::Why {
            config,
            modules,
            all: _,
            option,
        } => why(&config, &modules_dir(&cli.root, modules), option.as_deref()),
        Command::Build {
            config,
            modules,
            target,
        } => match model(&config, &modules_dir(&cli.root, modules)) {
            Some(model) => generations::build(&cli.root, &model, target).map(|path| {
                println!("{}", path.display());
                ExitCode::SUCCESS
            }),
            None => Ok(ExitCode::FAILURE),
        },
        Command::Apply { config, modules } => {
            match model(&config, &modules_dir(&cli.root, modules)) {
                Some(model) => generations::apply(&cli.root, &config, model),
                None => Ok(ExitCode::FAILURE),
            }
        }
        Command::Lock { config, modules } => lock(&cli.root, &config, modules, None),
        Command::Update {
            config,
            modules,
            sources,
        } => lock(&cli.root, &config, modules, Some(&sources)),
        Command::Activate { system } => generations::activate(&system),
        Command::FillTemplates { spec, secrets, out } => templates::fill(&spec, &secrets, &out),
        Command::History => generations::history(),
        Command::Rollback => generations::rollback(),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
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
    modules: Option<PathBuf>,
    update: Option<&[String]>,
) -> Result<ExitCode, String> {
    match model(config, &modules_dir(root, modules)) {
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
