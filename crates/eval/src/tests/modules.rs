//! Modules: declaring options, reading config, setting other options and rendering outputs.

use super::eval_with;
use crate::{
    ConfigReader, Diagnostic, Level, Module, OptionPath, OptionSpec, Output, ReadError,
    RenderError, Section, Setting, Type, Unit, Value, explain,
};

type RenderFn = dyn Fn(&mut dyn ConfigReader) -> Result<Output, RenderError>;

struct TestModule {
    name: &'static str,
    options: Vec<(OptionPath, OptionSpec)>,
    writes: Vec<OptionPath>,
    render: Box<RenderFn>,
}

impl Module for TestModule {
    fn name(&self) -> &str {
        self.name
    }

    fn options(&self) -> Vec<(OptionPath, OptionSpec)> {
        self.options.clone()
    }

    fn writes(&self) -> Vec<OptionPath> {
        self.writes.clone()
    }

    fn render(&self, config: &mut dyn ConfigReader) -> Result<Output, RenderError> {
        (self.render)(config)
    }
}

fn path(text: &str) -> OptionPath {
    OptionPath::parse(text).unwrap()
}

fn module(
    name: &'static str,
    options: Vec<(&str, OptionSpec)>,
    writes: &[&str],
    render: impl Fn(&mut dyn ConfigReader) -> Result<Output, RenderError> + 'static,
) -> Box<dyn Module> {
    Box::new(TestModule {
        name,
        options: options
            .into_iter()
            .map(|(option, spec)| (path(option), spec))
            .collect(),
        writes: writes.iter().map(|write| path(write)).collect(),
        render: Box::new(render),
    })
}

fn read(config: &mut dyn ConfigReader, option: &str) -> Result<Option<Value>, RenderError> {
    config.read(&path(option), None).map_err(|e| match e {
        ReadError::Invalid(message) => RenderError::Error(Diagnostic::error(message)),
        ReadError::Failed => RenderError::Failed,
    })
}

fn set(option: &str, value: Value, level: Level) -> Setting {
    Setting {
        path: path(option),
        value,
        level,
        at: None,
    }
}

fn strings(items: &[&str]) -> Value {
    Value::List(
        items
            .iter()
            .map(|item| Value::Str((*item).into()))
            .collect(),
    )
}

/// A syncthing-like module: an instance map, a unit per instance and a system user.
fn syncthing() -> Box<dyn Module> {
    module(
        "syncthing",
        vec![(
            "services.syncthing",
            OptionSpec::new(Type::map(OptionSpec::new(Type::record([(
                "port",
                OptionSpec::new(Type::port()).default(Value::Int(8384)),
            )])))),
        )],
        &["users"],
        |config| {
            let Some(Value::Table(instances)) = read(config, "services.syncthing")? else {
                return Ok(Output::default());
            };
            let mut output = Output::default();
            for (name, instance) in &instances {
                let Value::Table(fields) = instance else {
                    continue;
                };
                output.units.insert(
                    format!("syncthing-{name}.service"),
                    Unit {
                        sections: vec![Section {
                            name: "Service".into(),
                            entries: vec![(
                                "ExecStart".into(),
                                format!(
                                    "syncthing serve --gui-address=127.0.0.1:{}",
                                    fields["port"]
                                ),
                            )],
                        }],
                    },
                );
            }
            if !instances.is_empty() {
                output.packages.insert("syncthing".into());
                output.settings.push(set(
                    "users.syncthing.packages",
                    strings(&["syncthing"]),
                    Level::Default,
                ));
            }
            Ok(output)
        },
    )
}

const SYSTEM: (&str, &str) = ("system.toml", "[system]\nhostname = \"box\"\n");

#[test]
fn a_module_renders_from_config_and_creates_entries_elsewhere() {
    let evaluation = eval_with(
        &[
            SYSTEM,
            (
                "services/sync.toml",
                "[services.syncthing.main]\nport = 8385\n",
            ),
        ],
        &[syncthing()],
    )
    .unwrap();

    let rendered = &evaluation.modules()[0];
    assert_eq!(
        rendered.output.units["syncthing-main.service"].sections[0].entries,
        [(
            "ExecStart".to_owned(),
            "syncthing serve --gui-address=127.0.0.1:8385".to_owned()
        )]
    );
    assert_eq!(
        rendered
            .reads
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        ["services.syncthing"]
    );
    insta::assert_snapshot!(explain(&evaluation, Some(&path("users"))).unwrap(), @r#"
    users.syncthing.packages = ["syncthing"]
      default from module `syncthing`
      overrides [], default
    "#);
}

#[test]
fn config_overrides_a_module_default() {
    let evaluation = eval_with(
        &[
            SYSTEM,
            ("services/sync.toml", "[services.syncthing.main]\n"),
            (
                "users/syncthing.toml",
                "packages = [\"syncthing-custom\"]\n",
            ),
        ],
        &[syncthing()],
    )
    .unwrap();

    insta::assert_snapshot!(explain(&evaluation, Some(&path("users"))).unwrap(), @r#"
    users.syncthing.packages = ["syncthing-custom"]
      set at users/syncthing.toml:1:12
      overrides ["syncthing"], default from module `syncthing`
      overrides [], default
    "#);
}

#[test]
fn config_references_resolve_options_declared_by_modules() {
    let evaluation = eval_with(
        &[
            (
                "system.toml",
                "[system]\nhostname = \"box\"\ntimezone = \"${services.syncthing.main.port}\"\n",
            ),
            ("services/sync.toml", "[services.syncthing.main]\n"),
        ],
        &[syncthing()],
    );

    insta::assert_snapshot!(evaluation.unwrap_err(), @r#"
    error: invalid value for `system.timezone`
     --> system.toml:3:12
      |
    3 | timezone = "${services.syncthing.main.port}"
      |            ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^ expected a string, found 8384
    "#);
}

#[test]
fn without_instances_the_module_contributes_nothing() {
    let evaluation = eval_with(&[SYSTEM], &[syncthing()]).unwrap();

    assert!(evaluation.modules()[0].output.units.is_empty());
    assert_eq!(evaluation.get("users.syncthing.packages"), None);
}

#[test]
fn modules_cannot_read_what_they_write() {
    let nosy = module("nosy", vec![], &["users"], |config| {
        read(config, "users.john.packages")?;
        Ok(Output::default())
    });

    insta::assert_snapshot!(eval_with(&[SYSTEM], &[nosy]).unwrap_err(), @"error: module `nosy` sets `users`, so it cannot read `users.john.packages`");
}

#[test]
fn reading_an_unknown_option_suggests_one() {
    let typo = module("typo", vec![], &[], |config| {
        read(config, "system.hostnam")?;
        Ok(Output::default())
    });

    insta::assert_snapshot!(eval_with(&[SYSTEM], &[typo]).unwrap_err(), @"error: unknown option `system.hostnam`; did you mean `system.hostname`?");
}

#[test]
fn reading_a_missing_map_entry_is_none() {
    let probe = module("probe", vec![], &[], |config| {
        assert_eq!(read(config, "services.nginx.missing.port")?, None);
        assert_eq!(
            read(config, "services.nginx")?,
            Some(Value::Table(Default::default()))
        );
        Ok(Output::default())
    });

    eval_with(&[SYSTEM], &[probe]).unwrap();
}

#[test]
fn settings_outside_declared_writes_are_errors() {
    let sneaky = module("sneaky", vec![], &["users"], |_| {
        Ok(Output {
            settings: vec![set(
                "system.timezone",
                Value::Str("UTC".into()),
                Level::Default,
            )],
            ..Output::default()
        })
    });

    insta::assert_snapshot!(eval_with(&[SYSTEM], &[sneaky]).unwrap_err(), @"error: module `sneaky` sets `system.timezone`, but only declares that it writes `users`");
}

#[test]
fn conflicting_module_defaults_name_both_modules() {
    let first = module("first", vec![], &["users"], |_| {
        Ok(Output {
            settings: vec![set("users.app.packages", strings(&["a"]), Level::Default)],
            ..Output::default()
        })
    });
    let second = module("second", vec![], &["system.timezone"], |_| {
        Ok(Output {
            settings: vec![set(
                "system.timezone",
                Value::Str("UTC".into()),
                Level::Default,
            )],
            ..Output::default()
        })
    });
    let third = module("third", vec![], &["system.timezone"], |_| {
        Ok(Output {
            settings: vec![set(
                "system.timezone",
                Value::Str("CET".into()),
                Level::Default,
            )],
            ..Output::default()
        })
    });

    insta::assert_snapshot!(eval_with(&[SYSTEM], &[first, second, third]).unwrap_err(), @r#"
    error: conflicting values for `system.timezone`
     = module `second`: set to "UTC" here
     = module `third`: set to "CET" here
     = help: remove all but one, or force one with `{ value = ..., force = "reason" }`
    "#);
}

#[test]
fn cycles_through_modules_are_reported() {
    let a = module(
        "a",
        vec![("x.a", OptionSpec::new(Type::Str).optional())],
        &["x.a"],
        |config| {
            read(config, "x.b")?;
            Ok(Output::default())
        },
    );
    let b = module(
        "b",
        vec![("x.b", OptionSpec::new(Type::Str).optional())],
        &["x.b"],
        |config| {
            read(config, "x.a")?;
            Ok(Output::default())
        },
    );

    insta::assert_snapshot!(eval_with(&[SYSTEM], &[a, b]).unwrap_err(), @"error: cycle: module `a` -> module `b` -> module `a`");
}

#[test]
fn a_failed_read_stops_the_module_without_a_second_error() {
    let reader = module("reader", vec![], &[], |config| {
        read(config, "services.nginx.main.port")?;
        Ok(Output::default())
    });

    insta::assert_snapshot!(eval_with(
        &[SYSTEM, ("services/web.toml", "[services.nginx.main]\nport = 0\n")],
        &[reader],
    ).unwrap_err(), @"
    error: invalid value for `services.nginx.main.port`
     --> services/web.toml:2:8
      |
    2 | port = 0
      |        ^ expected an integer from 1 to 65535, found 0
    ");
}

#[test]
fn a_module_removes_items_like_config_does() {
    let trim = module("trim", vec![], &["users.john.packages"], |_| {
        let removal = Value::Table(
            [("remove".to_owned(), strings(&["nano"]))]
                .into_iter()
                .collect(),
        );
        Ok(Output {
            settings: vec![set("users.john.packages", removal, Level::Default)],
            ..Output::default()
        })
    });
    let evaluation = eval_with(
        &[
            SYSTEM,
            ("users/john.toml", "packages = [\"helix\", \"nano\"]\n"),
        ],
        &[trim],
    )
    .unwrap();

    insta::assert_snapshot!(explain(&evaluation, Some(&path("users.john.packages"))).unwrap(), @r#"
    users.john.packages = ["helix"]
      set at users/john.toml:1:12
      overrides [], default
      removes ["nano"], default from module `trim`
    "#);
}
