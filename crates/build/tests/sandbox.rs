//! The gate for step 1: a step sees only what it declares, and can't reach the network.

use std::collections::BTreeMap;
use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use distro_build::sandbox::{self, Limits, Mount, Outcome, Status, Step, View};
use distro_build::seed;
use distro_build::store::{Lab, StorePath};
use distro_build::view;

fn lab() -> Lab {
    Lab::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../build/work"))
}

struct Fixture {
    lab: Lab,
    seed: StorePath,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let lab = lab();
        let seed = seed::prepare(&lab).unwrap();
        let dir = tempfile::tempdir().unwrap();
        Self { lab, seed, dir }
    }

    fn view(&self, mount: Mount) -> View {
        let dir = self.dir.path().join("view");
        if !dir.exists() {
            view::compose(&self.lab, &dir, &[("seed", &self.seed)]).unwrap();
        }
        View { dir, mount }
    }

    fn build_dir(&self) -> PathBuf {
        self.dir.path().join("build")
    }

    fn step(&self, script: &str) -> Step {
        Step {
            name: "test".to_owned(),
            argv: vec!["/bin/sh".to_owned(), "-euc".to_owned(), script.to_owned()],
            inputs: vec![self.seed.clone()],
            output: None,
            view: Some(self.view(Mount::Usr)),
            build_dir: self.build_dir(),
            log: self.dir.path().join("log"),
            env: BTreeMap::new(),
            limits: Limits {
                total: Duration::from_secs(60),
                silence: Duration::from_secs(60),
            },
            links: Vec::new(),
        }
    }

    fn run(&self, step: &Step) -> (Outcome, String) {
        let outcome = sandbox::run(&self.lab, step).unwrap();
        (outcome, fs::read_to_string(&step.log).unwrap())
    }

    /// Runs `script`, which has to succeed, and gives what it printed.
    fn output(&self, script: &str) -> String {
        let (outcome, log) = self.run(&self.step(script));
        assert_eq!(outcome.status, Status::Passed, "{log}");
        log
    }
}

fn store_entry(lab: &Lab, name: &str) -> StorePath {
    let path = StorePath::derive("sandbox test", &[name.as_bytes()]);
    let dir = lab.on_host(&path);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("file"), name).unwrap();
    path
}

#[test]
fn the_seed_runs_inside_the_sandbox() {
    let fixture = Fixture::new();

    assert!(fixture.output("gcc --version").contains("15.2"));
}

#[test]
fn a_step_can_read_what_it_declares_and_not_change_it() {
    let fixture = Fixture::new();
    let input = store_entry(&fixture.lab, "declared");
    let mut step = fixture.step(&format!(
        "cat {0}/file; if touch {0}/new 2>/dev/null; then echo writable; else echo read-only; fi",
        input.inside()
    ));
    step.inputs.push(input);

    let (outcome, log) = fixture.run(&step);

    assert_eq!(outcome.status, Status::Passed, "{log}");
    assert_eq!(log, "declaredread-only\n");
}

#[test]
fn a_store_path_nobody_declared_is_not_there() {
    let fixture = Fixture::new();
    let hidden = store_entry(&fixture.lab, "undeclared");
    assert!(fixture.lab.on_host(&hidden).join("file").exists());

    let log = fixture.output(&format!(
        "test -e {} || echo absent; ls /store",
        hidden.inside()
    ));

    assert!(log.starts_with("absent\n"), "{log}");
    assert_eq!(
        log.trim_start_matches("absent\n").trim(),
        fixture.seed.hash()
    );
}

#[test]
fn nothing_from_the_host_is_visible() {
    let fixture = Fixture::new();
    let canary = fixture.dir.path().join("canary");
    fs::write(&canary, "secret").unwrap();

    let log = fixture.output(&format!(
        "for path in {} /nix /home /run /root /var /sys /etc/shadow; do \
           if test -e $path; then echo found $path; fi; \
         done; echo checked",
        canary.display()
    ));

    assert_eq!(log, "checked\n");
}

#[test]
fn the_host_environment_does_not_reach_a_step() {
    let fixture = Fixture::new();
    // cargo sets this for every test it runs.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());

    let log = fixture.output("env | sort");

    assert!(!log.contains("CARGO_MANIFEST_DIR"), "{log}");
    assert!(log.contains("SOURCE_DATE_EPOCH=1\n"), "{log}");
    assert!(log.contains("PATH=/usr/bin\n"), "{log}");
    assert!(log.contains("TZ=UTC\n"), "{log}");
}

#[test]
fn a_step_has_no_network_and_cannot_reach_the_hosts_loopback() {
    let fixture = Fixture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    TcpStream::connect(("127.0.0.1", port)).expect("reachable from outside the sandbox");

    let log = fixture.output(&format!(
        "if (exec 3<>/dev/tcp/127.0.0.1/{port}) 2>/dev/null; then echo loopback; fi; \
         if (exec 3<>/dev/tcp/1.1.1.1/53) 2>/dev/null; then echo internet; fi; \
         cut -d: -s -f1 /proc/net/dev | tr -d ' '"
    ));

    assert_eq!(log, "lo\n");
}

#[test]
fn a_step_writes_only_its_output_and_build_directory() {
    let fixture = Fixture::new();
    let output = StorePath::derive("sandbox test output", &[b"output"]);
    let mut step = fixture.step(&format!(
        "echo made > {}/made; echo here > /build/here; \
         if touch /usr/new 2>/dev/null; then echo usr-writable; fi; \
         if touch {}/new 2>/dev/null; then echo seed-writable; fi",
        output.inside(),
        fixture.seed.inside()
    ));
    step.output = Some(output.clone());

    let (outcome, log) = fixture.run(&step);

    assert_eq!(outcome.status, Status::Passed, "{log}");
    assert_eq!(log, "");
    assert_eq!(
        fs::read_to_string(fixture.lab.on_host(&output).join("made")).unwrap(),
        "made\n"
    );
    assert_eq!(
        fs::read_to_string(fixture.build_dir().join("here")).unwrap(),
        "here\n"
    );
}

#[test]
fn a_sentinel_view_is_at_a_store_length_path_and_usr_links_to_it() {
    let fixture = Fixture::new();
    let sentinel = StorePath::derive("sentinel", &[b"view"]);
    let mut step = fixture.step(&format!(
        "readlink /usr; {}/bin/gcc --version",
        sentinel.inside()
    ));
    step.view = Some(fixture.view(Mount::Sentinel(sentinel.clone())));

    let (outcome, log) = fixture.run(&step);

    assert_eq!(outcome.status, Status::Passed, "{log}");
    assert!(
        log.starts_with(&format!("{}\n", sentinel.inside())),
        "{log}"
    );
    assert_eq!(sentinel.inside().len(), fixture.seed.inside().len());
}

#[test]
fn a_failing_step_is_recorded_with_its_exit_code() {
    let fixture = Fixture::new();
    let step = fixture.step("exit 3");

    let (outcome, _) = fixture.run(&step);

    assert_eq!(outcome.status, Status::Failed);
    assert_eq!(outcome.exit_code, Some(3));
    let record = fs::read_to_string(format!("{}.json", step.log.display())).unwrap();
    assert!(record.contains("\"status\": \"failed\""), "{record}");
}

#[test]
fn a_step_past_its_time_limit_is_stopped_with_everything_it_started() {
    let fixture = Fixture::new();
    let mut step = fixture.step("(sleep 3; touch /build/late) & echo running; sleep 100");
    step.limits.total = Duration::from_secs(1);
    let started = Instant::now();

    let (outcome, _) = fixture.run(&step);

    assert_eq!(outcome.status, Status::Timeout);
    assert!(started.elapsed() < Duration::from_secs(10));
    thread::sleep(Duration::from_secs(4));
    assert!(!fixture.build_dir().join("late").exists());
}

#[test]
fn a_silent_step_is_stopped_but_a_slow_chatty_one_is_not() {
    let fixture = Fixture::new();
    let mut silent = fixture.step("echo started; sleep 100");
    silent.limits.silence = Duration::from_secs(1);
    let (outcome, _) = fixture.run(&silent);
    assert_eq!(outcome.status, Status::Silent);

    let mut chatty = fixture.step("for i in 1 2 3 4 5 6; do echo $i; sleep 0.5; done");
    chatty.limits.silence = Duration::from_secs(2);
    let (outcome, log) = fixture.run(&chatty);
    assert_eq!(outcome.status, Status::Passed, "{log}");
}
