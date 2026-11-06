use distro_eval::{RuntimePart, Section, Template, Unit, UnitAction, Value};

use crate::{File, Lock, Locked, Model, Secret, Source, plan};

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
      "backends": {},
      "base": {},
      "facts": {},
      "files": {},
      "homes": {},
      "locked": {},
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
      "source": {},
      "templates": {},
      "triggers": {},
      "units": {},
      "version": 9
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

    insta::assert_snapshot!(added.to_string(), @r#"
    options
      + services.nginx.main.port: 80
      + system.hostname: "box"
    packages
      + nginx (tier: live)
    files
      + /etc/nginx/main.conf (tier: live)
    units
      + nginx-main.service (tier: live)
    5 changes, tier: live
    "#);
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
fn a_change_to_a_backend_setting_may_need_a_reboot() {
    let before = model(80);
    let mut after = before.clone();
    after
        .backends
        .insert("nixos".into(), Value::Str("/srv/world#neptune".into()));

    insta::assert_snapshot!(plan(&before, &after).to_string(), @r#"
    backends
      + nixos (tier: reboot)
          "/srv/world#neptune"
    1 change, tier: reboot
    "#);
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
            };
            model.secrets.insert(name.into(), secret);
        }
        for (name, uses) in [
            ("app.service", "/run/secrets/api-token"),
            ("old.service", "/run/secrets/api-token-old"),
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
      ~ sync.service (for john) (tier: live)
          - Service.ExecStart=sync a
          + Service.ExecStart=sync b
    2 changes, tier: live
    ");
}

fn locked(version: &str, id: &str) -> Locked {
    Locked {
        version: version.into(),
        id: id.into(),
    }
}

/// A model with `ripgrep` for the system and `git` for john, under `lock`.
fn with_packages(lock: &Lock) -> Model {
    let mut model = Model::default();
    model.packages.insert("ripgrep".into());
    let home = model.homes.entry("john".into()).or_default();
    home.packages.insert("git".into());
    model.with_lock(lock)
}

fn lock(rev: &str, packages: &[(&str, Locked)]) -> Lock {
    let source = Source {
        url: "github:NixOS/nixpkgs/nixos-unstable".into(),
        rev: Some(rev.into()),
        hash: format!("sha256-{rev}"),
    };
    let packages = packages
        .iter()
        .map(|(name, locked)| ((*name).to_owned(), locked.clone()))
        .collect();
    Lock::new(source, packages)
}

#[test]
fn the_model_keeps_what_the_lock_says_about_its_packages() {
    let lock = lock(
        "aaaaaaa1",
        &[
            ("ripgrep", locked("14.1.0", "/nix/store/r1")),
            ("git", locked("2.51.0", "/nix/store/g1")),
            ("htop", locked("3.4", "/nix/store/h1")),
        ],
    );
    let model = with_packages(&lock);

    assert_eq!(model.source.as_ref(), Some(&lock.source));
    assert_eq!(
        model.locked.keys().collect::<Vec<_>>(),
        ["git", "ripgrep"],
        "packages the model doesn't use are left out"
    );
    assert_ne!(model.hash(), with_packages(&Lock::default()).hash());
}

#[test]
fn plan_shows_package_versions_and_the_package_source() {
    let before = with_packages(&lock(
        "aaaaaaa1",
        &[("ripgrep", locked("14.1.0", "/nix/store/r1"))],
    ));
    let after = with_packages(&lock(
        "bbbbbbb2",
        &[
            ("ripgrep", locked("14.2.0", "/nix/store/r2")),
            ("git", locked("2.51.0", "/nix/store/g1")),
        ],
    ));
    let rebuilt = with_packages(&lock(
        "bbbbbbb2",
        &[
            ("ripgrep", locked("14.2.0", "/nix/store/r3")),
            ("git", locked("2.51.0", "/nix/store/g1")),
        ],
    ));

    insta::assert_snapshot!(plan(&before, &after).to_string(), @"
    packages
      ~ package source (tier: live)
          github:NixOS/nixpkgs/nixos-unstable: aaaaaaa1 -> bbbbbbb2
      ~ git (for john) (tier: live)
          locked at 2.51.0
      ~ ripgrep (tier: live)
          14.1.0 -> 14.2.0
    3 changes, tier: live
    ");
    insta::assert_snapshot!(plan(&after, &rebuilt).to_string(), @"
    packages
      ~ ripgrep (tier: live)
          rebuilt, still 14.2.0
    1 change, tier: live
    ");
    insta::assert_snapshot!(plan(&Model::default(), &before).to_string(), @"
    packages
      + package source (tier: live)
          github:NixOS/nixpkgs/nixos-unstable at aaaaaaa1
      + git (for john) (tier: live)
          not locked yet
      + ripgrep (tier: live)
          14.1.0
    3 changes, tier: live
    ");
}

/// A model with something in every part.
fn full_model() -> Model {
    let mut model = with_packages(&lock(
        "aaaaaaa1",
        &[("ripgrep", locked("14.1.0", "/nix/store/r1"))],
    ));
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
    model.backends.insert(
        "nixos".into(),
        Value::Table([("x".into(), Value::Int(1))].into()),
    );
    model.triggers.insert(
        ("systemd-networkd.service".into(), UnitAction::Reload),
        ["/etc/app.conf".into()].into(),
    );
    model.facts.insert("host.cpus".into(), Value::Int(8));
    model.secrets.insert(
        "token".into(),
        Secret {
            source: String::new(),
            hash: "h".into(),
            owner: "app".into(),
            group: Some("wheel".into()),
            mode: 0o440,
            link: Some("/home/app/token".into()),
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
            "the model is in format version 1, and this distro reads version {}",
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
            .insert("~/.config/environment.d/60-distro.conf".into(), file);
        model
            .next_login
            .insert("~john/.config/environment.d/60-distro.conf".into());
        model
    };

    let plan = plan(&with_session("EDITOR=hx\n"), &with_session("EDITOR=vi\n"));

    insta::assert_snapshot!(plan.to_string(), @"
    files
      ~ ~john/.config/environment.d/60-distro.conf (tier: next-login)
    1 change, tier: next-login
    ");
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
