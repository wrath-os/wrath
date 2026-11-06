use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn distro(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_distro"))
        .args(args)
        .output()
        .unwrap()
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let acceptance = dir.path().join("acceptance");
    write(
        &acceptance.join("milestones.toml"),
        "[[milestone]]\nid = \"m1\"\ntitle = \"First\"\n\n[[milestone]]\nid = \"m2\"\ntitle = \"Second\"\n",
    );
    write(
        &acceptance.join("m1/version.toml"),
        "title = \"distro reports its version\"\n[command]\nrun = [\"distro\", \"--version\"]\nstdout_contains = [\"distro\"]\n",
    );
    write(
        &acceptance.join("m2/frobnicate.toml"),
        "title = \"frobnicate exists\"\n[command]\nrun = [\"distro\", \"frobnicate\"]\n",
    );
    dir
}

#[test]
fn status_runs_scenarios_and_prints_scoreboard() {
    let dir = fixture();

    let out = distro(&["dev", "status", "--root", dir.path().to_str().unwrap()]);

    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("M1  First  1/1"), "{stdout}");
    assert!(stdout.contains("PASS  version"), "{stdout}");
    assert!(stdout.contains("FAIL  frobnicate"), "{stdout}");
    assert!(
        stdout.contains("unrecognized subcommand 'frobnicate'"),
        "{stdout}"
    );
    assert!(stdout.ends_with("1/2 scenarios passing\n"), "{stdout}");
}

#[test]
fn status_filters_by_milestone() {
    let dir = fixture();

    let out = distro(&[
        "dev",
        "status",
        "--root",
        dir.path().to_str().unwrap(),
        "--milestone",
        "m2",
    ]);

    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(!stdout.contains("M1"), "{stdout}");
    assert!(stdout.ends_with("0/1 scenarios passing\n"), "{stdout}");
}

#[test]
fn status_rejects_unknown_milestone() {
    let dir = fixture();

    let out = distro(&[
        "dev",
        "status",
        "--root",
        dir.path().to_str().unwrap(),
        "--milestone",
        "m9",
    ]);

    assert!(!out.status.success());
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "error: unknown milestone `m9`; known milestones: m1, m2\n"
    );
}

#[test]
fn status_reports_invalid_scenarios() {
    let dir = fixture();
    write(
        &dir.path().join("acceptance/m1/broken.toml"),
        "title = \"no runner\"\n",
    );

    let out = distro(&["dev", "status", "--root", dir.path().to_str().unwrap()]);

    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.ends_with(
            "broken.toml: no runner: add one of [command], [manual], [vm] or [nspawn]\n"
        ),
        "{stderr}"
    );
}
