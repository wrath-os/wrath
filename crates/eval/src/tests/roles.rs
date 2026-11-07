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
     = help: pick one with `--role NAME`, or name this machine in `machines.toml`
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

#[test]
fn a_single_role_needs_no_name() {
    let files = [
        ("roles/neptune.toml", "[system]\nhostname = \"neptune\"\n"),
        ("users/john.toml", "packages = [\"helix\"]\n"),
    ];
    let evaluation = eval_role(&files, &[], None).unwrap();

    assert_eq!(
        evaluation.get("system.hostname"),
        Some(&Value::Str("neptune".into()))
    );
}

/// Roles where workers are given a zone, and the machines that run them.
fn with_machines(machines: &str) -> Vec<(&str, &str)> {
    let mut files = ROLES.to_vec();
    files[0] = (
        "roles/worker.toml",
        "include = [\"common.toml\"]\n[system]\nhostname = \"worker\"\n\n\
         [machine.values]\nzone = { type = \"string\" }\n",
    );
    files.push(("machines.toml", machines));
    files
}

#[test]
fn machines_are_listed_with_their_role_and_values() {
    let files = with_machines(
        "w-01 = { role = \"worker\", zone = \"eu-north-1a\" }\n\
         w-gpu-01 = { role = \"gpu-worker\", zone = \"eu-north-1b\" }\n",
    );
    let evaluation = eval_role(&files, &[], Some("worker")).unwrap();

    let machines: Vec<String> = evaluation
        .machines()
        .iter()
        .map(|(name, machine)| format!("{name} {} {:?}", machine.role, machine.values))
        .collect();
    assert_eq!(
        machines,
        [
            "w-01 worker {\"zone\": \"eu-north-1a\"}",
            "w-gpu-01 gpu-worker {\"zone\": \"eu-north-1b\"}",
        ]
    );
}

#[test]
fn error_bad_machines() {
    let files = with_machines(
        "w-01 = { role = \"worker\" }\n\
         w-02 = { role = \"worker\", zone = \"a\", zoen = \"b\", rack = 3 }\n\
         w-03 = { role = \"wroker\" }\n\
         w-04 = { zone = \"a\" }\n\
         w-05 = \"worker\"\n",
    );
    insta::assert_snapshot!(err(&files, Some("worker")), @r#"
    error: this machine isn't given `zone`
     --> machines.toml:1:1
      |
    1 | w-01 = { role = "worker" }
      | ^^^^ needs `zone`
     --> roles/worker.toml:6:1
      |
    6 | zone = { type = "string" }
      | ^^^^ declared here

    error: machine values are strings, found integer
     --> machines.toml:2:58
      |
    2 | w-02 = { role = "worker", zone = "a", zoen = "b", rack = 3 }
      |                                                          ^ expected a string

    error: the role doesn't declare a machine value `rack`
     --> machines.toml:2:51
      |
    2 | w-02 = { role = "worker", zone = "a", zoen = "b", rack = 3 }
      |                                                   ^^^^ given here
      = help: declare it in the role's `[machine.values]`

    error: the role doesn't declare a machine value `zoen`
     --> machines.toml:2:39
      |
    2 | w-02 = { role = "worker", zone = "a", zoen = "b", rack = 3 }
      |                                       ^^^^ given here
      = help: did you mean `zone`?

    error: there's no role `wroker`
     --> machines.toml:3:17
      |
    3 | w-03 = { role = "wroker" }
      |                 ^^^^^^^^ named here
      = the roles are gpu-worker, worker
      = help: did you mean `worker`?

    error: a machine needs a role
     --> machines.toml:4:1
      |
    4 | w-04 = { zone = "a" }
      | ^^^^ no `role` here
      = the roles are gpu-worker, worker

    error: a machine is a table with its role
     --> machines.toml:5:8
      |
    5 | w-05 = "worker"
      |        ^^^^^^^^ expected `{ role = "..." }`
    "#);
}

#[test]
fn error_machines_for_a_single_machine() {
    let files = [
        ("system.toml", "[system]\nhostname = \"box\"\n"),
        ("machines.toml", "box = {}\n"),
    ];
    insta::assert_snapshot!(err(&files, None), @"
    error: `machines.toml` names machines for roles
     = this configuration describes one machine in `system.toml`
     = help: set what differs in the configuration itself, or move to `roles/`
    ");
}
