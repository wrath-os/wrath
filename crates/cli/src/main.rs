use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use distro_check::{Outcome, RunContext, SIGNOFF_DIR, Scenario, Suite};
use distro_eval::{Evaluation, OptionPath};
use distro_model::Model;

mod system;

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
        /// Scenario files to run
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Run VM scenarios against this built image instead of building one
        #[arg(long)]
        image: Option<PathBuf>,
        /// After a failure, run the printed reproduction and report whether it fails the same way
        #[arg(long)]
        run_reproduction: bool,
    },
    /// Show what would change going from one configuration to another
    Plan {
        /// The configuration to plan
        #[arg(long)]
        config: PathBuf,
        /// The configuration to compare against; without it, everything is new
        #[arg(long)]
        against: Option<PathBuf>,
        /// Directory of modules [default: modules/luau under the root]
        #[arg(long)]
        modules: Option<PathBuf>,
    },
    /// Show where option values come from
    Why {
        /// The configuration to explain
        #[arg(long)]
        config: PathBuf,
        /// Directory of modules [default: modules/luau under the root]
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
            image,
            run_reproduction,
        } => check(&cli.root, &files, image, run_reproduction),
        Command::Plan {
            config,
            against,
            modules,
        } => plan(
            &config,
            against.as_deref(),
            &modules_dir(&cli.root, modules),
        ),
        Command::Why {
            config,
            modules,
            all: _,
            option,
        } => why(&config, &modules_dir(&cli.root, modules), option.as_deref()),
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
    image: Option<PathBuf>,
    run_reproduction: bool,
) -> Result<ExitCode, String> {
    let scenarios = files
        .iter()
        .map(|file| load_scenario(root, file))
        .collect::<Result<Vec<_>, _>>()?;

    let ctx = context(root, image)?;
    let mut all_passed = true;
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
    Ok(if all_passed {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
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
    modules.unwrap_or_else(|| {
        let dir = root.join("modules/luau");
        dir.strip_prefix(".").map(Path::to_path_buf).unwrap_or(dir)
    })
}

/// Evaluates a configuration, or prints why it can't and returns `None`.
fn evaluate(config: &Path, modules: &Path) -> Option<Evaluation> {
    system::evaluate_dir(config, modules)
        .inspect_err(|errors| eprint!("{errors}"))
        .ok()
}

fn model(config: &Path, modules: &Path) -> Option<Model> {
    evaluate(config, modules).map(|evaluation| Model::from_evaluation(&evaluation))
}

fn plan(config: &Path, against: Option<&Path>, modules: &Path) -> Result<ExitCode, String> {
    let Some(after) = model(config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    let before = match against {
        Some(against) => match model(against, modules) {
            Some(before) => before,
            None => return Ok(ExitCode::FAILURE),
        },
        None => Model::default(),
    };
    println!("model {}", &after.hash()[..16]);
    print!("{}", distro_model::plan(&before, &after));
    Ok(ExitCode::SUCCESS)
}

fn why(config: &Path, modules: &Path, option: Option<&str>) -> Result<ExitCode, String> {
    let prefix = option.map(OptionPath::parse).transpose()?;
    let Some(evaluation) = evaluate(config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    print!("{}", distro_eval::explain(&evaluation, prefix.as_ref())?);
    Ok(ExitCode::SUCCESS)
}
