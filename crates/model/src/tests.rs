use std::collections::BTreeSet;

use wrath_eval::{RuntimePart, Section, Template, Unit, UnitAction, Value, Wrapper};

use crate::{Change, ChangeKind, File, Lock, Model, Plan, Secret, Source, WRATH_SOURCE, plan};

fn model(port: i64) -> Model {
    let mut model = Model::default();
    model
        .options
        .insert("services.nginx.main.port".into(), Value::Int(port));
    model
        .options
        .insert("system.hostname".into(), Value::Str("box".into()));
    model.packages.insert("nginx".into());
    model.files.insert(
        "/etc/nginx/main.conf".into(),
        File {
            mode: 0o644,
            text: format!("listen {port};\n"),
        },
    );
    model.units.insert(
        "nginx-main.service".into(),
        Unit {
            sections: vec![Section {
                name: "Service".into(),
                entries: vec![("ExecStart".into(), format!("nginx -p {port}"))],
            }],
            restart_on: ["/etc/nginx/main.conf".into()].into(),
        },
    );
    model
}

#[test]
fn the_hash_follows_content_only() {
    assert_eq!(model(8080).hash(), model(8080).hash());
    assert_ne!(model(8080).hash(), model(8081).hash());
    assert_eq!(model(8080).hash().len(), 64);
}

#[test]
fn runtime_values_are_spelled_out_in_the_canonical_form() {
    let mut model = Model::default();
    model.options.insert(
        "grafana.password".into(),
        Value::Runtime(vec![
            RuntimePart::Text("pw:".into()),
            RuntimePart::Secret("grafana".into()),
        ]),
    );

    insta::assert_snapshot!(model.canonical(), @r#"
    {
      "base": {},
      "builds": {},
      "enabled": {},
      "enabled_user": {},
      "files": {},
      "homes": {},
      "machine": [],
      "next_login": [],
      "options": {
        "grafana.password": {
          "runtime": [
            {
              "text": "pw:"
            },
            {
              "secret": "grafana"
            }
          ]
        }
      },
      "packages": [],
      "secrets": {},
      "service_packages": [],
      "sources": {},
      "templates": {},
      "triggers": {},
      "units": {},
      "version": 14,
      "wrappers": {}
    }
    "#);
}

#[test]
fn plan_shows_changed_options_and_what_they_render() {
    insta::assert_snapshot!(plan(&model(8080), &model(8081)).to_string(), @"
    options
      ~ services.nginx.main.port: 8080 -> 8081
    files
      ~ /etc/nginx/main.conf (tier: live)
    units
      ~ nginx-main.service (tier: live)
          - Service.ExecStart=nginx -p 8080
          + Service.ExecStart=nginx -p 8081
    3 changes, tier: live
    ");
}

#[test]
fn plan_from_nothing_adds_everything() {
    let added = plan(&Model::default(), &model(80));

    insta::assert_snapshot!(added.to_string(), @"
    options
      + 2 options; `wrath why` shows where each is set
    packages
      + nginx (tier: live)
    files
      + /etc/nginx/main.conf (tier: live)
    units
      + nginx-main.service (tier: live)
    5 changes, tier: live
    ");
}

#[test]
fn an_unchanged_model_has_an_empty_plan() {
    let empty = plan(&model(80), &model(80));

    assert!(empty.is_empty());
    assert_eq!(empty.tier(), None);
    assert_eq!(empty.to_string(), "no changes\n");
    let options_only = {
        let mut changed = model(80);
        changed
            .options
            .insert("system.hostname".into(), Value::Str("other".into()));
        plan(&model(80), &changed)
    };
    assert!(
        options_only
            .to_string()
            .ends_with("1 change, nothing to apply\n")
    );
}

#[test]
fn base_changes_show_their_tier_and_boot_needs_a_reboot() {
    let mut before = model(80);
    before.base.insert("audio".into(), Value::Bool(false));
    before
        .base
        .insert("boot".into(), Value::Str("systemd-boot".into()));
    let mut after = before.clone();
    after.base.insert("audio".into(), Value::Bool(true));
    after.base.insert("boot".into(), Value::Str("grub".into()));

    insta::assert_snapshot!(plan(&before, &after).to_string(), @r#"
    base
      ~ audio (tier: live)
          false -> true
      ~ boot (tier: reboot)
          "systemd-boot" -> "grub"
    2 changes, tier: reboot
    "#);
}

#[test]
fn a_long_value_shows_only_what_changed_in_it() {
    let mount = |path: &str, options: Vec<Value>| {
        Value::Table(
            [
                (
                    "device".to_owned(),
                    Value::Str(format!("/dev/disk/by-partlabel{path}")),
                ),
                ("options".to_owned(), Value::List(options)),
                ("path".to_owned(), Value::Str(path.to_owned())),
            ]
            .into(),
        )
    };
    let storage = |boot_options: Vec<Value>, state: &str| {
        Value::Table(
            [
                (
                    "mounts".to_owned(),
                    Value::List(vec![mount("/", Vec::new()), mount("/boot", boot_options)]),
                ),
                ("state".to_owned(), Value::Str(state.to_owned())),
            ]
            .into(),
        )
    };
    let mut before = model(80);
    before
        .base
        .insert("storage".into(), storage(Vec::new(), ""));
    let mut after = before.clone();
    after.base.insert(
        "storage".into(),
        storage(vec![Value::Str("umask=0077".into())], "/keep"),
    );

    insta::assert_snapshot!(plan(&before, &after).to_string(), @r#"
    base
      ~ storage (tier: live)
          mounts[1].options: [] -> ["umask=0077"]
          state: "" -> "/keep"
    1 change, tier: live
    "#);
}

#[test]
fn a_unit_restarts_when_a_file_it_reads_changes() {
    let before = model(80);
    let mut after = before.clone();
    after.files.get_mut("/etc/nginx/main.conf").unwrap().text = "listen 80; # changed\n".into();

    insta::assert_snapshot!(plan(&before, &after).to_string(), @"
    files
      ~ /etc/nginx/main.conf (tier: live)
    units
      ~ nginx-main.service (tier: live)
          restarts: /etc/nginx/main.conf changed
    2 changes, tier: live
    ");
}

#[test]
fn services_the_system_provides_reload_or_restart_when_their_files_change() {
    let network = |text: &str, dns: &str| {
        let mut model = Model::default();
        for (path, text) in [
            ("/etc/systemd/network/10-lan.network", text),
            ("/etc/systemd/resolved.conf.d/dns.conf", dns),
        ] {
            let file = File {
                mode: 0o644,
                text: text.into(),
            };
            model.files.insert(path.into(), file);
        }
        model.triggers.insert(
            ("systemd-networkd.service".into(), UnitAction::Reload),
            ["/etc/systemd/network/10-lan.network".into()].into(),
        );
        model.triggers.insert(
            ("systemd-resolved.service".into(), UnitAction::Restart),
            ["/etc/systemd/resolved.conf.d/dns.conf".into()].into(),
        );
        model
    };
    let before = network("DHCP=yes\n", "DNSSEC=no\n");

    insta::assert_snapshot!(plan(&before, &network("DHCP=no\n", "DNSSEC=yes\n")).to_string(), @"
    files
      ~ /etc/systemd/network/10-lan.network (tier: live)
      ~ /etc/systemd/resolved.conf.d/dns.conf (tier: live)
    units
      ~ systemd-networkd.service (tier: live)
          reloads: /etc/systemd/network/10-lan.network changed
      ~ systemd-resolved.service (tier: live)
          restarts: /etc/systemd/resolved.conf.d/dns.conf changed
    4 changes, tier: live
    ");
    insta::assert_snapshot!(plan(&before, &network("DHCP=no\n", "DNSSEC=no\n")).to_string(), @"
    files
      ~ /etc/systemd/network/10-lan.network (tier: live)
    units
      ~ systemd-networkd.service (tier: live)
          reloads: /etc/systemd/network/10-lan.network changed
    2 changes, tier: live
    ");
}

#[test]
fn units_that_mention_a_secret_restart_when_it_changes() {
    let with_secrets = |token_hash: &str, owner: &str| {
        let mut model = Model::default();
        for (name, hash) in [("api-token", token_hash), ("api-token-old", "aaa")] {
            let secret = Secret {
                source: format!("/srv/config/secrets/{name}.age"),
                hash: hash.into(),
                owner: owner.into(),
                group: None,
                mode: 0o400,
                link: None,
                machine: None,
            };
            model.secrets.insert(name.into(), secret);
        }
        for (name, uses) in [
            ("app.service", "/run/wrath/secrets/api-token"),
            ("old.service", "/run/wrath/secrets/api-token-old"),
        ] {
            let unit = Unit {
                sections: vec![Section {
                    name: "Service".into(),
                    entries: vec![("LoadCredential".into(), format!("token:{uses}"))],
                }],
                ..Unit::default()
            };
            model.units.insert(name.into(), unit);
        }
        model
    };
    let before = with_secrets("abc", "root");

    assert_eq!(
        before.secrets_of(&before.units["app.service"]),
        ["api-token"].into()
    );
    insta::assert_snapshot!(plan(&before, &with_secrets("def", "root")).to_string(), @"
    secrets
      ~ api-token (tier: live)
          contents changed
    units
      ~ app.service (tier: live)
          restarts: secret api-token changed
    2 changes, tier: live
    ");
    insta::assert_snapshot!(plan(&before, &with_secrets("abc", "app")).to_string(), @"
    secrets
      ~ api-token (tier: live)
          owner root -> app
      ~ api-token-old (tier: live)
          owner root -> app
    units
      ~ app.service (tier: live)
          restarts: secret api-token changed
      ~ old.service (tier: live)
          restarts: secret api-token-old changed
    4 changes, tier: live
    ");

    let mut moved = before.clone();
    moved.secrets.get_mut("api-token").unwrap().source = "/elsewhere/api-token.age".into();
    assert_eq!(moved.hash(), before.hash());
    assert!(plan(&before, &moved).is_empty());
}

#[test]
fn plan_shows_files_packages_and_units_in_users_homes() {
    let with_home = |text: &str| {
        let mut model = Model::default();
        let home = model.homes.entry("john".into()).or_default();
        let file = File {
            mode: 0o644,
            text: text.into(),
        };
        home.files.insert("~/.config/git/config".into(), file);
        home.packages.insert("git".into());
        let unit = Unit {
            sections: vec![Section {
                name: "Service".into(),
                entries: vec![("ExecStart".into(), format!("sync {text}"))],
            }],
            ..Unit::default()
        };
        home.units.insert("sync.service".into(), unit);
        model
    };

    insta::assert_snapshot!(plan(&Model::default(), &with_home("a")).to_string(), @"
    packages
      + git (for john) (tier: live)
    files
      + ~john/.config/git/config (tier: live)
    units
      + sync.service (for john) (tier: live)
    3 changes, tier: live
    ");
    insta::assert_snapshot!(plan(&with_home("a"), &with_home("b")).to_string(), @"
    files
      ~ ~john/.config/git/config (tier: live)
    units
      ~ sync.service (for john) (tier: next-login)
          - Service.ExecStart=sync a
          + Service.ExecStart=sync b
          running sessions keep the old one until the next login
    2 changes, tier: next-login
    ");
}

/// A model with `ripgrep` for the system and `git` for john, under `lock`.
fn with_packages(lock: &Lock) -> Model {
    let mut model = Model::default();
    model.packages.insert("ripgrep".into());
    let home = model.homes.entry("john".into()).or_default();
    home.packages.insert("git".into());
    model.with_lock(lock)
}

fn source(url: &str, rev: &str) -> Source {
    Source {
        url: url.into(),
        rev: Some(rev.into()),
        hash: format!("sha256-{rev}"),
        date: None,
    }
}

/// A lock that pins wrath at `rev`.
fn lock(rev: &str) -> Lock {
    Lock {
        sources: [(WRATH_SOURCE.to_owned(), source(WRATH, rev))].into(),
        ..Lock::default()
    }
}

const WRATH: &str = "github:johnae/wrath";
const ACME: &str = "github:acme/recipes";

#[test]
fn the_model_keeps_the_sources_the_lock_pins() {
    let lock = lock("aaaaaaa1");
    let model = with_packages(&lock);

    assert_eq!(model.sources, lock.sources);
    assert_ne!(model.hash(), with_packages(&Lock::default()).hash());
}

#[test]
fn plan_shows_packages_and_where_wrath_moved() {
    let before = with_packages(&lock("aaaaaaa1"));
    let mut after = with_packages(&lock("bbbbbbb2"));
    after.packages.insert("htop".into());

    insta::assert_snapshot!(plan(&before, &after).to_string(), @"
    packages
      ~ source wrath (tier: live)
          github:johnae/wrath: aaaaaaa1 -> bbbbbbb2
      + htop (tier: live)
    2 changes, tier: live
    ");
    insta::assert_snapshot!(plan(&Model::default(), &before).to_string(), @"
    packages
      + source wrath (tier: live)
          github:johnae/wrath at aaaaaaa1
      + git (for john) (tier: live)
      + ripgrep (tier: live)
    3 changes, tier: live
    ");
}

/// A model with something in every part.
fn full_model() -> Model {
    let mut model = with_packages(&lock("aaaaaaa1"));
    model.options.insert(
        "services.app.token".into(),
        Value::Runtime(vec![
            RuntimePart::Text("Bearer ".into()),
            RuntimePart::Secret("token".into()),
            RuntimePart::QuotedSecret("token".into()),
        ]),
    );
    model
        .options
        .insert("services.app.ratio".into(), Value::Float(1.0));
    model.options.insert(
        "services.app.tags".into(),
        Value::List(vec![Value::Str("a".into()), Value::Int(-3)]),
    );
    let file = File {
        mode: 0o600,
        text: "x\n".into(),
    };
    model.files.insert("/etc/app.conf".into(), file.clone());
    let unit = Unit {
        sections: vec![Section {
            name: "Service".into(),
            entries: vec![
                ("ExecStartPre".into(), "a".into()),
                ("ExecStartPre".into(), "b".into()),
            ],
        }],
        restart_on: ["/etc/app.conf".into()].into(),
    };
    model.units.insert("app.service".into(), unit.clone());
    model.base.insert("audio".into(), Value::Bool(true));
    model.triggers.insert(
        ("systemd-networkd.service".into(), UnitAction::Reload),
        ["/etc/app.conf".into()].into(),
    );
    model.machine.insert("facts.cpu.count".into());
    model.secrets.insert(
        "token".into(),
        Secret {
            source: String::new(),
            hash: "h".into(),
            owner: "app".into(),
            group: Some("wheel".into()),
            mode: 0o440,
            link: Some("/home/app/token".into()),
            machine: None,
        },
    );
    model.secrets.insert(
        "secure-boot.w-01".into(),
        Secret {
            source: String::new(),
            hash: "k".into(),
            owner: "root".into(),
            group: None,
            mode: 0o400,
            link: None,
            machine: Some("w-01".into()),
        },
    );
    model.templates.insert(
        "/etc/app.env".into(),
        Template {
            parts: vec![
                RuntimePart::Text("TOKEN=".into()),
                RuntimePart::QuotedSecret("token".into()),
            ],
            owner: "app".into(),
            group: Some("app".into()),
            mode: 0o440,
        },
    );
    model.next_login.insert("~john/.config/app".into());
    let template = model.templates["/etc/app.env"].clone();
    let home = model.homes.get_mut("john").unwrap();
    home.files.insert("~/.config/app".into(), file);
    home.units.insert("sync.service".into(), unit);
    home.templates.insert("~/.netrc".into(), template);
    model
}

#[test]
fn a_model_reads_back_from_its_canonical_form() {
    let model = full_model();

    let read = Model::from_json(&model.canonical()).unwrap();

    assert_eq!(read, model);
    assert_eq!(read.hash(), model.hash());
    assert!(plan(&read, &model).is_empty());
}

#[test]
fn models_of_another_format_version_are_refused() {
    let older = Model::default().canonical().replace(
        &format!("\"version\": {}", crate::VERSION),
        "\"version\": 1",
    );

    assert_eq!(
        Model::from_json(&older).unwrap_err(),
        format!(
            "the model is in format version 1, and this wrath reads version {}",
            crate::VERSION
        )
    );
    assert!(Model::from_json("[]").is_err());
}

/// A model with a file `/etc/app.env` that has the secret `token` in it, and a unit that
/// restarts when the file does.
fn with_template(owner: &str, text: &str, secret_hash: &str) -> Model {
    let mut model = Model::default();
    model.secrets.insert(
        "token".into(),
        Secret {
            source: String::new(),
            hash: secret_hash.into(),
            owner: "root".into(),
            group: None,
            mode: 0o400,
            link: None,
            machine: None,
        },
    );
    model.templates.insert(
        "/etc/app.env".into(),
        Template {
            parts: vec![
                RuntimePart::Text(text.into()),
                RuntimePart::Secret("token".into()),
            ],
            owner: owner.into(),
            group: None,
            mode: 0o400,
        },
    );
    let unit = Unit {
        sections: vec![Section {
            name: "Service".into(),
            entries: vec![("ExecStart".into(), "app".into())],
        }],
        restart_on: ["/etc/app.env".into()].into(),
    };
    model.units.insert("app.service".into(), unit);
    model
}

#[test]
fn plan_shows_files_with_secrets_and_restarts_what_reads_them() {
    let before = with_template("app", "TOKEN=", "h1");

    insta::assert_snapshot!(plan(&Model::default(), &before).to_string(), @"
    files
      + /etc/app.env (tier: live)
          with secrets, filled in on the machine for app, mode 400
    secrets
      + token (tier: live)
    units
      + app.service (tier: live)
    3 changes, tier: live
    ");
    insta::assert_snapshot!(plan(&before, &with_template("root", "KEY=", "h1")).to_string(), @"
    files
      ~ /etc/app.env (tier: live)
          text changed
          owner app -> root
    units
      ~ app.service (tier: live)
          restarts: /etc/app.env changed
    2 changes, tier: live
    ");
    insta::assert_snapshot!(plan(&before, &with_template("app", "TOKEN=", "h2")).to_string(), @"
    secrets
      ~ token (tier: live)
          contents changed
    units
      ~ app.service (tier: live)
          restarts: secret token changed
    2 changes, tier: live
    ");
}

#[test]
fn files_read_at_login_change_at_the_next_login() {
    let with_session = |text: &str| {
        let mut model = Model::default();
        let home = model.homes.entry("john".into()).or_default();
        let file = File {
            mode: 0o644,
            text: text.into(),
        };
        home.files
            .insert("~/.config/environment.d/60-wrath.conf".into(), file);
        model
            .next_login
            .insert("~john/.config/environment.d/60-wrath.conf".into());
        model
    };

    let plan = plan(&with_session("EDITOR=hx\n"), &with_session("EDITOR=vi\n"));

    insta::assert_snapshot!(plan.to_string(), @"
    files
      ~ ~john/.config/environment.d/60-wrath.conf (tier: next-login)
    1 change, tier: next-login
    ");
}

#[test]
fn only_changes_to_setting_files_are_settings() {
    let model = |gaps: &str, sudoers: &str| {
        let mut model = Model::default();
        let file = |text: &str| File {
            mode: 0o644,
            text: text.into(),
        };
        let home = model.homes.entry("john".into()).or_default();
        home.files
            .insert("~/.config/niri/config.kdl".into(), file(gaps));
        model.files.insert("/etc/sudoers".into(), file(sudoers));
        model
            .setting_files
            .insert("~john/.config/niri/config.kdl".into());
        model
    };
    let before = model("gaps 4\n", "root ALL\n");

    assert!(plan(&before, &model("gaps 8\n", "root ALL\n")).settings_only());
    assert!(!plan(&before, &model("gaps 4\n", "%wheel ALL\n")).settings_only());
    assert!(!plan(&before, &model("gaps 8\n", "%wheel ALL\n")).settings_only());

    let mut with_package = model("gaps 8\n", "root ALL\n");
    with_package.packages.insert("jq".into());
    assert!(!plan(&before, &with_package).settings_only());

    let mut newly_a_setting = model("gaps 4\n", "root ALL\n");
    newly_a_setting.setting_files.clear();
    assert!(plan(&newly_a_setting, &model("gaps 8\n", "root ALL\n")).settings_only());

    let mut no_longer_a_setting = model("gaps 8\n", "root ALL\n");
    no_longer_a_setting.setting_files.clear();
    assert!(!plan(&before, &no_longer_a_setting).settings_only());
}

#[test]
fn plan_shows_files_with_secrets_in_homes() {
    let mut after = Model::default();
    let template = with_template("john", "password ", "h1").templates["/etc/app.env"].clone();
    after
        .homes
        .entry("john".into())
        .or_default()
        .templates
        .insert("~/.netrc".into(), template);

    insta::assert_snapshot!(plan(&Model::default(), &after).to_string(), @"
    files
      + ~john/.netrc (tier: live)
          with secrets, filled in on the machine for john, mode 400
    1 change, tier: live
    ");
}

/// `with_packages` plus files from a declared `acme` source that follows `url`.
fn with_acme(url: &str, lock: &Lock) -> Model {
    let mut model = Model::default();
    model.packages.insert("ripgrep".into());
    model.sources.insert("acme".into(), Source::follow(url));
    model.with_lock(lock)
}

fn two_sources(acme_rev: &str) -> Lock {
    let mut lock = lock("aaaaaaa1");
    lock.sources.insert("acme".into(), source(ACME, acme_rev));
    lock.sources
        .insert("gone".into(), source("github:x/y", "cccccc3"));
    lock
}

#[test]
fn named_sources_keep_their_pin_while_their_url_is_the_one_locked() {
    let model = with_acme(ACME, &two_sources("bbbbbbb2"));
    assert_eq!(
        model.sources.keys().collect::<Vec<_>>(),
        ["acme", "wrath"],
        "sources nothing declares are left out, except wrath"
    );
    assert!(model.sources["acme"].is_locked());
    assert!(model.is_locked());

    let moved = with_acme("github:acme/elsewhere", &two_sources("bbbbbbb2"));
    assert_eq!(
        moved.sources["acme"],
        Source::follow("github:acme/elsewhere")
    );
    assert!(!moved.is_locked());

    assert!(!with_acme(ACME, &Lock::default()).is_locked());
    let mut unused = Model::default();
    unused.packages.insert("ripgrep".into());
    assert!(!unused.is_locked(), "without a lock, wrath isn't pinned");
}

#[test]
fn plan_shows_each_source_and_only_wrath_needs_a_soft_reboot() {
    let before = with_acme(ACME, &two_sources("bbbbbbb2"));
    let after = with_acme(ACME, &two_sources("ddddddd4"));
    insta::assert_snapshot!(plan(&before, &after).to_string(), @"
    packages
      ~ source acme (tier: live)
          github:acme/recipes: bbbbbbb2 -> ddddddd4
    1 change, tier: live
    ");

    let unpinned = with_acme("github:acme/elsewhere", &two_sources("bbbbbbb2"));
    insta::assert_snapshot!(plan(&before, &unpinned).to_string(), @"
    packages
      ~ source acme (tier: live)
          github:acme/recipes at bbbbbbb2 -> github:acme/elsewhere at its newest version
    1 change, tier: live
    ");
}

#[test]
fn a_plan_in_one_line_names_what_people_change_first() {
    let change = |kind, name: &str| Change {
        kind,
        name: name.into(),
        details: Vec::new(),
        tier: None,
    };
    let plan = Plan {
        options: vec![
            change(ChangeKind::Changed, "services.web.port"),
            change(ChangeKind::Added, "users.john.shell"),
        ],
        packages: vec![change(ChangeKind::Removed, "htop")],
        files: vec![change(ChangeKind::Changed, "/etc/web.conf")],
        ..Plan::default()
    };
    assert_eq!(
        plan.summary(5),
        "~ services.web.port, + users.john.shell, - htop"
    );
    assert_eq!(plan.summary(1), "~ services.web.port and 2 more");

    let only_files = Plan {
        files: vec![change(ChangeKind::Changed, "/etc/web.conf")],
        ..Plan::default()
    };
    assert_eq!(only_files.summary(5), "~ /etc/web.conf");
    assert_eq!(Plan::default().summary(5), "no changes");
}

#[test]
fn the_activation_stops_starts_restarts_and_reloads_what_the_plan_shows() {
    let unit = |exec: &str| Unit {
        sections: vec![Section {
            name: "Service".into(),
            entries: vec![("ExecStart".into(), exec.into())],
        }],
        restart_on: ["/etc/web.conf".into()].into(),
    };
    let file = |text: &str| File {
        mode: 0o644,
        text: text.into(),
    };
    let mut before = Model::default();
    before.units.insert("gone.service".into(), unit("a"));
    before.units.insert("edited.service".into(), unit("a"));
    before.units.insert("web.service".into(), unit("a"));
    before.files.insert("/etc/web.conf".into(), file("1\n"));
    let home = before.homes.entry("john".into()).or_default();
    home.units.insert("mine.service".into(), unit("a"));
    let mut after = before.clone();
    after.units.remove("gone.service");
    after.units.insert("edited.service".into(), unit("b"));
    after.units.insert("new.service".into(), unit("a"));
    after.files.insert("/etc/web.conf".into(), file("2\n"));
    for (service, action) in [
        ("systemd-networkd.service", UnitAction::Reload),
        ("web.service", UnitAction::Reload),
    ] {
        after
            .triggers
            .insert((service.into(), action), ["/etc/web.conf".into()].into());
    }
    let home = after.homes.get_mut("john").unwrap();
    home.units.insert("mine.service".into(), unit("b"));

    let activation = plan(&before, &after).activation;
    let names = |set: &std::collections::BTreeSet<String>| set.iter().cloned().collect::<Vec<_>>();
    assert_eq!(names(&activation.stop), ["gone.service"]);
    assert_eq!(names(&activation.start), ["new.service"]);
    assert_eq!(
        names(&activation.restart),
        ["edited.service", "web.service"],
        "a unit that restarts anyway isn't reloaded too, and users' units are their homes'"
    );
    assert_eq!(names(&activation.reload), ["systemd-networkd.service"]);

    after.triggers.insert(
        ("edited.service".into(), UnitAction::Reload),
        ["/etc/web.conf".into()].into(),
    );
    let units = plan(&before, &after).units;
    let edited: Vec<_> = units
        .iter()
        .filter(|u| u.name == "edited.service")
        .collect();
    assert_eq!(
        edited.len(),
        1,
        "a unit that restarts anyway shows once: {units:?}"
    );
}

#[test]
fn templates_mounts_and_units_that_ask_not_to_are_left_running_when_they_change() {
    let unit = |section: &str, entries: &[(&str, &str)]| Unit {
        sections: vec![Section {
            name: section.into(),
            entries: entries
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        }],
        restart_on: BTreeSet::new(),
    };
    let mut before = Model::default();
    let names = [
        "worker@.service",
        "data.mount",
        "swapfile.swap",
        "greetd.service",
        "web.service",
    ];
    for name in names {
        before
            .units
            .insert(name.into(), unit("Service", &[("ExecStart", "a")]));
    }
    let mut after = Model::default();
    for name in names {
        after
            .units
            .insert(name.into(), unit("Service", &[("ExecStart", "b")]));
    }
    after.units.insert(
        "greetd.service".into(),
        Unit {
            sections: vec![
                unit("Unit", &[("X-RestartIfChanged", "false")]).sections[0].clone(),
                unit("Service", &[("ExecStart", "b")]).sections[0].clone(),
            ],
            restart_on: BTreeSet::new(),
        },
    );
    after
        .units
        .insert("job@.service".into(), unit("Service", &[]));
    let activation = plan(&before, &after).activation;
    assert_eq!(
        activation.restart,
        BTreeSet::from(["web.service".to_owned()])
    );
    assert!(activation.start.is_empty() && activation.reload.is_empty());

    let activation = plan(&after, &Model::default()).activation;
    assert!(!activation.stop.contains("job@.service"));
    assert!(activation.stop.contains("greetd.service"));
}

#[test]
fn a_drop_in_can_keep_a_package_unit_running() {
    let mut after = Model::default();
    let keep = Unit {
        sections: vec![Section {
            name: "Unit".into(),
            entries: vec![("X-RestartIfChanged".into(), "false".into())],
        }],
        restart_on: BTreeSet::new(),
    };
    after.units.insert("dbus.service.d/wrath.conf".into(), keep);
    let mut activation = plan(&Model::default(), &after).activation;
    activation.restart_too(&after, ["dbus.service".into(), "sshd.service".into()]);
    assert_eq!(
        activation.restart,
        BTreeSet::from(["sshd.service".to_owned()])
    );
}

#[test]
fn enabling_a_unit_starts_it_and_wrappers_show_as_files() {
    let before = Model::default();
    let mut after = Model::default();
    after
        .enabled
        .insert("polkit.service".into(), BTreeSet::new());
    after.enabled_user.insert(
        "pipewire.service".into(),
        BTreeSet::from(["default.target".to_owned()]),
    );
    after.wrappers.insert(
        "sudo".into(),
        Wrapper {
            source: "/run/current-system/sw/bin/sudo".into(),
            owner: "root".into(),
            group: "root".into(),
            mode: 0o4755,
        },
    );
    let plan = plan(&before, &after);
    insta::assert_snapshot!(plan.to_string(), @"
    files
      + /run/wrath/wrappers/bin/sudo (tier: live)
          root:root mode 4755, of /run/current-system/sw/bin/sudo
    units
      + pipewire.service (for every user) (tier: live)
          wanted by default.target
      + polkit.service (tier: live)
          enabled
    3 changes, tier: live
    ");
    assert!(plan.activation.start.contains("polkit.service"));
    let read = Model::from_json(&after.canonical()).unwrap();
    assert_eq!(read.wrappers, after.wrappers);
    assert_eq!(read.enabled, after.enabled);
}

#[test]
fn a_drop_in_restarts_its_unit_but_not_a_template() {
    let unit = |exec: &str| Unit {
        sections: vec![Section {
            name: "Service".into(),
            entries: vec![("ExecStart".into(), exec.into())],
        }],
        restart_on: BTreeSet::new(),
    };
    let mut after = Model::default();
    after
        .units
        .insert("nginx.service.d/limits.conf".into(), unit("a"));
    after
        .units
        .insert("getty@.service.d/wrath.conf".into(), unit("a"));
    let activation = plan(&Model::default(), &after).activation;
    assert_eq!(
        activation.restart,
        BTreeSet::from(["nginx.service".to_owned()])
    );
    assert!(activation.start.is_empty());
}

fn built(patch: &str) -> crate::Build {
    crate::Build {
        package: "whisper-cpp".into(),
        options: Default::default(),
        patches: vec![patch.into()],
    }
}

fn model_with_build(patch: &str) -> Model {
    let mut model = Model::default();
    model.packages.insert("whisper".into());
    model.builds.insert("whisper".into(), built(patch));
    model
}

#[test]
fn plans_say_how_a_build_changed() {
    let plan = plan(&model_with_build("a"), &model_with_build("b"));

    let change = plan
        .packages
        .iter()
        .find(|c| c.name == "build whisper")
        .unwrap();
    assert_eq!(change.details, ["patches changed"]);
}

#[test]
fn a_kernel_change_takes_a_reboot() {
    let with_kernel = |package: &str| {
        let mut model = Model::default();
        model
            .base
            .insert("kernel".into(), Value::Str(package.into()));
        model
    };

    let plan = plan(&with_kernel("linux"), &with_kernel("linux-latest"));
    assert_eq!(plan.tier(), Some(crate::Tier::Reboot));
}

#[test]
fn a_plan_for_a_terminal_is_the_same_text_with_color() {
    let mut after = Model::default();
    after.packages.insert("htop".into());
    after
        .base
        .insert("kernel".into(), Value::Str("linux-latest".into()));
    let plan = plan(&Model::default(), &after);
    let colored = plan.styled(true);
    assert!(colored.contains("\x1b[32m+ htop\x1b[0m"), "{colored:?}");
    assert!(colored.contains("\x1b[1;31mreboot\x1b[0m"), "{colored:?}");
    let plain: String = {
        let mut text = String::new();
        let mut chars = colored.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                chars.by_ref().find(|c| *c == 'm');
            } else {
                text.push(c);
            }
        }
        text
    };
    assert_eq!(plain, plan.to_string(), "color is all that changes");
    assert!(!plan.styled(false).contains('\x1b'));
}

#[test]
fn a_newer_wrath_rebuilds_what_its_recipes_changed_with_the_tier_each_takes() {
    use std::collections::BTreeMap;
    let mut model = Model::default();
    let kernel = BTreeMap::from([
        ("package".to_owned(), Value::Str("linux".into())),
        (
            "firmware".to_owned(),
            Value::List(vec![Value::Str("linux-firmware".into())]),
        ),
    ]);
    model.base.insert("kernel".into(), Value::Table(kernel));
    let keys = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(id, key)| ((*id).to_owned(), (*key).to_owned()))
            .collect()
    };
    let before = keys(&[
        ("linux", "k1"),
        ("linux-firmware", "f1"),
        ("systemd", "s1"),
        ("jq", "j1"),
        ("gone", "g1"),
    ]);
    let after = keys(&[
        ("linux", "k2"),
        ("linux-firmware", "f1"),
        ("systemd", "s2"),
        ("jq", "j2"),
    ]);
    let changes = crate::rebuilt(&model, &before, &after);
    let tiers: Vec<(&str, Option<crate::Tier>)> = changes
        .iter()
        .map(|change| (change.name.as_str(), change.tier))
        .collect();
    assert_eq!(
        tiers,
        [
            ("jq", Some(crate::Tier::Live)),
            ("linux", Some(crate::Tier::Reboot)),
            ("systemd", Some(crate::Tier::SoftReboot)),
        ],
        "unchanged packages and ones today's recipes don't have are left out"
    );
    assert_eq!(changes[0].details, ["rebuilt"]);
}

#[test]
fn a_rendered_unit_can_reload_when_a_file_changes_instead_of_restarting() {
    let unit = Unit {
        sections: vec![Section {
            name: "Service".into(),
            entries: vec![("ExecReload".into(), "nft -f /etc/nftables.conf".into())],
        }],
        restart_on: BTreeSet::new(),
    };
    let file = |text: &str| File {
        mode: 0o644,
        text: text.into(),
    };
    let mut before = Model::default();
    before.units.insert("nftables.service".into(), unit);
    before
        .files
        .insert("/etc/nftables.conf".into(), file("old\n"));
    before.triggers.insert(
        ("nftables.service".into(), UnitAction::Reload),
        ["/etc/nftables.conf".into()].into(),
    );
    let mut after = before.clone();
    after
        .files
        .insert("/etc/nftables.conf".into(), file("new\n"));
    let activation = plan(&before, &after).activation;
    assert_eq!(
        activation.reload,
        BTreeSet::from(["nftables.service".to_owned()])
    );
    assert!(activation.restart.is_empty() && activation.stop.is_empty());
}

#[test]
fn a_user_unit_wanted_elsewhere_takes_effect_at_the_next_login() {
    let wanted_by = |target: &str| {
        let mut model = Model::default();
        model.enabled_user.insert(
            "pipewire.service".into(),
            BTreeSet::from([target.to_owned()]),
        );
        model
    };
    let plan = plan(
        &wanted_by("default.target"),
        &wanted_by("graphical-session.target"),
    );
    insta::assert_snapshot!(plan.to_string(), @"
    units
      ~ pipewire.service (for every user) (tier: next-login)
          wanted by graphical-session.target
          running sessions keep the old one until the next login
    1 change, tier: next-login
    ");
    assert!(plan.activation.start.is_empty() && plan.activation.restart.is_empty());
}

#[test]
fn a_platform_from_a_source_is_kept_and_planned_and_wraths_changes_no_hash() {
    let wrath = Model::default();
    let acme = Model {
        platform: Some("acme".into()),
        ..Model::default()
    };
    assert!(!wrath.canonical().contains("platform"));
    assert_ne!(acme.hash(), wrath.hash());
    assert_eq!(
        Model::from_json(&acme.canonical()).unwrap().platform,
        acme.platform
    );
    insta::assert_snapshot!(plan(&wrath, &acme).to_string(), @"
    base
      ~ platform (tier: live)
          wrath -> acme
    1 change, tier: live
    ");
    assert!(plan(&acme, &acme).is_empty());
}

#[test]
fn a_system_from_nothing_restarts_nothing_a_file_would_restart() {
    let journald = "/etc/systemd/journald.conf";
    let mut after = Model::default();
    after.files.insert(
        journald.to_owned(),
        File {
            mode: 0o644,
            text: "[Journal]\n".to_owned(),
        },
    );
    after.triggers.insert(
        ("systemd-journald.service".to_owned(), UnitAction::Restart),
        BTreeSet::from([journald.to_owned()]),
    );
    let names = |plan: &Plan| -> Vec<String> {
        plan.units
            .iter()
            .map(|change| change.name.clone())
            .collect()
    };
    assert!(names(&plan(&Model::default(), &after)).is_empty());

    let mut before = Model::default();
    before.files.insert(
        "/etc/hostname".to_owned(),
        File {
            mode: 0o644,
            text: "box\n".to_owned(),
        },
    );
    assert_eq!(names(&plan(&before, &after)), ["systemd-journald.service"]);
}

#[test]
fn a_system_from_nothing_counts_its_options_and_no_plan_shows_a_password_hash() {
    let hash = "$6$salt$secret";
    let mut after = Model::default();
    after.options.insert(
        "users.ann.initial_hashed_password".to_owned(),
        Value::Str(hash.to_owned()),
    );
    after
        .options
        .insert("system.hostname".to_owned(), Value::Str("box".to_owned()));
    after.files.insert(
        "/etc/hostname".to_owned(),
        File {
            mode: 0o644,
            text: "box\n".to_owned(),
        },
    );
    let fresh = plan(&Model::default(), &after).to_string();
    assert!(
        fresh.contains("+ 2 options; `wrath why` shows where each is set"),
        "{fresh}"
    );
    assert!(!fresh.contains("system.hostname"), "{fresh}");

    let mut before = after.clone();
    before.options.insert(
        "users.ann.initial_hashed_password".to_owned(),
        Value::Str("$6$salt$older".to_owned()),
    );
    let changed = plan(&before, &after).to_string();
    assert!(changed.contains("initial_hashed_password"), "{changed}");
    assert!(changed.contains("a password hash"), "{changed}");
    assert!(!changed.contains(hash), "{changed}");
}
