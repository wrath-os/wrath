//! Running scenarios and reporting their outcome.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::scenario::{CommandSpec, Runner, Scenario};
use crate::vm::{self, Host, Reproduction, VmRun, VmSpec};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail(Failure),
    /// Not runnable yet: nothing can run it before a later milestone, or a manual scenario
    /// without a sign-off.
    Pending(String),
}

impl Outcome {
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub reason: String,
    /// Directory with logs from the failed run.
    pub logs: Option<PathBuf>,
    pub reproduction: Option<Reproduction>,
}

impl From<String> for Failure {
    fn from(reason: String) -> Self {
        Self {
            reason,
            logs: None,
            reproduction: None,
        }
    }
}

impl From<&str> for Failure {
    fn from(reason: &str) -> Self {
        reason.to_owned().into()
    }
}

/// Where and how scenarios run.
#[derive(Debug, Clone)]
pub struct RunContext {
    /// Working directory for commands and image builds, normally the repository root.
    pub workdir: PathBuf,
    /// The binary that `distro` in a command scenario resolves to.
    pub distro_exe: PathBuf,
    /// Directory holding `<milestone>-<id>.toml` sign-off files for manual scenarios.
    pub signoff_dir: PathBuf,
    /// Parent directory for VM run directories.
    pub run_root: PathBuf,
    /// Use this already-built disk image instead of building the scenario's image.
    pub image_override: Option<PathBuf>,
}

pub fn run(scenario: &Scenario, ctx: &RunContext) -> Outcome {
    match &scenario.runner {
        Runner::Command(spec) => run_command(spec, ctx),
        Runner::Manual(_) => {
            let signoff = ctx
                .signoff_dir
                .join(format!("{}-{}.toml", scenario.milestone, scenario.id));
            if signoff.is_file() {
                Outcome::Pass
            } else {
                Outcome::Pending("awaiting human sign-off".into())
            }
        }
        Runner::Vm(spec) => run_vm(scenario, spec, ctx),
        Runner::Pending { reason } => Outcome::Pending(reason.clone()),
    }
}

fn run_vm(scenario: &Scenario, spec: &VmSpec, ctx: &RunContext) -> Outcome {
    let host = Host {
        distro: &ctx.distro_exe,
        workdir: &ctx.workdir,
    };
    let built = match &ctx.image_override {
        Some(image) => Ok(image.clone()),
        None => vm::build_config_image(host, &spec.config),
    };
    let image = match built {
        Ok(image) => image,
        Err(reason) => return fail(reason),
    };
    let name = if scenario.milestone.is_empty() {
        scenario.id.clone()
    } else {
        format!("{}-{}", scenario.milestone, scenario.id)
    };
    let dir = ctx.run_root.join(name);
    let run = VmRun {
        spec,
        image: &image,
        dir: &dir,
        host,
    };
    match run.execute() {
        Ok(()) => Outcome::Pass,
        Err(reason) => {
            let relative = |path: &Path| {
                path.strip_prefix(&ctx.workdir)
                    .unwrap_or(path)
                    .to_path_buf()
            };
            let command = format!(
                "distro dev test {} --image {}",
                relative(&scenario.source).display(),
                image.display()
            );
            Outcome::Fail(Failure {
                reason,
                logs: Some(relative(&dir)),
                reproduction: Some(Reproduction { image, command }),
            })
        }
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(10);

fn run_command(spec: &CommandSpec, ctx: &RunContext) -> Outcome {
    let Some((program, args)) = spec.run.split_first() else {
        return fail("`command.run` is empty");
    };
    let program = if program == "distro" {
        ctx.distro_exe.clone()
    } else {
        PathBuf::from(program)
    };

    let mut child = match Command::new(&program)
        .args(args)
        .current_dir(&ctx.workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return fail(format!("could not start `{}`: {e}", program.display())),
    };

    let stdout = collect(child.stdout.take());
    let stderr = collect(child.stderr.take());

    let deadline = Instant::now() + spec.timeout();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // Best effort: the child may have exited between the check and the kill.
                let _ = child.kill();
                let _ = child.wait();
                return fail(format!("timed out after {}s", spec.timeout_secs));
            }
            Ok(None) => thread::sleep(POLL_INTERVAL),
            Err(e) => return fail(format!("waiting for the command failed: {e}")),
        }
    };

    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    check_output(spec, status, &stdout, &stderr)
}

fn collect(pipe: Option<impl Read + Send + 'static>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            // A read error leaves whatever was read so far, which is all we can report.
            let _ = pipe.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn check_output(spec: &CommandSpec, status: ExitStatus, stdout: &str, stderr: &str) -> Outcome {
    if status.code() != Some(spec.exit) {
        let got = status.code().map_or_else(
            || "killed by a signal".to_owned(),
            |c| format!("exit code {c}"),
        );
        let context = first_line(stderr).or_else(|| first_line(stdout));
        return fail(match context {
            Some(line) => format!("{got}, expected {}: {line}", spec.exit),
            None => format!("{got}, expected {}", spec.exit),
        });
    }

    let missing = |stream: &str, haystack: &str, needles: &[String]| {
        needles
            .iter()
            .find(|needle| !haystack.contains(needle.as_str()))
            .map(|needle| format!("{stream} does not contain {needle:?}"))
    };
    missing("stdout", stdout, &spec.stdout_contains)
        .or_else(|| missing("stderr", stderr, &spec.stderr_contains))
        .map_or(Outcome::Pass, fail)
}

fn fail(reason: impl Into<Failure>) -> Outcome {
    Outcome::Fail(reason.into())
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::ManualSpec;

    fn ctx(signoff_dir: PathBuf) -> RunContext {
        RunContext {
            workdir: std::env::temp_dir(),
            distro_exe: PathBuf::from("/bin/sh"),
            signoff_dir,
            run_root: PathBuf::from("/nonexistent"),
            image_override: None,
        }
    }

    fn command(run: &[&str]) -> CommandSpec {
        CommandSpec {
            run: run.iter().map(|s| (*s).to_owned()).collect(),
            exit: 0,
            stdout_contains: vec![],
            stderr_contains: vec![],
            timeout_secs: 10,
        }
    }

    fn scenario(runner: Runner) -> Scenario {
        Scenario {
            id: "example".into(),
            milestone: "m1".into(),
            title: "Example".into(),
            runner,
            source: PathBuf::from("m1/example.toml"),
        }
    }

    fn run_spec(spec: CommandSpec) -> Outcome {
        run(
            &scenario(Runner::Command(spec)),
            &ctx(PathBuf::from("/nonexistent")),
        )
    }

    #[test]
    fn command_passes_on_expected_exit_and_output() {
        let mut spec = command(&["sh", "-c", "echo hello; echo oops >&2"]);
        spec.stdout_contains = vec!["hello".into()];
        spec.stderr_contains = vec!["oops".into()];

        assert_eq!(run_spec(spec), Outcome::Pass);
    }

    #[test]
    fn command_fails_on_unexpected_exit_code_with_stderr_context() {
        let spec = command(&[
            "sh",
            "-c",
            "echo; echo 'error: no such command' >&2; exit 2",
        ]);

        assert_eq!(
            run_spec(spec),
            Outcome::Fail("exit code 2, expected 0: error: no such command".into())
        );
    }

    #[test]
    fn command_can_expect_a_nonzero_exit() {
        let mut spec = command(&["sh", "-c", "exit 1"]);
        spec.exit = 1;

        assert_eq!(run_spec(spec), Outcome::Pass);
    }

    #[test]
    fn command_fails_when_output_is_missing() {
        let mut spec = command(&["sh", "-c", "echo hello"]);
        spec.stdout_contains = vec!["hello".into(), "world".into()];

        assert_eq!(
            run_spec(spec),
            Outcome::Fail("stdout does not contain \"world\"".into())
        );

        let mut spec = command(&["sh", "-c", "true"]);
        spec.stderr_contains = vec!["warning".into()];

        assert_eq!(
            run_spec(spec),
            Outcome::Fail("stderr does not contain \"warning\"".into())
        );
    }

    #[test]
    fn command_fails_when_program_is_missing() {
        let outcome = run_spec(command(&["/nonexistent/program"]));

        let Outcome::Fail(failure) = outcome else {
            panic!("expected a failure, got {outcome:?}");
        };
        assert!(
            failure
                .reason
                .starts_with("could not start `/nonexistent/program`"),
            "{}",
            failure.reason
        );
    }

    #[test]
    fn command_times_out() {
        let mut spec = command(&["sh", "-c", "sleep 5"]);
        spec.timeout_secs = 0;

        assert_eq!(run_spec(spec), Outcome::Fail("timed out after 0s".into()));
    }

    #[test]
    fn distro_resolves_to_the_running_binary() {
        let mut spec = command(&["distro", "-c", "echo resolved"]);
        spec.stdout_contains = vec!["resolved".into()];

        assert_eq!(run_spec(spec), Outcome::Pass);
    }

    #[test]
    fn manual_scenario_passes_only_with_signoff() {
        let dir = tempfile::tempdir().unwrap();
        let manual = scenario(Runner::Manual(ManualSpec {
            confirm: "It works".into(),
        }));
        let ctx = ctx(dir.path().to_path_buf());

        assert_eq!(
            run(&manual, &ctx),
            Outcome::Pending("awaiting human sign-off".into())
        );

        std::fs::write(dir.path().join("m1-example.toml"), "by = \"human\"\n").unwrap();

        assert_eq!(run(&manual, &ctx), Outcome::Pass);
    }

    #[test]
    fn pending_runner_reports_its_reason() {
        let pending = scenario(Runner::Pending {
            reason: "no `nspawn` runner yet".into(),
        });

        assert_eq!(
            run(&pending, &ctx(PathBuf::from("/nonexistent"))),
            Outcome::Pending("no `nspawn` runner yet".into())
        );
    }

    #[test]
    fn vm_scenario_fails_with_the_build_error_when_the_image_cannot_be_built() {
        let workdir = tempfile::tempdir().unwrap();
        let vm = scenario(Runner::Vm(
            toml::from_str("config = \"configs/x\"\nstore = \"distro\"\n").unwrap(),
        ));
        let mut ctx = ctx(PathBuf::from("/nonexistent"));
        ctx.workdir = workdir.path().to_path_buf();
        ctx.distro_exe = PathBuf::from("/nonexistent/distro");

        let Outcome::Fail(failure) = run(&vm, &ctx) else {
            panic!("expected a failure");
        };
        assert!(
            failure.reason.starts_with(
                "could not run `/nonexistent/distro build --config configs/x --image --test`"
            ),
            "{}",
            failure.reason
        );
        assert_eq!(failure.reproduction, None);
    }
}
