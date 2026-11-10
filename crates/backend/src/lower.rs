//! Lowering the model to the input of `factory/nix/native`.
//!
//! The factory is fixed Nix code that assembles a system from each section, so everything that
//! needs logic happens here: unit files are rendered to text, their `[Install]` targets become
//! links the factory creates, and files are split by whether they live under `/etc`.

use std::collections::{BTreeMap, BTreeSet};

use distro_eval::{Section, Template, Unit, Value, formats, home_path};
use distro_model::{DISTRO_SOURCE, Model, Source, spell_out_parts};

/// Where packages' programs are on a system. Units run programs by name, so they search here.
const PROGRAMS: &str = "/run/current-system/sw/bin";

/// The factory's input, as pretty JSON with sorted keys.
pub fn lower(model: &Model) -> String {
    let mut etc = BTreeMap::new();
    let mut files = BTreeMap::new();
    for (path, file) in &model.files {
        let text = Value::Str(file.text.clone());
        match path.strip_prefix("/etc/") {
            Some(relative) => {
                let mode = if file.mode == 0o644 {
                    // Files with this mode are linked from the store instead of copied.
                    "symlink".to_owned()
                } else {
                    format!("{:04o}", file.mode)
                };
                let entry = table([("text", text), ("mode", Value::Str(mode))]);
                etc.insert(relative.to_owned(), entry);
            }
            // Linked from the store by tmpfiles, so they are read-only whatever their mode.
            None => {
                files.insert(path.clone(), table([("text", text)]));
            }
        }
    }
    let units = model
        .units
        .iter()
        .map(|(name, unit)| {
            let lowered = table([
                ("text", Value::Str(unit_text(unit, model, PROGRAMS))),
                ("wanted_by", strings(install(&unit.sections, "WantedBy"))),
                (
                    "required_by",
                    strings(install(&unit.sections, "RequiredBy")),
                ),
            ]);
            (name.clone(), lowered)
        })
        .collect();
    let input = table([
        ("model", Value::Str(model.canonical())),
        ("model_hash", Value::Str(model.hash())),
        ("packages", strings(model.packages.iter().cloned())),
        ("builds", distro_model::builds_value(&model.builds)),
        (
            "service_packages",
            strings(model.service_packages.iter().cloned()),
        ),
        ("enabled", distro_model::enabled_value(&model.enabled)),
        (
            "enabled_user",
            distro_model::enabled_value(&model.enabled_user),
        ),
        ("etc", Value::Table(etc)),
        ("files", Value::Table(files)),
        ("units", Value::Table(units)),
        ("secrets", secrets(model)),
        ("homes", homes(model)),
        ("sources", sources_value(model)),
        ("templates", templates(model)),
        ("base", Value::Table(model.base.clone())),
    ]);
    formats::json(&input).expect("the factory's input has no runtime values")
}

/// Each user's files, linked from the store, packages, and files with secrets in them, which the
/// machine fills in. User units are files in `~/.config/systemd/user`, linked again from each
/// target that wants them.
fn homes(model: &Model) -> Value {
    let homes = model
        .homes
        .iter()
        .map(|(user, home)| {
            let mut files: BTreeMap<String, Value> = home
                .files
                .iter()
                .map(|(path, file)| {
                    let entry = table([
                        ("text", Value::Str(file.text.clone())),
                        ("executable", Value::Bool(file.mode & 0o111 != 0)),
                    ]);
                    (path.clone(), entry)
                })
                .collect();
            let programs = format!("/etc/profiles/per-user/{user}/bin:{PROGRAMS}");
            for (name, unit) in &home.units {
                let entry = table([
                    ("text", Value::Str(unit_text(unit, model, &programs))),
                    ("executable", Value::Bool(false)),
                ]);
                for target in install(&unit.sections, "WantedBy") {
                    let link = format!("~/.config/systemd/user/{target}.wants/{name}");
                    files.insert(link, entry.clone());
                }
                files.insert(format!("~/.config/systemd/user/{name}"), entry);
            }
            let templates = home
                .templates
                .iter()
                .map(|(path, template)| {
                    (
                        path.clone(),
                        template_entry(&home_path(user, path), template),
                    )
                })
                .collect();
            let home = table([
                ("files", Value::Table(files)),
                ("packages", strings(home.packages.iter().cloned())),
                ("templates", Value::Table(templates)),
            ]);
            (user.clone(), home)
        })
        .collect();
    Value::Table(homes)
}

/// Where packages come from, by source name, each pinned by its revision and hash. A source
/// without a hash isn't pinned, and the factory takes its latest version; without a `nixpkgs`
/// source, the factory's nixpkgs. distro itself isn't one: the distro the lock pins is the one
/// building this.
fn sources_value(model: &Model) -> Value {
    let sources = model
        .sources
        .iter()
        .filter(|(name, _)| name.as_str() != DISTRO_SOURCE)
        .map(|(name, Source { url, rev, hash, .. })| {
            let mut source = BTreeMap::from([("url".to_owned(), Value::Str(url.clone()))]);
            if !hash.is_empty() {
                source.insert("hash".to_owned(), Value::Str(hash.clone()));
                if let Some(rev) = rev {
                    source.insert("rev".to_owned(), Value::Str(rev.clone()));
                }
            }
            (name.clone(), Value::Table(source))
        });
    Value::Table(sources.collect())
}

/// Files with secrets in them, by path: the name the machine fills each in under, in
/// `/run/distro/templates`, its owner as `chown` takes it, its mode and its parts.
fn templates(model: &Model) -> Value {
    Value::Table(
        model
            .templates
            .iter()
            .map(|(path, template)| (path.clone(), template_entry(path, template)))
            .collect(),
    )
}

fn template_entry(path: &str, template: &Template) -> Value {
    let hash = blake3::hash(path.as_bytes()).to_hex();
    let base = path.rsplit('/').next().unwrap_or(path);
    let owner = format!(
        "{}:{}",
        template.owner,
        template.group.as_deref().unwrap_or("")
    );
    table([
        ("name", Value::Str(format!("{}-{base}", &hash[..12]))),
        ("owner", Value::Str(owner)),
        ("mode", Value::Str(format!("{:04o}", template.mode))),
        ("parts", spell_out_parts(&template.parts)),
    ])
}

/// Each secret with its encrypted file as an absolute path, and the owner as `chown` takes it.
fn secrets(model: &Model) -> Value {
    let secrets = model
        .secrets
        .iter()
        .map(|(name, secret)| {
            let source = std::path::absolute(&secret.source)
                .map_or_else(|_| secret.source.clone(), |path| path.display().to_string());
            let owner = format!("{}:{}", secret.owner, secret.group.as_deref().unwrap_or(""));
            let entry = table([
                ("source", Value::Str(source)),
                ("owner", Value::Str(owner)),
                ("mode", Value::Str(format!("{:04o}", secret.mode))),
                ("link", Value::Str(secret.link.clone().unwrap_or_default())),
            ]);
            (name.clone(), entry)
        })
        .collect();
    Value::Table(secrets)
}

/// The unit file, with `programs` as the search path and PATH for programs named without a
/// directory. A unit that restarts on files or mentions secrets gets a hash of them, so the unit
/// changes with them.
fn unit_text(unit: &Unit, model: &Model, programs: &str) -> String {
    let mut sections = unit.sections.clone();
    let secrets = model.secrets_of(unit);
    if !unit.restart_on.is_empty() || !secrets.is_empty() {
        let trigger = (
            "X-Restart-Triggers".to_owned(),
            restart_hash(&unit.restart_on, &secrets, model),
        );
        match sections.iter_mut().find(|section| section.name == "Unit") {
            Some(section) => section.entries.push(trigger),
            None => sections.insert(
                0,
                Section {
                    name: "Unit".into(),
                    entries: vec![trigger],
                },
            ),
        }
    }
    for section in sections
        .iter_mut()
        .filter(|section| section.name == "Service")
    {
        let has_search_path = section.entries.iter().any(|(k, _)| k == "ExecSearchPath");
        let sets_path = section
            .entries
            .iter()
            .any(|(k, v)| k == "Environment" && v.trim_start_matches('"').starts_with("PATH="));
        if !has_search_path {
            section
                .entries
                .push(("ExecSearchPath".into(), programs.into()));
        }
        // Scripts a unit runs find programs on PATH, which ExecSearchPath doesn't set.
        if !sets_path {
            section
                .entries
                .push(("Environment".into(), format!("PATH={programs}")));
        }
    }
    Unit {
        sections,
        ..Unit::default()
    }
    .text()
}

/// A hash of the contents of files and secrets, with secrets' owner and mode, which changes when
/// any of them does.
fn restart_hash(paths: &BTreeSet<String>, secrets: &BTreeSet<&str>, model: &Model) -> String {
    let mut hasher = blake3::Hasher::new();
    for name in secrets {
        let Some(secret) = model.secrets.get(*name) else {
            continue;
        };
        let group = secret.group.as_deref().unwrap_or("");
        let fields = [name, secret.hash.as_str(), &secret.owner, group];
        for field in fields {
            hasher.update(field.as_bytes());
            hasher.update(&[0]);
        }
        hasher.update(&secret.mode.to_le_bytes());
    }
    for path in paths {
        let text = match (model.files.get(path), model.templates.get(path)) {
            (Some(file), _) => file.text.clone(),
            (None, Some(template)) => formats::json(&template_entry(path, template))
                .expect("a template's entry has no runtime values"),
            (None, None) => String::new(),
        };
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(text.as_bytes());
        hasher.update(&[0]);
    }
    hasher.finalize().to_hex().to_string()
}

/// The units named by `key` in the `[Install]` section.
fn install(sections: &[Section], key: &str) -> Vec<String> {
    sections
        .iter()
        .filter(|section| section.name == "Install")
        .flat_map(|section| &section.entries)
        .filter(|(entry, _)| entry == key)
        .flat_map(|(_, value)| value.split_whitespace().map(str::to_owned))
        .collect()
}

fn strings(items: impl IntoIterator<Item = String>) -> Value {
    Value::List(items.into_iter().map(Value::Str).collect())
}

fn table<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Table(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use distro_eval::RuntimePart;
    use distro_model::{File, Secret};

    use super::*;

    #[test]
    fn the_distro_a_lock_pins_isnt_a_source_to_build_from() {
        let mut model = Model::default();
        for (name, url) in [
            ("nixpkgs", "github:NixOS/nixpkgs/nixos-unstable"),
            (DISTRO_SOURCE, "git+https://x/distro"),
        ] {
            model.sources.insert(name.into(), Source::follow(url));
        }
        insta::assert_snapshot!(formats::json(&sources_value(&model)).unwrap(), @r#"
        {
          "nixpkgs": {
            "url": "github:NixOS/nixpkgs/nixos-unstable"
          }
        }
        "#);
    }

    #[test]
    fn units_get_text_and_install_targets_and_files_split_by_owner() {
        let mut model = Model::default();
        model.packages.insert("nginx".into());
        for (path, mode) in [
            ("/etc/nginx/main.conf", 0o644),
            ("/etc/secret.conf", 0o600),
            ("/srv/www/index.html", 0o644),
            ("/etc/pam.d/login", 0o644),
        ] {
            let file = File {
                mode,
                text: "x\n".into(),
            };
            model.files.insert(path.into(), file);
        }
        model.units.insert(
            "nginx.service".into(),
            Unit {
                sections: vec![
                    Section {
                        name: "Service".into(),
                        entries: vec![("ExecStart".into(), "nginx -g 'daemon off;'".into())],
                    },
                    Section {
                        name: "Install".into(),
                        entries: vec![(
                            "WantedBy".into(),
                            "multi-user.target default.target".into(),
                        )],
                    },
                ],
                restart_on: ["/etc/nginx/main.conf".into()].into(),
            },
        );
        model.base.insert("audio".into(), Value::Bool(true));

        // The model and its hash have tests of their own.
        let lowered: String = lower(&model)
            .lines()
            .filter(|line| !line.starts_with("  \"model"))
            .map(|line| format!("{line}\n"))
            .collect();
        insta::assert_snapshot!(lowered, @r#"
        {
          "base": {
            "audio": true
          },
          "builds": {},
          "enabled": {},
          "enabled_user": {},
          "etc": {
            "nginx/main.conf": {
              "mode": "symlink",
              "text": "x\n"
            },
            "pam.d/login": {
              "mode": "symlink",
              "text": "x\n"
            },
            "secret.conf": {
              "mode": "0600",
              "text": "x\n"
            }
          },
          "files": {
            "/srv/www/index.html": {
              "text": "x\n"
            }
          },
          "homes": {},
          "packages": [
            "nginx"
          ],
          "secrets": {},
          "service_packages": [],
          "sources": {},
          "templates": {},
          "units": {
            "nginx.service": {
              "required_by": [],
              "text": "[Unit]\nX-Restart-Triggers=515446256c8120cef626f645d64a6b92a29146ecb9f893c916bd06caad318528\n\n[Service]\nExecStart=nginx -g 'daemon off;'\nExecSearchPath=/run/current-system/sw/bin\nEnvironment=PATH=/run/current-system/sw/bin\n\n[Install]\nWantedBy=multi-user.target default.target\n",
              "wanted_by": [
                "multi-user.target",
                "default.target"
              ]
            }
          }
        }
        "#);
    }

    #[test]
    fn secrets_get_absolute_sources_and_units_that_mention_them_restart_with_them() {
        let model = |hash: &str| {
            let mut model = Model::default();
            let secret = Secret {
                source: "/srv/config/secrets/key.age".into(),
                hash: hash.into(),
                owner: "app".into(),
                group: None,
                mode: 0o400,
                link: Some("/home/app/.ssh/id_ed25519".into()),
            };
            model.secrets.insert("key".into(), secret);
            let unit = Unit {
                sections: vec![Section {
                    name: "Service".into(),
                    entries: vec![("ExecStart".into(), "app --key /run/secrets/key".into())],
                }],
                ..Unit::default()
            };
            model.units.insert("app.service".into(), unit);
            model
        };
        let text = |model: &Model| unit_text(&model.units["app.service"], model, PROGRAMS);

        insta::assert_snapshot!(formats::json(&secrets(&model("abc"))).unwrap(), @r#"
        {
          "key": {
            "link": "/home/app/.ssh/id_ed25519",
            "mode": "0400",
            "owner": "app:",
            "source": "/srv/config/secrets/key.age"
          }
        }
        "#);
        assert!(text(&model("abc")).contains("X-Restart-Triggers="));
        assert_ne!(text(&model("abc")), text(&model("def")));
    }

    #[test]
    fn home_files_say_whether_they_run() {
        let mut model = Model::default();
        let home = model.homes.entry("john".into()).or_default();
        for (path, mode) in [("~/.config/git/config", 0o644), ("~/bin/hello", 0o755)] {
            let file = File {
                mode,
                text: "x\n".into(),
            };
            home.files.insert(path.into(), file);
        }
        home.packages.insert("git".into());
        let netrc = Template {
            parts: vec![RuntimePart::Secret("pw".into())],
            owner: "john".into(),
            group: None,
            mode: 0o400,
        };
        home.templates.insert("~/.netrc".into(), netrc);

        insta::assert_snapshot!(formats::json(&homes(&model)).unwrap(), @r#"
        {
          "john": {
            "files": {
              "~/.config/git/config": {
                "executable": false,
                "text": "x\n"
              },
              "~/bin/hello": {
                "executable": true,
                "text": "x\n"
              }
            },
            "packages": [
              "git"
            ],
            "templates": {
              "~/.netrc": {
                "mode": "0400",
                "name": "d074adb2250a-.netrc",
                "owner": "john:",
                "parts": [
                  {
                    "secret": "pw"
                  }
                ]
              }
            }
          }
        }
        "#);
    }

    #[test]
    fn package_sources_are_pinned_unless_they_have_no_hash() {
        let mut model = Model::default();
        model.sources.insert(
            "nixpkgs".into(),
            Source {
                url: "github:NixOS/nixpkgs/nixos-unstable".into(),
                rev: Some("1d06008".into()),
                hash: "sha256-abc".into(),
                date: None,
            },
        );
        model.sources.insert(
            "stable".into(),
            Source::follow("github:NixOS/nixpkgs/nixos-25.11"),
        );

        insta::assert_snapshot!(formats::json(&sources_value(&model)).unwrap(), @r#"
        {
          "nixpkgs": {
            "hash": "sha256-abc",
            "rev": "1d06008",
            "url": "github:NixOS/nixpkgs/nixos-unstable"
          },
          "stable": {
            "url": "github:NixOS/nixpkgs/nixos-25.11"
          }
        }
        "#);
        assert_eq!(
            formats::json(&sources_value(&Model::default())).unwrap(),
            "{}\n"
        );
    }

    #[test]
    fn files_with_secrets_get_a_name_to_be_filled_in_under() {
        let mut model = Model::default();
        let template = Template {
            parts: vec![
                RuntimePart::Text("token = \"".into()),
                RuntimePart::QuotedSecret("token".into()),
                RuntimePart::Text("\"\n".into()),
            ],
            owner: "app".into(),
            group: None,
            mode: 0o400,
        };
        model
            .templates
            .insert("/etc/app/config.toml".into(), template);
        let unit = Unit {
            sections: vec![Section {
                name: "Service".into(),
                entries: vec![("ExecStart".into(), "app".into())],
            }],
            restart_on: ["/etc/app/config.toml".into()].into(),
        };
        model.units.insert("app.service".into(), unit);

        insta::assert_snapshot!(formats::json(&templates(&model)).unwrap(), @r#"
        {
          "/etc/app/config.toml": {
            "mode": "0400",
            "name": "300adfab0016-config.toml",
            "owner": "app:",
            "parts": [
              {
                "text": "token = \""
              },
              {
                "quoted_secret": "token"
              },
              {
                "text": "\"\n"
              }
            ]
          }
        }
        "#);
        let text = |model: &Model| unit_text(&model.units["app.service"], model, PROGRAMS);
        let mut changed = model.clone();
        changed
            .templates
            .get_mut("/etc/app/config.toml")
            .unwrap()
            .mode = 0o440;
        assert_ne!(text(&model), text(&changed));
    }
    #[test]
    fn user_units_are_files_in_the_home_with_links_for_their_targets() {
        let mut model = Model::default();
        let unit = Unit::from_value(&Value::Table(BTreeMap::from([
            (
                "Service".to_owned(),
                Value::Table(BTreeMap::from([(
                    "ExecStart".to_owned(),
                    Value::Str("sleep infinity".into()),
                )])),
            ),
            (
                "Install".to_owned(),
                Value::Table(BTreeMap::from([(
                    "WantedBy".to_owned(),
                    Value::Str("default.target".into()),
                )])),
            ),
        ])))
        .unwrap();
        let home = model.homes.entry("john".into()).or_default();
        home.units.insert("nap.service".into(), unit);

        insta::assert_snapshot!(formats::json(&homes(&model)).unwrap(), @r#"
        {
          "john": {
            "files": {
              "~/.config/systemd/user/default.target.wants/nap.service": {
                "executable": false,
                "text": "[Service]\nExecStart=sleep infinity\nExecSearchPath=/etc/profiles/per-user/john/bin:/run/current-system/sw/bin\nEnvironment=PATH=/etc/profiles/per-user/john/bin:/run/current-system/sw/bin\n\n[Install]\nWantedBy=default.target\n"
              },
              "~/.config/systemd/user/nap.service": {
                "executable": false,
                "text": "[Service]\nExecStart=sleep infinity\nExecSearchPath=/etc/profiles/per-user/john/bin:/run/current-system/sw/bin\nEnvironment=PATH=/etc/profiles/per-user/john/bin:/run/current-system/sw/bin\n\n[Install]\nWantedBy=default.target\n"
              }
            },
            "packages": [],
            "templates": {}
          }
        }
        "#);
    }
}
