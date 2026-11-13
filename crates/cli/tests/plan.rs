use std::path::Path;
use std::process::{Command, Output};

/// Runs `wrath` from the repository root, so fixture and module paths are relative to it.
fn wrath(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wrath"))
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
    stdout(&wrath(&[
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
    let output = stdout(&wrath(&["dev", "test", "--determinism", "modules"]));

    assert!(output.contains("PASS  s4"), "{output}");
    assert!(!output.contains("FAIL"), "{output}");
}

#[test]
fn why_names_the_file_and_line() {
    let why = stdout(&wrath(&[
        "why",
        "--config",
        "fixtures/configs/basic",
        "services.nginx.main.sites.blog",
    ]));

    insta::assert_snapshot!(why, @r#"
    services.nginx.main.sites.blog.domains = ["blog.example.com", "box.lan"]
      set at fixtures/configs/basic/services/web.toml:6:11
      overrides [], default
      uses system.hostname = "box", set at fixtures/configs/basic/system.toml:4:12
    services.nginx.main.sites.blog.root = "/srv/blog"
      set at fixtures/configs/basic/services/web.toml:7:8
    "#);
}

#[test]
fn errors_go_to_stderr_with_a_failing_exit() {
    let output = wrath(&["why", "--config", "fixtures/configs/missing", "--all"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("fixtures/configs/missing: no such configuration directory"),
        "{stderr}"
    );
}

#[test]
fn why_needs_an_option_or_all() {
    let output = wrath(&["why", "--config", "fixtures/configs/basic"]);

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

    let failed = wrath(&["dev", "test", dir]);
    let stdout = String::from_utf8(failed.stdout).unwrap();
    assert!(!failed.status.success());
    assert!(stdout.contains("FAIL  typo: the error differs"), "{stdout}");

    assert!(wrath(&["dev", "test", dir, "--bless"]).status.success());
    let passed = wrath(&["dev", "test", dir]);
    assert!(passed.status.success());
    let expected = std::fs::read_to_string(case.join("expected-error.txt")).unwrap();
    assert!(
        expected.starts_with("error: unknown option `system.hostnme`"),
        "{expected}"
    );
}

#[test]
fn the_modules_write_what_their_cases_expect() {
    let output = wrath(&["dev", "test", "modules/tests"]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("PASS  firewall"), "{stdout}");
}

#[test]
fn a_file_a_case_expects_fails_until_blessed_and_missing_ones_always_fail() {
    let dir = tempfile::tempdir().unwrap();
    let case = dir.path().join("firewall");
    std::fs::create_dir_all(case.join("expected/etc")).unwrap();
    let config = "[system]\nhostname = \"box\"\n[networking.firewall]\nallowed_tcp_ports = [22]\n";
    std::fs::write(case.join("system.toml"), config).unwrap();
    std::fs::write(case.join("expected/etc/nftables.conf"), "old\n").unwrap();
    let dir = dir.path().to_str().unwrap();

    let failed = wrath(&["dev", "test", dir]);
    let stdout = String::from_utf8(failed.stdout).unwrap();
    assert!(!failed.status.success());
    assert!(
        stdout.contains("FAIL  firewall: etc/nftables.conf differs"),
        "{stdout}"
    );

    assert!(wrath(&["dev", "test", dir, "--bless"]).status.success());
    assert!(wrath(&["dev", "test", dir]).status.success());
    let blessed = std::fs::read_to_string(case.join("expected/etc/nftables.conf")).unwrap();
    assert!(blessed.contains("tcp dport 22 accept"), "{blessed}");

    std::fs::write(case.join("expected/etc/nothing"), "").unwrap();
    let missing = wrath(&["dev", "test", dir, "--bless"]);
    let stdout = String::from_utf8(missing.stdout).unwrap();
    assert!(
        !missing.status.success(),
        "blessing can't make a file appear"
    );
    assert!(stdout.contains("doesn't write etc/nothing"), "{stdout}");
}

#[test]
fn bench_measures_plan_time() {
    let output = stdout(&wrath(&[
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

/// `fixtures/configs/NAME` copied to a temporary directory, for commands that write its lock.
fn copied(name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/configs")
        .join(name);
    let status = Command::new("cp")
        .arg("-r")
        .arg(source.join("."))
        .arg(dir.path())
        .status()
        .unwrap();
    assert!(status.success());
    dir
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
    std::fs::write(dir.path().join("wrath.lock"), lock).unwrap();
    dir
}

const LOCK: &str = r#"version = 2

[sources.wrath]
url = "github:johnae/wrath"
rev = "1d06008aaaaaaaaaaaa"
hash = "sha256-abc"

[packages]
nginx = { version = "1.29.1", id = "/nix/store/aaa-nginx-1.29.1" }
unused = { version = "1", id = "/nix/store/bbb-unused-1" }
"#;

#[test]
fn plan_shows_what_the_lock_pins() {
    let config = basic_with_lock(LOCK);
    let plan = stdout(&wrath(&[
        "plan",
        "--config",
        config.path().to_str().unwrap(),
        "--against",
        "fixtures/configs/basic",
    ]));

    let body = plan.split_once('\n').unwrap().1;
    insta::assert_snapshot!(body, @"
    packages
      + source wrath (tier: live)
          github:johnae/wrath at 1d06008aaaaa
    1 change, tier: live
    ");
}

#[test]
fn a_broken_lock_is_an_error_naming_the_file() {
    let config = basic_with_lock("version = 2\n[sources.nixpkgs]\n");
    let output = wrath(&[
        "plan",
        "--config",
        config.path().to_str().unwrap(),
        "--fresh",
    ]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("wrath.lock: missing field `url`"),
        "{stderr}"
    );
}

#[test]
fn plans_compare_against_a_model_file_like_the_one_a_system_carries() {
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("model.json");
    std::fs::write(&empty, wrath_model::Model::default().canonical()).unwrap();
    let fresh = stdout(&wrath(&[
        "plan",
        "--config",
        "fixtures/configs/basic",
        "--fresh",
    ]));

    let plan = stdout(&wrath(&[
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
            &wrath_model::Model::default().hash()[..16]
        )
    );
    assert_eq!(body, fresh.split_once('\n').unwrap().1);
}

#[test]
fn a_model_file_of_another_format_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("model.json");
    std::fs::write(&old, "{\"version\": 1}\n").unwrap();

    let output = wrath(&[
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
    let basic = copied("basic");
    let config = basic.path().to_str().unwrap();
    let output = wrath(&["check", "-C", config]);

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr, format!("config {config}\n"));
    let stdout = stdout(&output);
    assert!(stdout.starts_with("no errors: "), "{stdout}");

    let broken = wrath(&["check", "-C", "fixtures/bad-configs/wrong-type"]);
    assert!(!broken.status.success());
}

#[test]
fn run_refuses_sources_the_configuration_doesnt_have_before_building() {
    let output = wrath(&["run", "-C", "fixtures/configs/basic", "stabel#jq"]);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("there's no source `stabel`; the configuration has wrath"),
        "{stderr}"
    );
}

#[test]
fn add_and_remove_refuse_what_they_cant_do_before_changing_anything() {
    let config = basic_with_lock(LOCK);
    let dir = config.path().to_str().unwrap();
    let before = std::fs::read_to_string(config.path().join("system.toml")).unwrap();

    let unknown = wrath(&["add", "-C", dir, "stabel#jq"]);
    assert!(!unknown.status.success());
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(stderr.contains("there's no source `stabel`"), "{stderr}");

    let nobody = wrath(&["add", "-C", dir, "--home", "nobody", "jq"]);
    let stderr = String::from_utf8_lossy(&nobody.stderr);
    assert!(stderr.contains("there's no user `nobody`"), "{stderr}");

    let missing = wrath(&["remove", "-C", dir, "jq"]);
    let stderr = String::from_utf8_lossy(&missing.stderr);
    assert!(stderr.contains("`jq` isn't in packages"), "{stderr}");

    let after = std::fs::read_to_string(config.path().join("system.toml")).unwrap();
    assert_eq!(before, after);
}

#[test]
fn why_explains_files_and_packages_by_the_module_and_options_behind_them() {
    let file = stdout(&wrath(&[
        "why",
        "-C",
        "fixtures/configs/basic",
        "/etc/nginx/main.conf",
    ]));
    insta::assert_snapshot!(file, @r#"
    /etc/nginx/main.conf is written by module `nginx`
      probably from these settings, which the module read (a guess: not traced to this output):
        services.nginx.main.enable = true, set at fixtures/configs/basic/services/web.toml:2:10
        services.nginx.main.port = 8080, set at fixtures/configs/basic/services/web.toml:3:8
        services.nginx.main.sites.blog.domains = ["blog.example.com", "box.lan"], set at fixtures/configs/basic/services/web.toml:6:11
        services.nginx.main.sites.blog.root = "/srv/blog", set at fixtures/configs/basic/services/web.toml:7:8
    "#);

    let package = stdout(&wrath(&["why", "-C", "fixtures/configs/basic", "nftables"]));
    assert!(package.starts_with("nftables is installed for everyone by module `firewall`"));

    let unknown = wrath(&["why", "-C", "fixtures/configs/basic", "/etc/nope"]);
    assert!(!unknown.status.success());
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    assert!(stderr.contains("no module writes `/etc/nope`"), "{stderr}");
}

#[test]
fn json_is_the_plan_as_data_and_refused_where_it_isnt_supported() {
    let plan = stdout(&wrath(&[
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

    let unsupported = wrath(&["why", "--json", "-C", "fixtures/configs/basic", "--all"]);
    assert!(!unsupported.status.success());
}

#[test]
fn check_covers_every_role_unless_one_is_given() {
    let roles = copied("roles");
    let config = roles.path().to_str().unwrap();
    let all = wrath(&["check", "-C", config]);
    assert!(all.status.success());
    let stderr = String::from_utf8_lossy(&all.stderr);
    assert_eq!(
        stderr,
        format!("config {config}\nrole laptop\nrole worker\n")
    );
    assert_eq!(stdout(&all).matches("no errors: ").count(), 2);

    let one = wrath(&["check", "-C", config, "--role", "worker"]);
    assert!(one.status.success());
    let stderr = String::from_utf8_lossy(&one.stderr);
    assert_eq!(stderr, format!("config {config}, role worker\n"));

    let none = wrath(&["plan", "-C", "fixtures/configs/roles"]);
    assert!(!none.status.success());
    let stderr = String::from_utf8_lossy(&none.stderr);
    assert!(
        stderr.contains("so it needs one: laptop, worker"),
        "{stderr}"
    );
}

#[test]
fn dev_model_prints_the_resolved_model_as_json() {
    let text = stdout(&wrath(&[
        "dev",
        "model",
        "--config",
        "fixtures/configs/native-web",
    ]));
    assert!(text.starts_with('{'), "{text}");
    assert!(text.contains("\"/etc/nginx/main.conf\""), "{text}");
}

fn apply_record(config: &Path) -> serde_json::Value {
    let config = config.to_str().unwrap();
    let text = stdout(&wrath(&["dev", "apply-record", "--config", config]));
    serde_json::from_str(&text).unwrap()
}

#[test]
fn an_apply_record_keeps_the_configuration_and_where_values_came_from() {
    let record = apply_record(Path::new("fixtures/configs/native-web"));
    let web = record["sources"]["fixtures/configs/native-web/services/web.toml"]
        .as_str()
        .unwrap();
    assert!(web.contains("port = 8080"), "{web}");
    let port = &record["explanation"]["options"]["services.nginx.main.port"];
    assert_eq!(port["value"], "8080");
    let origin = port["set_by"][0]["origin"].as_str().unwrap();
    assert!(origin.contains("services/web.toml:3:8"), "{origin}");
    let nginx = &record["explanation"]["modules"]["nginx"];
    let outputs = nginx["outputs"].as_array().unwrap();
    assert!(
        outputs.iter().any(|name| name == "/etc/nginx/main.conf"),
        "{nginx}"
    );
    assert!(!nginx["reads"].as_array().unwrap().is_empty(), "{nginx}");
}

#[test]
fn moving_a_value_to_another_file_keeps_the_model_and_changes_only_the_record() {
    let config = tempfile::tempdir().unwrap();
    let dir = config.path();
    std::fs::create_dir(dir.join("services")).unwrap();
    std::fs::write(dir.join("system.toml"), "[system]\nhostname = \"native\"\n").unwrap();
    let web = "[services.nginx.main]\nenable = true\nport = 8080\n";
    std::fs::write(dir.join("services/one.toml"), web).unwrap();
    let path = dir.to_str().unwrap();
    let before = stdout(&wrath(&["dev", "model", "--config", path]));
    let cited_before = apply_record(dir);

    std::fs::rename(dir.join("services/one.toml"), dir.join("services/two.toml")).unwrap();
    let after = stdout(&wrath(&["dev", "model", "--config", path]));
    let cited_after = apply_record(dir);

    assert_eq!(before, after);
    let at = |record: &serde_json::Value| {
        record["explanation"]["options"]["services.nginx.main.port"]["set_by"][0]["origin"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(at(&cited_before).contains("services/one.toml"));
    assert!(at(&cited_after).contains("services/two.toml"));
}

#[test]
fn a_new_users_password_hash_reaches_the_model() {
    let output = wrath(&["-C", "scenarios/fixtures/native-password", "dev", "model"]);
    let model: serde_json::Value = serde_json::from_str(&stdout(&output)).unwrap();
    let hash = model["base"]["users"]["demo"]["initial_hashed_password"]
        .as_str()
        .unwrap();
    assert!(hash.starts_with("$6$usrviewtest$"), "{hash}");
}

#[test]
fn an_apply_record_keeps_the_module_and_library_code_its_origins_point_into() {
    let record = apply_record(Path::new("fixtures/configs/native-web"));
    let sources = record["sources"].as_object().unwrap();
    for file in ["nginx.star", "lib/pam.star"] {
        let (_, text) = sources
            .iter()
            .find(|(name, _)| name.ends_with(&format!("modules/{file}")))
            .unwrap_or_else(|| panic!("no {file} in {:?}", sources.keys()));
        let path = format!("{}/../../modules/{file}", env!("CARGO_MANIFEST_DIR"));
        let on_disk = std::fs::read_to_string(path).unwrap();
        assert_eq!(text.as_str().unwrap(), on_disk, "{file}");
    }
}

#[test]
fn a_home_of_its_own_is_refused_and_points_at_the_system_configuration() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("home.toml"), "packages = [\"jq\"]\n").unwrap();
    for command in ["plan", "apply", "status"] {
        let output = wrath(&[command, "-C", dir.path().to_str().unwrap()]);

        assert!(!output.status.success(), "{command}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("isn't supported for now") && stderr.contains("[users.NAME]"),
            "{stderr}"
        );
    }
}

#[test]
fn locking_keeps_each_users_uid_as_users_come_and_go() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().to_str().unwrap();
    let users = |names: &[&str]| {
        let tables: String = names
            .iter()
            .map(|name| format!("[users.{name}]\n"))
            .collect();
        let text = format!("[system]\nhostname = \"box\"\n{tables}");
        std::fs::write(dir.path().join("system.toml"), text).unwrap();
        stdout(&wrath(&["-C", config, "check"]));
        std::fs::read_to_string(dir.path().join("wrath.lock")).unwrap()
    };
    assert!(users(&["bo"]).contains("[ids.users]\nbo = 1000\n"));
    // A new name sorting first takes the next free uid.
    assert!(users(&["aaron", "bo"]).contains("[ids.users]\naaron = 1001\nbo = 1000\n"));
    // A removed user's uid stays theirs.
    let lock = users(&["carl"]);
    assert!(
        lock.contains("aaron = 1001\nbo = 1000\ncarl = 1002\n"),
        "{lock}"
    );
}

#[test]
fn check_pins_whats_new_and_with_locked_refuses_to() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().to_str().unwrap();
    let text = "[system]\nhostname = \"box\"\n[users.bo]\n";
    std::fs::write(dir.path().join("system.toml"), text).unwrap();
    let refused = wrath(&["-C", config, "check", "--locked"]);
    assert!(!refused.status.success());
    assert!(!dir.path().join("wrath.lock").exists());
    stdout(&wrath(&["-C", config, "check"]));
    assert!(
        std::fs::read_to_string(dir.path().join("wrath.lock"))
            .unwrap()
            .contains("bo = 1000")
    );
    stdout(&wrath(&["-C", config, "check", "--locked"]));
}

#[test]
fn the_reference_pages_say_what_the_code_does() {
    let output = wrath(&["dev", "docs", "--check"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_misspelled_package_fails_check_at_its_line_with_the_name_it_was_probably() {
    let dir = tempfile::tempdir().unwrap();
    let text = "packages = [\"jq\", \"ripgrpe\"]\n\n[system]\nhostname = \"box\"\n";
    std::fs::write(dir.path().join("system.toml"), text).unwrap();
    let output = wrath(&["-C", dir.path().to_str().unwrap(), "check"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("there's no package `ripgrpe`"), "{stderr}");
    assert!(stderr.contains("system.toml:1:12"), "{stderr}");
    assert!(stderr.contains("did you mean `ripgrep`?"), "{stderr}");
    assert!(!stderr.contains("`jq`"), "{stderr}");
}

#[test]
fn adding_a_misspelled_package_leaves_the_configuration_alone() {
    let dir = tempfile::tempdir().unwrap();
    let text = "[system]\nhostname = \"box\"\n";
    std::fs::write(dir.path().join("system.toml"), text).unwrap();
    let output = wrath(&["-C", dir.path().to_str().unwrap(), "add", "ripgrpe"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("did you mean `ripgrep`?"), "{stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("added"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("system.toml")).unwrap(),
        text
    );
}

#[test]
fn another_machine_is_planned_for_as_the_role_it_recorded() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    // Stands in for `ssh HOST SCRIPT`: the machine recorded that it's a worker, and runs nothing
    // wrath built.
    let ssh = dir.path().join("ssh");
    let script = "#!/bin/sh\ncase \"$*\" in\n  \
                  *machine.toml*) printf 'role = \"worker\"\\n' ;;\nesac\n";
    std::fs::write(&ssh, script).unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let roles = copied("roles");
    let output = Command::new(env!("CARGO_BIN_EXE_wrath"))
        .args([
            "-C",
            roles.path().to_str().unwrap(),
            "plan",
            "--host",
            "root@w-01",
        ])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .env("WRATH_SSH", &ssh)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("role worker, w-01's"), "{stderr}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("nginx"),
        "{stderr}"
    );
}
