use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use distro_check::{Outcome, RunContext, SIGNOFF_DIR, Scenario, Suite};

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
