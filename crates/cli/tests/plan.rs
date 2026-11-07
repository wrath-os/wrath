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
    let output = stdout(&distro(&["dev", "test", "--determinism", "modules"]));

    assert!(output.contains("PASS  s4"), "{output}");
    assert!(!output.contains("FAIL"), "{output}");
}

#[test]
fn a_hybrid_system_names_its_nixos_configuration_and_modules() {
    let plan = stdout(&distro(&[
        "plan",
        "--config",
        "fixtures/configs/hybrid",
        "--fresh",
    ]));
    let backends: Vec<_> = plan
        .lines()
        .skip_while(|line| *line != "backends")
        .skip(1)
        .take_while(|line| line.starts_with(' '))
        .collect();

    insta::assert_snapshot!(backends.join("\n"), @r#"
    + nixos (tier: reboot)
        { flake = "/etc/nixos", host = "box", modules = ["/etc/nixos/vm-stubs.nix"], replaces = ["ollama.service"] }
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
      uses system.hostname = "box", set at fixtures/configs/basic/system.toml:2:12
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
        stderr.contains("fixtures/configs/missing: no such configuration directory"),
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

    let failed = distro(&["dev", "test", dir]);
    let stdout = String::from_utf8(failed.stdout).unwrap();
    assert!(!failed.status.success());
    assert!(stdout.contains("FAIL  typo: the error differs"), "{stdout}");

    assert!(distro(&["dev", "test", dir, "--bless"]).status.success());
    let passed = distro(&["dev", "test", dir]);
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
        "dev",
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

/// `fixtures/configs/basic` copied to a temporary directory, with `lock` as its lock file.
fn basic_with_lock(lock: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/configs/basic");
    for file in ["system.toml", "services/web.toml"] {
        let target = dir.path().join(file);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(source.join(file), target).unwrap();
    }
    std::fs::write(dir.path().join("distro.lock"), lock).unwrap();
    dir
}

const LOCK: &str = r#"version = 2

[sources.nixpkgs]
url = "github:NixOS/nixpkgs/nixos-unstable"
rev = "1d06008aaaaaaaaaaaa"
hash = "sha256-abc"

[packages]
nginx = { version = "1.29.1", id = "/nix/store/aaa-nginx-1.29.1" }
unused = { version = "1", id = "/nix/store/bbb-unused-1" }
"#;

#[test]
fn plan_shows_what_the_lock_pins() {
    let config = basic_with_lock(LOCK);
    let plan = stdout(&distro(&[
        "plan",
        "--config",
        config.path().to_str().unwrap(),
        "--against",
        "fixtures/configs/basic",
    ]));

    let body = plan.split_once('\n').unwrap().1;
    insta::assert_snapshot!(body, @"
    packages
      + source nixpkgs (tier: live)
          github:NixOS/nixpkgs/nixos-unstable at 1d06008aaaaa
      ~ nginx (tier: live)
          locked at 1.29.1
    2 changes, tier: live
    ");
}

#[test]
fn a_broken_lock_is_an_error_naming_the_file() {
    let config = basic_with_lock("version = 2\n[sources.nixpkgs]\n");
    let output = distro(&[
        "plan",
        "--config",
        config.path().to_str().unwrap(),
        "--fresh",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("distro.lock: missing field `url`"),
        "{stderr}"
    );
}

#[test]
fn plans_compare_against_a_model_file_like_the_one_a_system_carries() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("model.json");
    std::fs::write(&empty, distro_model::Model::default().canonical()).unwrap();
    let fresh = stdout(&distro(&[
        "plan",
        "--config",
        "fixtures/configs/basic",
        "--fresh",
    ]));

    let plan = stdout(&distro(&[
        "plan",
        "--config",
        "fixtures/configs/basic",
        "--against",
        empty.to_str().unwrap(),
    ]));

    let (model, rest) = plan.split_once('\n').unwrap();
    let (against, body) = rest.split_once('\n').unwrap();
    assert!(fresh.starts_with(model));
    assert_eq!(
        against,
        format!(
            "against {} (model {})",
            empty.display(),
            &distro_model::Model::default().hash()[..16]
        )
    );
    assert_eq!(body, fresh.split_once('\n').unwrap().1);
}

#[test]
fn a_model_file_of_another_format_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("model.json");
    std::fs::write(&old, "{\"version\": 1}\n").unwrap();

    let output = distro(&[
        "plan",
        "--config",
        "fixtures/configs/basic",
        "--against",
        old.to_str().unwrap(),
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("the model is in format version 1"),
        "{stderr}"
    );
}

#[test]
fn check_evaluates_the_configuration_and_says_which_it_is() {
    let output = distro(&["check", "-C", "fixtures/configs/basic"]);

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr, "config fixtures/configs/basic\n");
    let stdout = stdout(&output);
    assert!(stdout.starts_with("no errors: "), "{stdout}");

    let broken = distro(&["check", "-C", "fixtures/bad-configs/wrong-type"]);
    assert!(!broken.status.success());
}

#[test]
fn run_refuses_sources_the_configuration_doesnt_have_before_building() {
    let output = distro(&["run", "-C", "fixtures/configs/basic", "stabel#jq"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("there's no source `stabel`; the configuration has nixpkgs"),
        "{stderr}"
    );
}

#[test]
fn add_and_remove_refuse_what_they_cant_do_before_changing_anything() {
    let config = basic_with_lock(LOCK);
    let dir = config.path().to_str().unwrap();
    let before = std::fs::read_to_string(config.path().join("system.toml")).unwrap();

    let unknown = distro(&["add", "-C", dir, "stabel#jq"]);
    assert!(!unknown.status.success());
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(stderr.contains("there's no source `stabel`"), "{stderr}");

    let nobody = distro(&["add", "-C", dir, "--home", "nobody", "jq"]);
    let stderr = String::from_utf8_lossy(&nobody.stderr);
    assert!(stderr.contains("there's no user `nobody`"), "{stderr}");

    let missing = distro(&["remove", "-C", dir, "jq"]);
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("`jq` isn't in packages"), "{stderr}");

    let after = std::fs::read_to_string(config.path().join("system.toml")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn why_explains_files_and_packages_by_the_module_and_options_behind_them() {
    let file = stdout(&distro(&[
        "why",
        "-C",
        "fixtures/configs/basic",
        "/etc/nginx/main.conf",
    ]));
    insta::assert_snapshot!(file, @r#"
    /etc/nginx/main.conf is written by module `nginx`
      from what the configuration sets:
        services.nginx.main.enable = true, set at fixtures/configs/basic/services/web.toml:2:10
        services.nginx.main.port = 8080, set at fixtures/configs/basic/services/web.toml:3:8
        services.nginx.main.sites.blog.domains = ["blog.example.com", "box.lan"], set at fixtures/configs/basic/services/web.toml:6:11
        services.nginx.main.sites.blog.root = "/srv/blog", set at fixtures/configs/basic/services/web.toml:7:8
    "#);

    let package = stdout(&distro(&[
        "why",
        "-C",
        "fixtures/configs/basic",
        "nftables",
    ]));
    assert!(package.starts_with("nftables is installed for everyone by module `firewall`"));

    let unknown = distro(&["why", "-C", "fixtures/configs/basic", "/etc/nope"]);
    assert!(!unknown.status.success());
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(stderr.contains("no module writes `/etc/nope`"), "{stderr}");
}

#[test]
fn json_is_the_plan_as_data_and_refused_where_it_isnt_supported() {
    let plan = stdout(&distro(&[
        "plan",
        "--json",
        "-C",
        "fixtures/configs/basic-changed",
        "--against",
        "fixtures/configs/basic",
    ]));
    let plan: serde_json::Value = serde_json::from_str(&plan).unwrap();
    assert_eq!(plan["tier"], "live");
    assert_eq!(plan["changes"][0]["name"], "services.nginx.main.port");
    assert_eq!(plan["changes"][0]["details"][0], "8080 -> 8081");

    let unsupported = distro(&["why", "--json", "-C", "fixtures/configs/basic", "--all"]);
    assert!(!unsupported.status.success());
}
