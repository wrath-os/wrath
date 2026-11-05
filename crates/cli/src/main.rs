use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use distro_check::{RunContext, SIGNOFF_DIR, Suite};

#[derive(Parser)]
#[command(name = "distro", version, about = "Declarative system configuration")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the acceptance scenarios and show which pass
    Status {
        /// Repository root containing `acceptance/`
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Only run the scenarios of this milestone, for example `m2`
        #[arg(long)]
        milestone: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Status { root, milestone } => status(&root, milestone.as_deref()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

fn status(root: &Path, milestone: Option<&str>) -> Result<(), String> {
    let acceptance = root.join("acceptance");
    let suite = Suite::load(&acceptance).map_err(|e| e.to_string())?;

    if let Some(id) = milestone
        && !suite.milestones.iter().any(|m| m.id == id)
    {
        let known: Vec<_> = suite.milestones.iter().map(|m| m.id.as_str()).collect();
        return Err(format!(
            "unknown milestone `{id}`; known milestones: {}",
            known.join(", ")
        ));
    }

    let ctx = RunContext {
        workdir: root.to_path_buf(),
        distro_exe: std::env::current_exe()
            .map_err(|e| format!("cannot locate the distro binary: {e}"))?,
        signoff_dir: acceptance.join(SIGNOFF_DIR),
    };
    let results: Vec<_> = suite
        .scenarios
        .iter()
        .filter(|s| milestone.is_none_or(|id| s.milestone == id))
        .map(|s| (s, distro_check::run(s, &ctx)))
        .collect();

    print!("{}", distro_check::render(&suite.milestones, &results));
    Ok(())
}
