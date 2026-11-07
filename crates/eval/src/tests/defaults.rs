//! Defaults written in configuration files: `{ default = ... }`, and which default wins.

use super::{err, ok, why};
use crate::Value;

const SYSTEM: (&str, &str) = ("system.toml", "[system]\nhostname = \"box\"\n");

#[test]
fn a_default_replaces_the_declared_default() {
    let evaluation = ok(&[
        SYSTEM,
        (
            "services/a.toml",
            "[services.nginx.main]\nport = { default = 81 }\n",
        ),
    ]);

    insta::assert_snapshot!(why(&evaluation, "services.nginx.main.port"), @"
    services.nginx.main.port = 81
      default at services/a.toml:2:8
      overrides 80, default
    ");
}

#[test]
fn a_set_value_replaces_a_default() {
    let evaluation = ok(&[
        SYSTEM,
        (
            "services/a.toml",
            "[services.nginx.main]\nport = { default = 81 }\n",
        ),
        ("services/b.toml", "[services.nginx.main]\nport = 82\n"),
    ]);

    insta::assert_snapshot!(why(&evaluation, "services.nginx.main.port"), @"
    services.nginx.main.port = 82
      set at services/b.toml:2:8
      overrides 81, default at services/a.toml:2:8
      overrides 80, default
    ");
}

#[test]
fn the_including_files_default_wins() {
    let evaluation = ok(&[
        (
            "system.toml",
            "include = [\"common.toml\"]\n[system]\nhostname = \"box\"\n\
             timezone = { default = \"America/New_York\" }\n",
        ),
        (
            "common.toml",
            "include = [\"base.toml\"]\n[system]\ntimezone = { default = \"Europe/Stockholm\" }\n",
        ),
        (
            "base.toml",
            "[system]\ntimezone = { default = \"Europe/London\" }\n",
        ),
    ]);

    insta::assert_snapshot!(why(&evaluation, "system.timezone"), @r#"
    system.timezone = "America/New_York"
      default at system.toml:4:12
      overrides "Europe/Stockholm", default at common.toml:3:12
      overrides "Europe/London", default at base.toml:2:12
      overrides "UTC", default
    "#);
}

#[test]
fn an_included_files_default_wins_over_the_files_it_includes() {
    let evaluation = ok(&[
        (
            "system.toml",
            "include = [\"common.toml\"]\n[system]\nhostname = \"box\"\n",
        ),
        (
            "common.toml",
            "include = [\"base.toml\"]\n[system]\ntimezone = { default = \"Europe/Stockholm\" }\n",
        ),
        (
            "base.toml",
            "[system]\ntimezone = { default = \"Europe/London\" }\n",
        ),
    ]);

    assert_eq!(
        evaluation.get("system.timezone"),
        Some(&Value::Str("Europe/Stockholm".into()))
    );
}

#[test]
fn defaults_that_agree_are_fine_without_includes() {
    let evaluation = ok(&[
        SYSTEM,
        (
            "services/a.toml",
            "[services.nginx.main]\nport = { default = 81 }\n",
        ),
        (
            "services/b.toml",
            "[services.nginx.main]\nport = { default = 81 }\n",
        ),
    ]);

    assert_eq!(
        evaluation.get("services.nginx.main.port"),
        Some(&Value::Int(81))
    );
}

#[test]
fn error_defaults_from_unrelated_files_disagree() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        ("services/a.toml", "[services.nginx.main]\nport = { default = 81 }\n"),
        ("services/b.toml", "[services.nginx.main]\nport = { default = 82 }\n"),
    ]), @r#"
    error: conflicting defaults for `services.nginx.main.port`
     --> services/a.toml:2:8
      |
    2 | port = { default = 81 }
      |        ^^^^^^^^^^^^^^^^ default 81 here
     --> services/b.toml:2:8
      |
    2 | port = { default = 82 }
      |        ^^^^^^^^^^^^^^^^ default 82 here
      = help: neither file includes the other, so neither default wins; remove one, or set the value
    "#);
}

#[test]
fn error_default_on_a_merged_list() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        ("users/john.toml", "packages = { default = [\"helix\"] }\n"),
    ]), @r#"
    error: `users.john.packages` merges with other files, so it has no default
     --> users/john.toml:1:12
      |
    1 | packages = { default = ["helix"] }
      |            ^^^^^^^^^^^^^^^^^^^^^^^ written as a default
      = help: write the list itself; others add to it, and take items out with `{ remove = [...] }`
    "#);
}

#[test]
fn error_default_mixed_with_other_keys() {
    insta::assert_snapshot!(err(&[
        SYSTEM,
        (
            "services/a.toml",
            "[services.nginx.main]\nport = { default = 81, force = \"why\" }\n",
        ),
    ]), @r#"
    error: unexpected `force` in a default
     --> services/a.toml:2:24
      |
    2 | port = { default = 81, force = "why" }
      |                        ^^^^^ only `default` belongs here
    "#);
}
