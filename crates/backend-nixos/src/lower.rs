//! Lowering the model to the input of the NixOS shim in `factory/nix`.
//!
//! The shim is fixed Nix code that maps each section onto NixOS options, so everything that
//! needs logic happens here: unit files are rendered to text, their `[Install]` targets become
//! dependencies NixOS creates, and files are split by whether NixOS manages them under `/etc`.

use std::collections::{BTreeMap, BTreeSet};

use distro_eval::{Section, Unit, Value, formats};
use distro_model::Model;

/// Where packages' programs are on NixOS. Units run programs by name, so they search here.
const PROGRAMS: &str = "/run/current-system/sw/bin";

/// Files NixOS keeps for itself in Phase A. PAM is part of the borrowed base system, so the PAM
/// stacks modules render aren't installed.
const BORROWED: [&str; 1] = ["/etc/pam.d/"];

/// The shim's input, as pretty JSON with sorted keys.
pub fn lower(model: &Model) -> String {
    let mut etc = BTreeMap::new();
    let mut files = BTreeMap::new();
    for (path, file) in &model.files {
        if BORROWED.iter().any(|prefix| path.starts_with(prefix)) {
            continue;
        }
        let text = Value::Str(file.text.clone());
        match path.strip_prefix("/etc/") {
            Some(relative) => {
                let mode = if file.mode == 0o644 {
                    // NixOS links files with this mode instead of copying them.
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
                ("text", Value::Str(unit_text(unit, model))),
                ("wanted_by", strings(install(&unit.sections, "WantedBy"))),
                (
                    "required_by",
                    strings(install(&unit.sections, "RequiredBy")),
                ),
            ]);
            (name.clone(), lowered)
        })
        .collect();
    // NixOS names services without the suffix and acts when a trigger's value changes.
    let mut triggers: BTreeMap<String, Value> = BTreeMap::new();
    for ((unit, action), paths) in &model.triggers {
        let service = unit.strip_suffix(".service").unwrap_or(unit);
        let Value::Table(actions) = triggers
            .entry(service.to_owned())
            .or_insert_with(|| Value::Table(BTreeMap::new()))
        else {
            unreachable!("each service's entry is a table");
        };
        actions.insert(
            action.as_str().to_owned(),
            Value::Str(files_hash(paths, model)),
        );
    }
    let input = table([
        ("model", Value::Str(model.canonical())),
        ("model_hash", Value::Str(model.hash())),
        ("packages", strings(model.packages.iter().cloned())),
        ("etc", Value::Table(etc)),
        ("files", Value::Table(files)),
        ("units", Value::Table(units)),
        ("triggers", Value::Table(triggers)),
        ("secrets", secrets(model)),
        ("base", Value::Table(model.base.clone())),
        (
            "nixos",
            model
                .backends
                .get("nixos")
                .cloned()
                .unwrap_or(Value::Table(BTreeMap::new())),
        ),
    ]);
    formats::json(&input).expect("the shim's input has no runtime values")
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

/// The unit file, with a search path and PATH for programs named without a directory. A unit that
/// restarts on files or mentions secrets gets a hash of them, so NixOS sees the unit change with
/// them.
fn unit_text(unit: &Unit, model: &Model) -> String {
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
                .push(("ExecSearchPath".into(), PROGRAMS.into()));
        }
        // Scripts a unit runs find programs on PATH, which ExecSearchPath doesn't set.
        if !sets_path {
            section
                .entries
                .push(("Environment".into(), format!("PATH={PROGRAMS}")));
        }
    }
    Unit {
        sections,
        ..Unit::default()
    }
    .text()
}

/// A hash of the paths and contents of files, which changes when any of them does.
fn files_hash(paths: &BTreeSet<String>, model: &Model) -> String {
    restart_hash(paths, &BTreeSet::new(), model)
}

/// Like [`files_hash`], covering secrets as well: their contents, owner and mode.
fn restart_hash(paths: &BTreeSet<String>, secrets: &BTreeSet<&str>, model: &Model) -> String {
    let mut hasher = blake3::Hasher::new();
    for name in secrets {
        let secret = &model.secrets[*name];
        let group = secret.group.as_deref().unwrap_or("");
        let fields = [name, secret.hash.as_str(), &secret.owner, group];
        for field in fields {
            hasher.update(field.as_bytes());
            hasher.update(&[0]);
        }
        hasher.update(&secret.mode.to_le_bytes());
    }
    for path in paths {
        let text = model.files.get(path).map_or("", |file| file.text.as_str());
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
    use distro_eval::UnitAction;
    use distro_model::{File, Secret};

    use super::*;

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
        model.triggers.insert(
            ("systemd-networkd.service".into(), UnitAction::Reload),
            ["/etc/nginx/main.conf".into()].into(),
        );

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
          "etc": {
            "nginx/main.conf": {
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
          "nixos": {},
          "packages": [
            "nginx"
          ],
          "secrets": {},
          "triggers": {
            "systemd-networkd": {
              "reload": "515446256c8120cef626f645d64a6b92a29146ecb9f893c916bd06caad318528"
            }
          },
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
        let text = |model: &Model| unit_text(&model.units["app.service"], model);

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
}
