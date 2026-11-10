use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

use clap::{Parser, Subcommand};
use distro_check::{Outcome, RunContext, SIGNOFF_DIR, Scenario, Suite};
use distro_eval::{Evaluation, OptionPath, Value};
use distro_model::{DISTRO_SOURCE, LOCK_FILE, Lock, Model};
use generations::Switch;

mod activate;
mod apply_record;
mod bench;
mod checks;
mod edit;
mod facts;
mod fetch;
mod generations;
mod install;
mod machine;
mod on_machine;
mod options;
mod pinned;
mod project;
mod run;
mod secret;
mod state;
mod switching;
mod system;
mod templates;
mod trees;

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
    /// The role to act as, for a configuration with kinds of machines in `roles/`
    #[arg(long, global = true, value_name = "NAME")]
    role: Option<String>,
    /// Print data as JSON instead of text, where a command supports it
    #[arg(long, global = true)]
    json: bool,
    /// Directory of modules [default: modules under the root]
    #[arg(long, global = true, hide = true)]
    modules: Option<PathBuf>,
    /// Repository root containing `acceptance/` and `build/` [default: found, see `find_root`]
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
        /// Switch without asking first
        #[arg(short, long)]
        yes: bool,
    },
    /// Switch to the configuration for a while, then back unless you `distro keep` it
    Try {
        /// A system that is already built
        system: Option<PathBuf>,
        /// How long to try it, such as 90s, 5m or 1h
        #[arg(long = "for", value_name = "DURATION", default_value = "5m")]
        duration: String,
        /// Switch without asking first
        #[arg(short, long)]
        yes: bool,
    },
    /// Keep the system being tried as the current generation
    Keep,
    /// Build the system without switching to it, and print where it is
    Build {
        #[arg(long, value_enum, default_value = "generation")]
        target: generations::BuildTarget,
        /// A kernel option for an image's boot entry, instead of the screen and serial consoles
        #[arg(long = "kernel-option")]
        kernel_options: Vec<String>,
        /// Put the configuration in the image's /etc/distro, so distro on it can plan
        #[arg(long)]
        with_configuration: bool,
    },
    /// Show the system's generations
    History,
    /// Remove old generations and what only they used
    Clean {
        /// How many generations before the current one to keep
        #[arg(long, default_value_t = 3)]
        keep: usize,
        /// Remove them without asking first
        #[arg(short, long)]
        yes: bool,
    },
    /// Show what changed between two generations
    Diff {
        /// The generation to compare from
        from: u32,
        /// The generation to compare to [default: the current one]
        to: Option<u32>,
    },
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
    /// Run a project's services until you stop them
    Up,
    /// Start a shell with packages, without installing them; in a project, with its packages and
    /// environment too
    Shell {
        /// The packages, such as `jq`, `stable#jq` or `github:jqlang/jq#jq`
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
    /// Lay out a machine's disks and install the configuration on them, from a live system
    Install {
        /// A disk to install on, as `NAME=/dev/...` for the configuration's disk NAME, or just
        /// the device when it has one disk. Repeat it for a disk with several devices. Without
        /// it, the machine's disks are matched to what the configuration describes
        #[arg(long = "disk", value_name = "DEVICE")]
        disks: Vec<String>,
        /// The machine's name, recorded on it
        #[arg(long)]
        name: Option<String>,
        /// A file with the disks' passphrase, instead of asking for it
        #[arg(long, value_name = "FILE")]
        passphrase_file: Option<PathBuf>,
        /// An age identity that decrypts the machine's keys under `secrets/machines/NAME`
        #[arg(long, value_name = "FILE")]
        identity: Option<PathBuf>,
        /// Where the new system is mounted while it's installed
        #[arg(long, default_value = "/mnt")]
        target: PathBuf,
        /// Erase disks that have partitions already
        #[arg(long)]
        wipe: bool,
        /// A system already built and in /store, to install instead of building one
        #[arg(long, value_name = "PATH")]
        system: Option<PathBuf>,
        /// Go ahead without asking
        #[arg(short, long)]
        yes: bool,
    },
    /// Lock what isn't locked yet, moving nothing that is
    Lock,
    /// Move sources to their newest versions and lock them
    Update {
        /// Sources to update, such as `distro` [default: all of them]
        sources: Vec<String>,
    },
    /// Show each source: what it follows, where it's pinned and how old that is
    Sources,
    /// Check the configuration for errors, without building it
    Check,
    /// Show the options there are, with their types, defaults and docs
    Options {
        /// Only the options under this one, such as `services.nginx` or `users.john`
        prefix: Option<String>,
    },
    /// Show where an option's value, a file or a package comes from
    Why {
        /// Explain every option
        #[arg(long, conflicts_with = "option", required_unless_present = "option")]
        all: bool,
        /// An option or table of options, a file such as `/etc/nginx/main.conf`, or a package
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
    /// Switch the machine to a built system
    #[command(hide = true)]
    Activate {
        /// The built system
        system: PathBuf,
        #[arg(long, value_enum, default_value = "switch")]
        mode: activate::Mode,
        /// The machine's root, such as a disk being installed
        #[arg(long, default_value = "/")]
        root: PathBuf,
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
    /// Print the configuration's model as JSON
    Model,
    /// Print what an apply keeps of the configuration, as JSON
    ApplyRecord,
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
    ROLE.set(cli.role.clone()).expect("the role is set once");
    fetch::set(match &cli.command {
        Command::Update { sources } => fetch::Fetching::Newest(sources.clone()),
        Command::Lock
        | Command::Apply { .. }
        | Command::Try { .. }
        | Command::Add { .. }
        | Command::Remove { .. } => fetch::Fetching::Missing,
        _ => fetch::Fetching::Pinned,
    });
    let json = cli.json;
    if json
        && !matches!(
            cli.command,
            Command::Plan { .. } | Command::Status | Command::History | Command::Sources
        )
    {
        eprintln!("error: --json works with plan, status, history and sources so far");
        return ExitCode::FAILURE;
    }
    // A distro told where its files are is being developed, and runs as it is.
    let developing = cli.root.is_some() || cli.modules.is_some();
    let root = find_root(cli.root, std::env::var_os("DISTRO_ROOT").map(PathBuf::from));
    if !developing
        && runs_pinned(&cli.command)
        && let Ok(config) = find_config(cli.config.as_deref())
    {
        match read_lock(&config) {
            Ok(lock) => pinned::warn_if_other(&root, lock.as_ref()),
            Err(message) => {
                eprintln!("error: {message}");
                return ExitCode::FAILURE;
            }
        }
    }
    let modules = modules_dir(&root, cli.modules);
    let root = root.as_path();
    let config = || config_dir(cli.config.as_deref());
    // Commands that switch the machine, which a project has nothing to do with.
    let machine = || config().and_then(|config| refuse_project(&config).map(|()| config));
    let result = match cli.command {
        Command::Status => machine().and_then(|config| {
            generations::warn_degraded();
            match model(&config, &modules) {
                Some(model) if json => print_serde(&generations::status_json(&model)),
                Some(model) => generations::status(&model),
                None => Ok(ExitCode::FAILURE),
            }
        }),
        Command::Plan { against, fresh } => {
            let against = match (against, fresh) {
                (_, true) => Against::Nothing,
                (Some(path), false) => Against::Path(path),
                (None, false) => Against::Running,
            };
            machine().and_then(|config| plan(&config, against, &modules, json))
        }
        Command::Apply { system, yes } => {
            switch(root, system, Switch::Apply, yes, &machine, &modules)
        }
        Command::Try {
            system,
            duration,
            yes,
        } => generations::parse_duration(&duration).and_then(|seconds| {
            switch(root, system, Switch::Try(seconds), yes, &machine, &modules)
        }),
        Command::Keep => generations::keep(),
        Command::Build {
            target,
            kernel_options,
            with_configuration,
        } => machine().and_then(|config| match model(&config, &modules) {
            Some(model) => {
                use generations::BuildTarget as T;
                let path = match target {
                    T::Generation | T::TestGeneration => {
                        let test = matches!(target, T::TestGeneration);
                        generations::build_generation(root, &model, test)?
                    }
                    T::Image | T::TestImage => {
                        let test = matches!(target, T::TestImage);
                        let keep = with_configuration.then_some(config.as_path());
                        generations::build_image(root, keep, &model, &kernel_options, test)?
                    }
                };
                println!("{}", path.display());
                Ok(ExitCode::SUCCESS)
            }
            None => Ok(ExitCode::FAILURE),
        }),
        Command::History if json => generations::history_json().and_then(|v| print_serde(&v)),
        Command::History => generations::history(),
        Command::Clean { keep, yes } => generations::clean(keep, yes),
        Command::Diff { from, to } => generations::diff(from, to),
        Command::Rollback => generations::rollback(),
        Command::Run { package, args } => package_context(cli.config.as_deref(), &modules)
            .and_then(|(model, _)| run::run(root, model, &package, &args)),
        Command::Up => {
            package_context(cli.config.as_deref(), &modules).and_then(|(model, project)| {
                let project =
                    project.ok_or("only a project has services to run; `distro up` runs in one")?;
                run::up(root, model, &project)
            })
        }
        Command::Shell { packages } => package_context(cli.config.as_deref(), &modules)
            .and_then(|(model, project)| run::shell(root, model, &packages, project.as_deref())),
        Command::Add { packages, home } => config()
            .and_then(|config| edit::add(root, &config, &modules, home.as_deref(), &packages)),
        Command::Remove { packages, home } => config()
            .and_then(|config| edit::remove(root, &config, &modules, home.as_deref(), &packages)),
        Command::Install {
            disks,
            name,
            passphrase_file,
            identity,
            target,
            wipe,
            yes,
            system,
        } => machine().and_then(|config| {
            let Some(model) = model(&config, &modules) else {
                return Ok(ExitCode::FAILURE);
            };
            // A system built already is installed as it is, with nothing left to lock.
            let model = match system {
                Some(_) => model,
                None => generations::locked(root, &config, model)?,
            };
            install::install(
                root,
                model,
                &install::Install {
                    config: &config,
                    disks: install::disks(&disks),
                    name: name.as_deref(),
                    role: given_role(),
                    passphrase: passphrase_file.as_deref(),
                    identity: identity.as_deref(),
                    target: &target,
                    wipe,
                    yes,
                    prebuilt: system.as_deref(),
                },
            )
        }),
        Command::Lock => config().and_then(|config| lock(root, &config, &modules, None)),
        Command::Update { sources } => {
            config().and_then(|config| lock(root, &config, &modules, Some(&sources)))
        }
        Command::Sources => config().and_then(|config| match model(&config, &modules) {
            Some(model) if json => print_serde(&generations::sources_json(&model)),
            Some(model) => generations::sources(&model),
            None => Ok(ExitCode::FAILURE),
        }),
        Command::Check => config().and_then(|config| check_config(&config, &modules)),
        Command::Options { prefix } => options::options(&modules, prefix.as_deref()),
        Command::Why { all: _, option } => {
            config().and_then(|config| why(&config, &modules, option.as_deref()))
        }
        Command::Secret { command } => {
            config().and_then(|config| secret::run(command, &config, &modules))
        }
        Command::Dev { command } => match command {
            DevCommand::Test { files, test } => dev_test(root, &files, test, &modules),
            DevCommand::Status { milestone } => status(root, milestone.as_deref()),
            DevCommand::Model => config().and_then(|config| print_model(&config, &modules)),
            DevCommand::ApplyRecord => {
                config().and_then(|config| print_apply_record(&config, &modules))
            }
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
        Command::Activate { system, mode, root } => {
            activate::activate(&system, mode, &root).map(|()| ExitCode::SUCCESS)
        }
        Command::FillTemplates { spec, secrets, out } => templates::machine_values()
            .and_then(|machine| templates::fill(&spec, &secrets, &machine, &out)),
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The role given with `--role`, set once when the command line is read.
static ROLE: OnceLock<Option<String>> = OnceLock::new();
/// The role this machine runs, looked up the first time a command needs it.
static MACHINE_ROLE: OnceLock<Option<String>> = OnceLock::new();

fn given_role() -> Option<&'static str> {
    ROLE.get().and_then(Option::as_deref)
}

/// The role to act as: the one given, or else this machine's, when the configuration has more
/// than one.
fn role(config: &Path) -> Option<&'static str> {
    given_role().or_else(|| MACHINE_ROLE.get_or_init(|| machine_role(config)).as_deref())
}

/// The role this machine runs in `config`: its only role, the one it was installed as, or else
/// its entry in `machines.toml`.
fn machine_role(config: &Path) -> Option<String> {
    match all_roles(config).as_deref() {
        Ok([_, _, ..]) => {}
        Ok([only]) => return only.clone(),
        Ok([]) | Err(_) => return None,
    }
    let given = machine::Given::read(Path::new(machine::MACHINE_FILE))
        .inspect_err(|message| eprintln!("warning: {message}"))
        .unwrap_or_default();
    let listed = machine::name(&given).and_then(|name| machine::listed(config, &name));
    let role = machine::role(&given, listed.as_ref())?;
    eprintln!("role {role}, this machine's");
    Some(role)
}

/// Whether `command` reads the configuration, so it says when the lock pins another distro.
/// Commands on built generations, and updates that move distro's own pin, don't.
fn runs_pinned(command: &Command) -> bool {
    match command {
        Command::Update { sources } => {
            !sources.is_empty() && !sources.iter().any(|name| name == DISTRO_SOURCE)
        }
        Command::Status
        | Command::Plan { .. }
        | Command::Apply { .. }
        | Command::Try { .. }
        | Command::Build { .. }
        | Command::Run { .. }
        | Command::Up
        | Command::Shell { .. }
        | Command::Add { .. }
        | Command::Remove { .. }
        | Command::Install { .. }
        | Command::Lock
        | Command::Sources
        | Command::Check
        | Command::Options { .. }
        | Command::Why { .. }
        | Command::Secret { .. } => true,
        Command::Keep
        | Command::History
        | Command::Clean { .. }
        | Command::Diff { .. }
        | Command::Rollback
        | Command::Dev { .. }
        | Command::Activate { .. }
        | Command::FillTemplates { .. } => false,
    }
}

/// The configuration a command acts on, as [`find_config`] finds it.
/// The context goes to stderr first, so output always says which configuration it's about.
fn config_dir(given: Option<&Path>) -> Result<PathBuf, String> {
    let config = find_config(given)?;
    if is_project(&config) {
        eprintln!("{}", project::header(&config));
    } else {
        let kind = if given.is_some() { "config" } else { "system" };
        match given_role() {
            Some(role) => eprintln!("{kind} {}, role {role}", config.display()),
            None => eprintln!("{kind} {}", config.display()),
        }
    }
    Ok(config)
}

/// The configuration a command acts on: the one given, the project it runs in, or the system's.
fn find_config(given: Option<&Path>) -> Result<PathBuf, String> {
    let found = match given {
        Some(_) => None,
        None => current_project(),
    };
    let config = given
        .or(found.as_deref())
        .unwrap_or(Path::new(SYSTEM_CONFIG));
    if !config.is_dir() {
        return Err(match given {
            Some(_) => format!("{}: no such configuration directory", config.display()),
            None => {
                format!("there's no system configuration in {SYSTEM_CONFIG}; give one with -C DIR")
            }
        });
    }
    if is_home(config) {
        return Err(format!(
            "{}: a home of its own, in `home.toml`, isn't supported for now; configure the \
             user under `[users.NAME]` in the system's configuration instead",
            config.display()
        ));
    }
    Ok(config.to_path_buf())
}

fn is_home(config: &Path) -> bool {
    distro_eval::kind_of(&distro_eval::DirFs::new(config)) == distro_eval::ConfigKind::Home
}

fn current_project() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| project::find(&cwd))
}

fn is_project(config: &Path) -> bool {
    distro_eval::kind_of(&distro_eval::DirFs::new(config)) == distro_eval::ConfigKind::Project
}

/// Refuses a project for commands about the machine and its generations.
fn refuse_project(config: &Path) -> Result<(), String> {
    if is_project(config) {
        return Err("a project has no generations to switch; `distro shell` enters it".into());
    }
    Ok(())
}

/// The model packages come from for `run` and `shell`, and the project when the context is one:
/// the configuration's, or with none, an empty one, whose packages come from this distro.
fn package_context(
    given: Option<&Path>,
    modules: &Path,
) -> Result<(Model, Option<PathBuf>), String> {
    if given.is_none() && current_project().is_none() && !Path::new(SYSTEM_CONFIG).is_dir() {
        return Ok((Model::default(), None));
    }
    let config = config_dir(given)?;
    let model = model(&config, modules).ok_or("the configuration has errors")?;
    Ok((model, is_project(&config).then_some(config)))
}

/// Prints the model of `config`, for tools that work from one.
fn print_model(config: &Path, modules: &Path) -> Result<ExitCode, String> {
    let Some(model) = model(config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    println!("{}", model.canonical());
    Ok(ExitCode::SUCCESS)
}

/// Prints what an apply of `config` keeps, for tools that record applies.
fn print_apply_record(config: &Path, modules: &Path) -> Result<ExitCode, String> {
    let Some(evaluation) = evaluate_as(config, modules, role(config)) else {
        return Ok(ExitCode::FAILURE);
    };
    println!("{}", apply_record::record(&evaluation, config)?);
    Ok(ExitCode::SUCCESS)
}

/// Evaluates the configuration and says whether it has errors, without building anything: for
/// the role given, or every role when none is.
fn check_config(config: &Path, modules: &Path) -> Result<ExitCode, String> {
    if let Some(role) = given_role() {
        return Ok(check_role(config, modules, Some(role)));
    }
    let mut failed = false;
    for role in all_roles(config)? {
        if let Some(role) = &role {
            eprintln!("role {role}");
        }
        failed |= check_role(config, modules, role.as_deref()) == ExitCode::FAILURE;
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn check_role(config: &Path, modules: &Path, role: Option<&str>) -> ExitCode {
    let Some(model) = model_as(config, modules, role) else {
        return ExitCode::FAILURE;
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
    ExitCode::SUCCESS
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

/// The directory with distro's own files for the binary at `exe`: the checkout it was built in,
/// or the `share/distro` beside it in the store.
fn files_beside(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .flat_map(|dir| [dir.to_path_buf(), dir.join("share/distro")])
        .find(|dir| dir.join("modules").is_dir())
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
    // A scenario's milestone is the directory it's in, such as `acceptance/m2`.
    let milestone = file
        .parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    distro_check::parse_scenario(&path, &id, &milestone, &text).map_err(|e| e.to_string())
}

fn modules_dir(root: &Path, modules: Option<PathBuf>) -> PathBuf {
    modules.unwrap_or_else(|| under_root(root, Path::new("modules")))
}

/// Evaluates a configuration for the role to act as, or prints why it can't and returns
/// `None`.
fn evaluate(config: &Path, modules: &Path) -> Option<Evaluation> {
    evaluate_as(config, modules, role(config))
}

fn evaluate_as(config: &Path, modules: &Path, role: Option<&str>) -> Option<Evaluation> {
    system::evaluate_dir(config, modules, system::ModuleOrder::Sorted, role)
        .inspect_err(|errors| eprint!("{}", ending_a_line(errors)))
        .ok()
}

/// Every role of a configuration, or `[None]` for one that describes one machine.
pub(crate) fn all_roles(config: &Path) -> Result<Vec<Option<String>>, String> {
    let roles = distro_eval::roles(&distro_eval::DirFs::new(config))
        .map_err(|e| format!("cannot list the roles in {}: {e}", config.display()))?;
    Ok(if roles.is_empty() {
        vec![None]
    } else {
        roles.into_iter().map(Some).collect()
    })
}

/// The configuration's model, with its lock when it has one, or prints why it can't and
/// returns `None`.
fn model(config: &Path, modules: &Path) -> Option<Model> {
    model_as(config, modules, role(config))
}

fn model_as(config: &Path, modules: &Path, role: Option<&str>) -> Option<Model> {
    with_lock(
        config,
        Model::from_evaluation(&evaluate_as(config, modules, role)?),
    )
}

/// `model` with the lock next to `config`, when it has one, or prints why it can't be read and
/// returns `None`.
fn with_lock(config: &Path, model: Model) -> Option<Model> {
    let mut model = match read_lock(config) {
        Ok(Some(lock)) => model.with_lock(&lock),
        Ok(None) => model,
        Err(message) => {
            eprintln!("error: {message}");
            return None;
        }
    };
    // Files were included from these versions, which the lock gets when it's written.
    model.sources.extend(fetch::fetched());
    Some(model)
}

/// The lock next to `config`, if there is one. A project's local file adds the sources it pins
/// on its own.
pub(crate) fn read_lock(config: &Path) -> Result<Option<Lock>, String> {
    let lock = read_lock_file(&config.join(LOCK_FILE))?;
    if !(is_project(config) && project::has_local(config)) {
        return Ok(lock);
    }
    let Some(local) = read_lock_file(&config.join(project::LOCAL_LOCK))? else {
        return Ok(lock);
    };
    let mut lock = lock.unwrap_or_default();
    for (name, source) in local.sources {
        lock.sources.entry(name).or_insert(source);
    }
    Ok(Some(lock))
}

fn read_lock_file(path: &Path) -> Result<Option<Lock>, String> {
    match std::fs::read_to_string(path) {
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
    if is_project(config) && project::has_local(config) {
        let shared = system::evaluate_shared(config, modules)
            .inspect_err(|errors| eprint!("{}", ending_a_line(errors)))
            .ok()
            .and_then(|evaluation| with_lock(config, Model::from_evaluation(&evaluation)));
        let (Some(shared), Some(mut model)) = (shared, model(config, modules)) else {
            return Ok(ExitCode::FAILURE);
        };
        // What the local file removes is still locked for everyone else.
        model.packages.extend(shared.packages.iter().cloned());
        return generations::lock(root, config, &[(None, model)], update, Some(&shared));
    }
    // One lock serves every role, so every role is locked, whichever `--role` says.
    let mut models = Vec::new();
    for role in all_roles(config)? {
        let Some(model) = model_as(config, modules, role.as_deref()) else {
            return Ok(ExitCode::FAILURE);
        };
        models.push((role, model));
    }
    generations::lock(root, config, &models, update, None)
}

/// What a plan compares a configuration against.
enum Against {
    Nothing,
    /// A configuration directory, or a model as `model.json`.
    Path(PathBuf),
    /// The running system's model, or nothing when distro didn't build it.
    Running,
}

fn plan(config: &Path, against: Against, modules: &Path, json: bool) -> Result<ExitCode, String> {
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
        Against::Running => match switching::running_model() {
            Some(path) => (
                read_model(&path)
                    .map_err(|e| format!("{e}; plan with --against or --fresh instead"))?,
                Some("the running system".to_owned()),
            ),
            None => (Model::default(), None),
        },
    };
    let plan = distro_model::plan(&before, &after);
    if json {
        let Value::Table(mut fields) = plan.to_value() else {
            unreachable!("a plan is a table")
        };
        fields.insert("model".into(), Value::Str(after.hash()));
        fields.insert("against".into(), Value::Str(before.hash()));
        return print_json(&Value::Table(fields));
    }
    println!("model {}", &after.hash()[..16]);
    if let Some(compared) = compared {
        println!("against {compared} (model {})", &before.hash()[..16]);
    }
    print!("{plan}");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn read_model(path: &Path) -> Result<Model, String> {
    std::fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|json| Model::from_json(&json))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Explains an option or table of options, a file such as `/etc/nginx/main.conf`, or a package.
fn why(config: &Path, modules: &Path, thing: Option<&str>) -> Result<ExitCode, String> {
    let Some(evaluation) = evaluate(config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    let Some(thing) = thing else {
        print!("{}", distro_eval::explain(&evaluation, None)?);
        return Ok(ExitCode::SUCCESS);
    };
    let as_option = OptionPath::parse(thing)
        .and_then(|option| distro_eval::explain(&evaluation, Some(&option)));
    match (as_option, distro_eval::explain_output(&evaluation, thing)) {
        (Ok(explained), _) | (Err(_), Some(explained)) => print!("{explained}"),
        (Err(_), None) if thing.starts_with(['/', '~']) => {
            return Err(format!("no module writes `{thing}`"));
        }
        (Err(message), None) => {
            return Err(format!(
                "{message}, and no module installs a package `{thing}`"
            ));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Where distro's own files are: `modules`, and in a checkout `build` and `acceptance` too.
/// It's `--root`, else `$DISTRO_ROOT`, else the checkout the running binary was built in or the
/// copy an installed binary keeps in `share/distro`, else the current directory. Inside that
/// checkout it's `.`, so paths in messages stay short.
fn find_root(given: Option<PathBuf>, from_env: Option<PathBuf>) -> PathBuf {
    if let Some(root) = given.or(from_env) {
        return root;
    }
    let Some(checkout) = std::env::current_exe()
        .ok()
        .and_then(|exe| files_beside(&exe))
    else {
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

/// Switches to `system`, already built, or else builds the configuration and switches to it.
fn switch(
    root: &Path,
    system: Option<PathBuf>,
    how: Switch,
    yes: bool,
    config: &impl Fn() -> Result<PathBuf, String>,
    modules: &Path,
) -> Result<ExitCode, String> {
    if let Some(system) = system {
        return generations::switch_to(&system, how);
    }
    let config = config()?;
    let role = role(&config);
    let Some(evaluation) = evaluate(&config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    let what = role.map_or("the configuration".to_owned(), |role| {
        format!("the role {role}")
    });
    facts::check(
        evaluation.requirements(),
        evaluation.sources(),
        &facts::detect(),
        &what,
    )?;
    let Some(model) = with_lock(&config, Model::from_evaluation(&evaluation)) else {
        return Ok(ExitCode::FAILURE);
    };
    let record = machine::prepare(
        &config,
        Path::new(machine::MACHINE_FILE),
        role,
        &model.machine,
        templates::facts(),
    )?;
    generations::apply(root, &config, model, how, yes, record)
}

/// Prints `value` as JSON, for `--json`.
fn print_json(value: &Value) -> Result<ExitCode, String> {
    print!("{}", distro_eval::formats::json(value)?);
    Ok(ExitCode::SUCCESS)
}

/// Prints data built with serde as JSON, for `--json`.
fn print_serde(value: &serde_json::Value) -> Result<ExitCode, String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(ExitCode::SUCCESS)
}

/// `text` ending in a newline, so the shell's prompt starts on a line of its own after it.
fn ending_a_line(text: &str) -> std::borrow::Cow<'_, str> {
    if text.ends_with('\n') {
        text.into()
    } else {
        format!("{text}\n").into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binary_in_the_store_finds_the_modules_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("store/abc");
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::create_dir_all(package.join("share/distro/modules")).unwrap();
        assert_eq!(
            files_beside(&package.join("bin/distro")),
            Some(package.join("share/distro"))
        );
    }

    #[test]
    fn a_binary_built_in_a_checkout_finds_the_checkouts_modules() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("distro");
        fs::create_dir_all(checkout.join("target/release")).unwrap();
        fs::create_dir_all(checkout.join("modules")).unwrap();
        assert_eq!(
            files_beside(&checkout.join("target/release/distro")),
            Some(checkout)
        );
    }

    #[test]
    fn an_error_ends_its_line() {
        assert_eq!(ending_a_line("no modules"), "no modules\n");
        assert_eq!(ending_a_line("two\nlines\n"), "two\nlines\n");
    }
}
