use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::OnceLock;

use clap::{Parser, Subcommand};
use generations::Switch;
use wrath_build::image::INSTALL;
use wrath_build::seed_pack;
use wrath_build::store::Lab;
use wrath_check::{Outcome, RunContext, SIGNOFF_DIR, Scenario, Suite};
use wrath_eval::{ConfigKind, Evaluation, OptionPath, Value};
use wrath_model::{LOCK_FILE, Lock, Model, Source, WRATH_SOURCE};

mod activate;
mod apply_record;
mod bench;
mod cache;
mod checks;
mod config_from;
mod edit;
mod facts;
mod fetch;
mod generations;
mod homes;
mod host;
mod install;
mod known;
mod machine;
mod on_machine;
mod options;
mod pinned;
mod places;
mod platforms;
mod project;
mod reference;
mod run;
mod schema;
mod secret;
mod state;
mod style;
mod switching;
mod system;
mod templates;
mod trees;
mod upgrade;
mod watch;

/// How many times `dev test --determinism` evaluates each configuration.
const DETERMINISM_RUNS: u64 = 5;

/// The system's configuration, which commands act on unless told otherwise.
const SYSTEM_CONFIG: &str = "/etc/wrath";

#[derive(Parser)]
#[command(name = "wrath", version, about = "Declarative system configuration")]
struct Cli {
    /// The configuration to act on: a directory, or a URL such as `github:owner/repo//dir`, read
    /// as it is [default: the system's, in /etc/wrath]
    #[arg(short = 'C', long = "config", global = true, value_name = "DIR|URL")]
    config: Option<PathBuf>,
    /// Where the configuration came from, as `apply --host` passes it to the machine
    #[arg(long, global = true, hide = true)]
    config_from: Option<String>,
    /// The role to act as, for a configuration with kinds of machines in `roles/`
    #[arg(long, global = true, value_name = "NAME")]
    role: Option<String>,
    /// Print data as JSON instead of text, where a command supports it
    #[arg(long, global = true)]
    json: bool,
    /// Show everything each build prints, as it prints it
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Directory of modules [default: modules under the root]
    #[arg(long, global = true, hide = true)]
    modules: Option<PathBuf>,
    /// Repository root containing `acceptance/` and `build/` [default: found, see `find_root`]
    #[arg(long, global = true, hide = true)]
    root: Option<PathBuf>,
    /// A source read from a directory instead of where the lock pins it, for this command only,
    /// such as `grapes=../grapes` to work on grapes beside a configuration
    #[arg(
        id = "source_dirs",
        long = "source",
        global = true,
        value_name = "NAME=DIR"
    )]
    sources: Vec<String>,
    /// Write no lock: fail where the configuration's lock or a recipe's would change, as CI wants
    #[arg(long, global = true)]
    locked: bool,
    /// A binary cache to fetch builds from before building them: a directory or an http(s) URL
    #[arg(long = "cache", global = true, value_name = "URL")]
    caches: Vec<String>,
    /// A key whose signature on what a cache holds is trusted, as `wrath cache key` printed it
    #[arg(long = "cache-key", global = true, value_name = "KEY")]
    cache_keys: Vec<String>,
    /// Ask no cache for anything: build what isn't here, and plan without saying what a cache has
    #[arg(long, global = true, conflicts_with = "caches")]
    offline: bool,
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
        /// Compare against what another machine runs, reached with `ssh NAME`
        #[arg(long, value_name = "NAME", conflicts_with_all = ["against", "fresh"])]
        host: Option<String>,
    },
    /// Switch to the configuration, locking its packages first, and say when it takes effect
    Apply {
        /// A system that is already built, such as one `build` made for this machine elsewhere
        system: Option<PathBuf>,
        /// Build here and switch another machine, reached with `ssh NAME`
        #[arg(long, value_name = "NAME", conflicts_with = "system")]
        host: Option<String>,
        /// Switch without asking first
        #[arg(short, long)]
        yes: bool,
        /// Keep watching: try each save, by itself when it only changes settings, until Ctrl-C
        #[arg(long, conflicts_with_all = ["system", "yes", "boot"])]
        watch: bool,
        /// Make it the system booted next, without switching to it now
        #[arg(long)]
        boot: bool,
        /// Upgrade from what the system follows, as `updates.switch` says, and keep how it went;
        /// what `wrath-upgrade.timer` runs
        #[arg(long, conflicts_with_all = ["system", "host", "watch", "boot"])]
        automatic: bool,
    },
    /// Switch to the configuration for a while, then back unless you `wrath keep` it
    Try {
        /// A system that is already built
        system: Option<PathBuf>,
        /// Build here and try it on another machine, reached with `ssh NAME`
        #[arg(long, value_name = "NAME", conflicts_with = "system")]
        host: Option<String>,
        /// How long to try it, such as 90s, 5m or 1h
        #[arg(long = "for", value_name = "DURATION", default_value = "5m")]
        duration: String,
        /// Switch without asking first
        #[arg(short, long)]
        yes: bool,
    },
    /// Keep the system being tried as the current generation
    Keep,
    /// Build the configuration's system, or the packages you name, and print where each is;
    /// `--role` builds another of the configuration's roles
    Build {
        /// Packages to build instead of the system, such as `jq`
        packages: Vec<String>,
        /// A disk image of the system, to write to a USB stick or boot in a VM
        #[arg(long, conflicts_with = "packages")]
        image: bool,
        /// Link what was built at this path, or in this directory under its own name; an image is
        /// copied there instead
        #[arg(short = 'o', long = "link", value_name = "PATH")]
        link: Option<PathBuf>,
        /// Then run each package's smoke commands, and its own tests in a build of it thrown away
        #[arg(long, requires = "packages")]
        check: bool,
        /// The variant the VM tests drive
        #[arg(long, hide = true)]
        test: bool,
        /// A kernel option for an image's boot entry, instead of the screen and serial consoles
        #[arg(long = "kernel-option", requires = "image")]
        kernel_options: Vec<String>,
        /// Put the configuration in the image's /etc/wrath, so wrath on it can plan
        #[arg(long, requires = "image")]
        with_configuration: bool,
    },
    /// Show the system's generations; with one, what changed in it, and with two, between them
    History {
        /// A generation, or two to compare
        #[arg(num_args = 0..=2, value_name = "GENERATION")]
        generations: Vec<u32>,
    },
    /// Remove old generations and what only they used, disk images of systems among it
    Clean {
        /// How many generations before the current one to keep
        #[arg(long, default_value_t = 3)]
        keep: usize,
        /// Remove them without asking first
        #[arg(short, long)]
        yes: bool,
        /// Also remove what packages still in use were built from before their fix-ups; a newer
        /// wrath then builds them again where it would have fixed them up again
        #[arg(long)]
        raw: bool,
    },
    /// Switch back to the previous generation, or to generation N
    Rollback {
        /// The generation, as `wrath history` numbers it
        #[arg(value_name = "N")]
        generation: Option<u32>,
    },
    /// Run a package's program without installing it
    Run {
        /// The package, such as `jq`
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
        /// The packages, such as `jq` and `ripgrep`
        packages: Vec<String>,
    },
    /// Add packages to the configuration and lock them
    Add {
        /// The packages, such as `jq` and `ripgrep`
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
        /// A system already built and in /wrath/store, to install instead of building one [default:
        /// the one an image of a configuration with disks carries to install]
        #[arg(long, value_name = "PATH")]
        system: Option<PathBuf>,
        /// Go ahead without asking
        #[arg(short, long)]
        yes: bool,
    },
    /// Move sources to their newest versions and lock them
    Update {
        /// Sources to update, such as `wrath` [default: all of them]
        sources: Vec<String>,
        /// Only show each source: what it follows, where it's pinned and how old that is
        #[arg(long, conflicts_with = "sources")]
        list: bool,
    },
    /// Check the configuration for errors, without building it, and pin what's new in it
    Check,
    /// Show the options there are, with their types, defaults and docs
    Options {
        /// Only the options under this one, such as `services.nginx` or `users.john`
        prefix: Option<String>,
        /// Print them as a JSON Schema for editors, of the whole configuration or of a file whose
        /// options start at the prefix, such as `users.john` for `users/john.toml`
        #[arg(long)]
        schema: bool,
        /// The options of a project, `wrath.toml`, instead of a system's
        #[arg(long)]
        project: bool,
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
    /// Sign and push builds to a binary cache, which machines then fetch from
    Cache {
        #[command(subcommand)]
        command: cache::CacheCommand,
    },
    /// Commands for working on wrath itself
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
    Acceptance {
        /// Only run the scenarios of this milestone, for example `m2`
        #[arg(long)]
        milestone: Option<String>,
    },
    /// Write the reference pages in docs/reference from the code: commands, options, module API
    Docs {
        /// Fail when a page differs from what the code says, instead of writing it
        #[arg(long)]
        check: bool,
    },
    /// Point out recipes that write a target's name, such as `x86_64`, where a placeholder says it,
    /// and record what each recipe's style makes where its lock doesn't yet
    Lint {
        /// Also list dependencies another listed one already brings, for information; this builds
        /// what isn't built yet
        #[arg(long)]
        redundant: bool,
        /// With `--redundant`, only these recipes [default: every recipe]
        #[arg(requires = "redundant")]
        packages: Vec<String>,
    },
    /// Build packages a second time, as another user on fewer cores, and say which come out
    /// different [default: every recipe, writing a report to build/results]
    Repro {
        /// Only these recipes
        packages: Vec<String>,
    },
    /// Work on the seed, the prebuilt compilers every build starts from
    Seed {
        #[command(subcommand)]
        command: SeedCommand,
    },
    /// Compare each recipe's version with upstream's newest release
    Outdated {
        /// List every recipe, including those that are current or couldn't be checked
        #[arg(long)]
        all: bool,
    },
    /// Print every recipe's key, as `ID KEY` lines, to compare what a change to recipes or the
    /// builder rebuilds
    Keys,
    /// Print the configuration's model as JSON
    Model,
    /// Print what an apply keeps of the configuration, as JSON
    #[command(hide = true)]
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

#[derive(Subcommand)]
enum SeedCommand {
    /// Build the toolchain and check it stands on its own: nothing refers to the seed, and a C
    /// and a C++ program run without /usr
    Check,
    /// Pack the toolchain and what it needs into a new seed, and print its hash for seed.pin
    Pack,
    /// Print where every package lands, to compare what two seeds build
    Addresses,
}

#[derive(clap::Args)]
struct TestOptions {
    /// Check that the modules in this directory [default: the set in use's] render the fixture
    /// configs byte for byte the same across fresh interpreters and module orders
    #[arg(long, num_args = 0..=1, value_name = "DIR", conflicts_with = "files")]
    determinism: Option<Option<PathBuf>>,
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
    let mut cli = Cli::parse();
    ROLE.set(cli.role.clone()).expect("the role is set once");
    if let Some(given) = &cli.config_from {
        match config_from::ConfigFrom::from_arg(given) {
            Some(from) => config_from::set(from),
            None => {
                style::error("--config-from isn't what `apply --host` passes");
                return ExitCode::FAILURE;
            }
        }
    }
    // With no configuration given, a machine that follows a URL acts on it.
    let follows = (cli.config.is_none() && current_project().is_none())
        .then(config_from::running_follows)
        .flatten();
    if let Some(follows) = &follows {
        let changing = match &cli.command {
            Command::Add { .. } => Some("add"),
            Command::Remove { .. } => Some("remove"),
            Command::Update { list: false, .. } => Some("update"),
            _ => None,
        };
        if let Some(verb) = changing {
            style::error(&config_from::changed_elsewhere(follows, verb));
            return ExitCode::FAILURE;
        }
    }
    let fetches_follows = matches!(
        cli.command,
        Command::Plan {
            against: None,
            fresh: false,
            ..
        } | Command::Apply { system: None, .. }
            | Command::Try { system: None, .. }
            | Command::Check
            | Command::Build { .. }
            | Command::Why { .. }
            | Command::Options { .. }
            | Command::Update { list: true, .. }
    );
    // A configuration from a URL is fetched once, then acted on like a directory, read-only.
    let url = cli
        .config
        .as_deref()
        .and_then(Path::to_str)
        .filter(|given| config_from::is_url(given))
        .map(str::to_owned)
        .or_else(|| follows.clone().filter(|_| fetches_follows));
    let automatic = matches!(
        cli.command,
        Command::Apply {
            automatic: true,
            ..
        }
    );
    if automatic {
        let Some(follows) = &follows else {
            style::error(
                "the running system names nothing to upgrade from; its configuration says where \
                 with `updates.from`",
            );
            return ExitCode::FAILURE;
        };
        let from = generations::current_generation().and_then(|generation| generation.from);
        match upgrade::pause(Path::new("/"), follows, from.as_ref()) {
            Ok(true) => return ExitCode::SUCCESS,
            Ok(false) => {}
            Err(message) => {
                style::error(&message);
                return ExitCode::FAILURE;
            }
        }
    }
    if let Some(url) = &url {
        match config_from::fetch(url) {
            Ok(dir) => cli.config = Some(dir),
            Err(message) => {
                if automatic {
                    let _ = upgrade::keep(Path::new("/"), upgrade::Kind::Failed, &message);
                }
                style::error(&message);
                return ExitCode::FAILURE;
            }
        }
    }
    if cli.locked || url.is_some() {
        LOCKED.set(true).expect("`--locked` is set once");
        wrath_build::chain::read_only_locks();
    }
    if cli.offline {
        cache::go_offline();
    } else if let Err(message) = cache::use_caches(&cli.caches, &cli.cache_keys) {
        style::error(&message);
        return ExitCode::FAILURE;
    }
    wrath_build::progress::set(wrath_build::progress::chosen(
        cli.verbose,
        std::io::IsTerminal::is_terminal(&std::io::stderr()),
    ));
    fetch::set(match &cli.command {
        _ if cli.locked => fetch::Fetching::Pinned,
        Command::Update {
            sources,
            list: false,
        } => fetch::Fetching::Newest(sources.clone()),
        Command::Check
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
            Command::Plan { .. }
                | Command::Status
                | Command::History { .. }
                | Command::Update { list: true, .. }
        )
    {
        style::error("--json works with plan, status, history and update --list so far");
        return ExitCode::FAILURE;
    }
    // A wrath told where its files are is being developed, and runs as it is.
    let developing = cli.root.is_some() || cli.modules.is_some();
    let root = find_root(cli.root, std::env::var_os("WRATH_ROOT").map(PathBuf::from));
    if !developing
        && runs_pinned(&cli.command)
        && let Ok(config) = find_config(cli.config.as_deref())
    {
        match read_lock(&config) {
            Ok(lock) => pinned::warn_if_other(&root, lock.as_ref()),
            Err(message) => {
                style::error(&message);
                return ExitCode::FAILURE;
            }
        }
    }
    let grapes = match source_dirs(&cli.sources) {
        Ok(grapes) => grapes,
        Err(message) => {
            style::error(&message);
            return ExitCode::FAILURE;
        }
    };
    let configured = find_config(cli.config.as_deref()).ok();
    places::init(&root, grapes, configured.as_deref(), cli.config.is_some());
    if let Ok(checkout) = std::path::absolute(&root) {
        wrath_build::chain::set_wrath_tree(&checkout);
    }
    // Only commands that evaluate read the modules, so only they find grapes.
    let given_modules = cli.modules;
    let modules = std::cell::LazyCell::new(move || given_modules.unwrap_or_else(places::modules));
    let root = root.as_path();
    REPOSITORY
        .set(root.to_path_buf())
        .expect("the repository is set once");
    let config = || config_dir(cli.config.as_deref());
    // Commands that switch the machine, which a project has nothing to do with.
    let machine = || config().and_then(|config| refuse_project(&config).map(|()| config));
    let result = match cli.command {
        // Stays offline: what the newest commit would change is for `plan`.
        Command::Status if let Some(follows) = &follows => {
            generations::warn_degraded();
            if json {
                print_serde(&generations::following_json(follows))
            } else {
                generations::following(follows)
            }
        }
        Command::Status => machine().and_then(|config| {
            generations::warn_degraded();
            match model(&config, &modules) {
                Some(model) if json => print_serde(&generations::status_json(&model)),
                Some(model) => generations::status(&model),
                None if json => Ok(ExitCode::FAILURE),
                None => Ok(generations::status_unread()),
            }
        }),
        Command::Plan {
            against,
            fresh,
            host,
        } => {
            let against = match (against, fresh, host) {
                (_, true, _) => Against::Nothing,
                (Some(path), false, _) => Against::Path(path),
                (None, false, Some(host)) => Against::Host(host),
                (None, false, None) => Against::Running,
            };
            // A plan reads grapes' locks and fetches nothing to pin what they lack.
            wrath_build::chain::planned_locks();
            machine().and_then(|config| plan(&config, against, &modules, json))
        }
        Command::Apply {
            watch: true, host, ..
        } => watching(root, host.as_deref(), &machine, &modules),
        Command::Apply {
            automatic: true, ..
        } => {
            let follows = follows.as_deref().unwrap_or_default();
            let result = switch(root, None, Switch::Automatic, true, &machine, &modules);
            upgrade::finish(Path::new("/"), follows, result)
        }
        Command::Apply {
            system,
            host,
            yes,
            boot,
            ..
        } => {
            let how = if boot { Switch::Boot } else { Switch::Apply };
            match host {
                Some(host) => on_host(root, &host, how, yes, &machine, &modules),
                None => {
                    let result = switch(root, system, how, yes, &machine, &modules);
                    if let (Some(follows), Ok(ExitCode::SUCCESS)) = (&follows, &result) {
                        let from = generations::current_generation().and_then(|g| g.from);
                        let _ = upgrade::applied_by_hand(Path::new("/"), follows, from.as_ref());
                    }
                    result
                }
            }
        }
        Command::Try {
            system,
            host,
            duration,
            yes,
        } => generations::parse_duration(&duration).and_then(|seconds| {
            let how = Switch::Try(seconds);
            match host {
                Some(host) => on_host(root, &host, how, yes, &machine, &modules),
                None => switch(root, system, how, yes, &machine, &modules),
            }
        }),
        Command::Keep => generations::keep(),
        Command::Build {
            packages,
            link,
            check,
            ..
        } if !packages.is_empty() => package_context(cli.config.as_deref(), &modules)
            .and_then(|(model, _)| run::built(&model, &packages))
            .and_then(|built| {
                let paths = built.paths.iter().map(|path| built.store.join(path.hash()));
                let named: Vec<_> = built.packages.iter().cloned().zip(paths).collect();
                let code = generations::show_built(&named, link.as_deref())?;
                if !check {
                    return Ok(code);
                }
                let ids: Vec<&str> = built.packages.iter().map(String::as_str).collect();
                show_checked(&wrath_build::pilot::check_packages(
                    &places::grapes()?,
                    &places::work(),
                    &ids,
                )?)
            }),
        Command::Build {
            image,
            link,
            test,
            kernel_options,
            with_configuration,
            ..
        } => machine().and_then(|config| match model(&config, &modules) {
            Some(model) => {
                let name = role(&config).unwrap_or("system");
                let built = if image {
                    let keep = with_configuration.then_some(config.as_path());
                    // A system with disks waits for them as it boots, and they're only there once
                    // it's installed: the image boots the configuration without them and carries
                    // the configured system to install.
                    let live = if model.base.contains_key("storage") {
                        let Some(live) = live_model(&config, &modules) else {
                            return Ok(ExitCode::FAILURE);
                        };
                        Some(live)
                    } else {
                        None
                    };
                    let (boots, install) = match &live {
                        Some(live) => (live, Some(&model)),
                        None => (&model, None),
                    };
                    let path =
                        generations::build_image(keep, boots, install, &kernel_options, test)?;
                    (format!("{name}.img"), path)
                } else {
                    let path = generations::build_generation(&model, test)?;
                    (name.to_owned(), path)
                };
                generations::show_built(&[built], link.as_deref())
            }
            None => Ok(ExitCode::FAILURE),
        }),
        Command::History { generations } => match generations.as_slice() {
            [] if json => generations::history_json().and_then(|v| print_serde(&v)),
            [] => generations::history(),
            [number] => generations::changes_in(*number),
            [from, to, ..] => generations::diff(*from, *to),
        },
        Command::Clean { keep, yes, raw } => generations::clean(keep, yes, raw),
        Command::Rollback { generation } => generations::rollback(generation),
        Command::Run { package, args } => package_context(cli.config.as_deref(), &modules)
            .and_then(|(model, _)| run::run(model, &package, &args)),
        Command::Up => {
            package_context(cli.config.as_deref(), &modules).and_then(|(model, project)| {
                let project =
                    project.ok_or("only a project has services to run; `wrath up` runs in one")?;
                run::up(model, &project)
            })
        }
        Command::Shell { packages } => package_context(cli.config.as_deref(), &modules)
            .and_then(|(model, project)| run::shell(model, &packages, project.as_deref())),
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
            let system = system.or_else(|| fs::read_link(Path::new("/").join(INSTALL)).ok());
            // A system built already is installed as it is, with the disks its own model
            // describes: nothing to evaluate, lock or build, as on an image, and no grapes needed.
            let model = match &system {
                Some(system) => {
                    // Evaluating would have asked which role; the machine needs to know it too.
                    let roles: Vec<String> = all_roles(&config)?.into_iter().flatten().collect();
                    if roles.len() > 1 && role(&config).is_none() {
                        return Err(format!(
                            "the configuration has the roles {}; say which this machine is with \
                             `--role`",
                            roles.join(", ")
                        ));
                    }
                    let path = system.join(switching::MODEL);
                    Some(read_model(&path).map_err(|e| {
                        format!("{} isn't a system wrath built: {e}", system.display())
                    })?)
                }
                None => model(&config, &modules)
                    .map(|model| generations::locked(root, &config, model))
                    .transpose()?,
            };
            let Some(model) = model else {
                return Ok(ExitCode::FAILURE);
            };
            install::install(
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
        Command::Update {
            sources,
            list: false,
        } => config().and_then(|config| lock(root, &config, &modules, Some(&sources))),
        // What the lock pins, read without evaluating or fetching anything.
        Command::Update { list: true, .. } => config().and_then(|config| {
            let mut model = Model {
                sources: read_lock(&config)?.unwrap_or_default().sources,
                ..Model::default()
            };
            model
                .sources
                .entry(places::GRAPES_SOURCE.to_owned())
                .or_insert_with(|| Source::follow(&places::grapes_url(&config)));
            if json {
                print_serde(&generations::sources_json(&model))
            } else {
                generations::sources(&model)
            }
        }),
        Command::Check => config().and_then(|config| check_config(root, &config, &modules)),
        Command::Options {
            prefix,
            schema,
            project,
        } => {
            let kind = if project {
                ConfigKind::Project
            } else {
                ConfigKind::System
            };
            if schema {
                options::schema(&modules, kind, prefix.as_deref())
            } else {
                options::options(&modules, kind, prefix.as_deref())
            }
        }
        Command::Why { all: _, option } => {
            config().and_then(|config| why(&config, &modules, option.as_deref()))
        }
        Command::Secret { command } => {
            config().and_then(|config| secret::run(command, &config, &modules))
        }
        Command::Cache {
            command: command @ cache::CacheCommand::Check { .. },
        } => {
            // A check reads locks as a plan does, and asks the configuration's caches and keys.
            wrath_build::chain::planned_locks();
            if let Ok(config) = config()
                && evaluate(&config, &modules).is_none()
            {
                return ExitCode::FAILURE;
            }
            cache::run(command, &places::work())
        }
        Command::Cache { command } => cache::run(command, &places::work()),
        Command::Dev { command } => match command {
            DevCommand::Test { files, test } => dev_test(root, &files, test, &modules),
            DevCommand::Acceptance { milestone } => status(root, milestone.as_deref()),
            DevCommand::Lint {
                redundant,
                packages,
            } => lint(&packages, redundant),
            DevCommand::Docs { check } => reference::write(
                root,
                &modules,
                &<Cli as clap::CommandFactory>::command(),
                check,
            ),
            DevCommand::Repro { packages } => {
                let only: Vec<&str> = packages.iter().map(String::as_str).collect();
                builder()
                    .and_then(|(lab, repository)| {
                        wrath_build::experiments::reproduce(&lab, &repository, &only)
                    })
                    .map(|()| ExitCode::SUCCESS)
            }
            DevCommand::Seed { command } => builder()
                .and_then(|(lab, repository)| match command {
                    SeedCommand::Check => seed_pack::check_toolchain(&lab, &repository),
                    SeedCommand::Pack => seed_pack::pack(&lab, &repository),
                    SeedCommand::Addresses => seed_pack::addresses(&lab, &repository),
                })
                .map(|()| ExitCode::SUCCESS),
            DevCommand::Outdated { all } => outdated(all),
            DevCommand::Keys => places::grapes()
                .and_then(|grapes| wrath_build::graph::all_keys(&grapes, &places::work()))
                .map(|keys| {
                    for (id, key) in keys {
                        println!("{id} {key}");
                    }
                    ExitCode::SUCCESS
                }),
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
            style::error(&message);
            ExitCode::FAILURE
        }
    }
}

/// The role given with `--role`, set once when the command line is read.
static ROLE: OnceLock<Option<String>> = OnceLock::new();
/// Whether `--locked` was given, so no lock is written.
static LOCKED: OnceLock<bool> = OnceLock::new();
/// The repository wrath runs from, whose recipes name the packages there are.
static REPOSITORY: OnceLock<PathBuf> = OnceLock::new();
/// The role this machine runs, looked up the first time a command needs it.
static MACHINE_ROLE: OnceLock<Option<String>> = OnceLock::new();

pub(crate) fn locked() -> bool {
    LOCKED.get().copied().unwrap_or(false)
}

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
        .inspect_err(|message| style::warning(message))
        .unwrap_or_default();
    let listed = machine::name(&given).and_then(|name| machine::listed(config, &name));
    let role = machine::role(&given, listed.as_ref())?;
    eprintln!("role {role}, this machine's");
    Some(role)
}

/// Whether `command` reads the configuration, so it says when the lock pins another version of
/// wrath. Commands on built generations, and updates that move wrath's own pin, don't.
fn runs_pinned(command: &Command) -> bool {
    match command {
        Command::Update { sources, list } => {
            *list || (!sources.is_empty() && !sources.iter().any(|name| name == WRATH_SOURCE))
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
        | Command::Check
        | Command::Options { .. }
        | Command::Why { .. }
        | Command::Secret { .. } => true,
        Command::Keep
        | Command::History { .. }
        | Command::Clean { .. }
        | Command::Rollback { .. }
        | Command::Dev { .. }
        | Command::Cache { .. }
        | Command::Activate { .. }
        | Command::FillTemplates { .. } => false,
    }
}

/// The configuration a command acts on, as [`find_config`] finds it.
/// The context goes to stderr first, so output always says which configuration it's about.
fn config_dir(given: Option<&Path>) -> Result<PathBuf, String> {
    let config = find_config(given)?;
    // A URL said so already, and its tree's place in the cache means nothing to anyone.
    if let Ok(dir) = std::path::absolute(&config) {
        config_from::set(config_from::ConfigFrom::Dir(dir));
    }
    let shown = match config_from::get() {
        Some(from @ config_from::ConfigFrom::Url { .. }) => from.to_string(),
        _ => config.display().to_string(),
    };
    if is_project(&config) {
        eprintln!("{}", project::header(&config));
    } else {
        let kind = if given.is_some() { "config" } else { "system" };
        match given_role() {
            Some(role) => eprintln!("{kind} {shown}, role {role}"),
            None => eprintln!("{kind} {shown}"),
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
    wrath_eval::kind_of(&wrath_eval::DirFs::new(config)) == wrath_eval::ConfigKind::Home
}

fn current_project() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| project::find(&cwd))
}

fn is_project(config: &Path) -> bool {
    wrath_eval::kind_of(&wrath_eval::DirFs::new(config)) == wrath_eval::ConfigKind::Project
}

/// Refuses a project for commands about the machine and its generations.
fn refuse_project(config: &Path) -> Result<(), String> {
    if is_project(config) {
        return Err("a project has no generations to switch; `wrath shell` enters it".into());
    }
    Ok(())
}

/// The model packages come from for `run` and `shell`, and the project when the context is one:
/// the configuration's, or with none, an empty one, whose packages come from this wrath.
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

/// Checks every role, or the one `--role` names, then pins what's new in the lock, or with
/// `--locked` fails when something isn't pinned.
fn check_config(root: &Path, config: &Path, modules: &Path) -> Result<ExitCode, String> {
    let roles = match given_role() {
        Some(role) => vec![Some(role.to_owned())],
        None => all_roles(config)?,
    };
    let mut models = Vec::new();
    for role in &roles {
        if roles.len() > 1
            && let Some(role) = role
        {
            eprintln!("role {role}");
        }
        match check_role(config, modules, role.as_deref()) {
            Some(model) => models.push(model),
            None => return Ok(ExitCode::FAILURE),
        }
    }
    let mut unpinned = false;
    for model in &models {
        unpinned |= generations::needs_locking(root, config, model)?;
    }
    match (unpinned, locked()) {
        (false, _) => Ok(ExitCode::SUCCESS),
        (true, true) => Err(match config_from::fetched_url() {
            Some(url) => config_from::unpinned(url),
            None => "the lock doesn't pin everything the configuration uses; `wrath check` pins it"
                .into(),
        }),
        (true, false) => lock(root, config, modules, None),
    }
}

/// `count` of `noun`, in the plural unless there's one.
pub(crate) fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Checks one role and says what it holds, or prints why it can't be evaluated.
fn check_role(config: &Path, modules: &Path, role: Option<&str>) -> Option<Model> {
    let model = model_as(config, modules, role)?;
    let packages = model.all_packages().count();
    let all_pinned = model.sources.values().all(|source| source.is_locked());
    let pinned = match (model.sources.contains_key(WRATH_SOURCE), all_pinned) {
        (_, false) => "sources not pinned yet",
        (true, true) => "sources pinned",
        (false, true) => "wrath unpinned, since it was built from a checkout",
    };
    println!(
        "no errors: {}, {}, {pinned}, model {}",
        counted(model.options.len(), "option"),
        counted(packages, "package"),
        &model.hash()[..16]
    );
    Some(model)
}

fn dev_test(
    root: &Path,
    files: &[PathBuf],
    test: TestOptions,
    modules: &Path,
) -> Result<ExitCode, String> {
    match test.determinism {
        Some(dir) => checks::determinism(
            &dir.map_or_else(|| modules.to_path_buf(), |dir| under_root(root, &dir)),
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
        wrath_exe: std::env::current_exe()
            .map_err(|e| format!("cannot locate the wrath binary: {e}"))?,
        signoff_dir: root.join("acceptance").join(SIGNOFF_DIR),
        run_root: root.join("target/wrath-check/runs"),
        image_override,
    })
}

/// Prints each check `build --check` ran, and fails when any did.
fn show_checked(checked: &[wrath_build::pilot::Checked]) -> Result<ExitCode, String> {
    if checked.is_empty() {
        println!("nothing to check: these recipes have no smoke commands or tests");
    }
    let mut failed = false;
    for check in checked {
        if check.passed {
            println!("  ok      {}  {}", check.id, check.what);
        } else {
            failed = true;
            println!("  FAILED  {}  {}, see {}", check.id, check.what, check.log);
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The builder's work directory and grapes, where its recipes are.
fn builder() -> Result<(Lab, PathBuf), String> {
    Ok((Lab::new(places::work()), places::grapes()?))
}

/// Points out recipes that write a target's name where a placeholder says it, which fails lint;
/// with `redundant`, also lists dependencies another listed one brings, for information only.
fn lint(packages: &[String], redundant: bool) -> Result<ExitCode, String> {
    let grapes = places::grapes()?;
    let recorded = wrath_build::graph::record_styles(&grapes, &places::work())?;
    if !recorded.is_empty() {
        println!(
            "recorded what the style makes for {}",
            counted(recorded.len(), "recipe")
        );
    }
    let mut found = wrath_build::graph::misplaced_recipes(&grapes)?;
    found.push_str(&wrath_build::graph::targets_written(&grapes)?);
    if redundant {
        let only: Vec<&str> = packages.iter().map(String::as_str).collect();
        let listed = wrath_build::graph::redundant(&grapes, &places::work(), &only)?;
        if !listed.is_empty() {
            println!("listed, though another dependency brings them:\n{listed}");
        }
    }
    if found.is_empty() {
        println!("nothing to point out");
        return Ok(ExitCode::SUCCESS);
    }
    print!("{found}");
    Ok(ExitCode::FAILURE)
}

/// Prints the recipes upstream has released something newer of, and those it couldn't check.
fn outdated(all: bool) -> Result<ExitCode, String> {
    let reports = wrath_build::upstream::outdated(&places::grapes()?, &wrath_build::upstream::run)?;
    let width = reports.iter().map(|r| r.id.len()).max().unwrap_or(0);
    let (mut newer, mut held, mut unchecked) = (0, 0, Vec::new());
    for report in &reports {
        let outdated = report.is_outdated();
        newer += usize::from(outdated);
        if let Some(error) = &report.error {
            unchecked.push(report.id.as_str());
            if all {
                println!(
                    "{:width$}  {}  couldn't check: {error}",
                    report.id, report.version
                );
            }
            continue;
        }
        if outdated || all {
            let latest = report.latest.as_deref().unwrap_or("no releases found");
            let arrow = if outdated { "->" } else { "  " };
            println!(
                "{:width$}  {} {arrow} {latest}  ({})",
                report.id, report.version, report.place
            );
        }
        if let Some((release, reason)) = &report.held {
            held += 1;
            println!(
                "{:width$}  {} held back from {release}: {reason}",
                report.id, report.version
            );
        }
    }
    println!(
        "{newer} of {} recipes have a newer release upstream; {held} held back; {} couldn't be \
         checked{}",
        reports.len(),
        unchecked.len(),
        if unchecked.is_empty() || all {
            String::new()
        } else {
            format!(": {}", unchecked.join(", "))
        }
    );
    Ok(ExitCode::SUCCESS)
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
        .map(|s| (s, wrath_check::run(s, &ctx)))
        .collect();

    print!("{}", wrath_check::render(&suite.milestones, &results));
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
        all_passed &= checks::cases(&under_root(root, dir), modules, bless)?;
    }
    let ctx = context(root, image)?;
    for scenario in &scenarios {
        let outcome = wrath_check::run(scenario, &ctx);
        all_passed &= outcome.is_pass();
        print!(
            "{}",
            wrath_check::render_list(&[(scenario, outcome.clone())])
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

/// The directory with wrath's own files for the binary at `exe`: the checkout it was built in,
/// or the `share/wrath` beside it in the store.
fn files_beside(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find_map(|dir| {
        if dir.join("crates/cli").is_dir() {
            return Some(dir.to_path_buf());
        }
        let package = dir.join("share/wrath");
        package.is_dir().then_some(package)
    })
}

/// Reruns a failed scenario against the image of the failed run and reports whether it fails
/// with the same reason.
fn reproduce(scenario: &Scenario, failure: &wrath_check::Failure, ctx: &RunContext) -> String {
    let Some(reproduction) = &failure.reproduction else {
        return "reproduced: no reproduction available for this scenario".into();
    };
    let ctx = RunContext {
        image_override: Some(reproduction.image.clone()),
        ..ctx.clone()
    };
    match wrath_check::run(scenario, &ctx) {
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
    wrath_check::parse_scenario(&path, &id, &milestone, &text).map_err(|e| e.to_string())
}

/// Grapes' directory from `--source` arguments, `NAME=DIR` each. Only grapes can be given so far.
fn source_dirs(given: &[String]) -> Result<Option<PathBuf>, String> {
    let mut grapes = None;
    for source in given {
        let Some((name, dir)) = source.split_once('=') else {
            return Err(format!(
                "`--source {source}` needs a name and a directory, NAME=DIR"
            ));
        };
        if name != "grapes" {
            return Err(format!(
                "only grapes can be read from a directory so far, not `{name}`"
            ));
        }
        grapes = Some(PathBuf::from(dir));
    }
    Ok(grapes)
}

/// Evaluates a configuration for the role to act as, or prints why it can't and returns
/// `None`.
fn evaluate(config: &Path, modules: &Path) -> Option<Evaluation> {
    evaluate_as(config, modules, role(config))
}

fn evaluate_as(config: &Path, modules: &Path, role: Option<&str>) -> Option<Evaluation> {
    system::evaluate_dir(config, modules, system::ModuleOrder::Sorted, role)
        .inspect_err(|errors| style::print_report(&ending_a_line(errors)))
        .ok()
}

/// Every role of a configuration, or `[None]` for one that describes one machine.
pub(crate) fn all_roles(config: &Path) -> Result<Vec<Option<String>>, String> {
    let roles = wrath_eval::roles(&wrath_eval::DirFs::new(config))
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
    model_of(config, evaluate_as(config, modules, role)?)
}

/// The model of the system an image of the configuration boots, without its disks
/// (`image.live`), or prints why there's none and returns `None`.
fn live_model(config: &Path, modules: &Path) -> Option<Model> {
    let evaluation = system::evaluate_live(config, modules, role(config))
        .inspect_err(|errors| style::print_report(&ending_a_line(errors)))
        .ok()?;
    model_of(config, evaluation)
}

fn model_of(config: &Path, evaluation: Evaluation) -> Option<Model> {
    let model = with_lock(config, Model::from_evaluation(&evaluation))?;
    packages_known(&evaluation, &model).then_some(model)
}

/// Whether every package `model` installs has a recipe or a build, printing an error for each one
/// that doesn't, at where the configuration names it.
fn packages_known(evaluation: &Evaluation, model: &Model) -> bool {
    // Without recipes, as on a machine, there's nothing to check names against; what would build
    // them says so itself.
    if !known::recipes_here() {
        return true;
    }
    let names = match known::names(model) {
        Ok(names) => names,
        Err(message) => {
            style::error(&message);
            return false;
        }
    };
    let unknown = known::unknown(evaluation, model, &names);
    if !unknown.is_empty() {
        let text = wrath_eval::render(&unknown, evaluation.sources());
        style::print_report(&ending_a_line(&text));
    }
    unknown.is_empty()
}

/// `model` with the lock next to `config`, when it has one, or prints why it can't be read and
/// returns `None`.
fn with_lock(config: &Path, model: Model) -> Option<Model> {
    let mut model = match read_lock(config) {
        Ok(Some(lock)) => model.with_lock(&lock),
        Ok(None) => model,
        Err(message) => {
            style::error(&message);
            return None;
        }
    };
    // Files were included from these versions, which the lock gets when it's written.
    model.sources.extend(fetch::fetched());
    // Grapes from a directory given for the command isn't the version the lock pins, so the model,
    // which a generation keeps, doesn't say it is.
    if places::given() {
        model.sources.remove(places::GRAPES_SOURCE);
    }
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
            .inspect_err(|errors| style::print_report(&ending_a_line(errors)))
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
    /// The running system's model, or nothing when wrath didn't build it.
    Running,
    /// What another machine runs.
    Host(String),
}

fn plan(config: &Path, against: Against, modules: &Path, json: bool) -> Result<ExitCode, String> {
    // Another machine is planned for as the role it has, as `apply --host` builds it.
    let role = match &against {
        Against::Host(name) => remote_role(config, &host::Remote::new(name)?)?,
        _ => role(config).map(str::to_owned),
    };
    let Some(after) = model_as(config, modules, role.as_deref()) else {
        return Ok(ExitCode::FAILURE);
    };
    // What the system compared against was built from, for a system that was built.
    let (before, compared, built_from) = match against {
        Against::Nothing => (Model::default(), None, None),
        Against::Path(path) if path.is_file() => {
            let keys = path.with_file_name("packages.json");
            let built_from = Some(std::fs::read_to_string(keys).ok());
            (
                read_model(&path)?,
                Some(path.display().to_string()),
                built_from,
            )
        }
        Against::Path(path) => match model(&path, modules) {
            Some(before) => (before, None, None),
            None => return Ok(ExitCode::FAILURE),
        },
        Against::Host(name) => {
            let remote = host::Remote::new(&name)?;
            let model = remote.running_model()?.unwrap_or_default();
            (model, Some(name), Some(remote.running_built_from()?))
        }
        Against::Running => match switching::running_model() {
            Some(path) => (
                read_model(&path)
                    .map_err(|e| format!("{e}; plan with --against or --fresh instead"))?,
                Some("the running system".to_owned()),
                Some(switching::running_built_from()),
            ),
            None => (Model::default(), None, None),
        },
    };
    let rebuilt = built_from.and_then(|text| generations::rebuilt(&after, text.as_deref()));
    let plan = generations::with_rebuilt(wrath_model::plan(&before, &after), rebuilt);
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
    crate::print_plan(&plan);
    generations::print_takes(&after);
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
        print!("{}", wrath_eval::explain(&evaluation, None)?);
        return Ok(ExitCode::SUCCESS);
    };
    let as_option =
        OptionPath::parse(thing).and_then(|option| wrath_eval::explain(&evaluation, Some(&option)));
    match (as_option, wrath_eval::explain_output(&evaluation, thing)) {
        (Ok(explained), _) | (Err(_), Some(explained)) => print!("{explained}"),
        (Err(_), None) if thing.starts_with(['/', '~']) => {
            return Err(format!("no module writes `{thing}`"));
        }
        (Err(message), None) => {
            return Err(format!(
                "`{thing}` isn't a package any module installs, and {message}"
            ));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Where wrath's own files are: `modules`, and in a checkout `build` and `acceptance` too.
/// It's `--root`, else `$WRATH_ROOT`, else the checkout the running binary was built in or the
/// copy an installed binary keeps in `share/wrath`, else the current directory. Inside that
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
        return generations::apply_built(&system, how, yes);
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
    if !packages_known(&evaluation, &model) {
        return Ok(ExitCode::FAILURE);
    }
    let record = machine::prepare(
        &config,
        Path::new(machine::MACHINE_FILE),
        role,
        &model.machine,
        templates::facts(),
    )?;
    generations::apply(root, &config, model, how, yes, record)
}

/// Builds the configuration here and switches the machine `host` to it, as `how` says.
fn on_host(
    root: &Path,
    host: &str,
    how: Switch,
    yes: bool,
    config: &impl Fn() -> Result<PathBuf, String>,
    modules: &Path,
) -> Result<ExitCode, String> {
    let config = config()?;
    let remote = host::Remote::new(host)?;
    let role = remote_role(&config, &remote)?;
    let Some(model) = model_as(&config, modules, role.as_deref()) else {
        return Ok(ExitCode::FAILURE);
    };
    host::apply(root, &config, model, &remote, how, yes)
}

/// Watches the configuration and tries each save on this machine, or `host`.
fn watching(
    root: &Path,
    host: Option<&str>,
    config: &impl Fn() -> Result<PathBuf, String>,
    modules: &Path,
) -> Result<ExitCode, String> {
    let config = config()?;
    match host {
        Some(host) => {
            let remote = host::Remote::new(host)?.quietly();
            // sudo on the machine asks once for the whole watch.
            remote.hold_root()?;
            let role = remote_role(&config, &remote)?;
            let evaluate = || model_as(&config, modules, role.as_deref());
            watch::watch(root, &config, watch::Machine::Remote(&remote), evaluate)
        }
        None => {
            let evaluate = || model_as(&config, modules, role(&config));
            watch::watch(root, &config, watch::Machine::Here, evaluate)
        }
    }
}

/// The role to build for another machine: `--role`, the configuration's only role, or the one the
/// machine recorded when it was installed or `machines.toml` lists for it, never this machine's.
fn remote_role(config: &Path, remote: &host::Remote) -> Result<Option<String>, String> {
    if let Some(role) = given_role() {
        return Ok(Some(role.to_owned()));
    }
    let roles = all_roles(config)?;
    if let [only] = roles.as_slice() {
        return Ok(only.clone());
    }
    let given = remote.given()?;
    let name = given.name.as_deref().unwrap_or(remote.host_name());
    let role = machine::role(&given, machine::listed(config, name).as_ref()).ok_or_else(|| {
        format!("{name} has no role on record and machines.toml doesn't list it: pass --role")
    })?;
    eprintln!("role {role}, {name}'s");
    Ok(Some(role))
}

/// Prints `plan`, in color on a terminal.
pub(crate) fn print_plan(plan: &wrath_model::Plan) {
    print!("{}", plan.styled(style::stdout()));
}

/// Prints `value` as JSON, for `--json`.
fn print_json(value: &Value) -> Result<ExitCode, String> {
    print!("{}", wrath_eval::formats::json(value)?);
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
    fn the_sources_update_names_are_apart_from_the_directories_given_with_source() {
        let cli =
            Cli::try_parse_from(["wrath", "update", "small", "--source", "grapes=../g"]).unwrap();
        assert_eq!(cli.sources, ["grapes=../g"]);
        let Command::Update { sources, .. } = cli.command else {
            panic!("not update");
        };
        assert_eq!(sources, ["small"]);
    }

    #[test]
    fn a_binary_in_the_store_finds_its_files_beside_it() {
        let dir = tempfile::tempdir().unwrap();
        let package = dir.path().join("store/abc");
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::create_dir_all(package.join("share/wrath")).unwrap();
        assert_eq!(
            files_beside(&package.join("bin/wrath")),
            Some(package.join("share/wrath"))
        );
    }

    #[test]
    fn a_binary_built_in_a_checkout_finds_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("wrath");
        fs::create_dir_all(checkout.join("target/release")).unwrap();
        fs::create_dir_all(checkout.join("crates/cli")).unwrap();
        assert_eq!(
            files_beside(&checkout.join("target/release/wrath")),
            Some(checkout)
        );
    }

    #[test]
    fn an_error_ends_its_line() {
        assert_eq!(ending_a_line("no modules"), "no modules\n");
        assert_eq!(ending_a_line("two\nlines\n"), "two\nlines\n");
    }
}
