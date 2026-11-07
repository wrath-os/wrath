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

#[test]
fn a_file_included_as_defaults_gives_defaults() {
    let evaluation = ok(&[
        (
            "system.toml",
            "include = [{ path = \"common.toml\", as = \"defaults\" }]\n\
             [system]\nhostname = \"box\"\ntimezone = \"America/New_York\"\n",
        ),
        (
            "common.toml",
            "[system]\ntimezone = \"Europe/Stockholm\"\n[services.nginx.main]\nport = 81\n",
        ),
    ]);

    insta::assert_snapshot!(why(&evaluation, "system.timezone"), @r#"
    system.timezone = "America/New_York"
      set at system.toml:4:12
      overrides "Europe/Stockholm", default at common.toml:2:12
      overrides "UTC", default
    "#);
    insta::assert_snapshot!(why(&evaluation, "services.nginx.main.port"), @"
    services.nginx.main.port = 81
      default at common.toml:4:8
      overrides 80, default
    ");
}

#[test]
fn files_included_from_a_defaults_file_give_defaults_too() {
    let evaluation = ok(&[
        (
            "system.toml",
            "include = [{ path = \"common.toml\", as = \"defaults\" }]\n\
             [system]\nhostname = \"box\"\n[services.nginx.main]\nport = 82\n",
        ),
        ("common.toml", "include = [\"base.toml\"]\n"),
        ("base.toml", "[services.nginx.main]\nport = 81\n"),
    ]);

    assert_eq!(
        evaluation.get("services.nginx.main.port"),
        Some(&Value::Int(82))
    );
}

#[test]
fn a_projects_defaults_beat_its_templates_from_a_personal_file() {
    let evaluation = ok(&[
        (
            "system.toml",
            "include = [{ path = \"project.toml\", as = \"defaults\" }]\n\
             [system]\nhostname = \"box\"\n",
        ),
        (
            "project.toml",
            "include = [{ path = \"template.toml\", as = \"defaults\" }]\n\
             [system]\ntimezone = \"Europe/Stockholm\"\n",
        ),
        ("template.toml", "[system]\ntimezone = \"UTC\"\n"),
    ]);

    assert_eq!(
        evaluation.get("system.timezone"),
        Some(&Value::Str("Europe/Stockholm".into()))
    );
}

#[test]
fn lists_in_a_defaults_file_merge() {
    let evaluation = ok(&[
        SYSTEM,
        (
            "users/john.toml",
            "include = [{ path = \"dev.toml\", as = \"defaults\" }]\npackages = [\"helix\"]\n",
        ),
        ("dev.toml", "packages = [\"ripgrep\"]\n"),
    ]);

    assert_eq!(
        evaluation.get("users.john.packages"),
        Some(&Value::List(vec![
            Value::Str("helix".into()),
            Value::Str("ripgrep".into()),
        ]))
    );
}

#[test]
fn error_force_in_a_defaults_file() {
    insta::assert_snapshot!(err(&[
        ("system.toml", "include = [{ path = \"common.toml\", as = \"defaults\" }]\n[system]\nhostname = \"box\"\n"),
        ("common.toml", "[system]\ntimezone = { value = \"UTC\", force = \"why\" }\n"),
    ]), @r#"
    error: a file included as defaults can't force a value
     --> common.toml:2:12
      |
    2 | timezone = { value = "UTC", force = "why" }
      |            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ forced here
      = help: force it in the file that includes this one
    "#);
}

#[test]
fn error_a_file_included_both_as_defaults_and_as_values() {
    insta::assert_snapshot!(err(&[
        ("system.toml", "include = [\"base.toml\", { path = \"common.toml\", as = \"defaults\" }]\n[system]\nhostname = \"box\"\n"),
        ("common.toml", "include = [\"base.toml\"]\n"),
        ("base.toml", "[system]\ntimezone = \"UTC\"\n"),
    ]), @r#"
    error: `base.toml` is included both as defaults and as values
     --> common.toml:1:12
      |
    1 | include = ["base.toml"]
      |            ^^^^^^^^^^^ included from a file that's included as defaults
     --> system.toml:1:12
      |
    1 | include = ["base.toml", { path = "common.toml", as = "defaults" }]
      |            ^^^^^^^^^^^ included as values here
      = help: include it one way; a file's values are defaults or they aren't
    "#);
}

#[test]
fn error_malformed_includes() {
    insta::assert_snapshot!(err(&[
        ("system.toml", "include = [{ path = \"a.toml\", as = \"values\" }, { as = \"defaults\" }, { path = \"b.toml\", if = 1 }]\n[system]\nhostname = \"box\"\n"),
    ]), @r#"
    error: `as` can only be "defaults"
     --> system.toml:1:36
      |
    1 | include = [{ path = "a.toml", as = "values" }, { as = "defaults" }, { path = "b.toml", if = 1 }]
      |                                    ^^^^^^^^ expected "defaults"

    error: an include needs `path`
     --> system.toml:1:48
      |
    1 | include = [{ path = "a.toml", as = "values" }, { as = "defaults" }, { path = "b.toml", if = 1 }]
      |                                                ^^^^^^^^^^^^^^^^^^^ add `path = "..."`

    error: unexpected `if` in an include
     --> system.toml:1:88
      |
    1 | include = [{ path = "a.toml", as = "values" }, { as = "defaults" }, { path = "b.toml", if = 1 }]
      |                                                                                        ^^ only `path` and `as` belong here
    "#);
}

#[test]
fn why_says_where_a_referenced_value_came_from() {
    let evaluation = ok(&[
        (
            "system.toml",
            "include = [{ path = \"project.toml\", as = \"defaults\" }]\n\
             [system]\nhostname = \"box\"\n[services.grafana.main]\nport = 3002\n",
        ),
        (
            "project.toml",
            "[services.grafana.main]\nport = 3001\n\
             [services.nginx.main.sites.blog]\n\
             proxy = \"http://127.0.0.1:${services.grafana.main.port}\"\n",
        ),
    ]);

    insta::assert_snapshot!(why(&evaluation, "services.nginx.main.sites.blog.proxy"), @r#"
    services.nginx.main.sites.blog.proxy = "http://127.0.0.1:3002"
      default at project.toml:4:9
      uses services.grafana.main.port = 3002, set at system.toml:5:8
    "#);
}

#[test]
fn default_is_an_ordinary_key_in_freeform_settings() {
    let evaluation = ok(&[
        SYSTEM,
        (
            "services/grafana.toml",
            "[services.grafana.main.settings]\npush.default = \"upstream\"\n",
        ),
    ]);

    assert_eq!(
        evaluation.get("services.grafana.main.settings.push.default"),
        Some(&Value::Str("upstream".into()))
    );
}
