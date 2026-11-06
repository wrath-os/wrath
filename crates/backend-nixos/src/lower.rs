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

/// The unit file, with a search path for programs named without a directory. A unit that
/// restarts on files gets a hash of their contents, so NixOS sees the unit change with them.
fn unit_text(unit: &Unit, model: &Model) -> String {
    let mut sections = unit.sections.clone();
    if !unit.restart_on.is_empty() {
        let trigger = (
            "X-Restart-Triggers".to_owned(),
            files_hash(&unit.restart_on, model),
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
    let mut text = String::new();
    for section in &sections {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("[{}]\n", section.name));
        for (key, value) in &section.entries {
            text.push_str(&format!("{key}={value}\n"));
        }
        if section.name == "Service"
            && !section
                .entries
                .iter()
                .any(|(key, _)| key == "ExecSearchPath")
        {
            text.push_str(&format!("ExecSearchPath={PROGRAMS}\n"));
        }
    }
    text
}

/// A hash of the paths and contents of files, which changes when any of them does.
fn files_hash(paths: &BTreeSet<String>, model: &Model) -> String {
    let mut hasher = blake3::Hasher::new();
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
    use distro_model::File;

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
          "triggers": {
            "systemd-networkd": {
              "reload": "515446256c8120cef626f645d64a6b92a29146ecb9f893c916bd06caad318528"
            }
          },
          "units": {
            "nginx.service": {
              "required_by": [],
              "text": "[Unit]\nX-Restart-Triggers=515446256c8120cef626f645d64a6b92a29146ecb9f893c916bd06caad318528\n\n[Service]\nExecStart=nginx -g 'daemon off;'\nExecSearchPath=/run/current-system/sw/bin\n\n[Install]\nWantedBy=multi-user.target default.target\n",
              "wanted_by": [
                "multi-user.target",
                "default.target"
              ]
            }
          }
        }
        "#);
    }
}
