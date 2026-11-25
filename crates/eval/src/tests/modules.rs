//! Modules: declaring options, reading config, setting other options and rendering outputs.

use super::eval_with;
use crate::{
    ConfigReader, Diagnostic, File, Level, ListMerge, Module, OptionPath, OptionSpec, Output,
    ReadError, RenderError, Secret, Section, Setting, Type, Unit, UnitAction, Value, explain,
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
                        ..Unit::default()
                    },
                );
            }
            if !instances.is_empty() {
                output.packages.insert("syncthing".into());
                output.settings.push(set(
                    "users.syncthing.packages",
                    strings(&["syncthing"]),
                    Level::ModuleDefault,
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
fn a_module_default_list_merges_with_config_and_can_be_removed_from() {
    let evaluation = eval_with(
        &[
            SYSTEM,
            ("services/sync.toml", "[services.syncthing.main]\n"),
            (
                "users/syncthing.toml",
                "include = [\"custom.toml\"]\npackages = { remove = [\"syncthing\"] }\n",
            ),
            ("custom.toml", "packages = [\"syncthing-custom\"]\n"),
        ],
        &[syncthing()],
    )
    .unwrap();

    insta::assert_snapshot!(explain(&evaluation, Some(&path("users"))).unwrap(), @r#"
    users.syncthing.packages = ["syncthing-custom"]
      set at custom.toml:1:12: ["syncthing-custom"]
      default from module `syncthing`: ["syncthing"]
      overrides [], default
      removes ["syncthing"], set at users/syncthing.toml:2:12
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
                Level::ModuleDefault,
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
            settings: vec![set(
                "users.app.packages",
                strings(&["a"]),
                Level::ModuleDefault,
            )],
            ..Output::default()
        })
    });
    let second = module("second", vec![], &["system.timezone"], |_| {
        Ok(Output {
            settings: vec![set(
                "system.timezone",
                Value::Str("UTC".into()),
                Level::ModuleDefault,
            )],
            ..Output::default()
        })
    });
    let third = module("third", vec![], &["system.timezone"], |_| {
        Ok(Output {
            settings: vec![set(
                "system.timezone",
                Value::Str("CET".into()),
                Level::ModuleDefault,
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
            settings: vec![set("users.john.packages", removal, Level::ModuleDefault)],
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

#[test]
fn table_reads_include_empty_maps_and_unset_records() {
    let probe = module("probe", vec![], &[], |config| {
        let Some(Value::Table(instance)) = read(config, "services.nginx.main")? else {
            panic!("expected a table");
        };
        assert_eq!(
            instance.get("sites"),
            Some(&Value::Table(Default::default()))
        );
        assert_eq!(instance.get("port"), Some(&Value::Int(80)));
        Ok(Output::default())
    });

    eval_with(
        &[SYSTEM, ("services/web.toml", "[services.nginx.main]\n")],
        &[probe],
    )
    .unwrap();
}

#[test]
fn an_empty_table_from_a_module_is_an_empty_list_where_a_list_is_expected() {
    let empty = module("empty", vec![], &["users.john.packages"], |_| {
        Ok(Output {
            settings: vec![set(
                "users.john.packages",
                Value::Table(Default::default()),
                Level::Normal,
            )],
            ..Output::default()
        })
    });
    let evaluation = eval_with(&[SYSTEM], &[empty]).unwrap();

    assert_eq!(
        evaluation.get("users.john.packages"),
        Some(&Value::List(vec![]))
    );
}

#[test]
fn two_modules_rendering_the_same_output_name_both() {
    let render = |_: &mut dyn ConfigReader| {
        let mut output = Output::default();
        output.files.insert(
            "/etc/motd".into(),
            File {
                text: "hi\n".into(),
                mode: 0o644,
            },
        );
        output.units.insert("app.service".into(), Unit::default());
        output.base.insert("audio".into(), Value::Bool(true));
        Ok(output)
    };
    let first = module("first", vec![], &[], render);
    let second = module("second", vec![], &[], render);

    insta::assert_snapshot!(eval_with(&[SYSTEM], &[first, second]).unwrap_err(), @"
    error: base setting `audio` is rendered by modules `first` and `second`
     = each one belongs to a single module; the others can set its options instead

    error: file `/etc/motd` is rendered by modules `first` and `second`
     = each one belongs to a single module; the others can set its options instead

    error: unit `app.service` is rendered by modules `first` and `second`
     = each one belongs to a single module; the others can set its options instead
    ");
}

#[test]
fn modules_contribute_base_settings() {
    let audio = module("audio", vec![], &[], |_| {
        let mut output = Output::default();
        output.base.insert("audio".into(), Value::Bool(true));
        Ok(output)
    });

    let evaluation = eval_with(&[SYSTEM], &[audio]).unwrap();

    assert_eq!(
        evaluation.modules()[0].output.base["audio"],
        Value::Bool(true)
    );
}

#[test]
fn a_unit_restarts_on_files_some_module_renders() {
    let render = |restart_on: &'static str| {
        move |_: &mut dyn ConfigReader| {
            let mut output = Output::default();
            output.files.insert(
                "/etc/app.conf".into(),
                File {
                    text: "x\n".into(),
                    mode: 0o644,
                },
            );
            output.units.insert(
                "app.service".into(),
                Unit {
                    restart_on: [restart_on.to_owned()].into(),
                    ..Unit::default()
                },
            );
            Ok(output)
        }
    };

    eval_with(
        &[SYSTEM],
        &[module("app", vec![], &[], render("/etc/app.conf"))],
    )
    .unwrap();
    insta::assert_snapshot!(
        eval_with(&[SYSTEM], &[module("app", vec![], &[], render("/etc/ap.conf"))]).unwrap_err(),
        @"error: unit `app.service` from module `app` restarts on `/etc/ap.conf`, which no module renders"
    );
}

#[test]
fn triggers_on_units_the_system_provides_merge_across_modules() {
    let network = |name: &'static str, action: UnitAction| {
        module(name, vec![], &[], move |_: &mut dyn ConfigReader| {
            let mut output = Output::default();
            let path = format!("/etc/systemd/network/{name}.network");
            output.files.insert(
                path.clone(),
                File {
                    text: "x\n".into(),
                    mode: 0o644,
                },
            );
            output
                .triggers
                .insert(("systemd-networkd.service".into(), action), [path].into());
            Ok(output)
        })
    };

    let evaluation = eval_with(
        &[SYSTEM],
        &[
            network("lan", UnitAction::Reload),
            network("wifi", UnitAction::Reload),
        ],
    )
    .unwrap();

    let on: Vec<_> = evaluation
        .modules()
        .iter()
        .flat_map(|rendered| rendered.output.triggers.values().flatten())
        .collect();
    assert_eq!(
        on,
        [
            "/etc/systemd/network/lan.network",
            "/etc/systemd/network/wifi.network"
        ]
    );
}

#[test]
fn a_trigger_needs_rendered_files_and_a_unit_no_module_renders() {
    let render = |unit: &'static str, file: &'static str| {
        move |_: &mut dyn ConfigReader| {
            let mut output = Output::default();
            output.files.insert(
                "/etc/app.conf".into(),
                File {
                    text: "x\n".into(),
                    mode: 0o644,
                },
            );
            output.units.insert("app.service".into(), Unit::default());
            output
                .triggers
                .insert((unit.into(), UnitAction::Restart), [file.to_owned()].into());
            Ok(output)
        }
    };
    let app = |unit, file| module("app", vec![], &[], render(unit, file));

    insta::assert_snapshot!(
        eval_with(&[SYSTEM], &[app("other.service", "/etc/ap.conf")]).unwrap_err(),
        @"error: unit `other.service` from module `app` restarts on `/etc/ap.conf`, which no module renders"
    );
    assert!(eval_with(&[SYSTEM], &[app("app.service", "/etc/app.conf")]).is_ok());
    insta::assert_snapshot!(
        eval_with(&[SYSTEM], &[app("other.socket", "/etc/app.conf")]).unwrap_err(),
        @"error: module `app` triggers `other.socket`; only services reload or restart"
    );
}

/// A module that declares the secret `name`, encrypted in `file`.
pub(super) fn secrets(
    module_name: &'static str,
    name: &'static str,
    file: &'static str,
) -> Box<dyn Module> {
    module(module_name, vec![], &[], move |_| {
        let mut output = Output::default();
        let secret = Secret {
            file: file.into(),
            owner: "root".into(),
            group: None,
            mode: 0o400,
            link: None,
        };
        output.secrets.insert(name.into(), secret);
        Ok(output)
    })
}

#[test]
fn a_module_reads_a_secret_in_a_path_option_as_the_path_it_is_decrypted_to() {
    let app = module(
        "app",
        vec![
            ("app.key", OptionSpec::new(Type::Path).runtime()),
            (
                "app.keys",
                OptionSpec::new(Type::List {
                    item: Box::new(Type::Path),
                    merge: ListMerge::Set,
                })
                .runtime(),
            ),
            ("app.password", OptionSpec::new(Type::Str).runtime()),
        ],
        &[],
        |config| {
            let mut output = Output::default();
            let app = read(config, "app")?.unwrap();
            output.files.insert(
                "/etc/app.conf".into(),
                File {
                    text: format!("{app}\n"),
                    mode: 0o644,
                },
            );
            Ok(output)
        },
    );
    let config = (
        "system.toml",
        r#"
[system]
hostname = "box"

[app]
key = { secret = "key" }
keys = [{ secret = "key" }, "/etc/other.pem"]
password = { secret = "key" }
"#,
    );

    let evaluation = eval_with(
        &[config, ("secrets/key.age", "ciphertext")],
        &[secrets("secrets", "key", "secrets/key.age"), app],
    )
    .unwrap();

    let app = evaluation
        .modules()
        .iter()
        .find(|rendered| rendered.name == "app")
        .unwrap();
    insta::assert_snapshot!(app.output.files["/etc/app.conf"].text, @r#"{ key = "/run/wrath/secrets/key", keys = ["/etc/other.pem", "/run/wrath/secrets/key"], password = { secret = "key" } }"#);
    let file = &evaluation.secret_files()["key"];
    assert_eq!(file.source, "secrets/key.age");
    assert_eq!(file.hash, blake3::hash(b"ciphertext").to_hex().to_string());
}

#[test]
fn a_secret_belongs_to_one_module_and_its_file_must_be_readable() {
    insta::assert_snapshot!(
        eval_with(
            &[SYSTEM, ("secrets/key.age", "x")],
            &[secrets("a", "key", "secrets/key.age"), secrets("b", "key", "secrets/key.age")],
        )
        .unwrap_err(),
        @"
    error: secret `key` is rendered by modules `a` and `b`
     = each one belongs to a single module; the others can set its options instead
    "
    );
    insta::assert_snapshot!(
        eval_with(&[SYSTEM], &[secrets("a", "key", "secrets/key.age")]).unwrap_err(),
        @"
    error: cannot read secret `key` from secrets/key.age: entity not found
     = help: create it with `wrath secret edit key`, or set its `file`
    "
    );
}

#[test]
fn modules_declare_options_every_user_has() {
    let git = module(
        "git",
        vec![(
            "users.\"*\".programs.git.enable",
            OptionSpec::new(Type::Bool).default(Value::Bool(false)),
        )],
        &[],
        |config| {
            let mut output = Output::default();
            let Some(Value::Table(users)) = read(config, "users")? else {
                unreachable!("users is a table");
            };
            for (name, user) in users {
                let enabled = user.to_string().contains("enable = true");
                output.packages.insert(format!("{name}:{enabled}"));
            }
            Ok(output)
        },
    );
    let files = [
        SYSTEM,
        ("users/john.toml", "[programs.git]\nenable = true\n"),
        ("users/jane.toml", "packages = [\"vim\"]\n"),
    ];

    let evaluation = eval_with(&files, &[git]).unwrap();

    assert_eq!(
        evaluation.modules()[0].output.packages,
        ["jane:false".to_owned(), "john:true".to_owned()].into()
    );
    insta::assert_snapshot!(
        explain(&evaluation, Some(&path("users.john.programs.git.enable"))).unwrap(),
        @"
    users.john.programs.git.enable = true
      set at users/john.toml:2:10
      overrides false, default
    "
    );
}

#[test]
fn a_default_in_a_file_beats_a_module_default() {
    let zone = module("zone", vec![], &["system.timezone"], |_| {
        Ok(Output {
            settings: vec![set(
                "system.timezone",
                Value::Str("CET".into()),
                Level::ModuleDefault,
            )],
            ..Output::default()
        })
    });
    let evaluation = eval_with(
        &[(
            "system.toml",
            "[system]\nhostname = \"box\"\ntimezone = { default = \"Europe/London\" }\n",
        )],
        &[zone],
    )
    .unwrap();

    insta::assert_snapshot!(explain(&evaluation, Some(&path("system.timezone"))).unwrap(), @r#"
    system.timezone = "Europe/London"
      default at system.toml:3:12
      overrides "CET", default from module `zone`
      overrides "UTC", default
    "#);
}

/// A module that says the system boots with Secure Boot, or not.
fn secure_boot(on: bool) -> Box<dyn Module> {
    module("base", vec![], &[], move |_| {
        let mut output = Output::default();
        let boot = [("secure_boot".to_owned(), Value::Bool(on))].into();
        output.base.insert("boot".into(), Value::Table(boot));
        Ok(output)
    })
}

#[test]
fn a_system_with_secure_boot_carries_each_machines_keys_for_that_machine_only() {
    let files = [
        SYSTEM,
        ("secrets/machines/w-01/secure-boot.age", "one"),
        ("secrets/machines/w-01/ssh_host_ed25519_key.age", "host"),
        ("secrets/machines/w-02/ssh_host_ed25519_key.age", "host"),
    ];

    let evaluation = eval_with(&files, &[secure_boot(true)]).unwrap();
    let keys = evaluation.machine_secrets();
    assert_eq!(keys.keys().collect::<Vec<_>>(), ["secure-boot.w-01"]);
    let keys = &keys["secure-boot.w-01"];
    assert_eq!(keys.machine, "w-01");
    assert_eq!(keys.file.source, "secrets/machines/w-01/secure-boot.age");
    assert_eq!(keys.file.hash, blake3::hash(b"one").to_hex().to_string());

    let evaluation = eval_with(&files, &[secure_boot(false)]).unwrap();
    assert!(evaluation.machine_secrets().is_empty());
}
