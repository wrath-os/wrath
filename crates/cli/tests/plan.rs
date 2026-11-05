use std::path::Path;
use std::process::{Command, Output};

/// Runs `distro` from the repository root, so fixture and module paths are relative to it.
fn distro(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_distro"))
        .args(args)
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn plan(modules: &str) -> String {
    stdout(&distro(&[
        "plan",
        "--config",
        "fixtures/configs/basic-changed",
        "--against",
        "fixtures/configs/basic",
        "--modules",
        modules,
    ]))
}

#[test]
fn plan_shows_the_changed_option_and_what_it_renders() {
    let plan = plan("modules");

    let body = plan.split_once('\n').unwrap().1;
    insta::assert_snapshot!(body, @"
    options
      ~ services.nginx.main.port: 8080 -> 8081
    files
      ~ /etc/nginx/main.conf (tier: live)
    units
      ~ nginx-main.service (tier: live)
          restarts: /etc/nginx/main.conf changed
    3 changes, tier: live
    ");
}

#[test]
fn modules_render_the_same_model_in_any_load_order() {
    let output = stdout(&distro(&["check", "--determinism", "modules"]));

    assert!(output.contains("PASS  s4"), "{output}");
    assert!(!output.contains("FAIL"), "{output}");
}

#[test]
fn a_hybrid_system_names_its_nixos_configuration_and_modules() {
    let plan = stdout(&distro(&["plan", "--config", "fixtures/configs/hybrid"]));
    let backends: Vec<_> = plan
        .lines()
        .skip_while(|line| *line != "backends")
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .collect();

    insta::assert_snapshot!(backends.join("\n"), @r#"
    + nixos (tier: reboot)
        { flake = "/etc/nixos", host = "box", modules = ["/etc/nixos/vm-stubs.nix"] }
    "#);
}

#[test]
fn why_names_the_file_and_line() {
    let why = stdout(&distro(&[
        "why",
        "--config",
        "fixtures/configs/basic",
        "services.nginx.main.sites.blog",
    ]));

    insta::assert_snapshot!(why, @r#"
    services.nginx.main.sites.blog.domains = ["blog.example.com", "box.lan"]
      set at fixtures/configs/basic/services/web.toml:6:11
      overrides [], default
      uses system.hostname at fixtures/configs/basic/services/web.toml:6:33
    services.nginx.main.sites.blog.root = "/srv/blog"
      set at fixtures/configs/basic/services/web.toml:7:8
    "#);
}

#[test]
fn errors_go_to_stderr_with_a_failing_exit() {
    let output = distro(&["why", "--config", "fixtures/configs/missing", "--all"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("fixtures/configs/missing/system.toml` does not exist"),
        "{stderr}"
    );
}

#[test]
fn why_needs_an_option_or_all() {
    let output = distro(&["why", "--config", "fixtures/configs/basic"]);

    assert!(!output.status.success());
}

#[test]
fn a_bad_config_whose_error_changed_fails_until_blessed() {
    let dir = tempfile::tempdir().unwrap();
    let case = dir.path().join("typo");
    std::fs::create_dir_all(&case).unwrap();
    std::fs::write(case.join("system.toml"), "[system]\nhostnme = \"box\"\n").unwrap();
    std::fs::write(case.join("expected-error.txt"), "an older message\n").unwrap();
    let dir = dir.path().to_str().unwrap();

    let failed = distro(&["check", dir]);
    let stdout = String::from_utf8(failed.stdout).unwrap();
    assert!(!failed.status.success());
    assert!(stdout.contains("FAIL  typo: the error differs"), "{stdout}");

    assert!(distro(&["check", dir, "--bless"]).status.success());
    let passed = distro(&["check", dir]);
    assert!(passed.status.success());
    let expected = std::fs::read_to_string(case.join("expected-error.txt")).unwrap();
    assert!(
        expected.starts_with("error: unknown option `system.hostnme`"),
        "{expected}"
    );
}

#[test]
fn bench_measures_plan_time() {
    let output = stdout(&distro(&[
        "bench",
        "--synthetic",
        "4",
        "--runs",
        "1",
        "--budget-ms",
        "60000",
    ]));

    assert!(output.starts_with("PASS "), "{output}");
    assert!(
        output.contains(" modules: plan after a one-line change takes"),
        "{output}"
    );
}
