use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn wrath(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wrath"))
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
        "title = \"wrath reports its version\"\n[command]\nrun = [\"wrath\", \"--version\"]\nstdout_contains = [\"wrath\"]\n",
    );
    write(
        &acceptance.join("m2/frobnicate.toml"),
        "title = \"frobnicate exists\"\n[command]\nrun = [\"wrath\", \"frobnicate\"]\n",
    );
    dir
}

/// `wrath dev test` on `dir` under the fixture, such as `acceptance` or `acceptance/m2`.
fn test(fixture: &Path, dir: &str) -> Output {
    let root = fixture.to_str().unwrap();
    wrath(&["dev", "test", &format!("{root}/{dir}"), "--root", root])
}

#[test]
fn acceptance_runs_scenarios_and_prints_scoreboard() {
    let dir = fixture();

    let out = test(dir.path(), "acceptance");

    // Scenarios of milestones not done yet fail without failing the run.
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
fn acceptance_runs_one_milestone_by_its_directory() {
    let dir = fixture();

    let out = test(dir.path(), "acceptance/m2");

    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(!stdout.contains("M1"), "{stdout}");
    assert!(stdout.ends_with("0/1 scenarios passing\n"), "{stdout}");
}

#[test]
fn acceptance_refuses_a_directory_that_is_no_milestone() {
    let dir = fixture();
    fs::create_dir(dir.path().join("acceptance/m9")).unwrap();

    let out = test(dir.path(), "acceptance/m9");

    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.ends_with("acceptance/m9: directory is not a milestone listed in milestones.toml\n"),
        "{stderr}"
    );
}

#[test]
fn acceptance_reports_invalid_scenarios() {
    let dir = fixture();
    write(
        &dir.path().join("acceptance/m1/broken.toml"),
        "title = \"no runner\"\n",
    );

    let out = test(dir.path(), "acceptance");

    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(
        stderr.ends_with(
            "broken.toml: no runner: add one of [command], [manual], [vm] or [nspawn]\n"
        ),
        "{stderr}"
    );
}
