//! Running one step of a build in a rootless sandbox. The sandbox's root is empty apart from what
//! the step declares, it has no network, and its environment is fixed.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::progress::{self, Mode, Tail};
use crate::store::{Lab, StorePath, with_path};

/// A composed view, mounted at `/usr` in the sandbox.
#[derive(Debug, Clone)]
pub struct View {
    pub dir: PathBuf,
}

/// How long a step may take, when there's a limit at all, and how long it may go without
/// printing anything, which is how a step that hangs is told from one that takes a while.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub total: Option<Duration>,
    pub silence: Duration,
}

impl Limits {
    /// No limit on time, and an hour without output.
    pub const DEFAULT: Self = Self {
        total: None,
        silence: Duration::from_secs(60 * 60),
    };
}

#[derive(Debug, Clone)]
pub struct Step {
    pub name: String,
    pub argv: Vec<String>,
    /// Store paths the step can read.
    pub inputs: Vec<StorePath>,
    /// The store path the step writes.
    pub output: Option<StorePath>,
    pub view: Option<View>,
    /// A directory on the host, shown as `/build`, where the step runs.
    pub build_dir: PathBuf,
    /// The file that gets everything the step prints.
    pub log: PathBuf,
    /// Variables on top of the fixed environment.
    pub env: BTreeMap<String, String>,
    pub limits: Limits,
    /// Links to make in the sandbox, as (where it points, the link).
    pub links: Vec<(String, String)>,
    /// A path the step sees somewhere else, as (the path, where it's mounted): a package being
    /// built, at the path its build saw, as an output and when its tests read it.
    pub at: Option<(StorePath, StorePath)>,
}

impl Step {
    /// Where the step sees `path`.
    fn inside(&self, path: &StorePath) -> String {
        match &self.at {
            Some((from, to)) if from == path => to.inside(),
            _ => path.inside(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Passed,
    Failed,
    Timeout,
    Silent,
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub name: String,
    pub status: Status,
    pub exit_code: Option<i32>,
    pub milliseconds: u128,
}

/// Who a build runs as, which a build shouldn't be able to tell from any other.
#[derive(Debug, Clone)]
pub struct Identity {
    pub uid: u32,
    pub hostname: String,
    pub jobs: u32,
}

static IDENTITY: Mutex<Option<Identity>> = Mutex::new(None);

impl Identity {
    pub fn standard() -> Self {
        Self {
            uid: 1000,
            hostname: "build".to_owned(),
            jobs: 32,
        }
    }

    /// The identity builds run with now.
    pub fn current() -> Self {
        IDENTITY
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(Self::standard)
    }

    /// Makes the builds from now on run as this one.
    pub fn set(identity: Identity) {
        *IDENTITY.lock().unwrap() = Some(identity);
    }
}

const PASSWD: &str = "root:x:0:0::/build:/bin/sh\nbuild:x:1000:1000::/build:/bin/sh\n";
const GROUP: &str = "root:x:0:\nbuild:x:1000:\n";
const HOSTS: &str = "127.0.0.1 localhost\n::1 localhost\n";

/// The environment every step starts from.
const FIXED_ENVIRONMENT: [(&str, &str); 8] = [
    ("HOME", "/build"),
    ("LANG", "C"),
    ("LC_ALL", "C"),
    ("PATH", "/usr/bin"),
    ("SHELL", "/bin/sh"),
    ("SOURCE_DATE_EPOCH", "1"),
    ("TERM", "dumb"),
    ("TZ", "UTC"),
];

fn arg(args: &mut Vec<String>, parts: &[&str]) {
    args.extend(parts.iter().map(|part| (*part).to_owned()));
}

fn host(path: &Path) -> String {
    path.display().to_string()
}

/// What to pass `bwrap` for `step`.
fn bwrap_args(lab: &Lab, step: &Step, etc: &Path) -> Vec<String> {
    let mut args = Vec::new();
    let identity = Identity::current();
    let uid = identity.uid.to_string();
    arg(
        &mut args,
        &[
            "--unshare-all",
            "--die-with-parent",
            "--new-session",
            "--clearenv",
            "--uid",
            &uid,
            "--gid",
            &uid,
            "--hostname",
            &identity.hostname,
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--tmpfs",
            "/tmp",
            "--dir",
            "/store",
            "--dir",
            "/etc",
            "--ro-bind",
            &host(&etc.join("passwd")),
            "/etc/passwd",
            "--ro-bind",
            &host(&etc.join("group")),
            "/etc/group",
            "--ro-bind",
            &host(&etc.join("hosts")),
            "/etc/hosts",
            "--bind",
            &host(&step.build_dir),
            "/build",
            "--chdir",
            "/build",
        ],
    );
    for input in &step.inputs {
        arg(
            &mut args,
            &["--ro-bind", &host(&lab.on_host(input)), &step.inside(input)],
        );
    }
    if let Some(output) = &step.output {
        arg(
            &mut args,
            &["--bind", &host(&lab.on_host(output)), &step.inside(output)],
        );
    }
    if let Some(view) = &step.view {
        arg(&mut args, &["--ro-bind", &host(&view.dir), "/usr"]);
        arg(
            &mut args,
            &[
                "--dir",
                "/bin",
                "--symlink",
                "/usr/bin/bash",
                "/bin/sh",
                "--symlink",
                "/usr/bin/bash",
                "/bin/bash",
            ],
        );
    }
    for (target, link) in &step.links {
        arg(&mut args, &["--symlink", target, link]);
    }
    for (name, value) in FIXED_ENVIRONMENT {
        arg(&mut args, &["--setenv", name, value]);
    }
    for (name, value) in &step.env {
        arg(&mut args, &["--setenv", name, value]);
    }
    args.push("--".to_owned());
    args.extend(step.argv.iter().cloned());
    args
}

/// Where what a step prints goes: its log, the time it last printed, and its last line.
#[derive(Clone)]
struct Output {
    log: Arc<Mutex<File>>,
    last: Arc<Mutex<Instant>>,
    tail: Arc<Mutex<Tail>>,
    /// Whether it's also copied to stderr, as `--verbose` asks.
    echo: bool,
}

/// Copies `from` to the log, noting the time of everything read and its last line.
fn pump(mut from: impl Read + Send + 'static, output: Output) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0; 8192];
        while let Ok(count) = from.read(&mut buffer) {
            if count == 0 {
                break;
            }
            let read = &buffer[..count];
            *output.last.lock().unwrap() = Instant::now();
            output.tail.lock().unwrap().push(read);
            let _ = output.log.lock().unwrap().write_all(read);
            if output.echo {
                let _ = std::io::stderr().lock().write_all(read);
            }
        }
    })
}

/// Runs `step` and writes what happened next to its log, as `<log>.json`.
pub fn run(lab: &Lab, step: &Step) -> Result<Outcome, String> {
    run_shown(lab, step, None)
}

/// Runs `step` as [`run`] does, showing it on stderr by `verbs`, such as `("build", "built")` for
/// while it runs and once it has, the way the progress mode says: a line as it starts, a status
/// line kept up to date, or all it prints.
pub fn run_shown(lab: &Lab, step: &Step, verbs: Option<(&str, &str)>) -> Result<Outcome, String> {
    let mode = progress::mode();
    let live = verbs.filter(|_| mode == Mode::Live);
    if let Some((verb, _)) = verbs.filter(|_| mode != Mode::Live) {
        eprintln!("  {verb}  {}", step.name);
    }
    let etc = lab.work.join("sandbox-etc");
    fs::create_dir_all(&etc).map_err(with_path(&etc))?;
    let uid = Identity::current().uid.to_string();
    for (name, text) in [("passwd", PASSWD), ("group", GROUP), ("hosts", HOSTS)] {
        fs::write(etc.join(name), text.replace("1000", &uid))
            .map_err(|e| format!("{name}: {e}"))?;
    }
    fs::create_dir_all(&step.build_dir).map_err(with_path(&step.build_dir))?;
    if let Some(output) = &step.output {
        let dir = lab.on_host(output);
        fs::create_dir_all(&dir).map_err(with_path(&dir))?;
    }
    if let Some(parent) = step.log.parent() {
        fs::create_dir_all(parent).map_err(with_path(parent))?;
    }
    let log = File::create(&step.log).map_err(with_path(&step.log))?;
    let log = Arc::new(Mutex::new(log));

    crate::store::usual_umask();
    let start = Instant::now();
    let mut child = Command::new("bwrap")
        .args(bwrap_args(lab, step, &etc))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            format!(
                "cannot run bwrap: {e}; builds run in a sandbox that bubblewrap makes, so it \
                 needs to be installed"
            )
        })?;
    let output = Output {
        log,
        last: Arc::new(Mutex::new(Instant::now())),
        tail: Arc::default(),
        echo: verbs.is_some() && mode == Mode::Verbose,
    };
    let pumps = [
        pump(child.stdout.take().expect("piped"), output.clone()),
        pump(child.stderr.take().expect("piped"), output.clone()),
    ];
    let width = progress::width();
    let mut drawn: Option<Instant> = None;

    let (status, exit_code) = loop {
        if let Some(exit) = child.try_wait().map_err(|e| format!("waiting: {e}"))? {
            let status = if exit.success() {
                Status::Passed
            } else {
                Status::Failed
            };
            break (status, exit.code());
        }
        let stopped = if step
            .limits
            .total
            .is_some_and(|total| start.elapsed() > total)
        {
            Some(Status::Timeout)
        } else if output.last.lock().unwrap().elapsed() > step.limits.silence {
            Some(Status::Silent)
        } else {
            None
        };
        if let Some(status) = stopped {
            // Everything else in the sandbox goes with it, through --die-with-parent.
            let _ = child.kill();
            let _ = child.wait();
            break (status, None);
        }
        if let Some((verb, _)) = live
            && drawn.is_none_or(|at| at.elapsed() >= Duration::from_millis(250))
        {
            let tail = output.tail.lock().unwrap().line().to_owned();
            let line = progress::status(verb, &step.name, start.elapsed(), &tail, width);
            eprint!("\r\x1b[K{line}");
            drawn = Some(Instant::now());
        }
        thread::sleep(Duration::from_millis(50));
    };
    for pump in pumps {
        let _ = pump.join();
    }
    if let Some((_, done)) = verbs.filter(|_| mode != Mode::Lines) {
        let how = match status {
            Status::Passed => String::new(),
            other => format!("  {other:?}").to_lowercase(),
        };
        let clear = if live.is_some() { "\r\x1b[K" } else { "" };
        let time = progress::clock(start.elapsed());
        eprintln!("{clear}  {done}  {}  {time}{how}", step.name);
    }

    let outcome = Outcome {
        name: step.name.clone(),
        status,
        exit_code,
        milliseconds: start.elapsed().as_millis(),
    };
    let record = PathBuf::from(format!("{}.json", step.log.display()));
    let json = serde_json::to_string_pretty(&outcome).map_err(|e| e.to_string())?;
    fs::write(&record, json + "\n").map_err(with_path(&record))?;
    Ok(outcome)
}
