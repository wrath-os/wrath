//! Roles: a configuration with `roles/` starts from the role it's evaluated for.

use super::eval_role;
use crate::Value;

const ROLES: [(&str, &str); 4] = [
    (
        "roles/worker.toml",
        "include = [\"common.toml\"]\n[system]\nhostname = \"worker\"\n",
    ),
    (
        "roles/gpu-worker.toml",
        "include = [\"roles/worker.toml\"]\n[services.nginx.main]\nport = 81\n",
    ),
    ("common.toml", "[system]\ntimezone = \"Europe/Stockholm\"\n"),
    ("users/john.toml", "packages = [\"helix\"]\n"),
];

fn err(files: &[(&str, &str)], role: Option<&str>) -> String {
    eval_role(files, &[], role).expect_err("evaluation succeeded")
}

#[test]
fn a_role_is_the_starting_file() {
    let evaluation = eval_role(&ROLES, &[], Some("gpu-worker")).unwrap();

    assert_eq!(
        evaluation.get("system.hostname"),
        Some(&Value::Str("worker".into()))
    );
    assert_eq!(
        evaluation.get("services.nginx.main.port"),
        Some(&Value::Int(81))
    );
    assert_eq!(
        evaluation.get("users.john.packages"),
        Some(&Value::List(vec![Value::Str("helix".into())]))
    );
    let starting: Vec<&str> = evaluation
        .sources()
        .files()
        .filter(|&file| evaluation.is_starting_file(file))
        .map(|file| evaluation.sources().name(file))
        .collect();
    assert_eq!(starting, ["roles/gpu-worker.toml", "users/john.toml"]);
}

#[test]
fn the_other_roles_are_left_out() {
    let evaluation = eval_role(&ROLES, &[], Some("worker")).unwrap();

    assert_eq!(evaluation.get("services.nginx.main.port"), None);
}

#[test]
fn error_no_role_given() {
    insta::assert_snapshot!(err(&ROLES, None), @"
    error: this configuration has roles, so it needs one: gpu-worker, worker
     = help: pick one with `--role NAME`
    ");
}

#[test]
fn error_unknown_role() {
    insta::assert_snapshot!(err(&ROLES, Some("wroker")), @"
    error: there's no role `wroker`
     = the roles are gpu-worker, worker
     = help: did you mean `worker`?
    ");
}

#[test]
fn error_role_for_a_single_machine() {
    let single = [("system.toml", "[system]\nhostname = \"box\"\n")];
    insta::assert_snapshot!(err(&single, Some("worker")), @"
    error: there's no role `worker`: this configuration describes one machine
     = help: roles are files in `roles/`, used instead of `system.toml`
    ");
}

#[test]
fn error_system_toml_next_to_roles() {
    let mut files = ROLES.to_vec();
    files.push(("system.toml", "[system]\nhostname = \"box\"\n"));
    insta::assert_snapshot!(err(&files, Some("worker")), @"
    error: this configuration has both `system.toml` and `roles/`
     = help: a configuration describes one machine in `system.toml`, or kinds of machines in `roles/`
    ");
}
