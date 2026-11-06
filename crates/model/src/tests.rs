use distro_eval::{RuntimePart, Section, Unit, UnitAction, Value};

use crate::{File, Model, Secret, plan};

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
      "triggers": {},
      "units": {},
      "version": 4
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
fn plan_shows_files_and_packages_in_users_homes() {
    let with_home = |text: &str| {
        let mut model = Model::default();
        let home = model.homes.entry("john".into()).or_default();
        let file = File {
            mode: 0o644,
            text: text.into(),
        };
        home.files.insert("~/.config/git/config".into(), file);
        home.packages.insert("git".into());
        model
    };

    insta::assert_snapshot!(plan(&Model::default(), &with_home("a")).to_string(), @"
    packages
      + git (for john) (tier: live)
    files
      + ~john/.config/git/config (tier: live)
    2 changes, tier: live
    ");
    insta::assert_snapshot!(plan(&with_home("a"), &with_home("b")).to_string(), @"
    files
      ~ ~john/.config/git/config (tier: live)
    1 change, tier: live
    ");
}
