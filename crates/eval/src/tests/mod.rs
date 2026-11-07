//! End-to-end evaluation tests: configuration in, values, provenance and errors out.

use crate::{
    ConfigKind, Input, ListMerge, MemFs, Module, OptionPath, OptionSpec, Schema, Sources, Type,
    Value, evaluate, explain,
};

pub(crate) fn schema() -> Schema {
    let mut schema = Schema::default();
    let declare = |schema: &mut Schema, path: &str, spec: OptionSpec| {
        schema.declare(path, spec).unwrap();
    };
    declare(&mut schema, "system.hostname", OptionSpec::new(Type::Str));
    declare(
        &mut schema,
        "system.timezone",
        OptionSpec::new(Type::Str).default(Value::Str("UTC".into())),
    );
    let site = OptionSpec::new(Type::record([
        (
            "domains",
            OptionSpec::new(Type::list(Type::Str, ListMerge::Set)).default(Value::List(vec![])),
        ),
        ("root", OptionSpec::new(Type::Path).optional()),
        ("proxy", OptionSpec::new(Type::Str).optional()),
    ]));
    declare(
        &mut schema,
        "services.nginx",
        OptionSpec::new(Type::map(OptionSpec::new(Type::record([
            (
                "enable",
                OptionSpec::new(Type::Bool).default(Value::Bool(false)),
            ),
            (
                "port",
                OptionSpec::new(Type::port()).default(Value::Int(80)),
            ),
            ("workers", OptionSpec::new(Type::int()).optional()),
            ("sites", OptionSpec::new(Type::map(site))),
        ])))),
    );
    declare(
        &mut schema,
        "services.grafana",
        OptionSpec::new(Type::map(OptionSpec::new(Type::record([
            (
                "port",
                OptionSpec::new(Type::port()).default(Value::Int(3000)),
            ),
            ("settings", OptionSpec::new(Type::Freeform).optional()),
        ])))),
    );
    declare(
        &mut schema,
        "users",
        OptionSpec::new(Type::map(OptionSpec::new(Type::record([
            (
                "packages",
                OptionSpec::new(Type::list(Type::Str, ListMerge::Set)).default(Value::List(vec![])),
            ),
            ("shell", OptionSpec::new(Type::Str).optional().account()),
        ])))),
    );
    let rule = Type::record([
        ("name", OptionSpec::new(Type::Str)),
        ("control", OptionSpec::new(Type::Str)),
        ("module", OptionSpec::new(Type::Str)),
        (
            "args",
            OptionSpec::new(Type::list(Type::Str, ListMerge::Atomic)).optional(),
        ),
    ]);
    declare(
        &mut schema,
        "pam",
        OptionSpec::new(Type::map(OptionSpec::new(Type::record([(
            "auth",
            OptionSpec::new(Type::list(rule, ListMerge::Ordered)).default(Value::List(vec![])),
        )])))),
    );
    declare(
        &mut schema,
        "system.kernel_params",
        OptionSpec::new(Type::list(Type::Str, ListMerge::Atomic)).optional(),
    );
    declare(
        &mut schema,
        "system.motd",
        OptionSpec::new(Type::Str).optional().runtime(),
    );
    schema
}

fn eval(files: &[(&str, &str)]) -> Result<crate::Evaluation, String> {
    eval_with(files, &[])
}

pub(crate) fn eval_with(
    files: &[(&str, &str)],
    modules: &[Box<dyn Module>],
) -> Result<crate::Evaluation, String> {
    eval_role(files, modules, None)
}

/// Evaluates the configuration in `config`, with no modules.
pub(crate) fn eval_fs(config: &MemFs) -> Result<crate::Evaluation, String> {
    evaluate(Input {
        sources: Sources::default(),
        config,
        kind: ConfigKind::System,
        user: None,
        role: None,
        schema: schema(),
        modules: &[],
    })
    .map_err(|e| e.to_string())
}

pub(crate) fn eval_role(
    files: &[(&str, &str)],
    modules: &[Box<dyn Module>],
    role: Option<&str>,
) -> Result<crate::Evaluation, String> {
    evaluate(Input {
        sources: Sources::default(),
        config: &MemFs::new(files.iter().copied()),
        kind: ConfigKind::System,
        user: None,
        role,
        schema: schema(),
        modules,
    })
    .map_err(|e| e.to_string())
}

fn ok(files: &[(&str, &str)]) -> crate::Evaluation {
    eval(files).unwrap_or_else(|e| panic!("evaluation failed:\n{e}"))
}

fn err(files: &[(&str, &str)]) -> String {
    match eval(files) {
        Ok(_) => panic!("evaluation succeeded"),
        Err(e) => e,
    }
}

fn why(evaluation: &crate::Evaluation, prefix: &str) -> String {
    explain(evaluation, Some(&OptionPath::parse(prefix).unwrap())).unwrap()
}

const SYSTEM: &str = "[system]\nhostname = \"box\"\n";

#[test]
fn literal_values_and_defaults_resolve_with_provenance() {
    let evaluation = ok(&[
        ("system.toml", SYSTEM),
        (
            "services/web.toml",
            "[services.nginx.main]\nenable = true\nport = 8080\n",
        ),
    ]);

    assert_eq!(
        evaluation.get("system.hostname"),
        Some(&Value::Str("box".into()))
    );
    assert_eq!(
        evaluation.get("system.timezone"),
        Some(&Value::Str("UTC".into()))
    );
    assert_eq!(
        evaluation.get("services.nginx.main.port"),
        Some(&Value::Int(8080))
    );
    assert_eq!(evaluation.get("services.nginx.main.workers"), None);
    insta::assert_snapshot!(explain(&evaluation, None).unwrap(), @r#"
    services.nginx.main.enable = true
      set at services/web.toml:2:10
      overrides false, default
    services.nginx.main.port = 8080
      set at services/web.toml:3:8
      overrides 80, default
    system.hostname = "box"
      set at system.toml:2:12
    system.timezone = "UTC"
      default
    "#);
}

#[test]
fn an_empty_instance_table_gets_its_defaults() {
    let evaluation = ok(&[
        ("system.toml", SYSTEM),
        ("services/web.toml", "[services.nginx.main]\n"),
    ]);

    assert_eq!(
        evaluation.get("services.nginx.main.port"),
        Some(&Value::Int(80))
    );
}

#[test]
fn references_keep_their_type_or_build_strings() {
    let evaluation = ok(&[
        ("system.toml", SYSTEM),
        (
            "services/web.toml",
            r#"
[services.grafana.main]
port = 3001

[services.nginx.main.sites.blog]
domains = ["blog.example.com", "${system.hostname}.lan"]
proxy = "http://127.0.0.1:${services.grafana.main.port}"
"#,
        ),
    ]);

    assert_eq!(
        evaluation.get("services.nginx.main.sites.blog.proxy"),
        Some(&Value::Str("http://127.0.0.1:3001".into()))
    );
    insta::assert_snapshot!(why(&evaluation, "services.nginx.main.sites.blog"), @r#"
    services.nginx.main.sites.blog.domains = ["blog.example.com", "box.lan"]
      set at services/web.toml:6:11
      overrides [], default
      uses system.hostname = "box", set at system.toml:2:12
    services.nginx.main.sites.blog.proxy = "http://127.0.0.1:3001"
      set at services/web.toml:7:9
      uses services.grafana.main.port = 3001, set at services/web.toml:3:8
    "#);
}

#[test]
fn double_dollar_is_a_literal_dollar_brace() {
    let evaluation = ok(&[("system.toml", "[system]\nhostname = \"$${not-a-ref}\"\n")]);

    assert_eq!(
        evaluation.get("system.hostname"),
        Some(&Value::Str("${not-a-ref}".into()))
    );
}

#[test]
fn set_lists_merge_across_files_sorted_without_duplicates() {
    let evaluation = ok(&[
        ("system.toml", SYSTEM),
        (
            "services/a.toml",
            "[services.nginx.main.sites.blog]\ndomains = [\"b\", \"a\"]\n",
        ),
        (
            "services/b.toml",
            "[services.nginx.main.sites.blog]\ndomains = [\"c\", \"a\"]\n",
        ),
    ]);

    insta::assert_snapshot!(why(&evaluation, "services.nginx.main.sites.blog.domains"), @r#"
    services.nginx.main.sites.blog.domains = ["a", "b", "c"]
      set at services/a.toml:2:11: ["b", "a"]
      set at services/b.toml:2:11: ["c", "a"]
      overrides [], default
    "#);
}

#[test]
fn equal_values_in_two_files_agree() {
    let evaluation = ok(&[
        ("system.toml", SYSTEM),
        ("services/a.toml", "[services.nginx.main]\nport = 81\n"),
        ("services/b.toml", "[services.nginx.main]\nport = 81\n"),
    ]);

    assert_eq!(
        evaluation.get("services.nginx.main.port"),
        Some(&Value::Int(81))
    );
}

#[test]
fn forced_value_wins_and_records_the_override() {
    let evaluation = ok(&[
        ("system.toml", SYSTEM),
        ("services/a.toml", "[services.nginx.main]\nport = 81\n"),
        (
            "services/b.toml",
            "[services.nginx.main]\nport = { value = 82, force = \"testing a proxy\" }\n",
        ),
    ]);

    insta::assert_snapshot!(why(&evaluation, "services.nginx.main.port"), @"
    services.nginx.main.port = 82
      forced at services/b.toml:2:8 (testing a proxy)
      overrides 81, set at services/a.toml:2:8
      overrides 80, default
    ");
}

#[test]
fn secrets_are_allowed_in_runtime_options_only() {
    let evaluation = eval_with(
        &[
            ("system.toml", SYSTEM),
            (
                "services/grafana.toml",
                r#"
[services.grafana.main.settings.security]
admin_user = "admin"
admin_password = { secret = "grafana-admin" }
"#,
            ),
            ("grafana-admin.age", ""),
        ],
        &[modules::secrets(
            "secrets",
            "grafana-admin",
            "grafana-admin.age",
        )],
    )
    .unwrap();

    insta::assert_snapshot!(why(&evaluation, "services.grafana.main.settings"), @r#"
    services.grafana.main.settings.security.admin_password = { secret = "grafana-admin" }
      set at services/grafana.toml:4:18
    services.grafana.main.settings.security.admin_user = "admin"
      set at services/grafana.toml:3:14
    "#);

    insta::assert_snapshot!(err(&[(
        "system.toml",
        "[system]\nhostname = { secret = \"name\" }\n",
    )]), @r#"
    error: invalid value for `system.hostname`
     --> system.toml:2:12
      |
    2 | hostname = { secret = "name" }
      |            ^^^^^^^^^^^^^^^^^^^ `system.hostname` is fixed when the system is built, so it cannot hold a value only known at service start
    "#);
}

#[test]
fn a_reference_to_a_secret_is_a_runtime_value_too() {
    insta::assert_snapshot!(err(&[
        ("system.toml", SYSTEM),
        (
            "services/a.toml",
            r#"
[services.grafana.main.settings]
password = { secret = "pw" }

[services.nginx.main.sites.blog]
proxy = "http://admin:${services.grafana.main.settings.password}@localhost"
"#,
        ),
    ]), @r#"
    error: invalid value for `services.nginx.main.sites.blog.proxy`
     --> services/a.toml:6:9
      |
    6 | proxy = "http://admin:${services.grafana.main.settings.password}@localhost"
      |         ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ `services.nginx.main.sites.blog.proxy` is fixed when the system is built, so it cannot hold a value only known at service start
    "#);
}

#[test]
fn strings_refer_to_secrets_by_name() {
    let files = |reference: &str| {
        let settings = format!(
            "[services.grafana.main.settings.database]\n\
             url = \"postgres://app:${{{reference}}}@db\"\n"
        );
        eval_with(
            &[
                ("system.toml", SYSTEM),
                ("services/grafana.toml", settings.as_str()),
                ("grafana-admin.age", ""),
            ],
            &[modules::secrets(
                "secrets",
                "grafana-admin",
                "grafana-admin.age",
            )],
        )
    };

    let evaluation = files("secret.grafana-admin").unwrap();
    insta::assert_snapshot!(why(&evaluation, "services.grafana.main.settings"), @r#"
    services.grafana.main.settings.database.url = { runtime = "postgres://app:${secret.grafana-admin}@db" }
      set at services/grafana.toml:2:7
    "#);
    insta::assert_snapshot!(files("secret.grafana-admn").unwrap_err(), @r#"
    error: there is no secret `grafana-admn`
     --> services/grafana.toml:2:7
      |
    2 | url = "postgres://app:${secret.grafana-admn}@db"
      |       ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ `services.grafana.main.settings.database.url` refers to it
      = help: did you mean `grafana-admin`?
    "#);
    insta::assert_snapshot!(files("secret.a.b").unwrap_err(), @r#"
    error: `secret.a.b` doesn't name a secret
     --> services/grafana.toml:2:23
      |
    2 | url = "postgres://app:${secret.a.b}@db"
      |                       ^^^^^^^^^^^^^ referenced here
      = help: refer to a secret as `${secret.NAME}`
    "#);
}

#[test]
fn include_and_user_files() {
    let evaluation = ok(&[
        (
            "system.toml",
            "include = [\"profiles/base.toml\"]\n[system]\nhostname = \"box\"\n",
        ),
        (
            "profiles/base.toml",
            "[system]\ntimezone = \"Europe/Stockholm\"\n",
        ),
        (
            "users/john.toml",
            "include = [\"profiles/dev.toml\"]\npackages = [\"helix\"]\n",
        ),
        ("profiles/dev.toml", "packages = [\"ripgrep\"]\n"),
    ]);

    assert_eq!(
        evaluation.get("system.timezone"),
        Some(&Value::Str("Europe/Stockholm".into()))
    );
    assert_eq!(
        evaluation.get("users.john.packages"),
        Some(&Value::List(vec![
            Value::Str("helix".into()),
            Value::Str("ripgrep".into()),
        ]))
    );
}

#[test]
fn error_unknown_option_suggests_a_name() {
    insta::assert_snapshot!(err(&[(
        "system.toml",
        "[system]\nhostnme = \"box\"\n",
    )]), @r#"
    error: unknown option `system.hostnme`
     --> system.toml:2:1
      |
    2 | hostnme = "box"
      | ^^^^^^^ no option with this name
      = help: did you mean `hostname`?
    "#);
}

#[test]
fn error_wrong_type() {
    insta::assert_snapshot!(err(&[
        ("system.toml", SYSTEM),
        ("services/web.toml", "[services.nginx.main]\nport = 70000\nenable = \"yes\"\n"),
    ]), @r#"
    error: invalid value for `services.nginx.main.enable`
     --> services/web.toml:3:10
      |
    3 | enable = "yes"
      |          ^^^^^ expected a boolean, found "yes"

    error: invalid value for `services.nginx.main.port`
     --> services/web.toml:2:8
      |
    2 | port = 70000
      |        ^^^^^ expected an integer from 1 to 65535, found 70000
    "#);
}

#[test]
fn error_table_expected() {
    insta::assert_snapshot!(err(&[
        ("system.toml", SYSTEM),
        ("services/web.toml", "[services]\nnginx = 1\n"),
    ]), @"
    error: `services.nginx` is a table of options, found integer
     --> services/web.toml:2:9
      |
    2 | nginx = 1
      |         ^ expected a table
    ");
}

#[test]
fn error_conflicting_values() {
    insta::assert_snapshot!(err(&[
        ("system.toml", SYSTEM),
        ("services/a.toml", "[services.nginx.main]\nport = 8080\n"),
        ("services/b.toml", "[services.nginx.main]\nport = 8081\n"),
    ]), @r#"
    error: conflicting values for `services.nginx.main.port`
     --> services/a.toml:2:8
      |
    2 | port = 8080
      |        ^^^^ set to 8080 here
     --> services/b.toml:2:8
      |
    2 | port = 8081
      |        ^^^^ set to 8081 here
      = help: remove all but one, make one a default others may change with `{ default = ... }`, or force one with `{ value = ..., force = "reason" }`
    "#);
}

#[test]
fn error_unknown_reference_suggests_an_option() {
    insta::assert_snapshot!(err(&[(
        "system.toml",
        "[system]\nhostname = \"box\"\ntimezone = \"${system.hostnam}\"\n",
    )]), @r#"
    error: unknown option `system.hostnam`
     --> system.toml:3:13
      |
    3 | timezone = "${system.hostnam}"
      |             ^^^^^^^^^^^^^^^^^ referenced here
      = help: did you mean `system.hostname`?
    "#);
}

#[test]
fn error_reference_to_a_table_or_list_in_a_string() {
    insta::assert_snapshot!(err(&[
        (
            "system.toml",
            "[system]\nhostname = \"${services.nginx.main}\"\ntimezone = \"x ${users.john.packages}\"\n",
        ),
        ("services/web.toml", "[services.nginx.main]\n"),
        ("users/john.toml", "packages = [\"a\"]\n"),
    ]), @r#"
    error: `services.nginx.main` is a table of options
     --> system.toml:2:13
      |
    2 | hostname = "${services.nginx.main}"
      |             ^^^^^^^^^^^^^^^^^^^^^^ a reference must name a single option

    error: cannot put a list into a string
     --> system.toml:3:15
      |
    3 | timezone = "x ${users.john.packages}"
      |               ^^^^^^^^^^^^^^^^^^^^^^ `users.john.packages` is ["a"]
      = help: a reference that is the whole string keeps its type
    "#);
}

#[test]
fn error_reference_cycle() {
    insta::assert_snapshot!(err(&[(
        "system.toml",
        "[system]\nhostname = \"${system.timezone}\"\ntimezone = \"${system.hostname}\"\n",
    )]), @r#"
    error: cycle: system.hostname -> system.timezone -> system.hostname
     --> system.toml:3:13
      |
    3 | timezone = "${system.hostname}"
      |             ^^^^^^^^^^^^^^^^^^ this closes the cycle
    "#);
}

#[test]
fn error_unknown_fact_and_malformed_reference() {
    insta::assert_snapshot!(err(&[(
        "system.toml",
        "[system]\nhostname = \"${facts.gpu}\"\ntimezone = \"${system.\"\n",
    )]), @r#"
    error: unterminated reference, expected `}`
     --> system.toml:3:13
      |
    3 | timezone = "${system."
      |             ^^^^^^^^^ in this reference

    error: unknown machine value `facts.gpu`
     --> system.toml:2:13
      |
    2 | hostname = "${facts.gpu}"
      |             ^^^^^^^^^^^^ referenced here
      = the machine values are facts.cpu.count, machine.name
    "#);
}

#[test]
fn error_invalid_toml_and_missing_system_file() {
    insta::assert_snapshot!(err(&[("system.toml", "[system\nhostname = 1\n")]), @"
    error: invalid TOML
     --> system.toml:1:8
      |
    1 | [system
      |        ^ unclosed table, expected `]`
    ");
    insta::assert_snapshot!(err(&[]), @"
    error: `system.toml` does not exist
     = help: every configuration starts from `system.toml`
    ");
}

#[test]
fn error_bad_force_and_include() {
    insta::assert_snapshot!(err(&[
        ("system.toml", "include = [\"missing.toml\", 3]\n[system]\nhostname = { value = \"a\", force = \"\" }\n"),
    ]), @r#"
    error: `include` must be a list of file paths
     --> system.toml:1:28
      |
    1 | include = ["missing.toml", 3]
      |                            ^ expected a path, or `{ path = "...", as = "defaults" }`
      = help: for example `include = ["profiles/laptop.toml"]`

    error: cannot read `missing.toml`: entity not found
     --> system.toml:1:12
      |
    1 | include = ["missing.toml", 3]
      |            ^^^^^^^^^^^^^^ included here

    error: `force` needs a reason
     --> system.toml:3:35
      |
    3 | hostname = { value = "a", force = "" }
      |                                   ^^ expected a non-empty string
      = help: for example `force = "the vendor default breaks suspend"`
    "#);
}

#[test]
fn error_missing_include_points_at_the_include() {
    insta::assert_snapshot!(err(&[(
        "system.toml",
        "include = [\"profiles/missing.toml\"]\n[system]\nhostname = \"box\"\n",
    )]), @r#"
    error: cannot read `profiles/missing.toml`: entity not found
     --> system.toml:1:12
      |
    1 | include = ["profiles/missing.toml"]
      |            ^^^^^^^^^^^^^^^^^^^^^^^ included here
    "#);
}

#[test]
fn why_reports_unknown_prefixes() {
    let evaluation = ok(&[("system.toml", SYSTEM)]);

    assert_eq!(
        explain(
            &evaluation,
            Some(&OptionPath::parse("system.hostnme").unwrap())
        )
        .unwrap_err(),
        "no option `system.hostnme` has a value; did you mean `system.hostname`?"
    );
}

#[test]
fn error_required_option_not_set() {
    insta::assert_snapshot!(err(&[("system.toml", "[system]\ntimezone = \"UTC\"\n")]), @"
    error: `system.hostname` is not set
     = help: this option has no default, so the configuration must set it
    ");
}

mod defaults;
mod homes;
mod lists;
mod machine;
mod modules;
mod projects;
mod roles;
mod sources;
