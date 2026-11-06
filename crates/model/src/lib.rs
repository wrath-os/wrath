//! The resolved model: a complete description of one machine with no logic, references or
//! defaults left in it. It is what every backend consumes, and its hash identifies the system.
//!
//! The model leaves out provenance (which file or module set what), so moving a value from one
//! file to another doesn't change the system. This is a first draft; the format is versioned.

mod lock;
mod plan;
mod read;

use std::collections::{BTreeMap, BTreeSet};

use distro_eval::{
    Evaluation, RuntimePart, Secret as Declared, Section, Unit, UnitAction, Value, formats,
};

pub use lock::{LOCK_FILE, Lock, Locked, Source};
pub use plan::{Change, ChangeKind, Plan, Tier, plan};

/// The version of the model format, part of the hash.
pub const VERSION: i64 = 6;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    /// Every option with a value, by dotted path.
    pub options: BTreeMap<String, Value>,
    pub packages: BTreeSet<String>,
    pub files: BTreeMap<String, File>,
    pub units: BTreeMap<String, Unit>,
    /// Settings for the base system the backend provides, such as `audio` or `session`.
    pub base: BTreeMap<String, Value>,
    /// Settings only one backend understands, by backend name.
    pub backends: BTreeMap<String, Value>,
    /// Files that make services the system provides reload or restart, by service and action.
    pub triggers: BTreeMap<(String, UnitAction), BTreeSet<String>>,
    /// Machine facts the configuration read, by dotted path.
    pub facts: BTreeMap<String, Value>,
    /// Secrets decrypted on the machine, by name.
    pub secrets: BTreeMap<String, Secret>,
    /// What goes in users' homes, by user.
    pub homes: BTreeMap<String, Home>,
    /// Where packages come from, when the configuration has a lock.
    pub source: Option<Source>,
    /// The packages, the system's and users', that the lock has resolved.
    pub locked: BTreeMap<String, Locked>,
}

/// A user's files, by path as `~/.config/git/config`, packages and user units.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Home {
    pub files: BTreeMap<String, File>,
    pub packages: BTreeSet<String>,
    pub units: BTreeMap<String, Unit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub mode: u32,
    pub text: String,
}

/// A secret, decrypted on the machine to `/run/secrets/<name>`.
#[derive(Debug, Clone, Eq)]
pub struct Secret {
    /// The encrypted file where the system is built. It isn't part of the hash, so moving the
    /// configuration doesn't change the system.
    pub source: String,
    /// The BLAKE3 hash of the encrypted contents, which changes when the secret does.
    pub hash: String,
    pub owner: String,
    /// The owner's group when unset.
    pub group: Option<String>,
    pub mode: u32,
    /// Another path linked to the decrypted file.
    pub link: Option<String>,
}

/// Secrets are the same when everything but where the encrypted file is matches.
impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        fn key(secret: &Secret) -> (&str, &str, &Option<String>, u32, &Option<String>) {
            (
                &secret.hash,
                &secret.owner,
                &secret.group,
                secret.mode,
                &secret.link,
            )
        }
        key(self) == key(other)
    }
}

impl Model {
    pub fn from_evaluation(evaluation: &Evaluation) -> Self {
        let mut model = Self {
            options: evaluation
                .options()
                .iter()
                .map(|(path, resolved)| (path.to_string(), resolved.value.clone()))
                .collect(),
            facts: evaluation
                .facts_used()
                .iter()
                .map(|(path, value)| (path.to_string(), value.clone()))
                .collect(),
            ..Self::default()
        };
        for rendered in evaluation.modules() {
            let output = &rendered.output;
            model.packages.extend(output.packages.iter().cloned());
            model.files.extend(files_of(&output.files));
            for (user, home) in &output.homes {
                let ours = model.homes.entry(user.clone()).or_default();
                ours.files.extend(files_of(&home.files));
                ours.packages.extend(home.packages.iter().cloned());
                ours.units.extend(home.units.clone());
            }
            model.base.extend(output.base.clone());
            model.backends.extend(output.backends.clone());
            model.units.extend(output.units.clone());
            model
                .secrets
                .extend(output.secrets.iter().map(|(name, declared)| {
                    let file = &evaluation.secret_files()[name];
                    let secret = Secret {
                        source: file.source.clone(),
                        hash: file.hash.clone(),
                        owner: declared.owner.clone(),
                        group: declared.group.clone(),
                        mode: declared.mode,
                        link: declared.link.clone(),
                    };
                    (name.clone(), secret)
                }));
            for (trigger, paths) in &output.triggers {
                model
                    .triggers
                    .entry(trigger.clone())
                    .or_default()
                    .extend(paths.iter().cloned());
            }
        }
        model
    }

    /// The model with what `lock` says about its packages. Packages the lock doesn't have yet
    /// stay unlocked, and the ones it has that the model doesn't use are left out.
    pub fn with_lock(mut self, lock: &Lock) -> Self {
        self.locked = self
            .all_packages()
            .filter_map(|name| Some((name.to_owned(), lock.packages.get(name)?.clone())))
            .collect();
        self.source = Some(lock.source.clone());
        self
    }

    /// The system's and every user's packages, each once.
    pub fn all_packages(&self) -> impl Iterator<Item = &str> {
        let users = self.homes.values().flat_map(|home| &home.packages);
        self.packages
            .iter()
            .chain(users)
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            .into_iter()
    }

    /// The model as one value, with runtime values spelled out as tables.
    pub fn to_value(&self) -> Value {
        let packages = self
            .packages
            .iter()
            .map(|name| Value::Str(name.clone()))
            .collect();
        let homes = self
            .homes
            .iter()
            .map(|(user, home)| {
                let packages = home.packages.iter().cloned().map(Value::Str).collect();
                let home = table([
                    ("files", files_value(&home.files)),
                    ("packages", Value::List(packages)),
                    ("units", units_value(&home.units)),
                ]);
                (user.clone(), home)
            })
            .collect();
        let mut triggers: BTreeMap<String, Value> = BTreeMap::new();
        for ((unit, action), paths) in &self.triggers {
            let Value::Table(actions) = triggers
                .entry(unit.clone())
                .or_insert_with(|| Value::Table(BTreeMap::new()))
            else {
                unreachable!("each unit's entry is a table");
            };
            let paths = paths.iter().cloned().map(Value::Str).collect();
            actions.insert(action.as_str().to_owned(), Value::List(paths));
        }
        let secrets = self
            .secrets
            .iter()
            .map(|(name, secret)| {
                let mut entries = BTreeMap::from([
                    ("hash".to_owned(), Value::Str(secret.hash.clone())),
                    ("owner".to_owned(), Value::Str(secret.owner.clone())),
                    ("mode".to_owned(), Value::Int(i64::from(secret.mode))),
                ]);
                if let Some(group) = &secret.group {
                    entries.insert("group".to_owned(), Value::Str(group.clone()));
                }
                if let Some(link) = &secret.link {
                    entries.insert("link".to_owned(), Value::Str(link.clone()));
                }
                (name.clone(), Value::Table(entries))
            })
            .collect();
        let mut source = BTreeMap::new();
        if let Some(Source { url, rev, hash }) = &self.source {
            source.insert("url".to_owned(), Value::Str(url.clone()));
            source.insert("hash".to_owned(), Value::Str(hash.clone()));
            if let Some(rev) = rev {
                source.insert("rev".to_owned(), Value::Str(rev.clone()));
            }
        }
        let locked = self
            .locked
            .iter()
            .map(|(name, locked)| {
                let entry = table([
                    ("version", Value::Str(locked.version.clone())),
                    ("id", Value::Str(locked.id.clone())),
                ]);
                (name.clone(), entry)
            })
            .collect();
        table([
            ("version", Value::Int(VERSION)),
            ("source", Value::Table(source)),
            ("locked", Value::Table(locked)),
            ("secrets", Value::Table(secrets)),
            ("triggers", Value::Table(triggers)),
            ("options", spell_out_entries(&self.options)),
            ("packages", Value::List(packages)),
            ("files", files_value(&self.files)),
            ("homes", Value::Table(homes)),
            ("units", units_value(&self.units)),
            ("base", spell_out_entries(&self.base)),
            ("backends", spell_out_entries(&self.backends)),
            ("facts", spell_out_entries(&self.facts)),
        ])
    }

    /// The secrets whose decrypted path a unit mentions. The unit restarts when one changes.
    pub fn secrets_of(&self, unit: &Unit) -> BTreeSet<&str> {
        let text = unit.text();
        self.secrets
            .keys()
            .filter(|name| {
                let path = Declared::path(name);
                text.match_indices(&path).any(|(at, _)| {
                    !text[at + path.len()..]
                        .starts_with(|c: char| c.is_alphanumeric() || "-_.".contains(c))
                })
            })
            .map(String::as_str)
            .collect()
    }

    /// The canonical serialization: JSON with sorted keys.
    pub fn canonical(&self) -> String {
        formats::json(&self.to_value()).expect("a model without runtime values renders as JSON")
    }

    /// The BLAKE3 hash of the canonical serialization, in hex.
    pub fn hash(&self) -> String {
        blake3::hash(self.canonical().as_bytes())
            .to_hex()
            .to_string()
    }
}

fn files_of(
    files: &BTreeMap<String, distro_eval::File>,
) -> impl Iterator<Item = (String, File)> + '_ {
    files.iter().map(|(path, file)| {
        let file = File {
            mode: file.mode,
            text: file.text.clone(),
        };
        (path.clone(), file)
    })
}

fn files_value(files: &BTreeMap<String, File>) -> Value {
    Value::Table(
        files
            .iter()
            .map(|(path, file)| {
                let file = table([
                    ("mode", Value::Int(i64::from(file.mode))),
                    ("text", Value::Str(file.text.clone())),
                ]);
                (path.clone(), file)
            })
            .collect(),
    )
}

fn units_value(units: &BTreeMap<String, Unit>) -> Value {
    Value::Table(
        units
            .iter()
            .map(|(name, unit)| {
                let sections = unit.sections.iter().map(section_value).collect();
                let restart_on = unit.restart_on.iter().cloned().map(Value::Str).collect();
                let unit = table([
                    ("sections", Value::List(sections)),
                    ("restart_on", Value::List(restart_on)),
                ]);
                (name.clone(), unit)
            })
            .collect(),
    )
}

fn table<const N: usize>(entries: [(&str, Value); N]) -> Value {
    Value::Table(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

/// A unit section as `{ name, entries = [[key, value], ...] }`.
fn section_value(section: &Section) -> Value {
    let entries = section
        .entries
        .iter()
        .map(|(key, value)| Value::List(vec![Value::Str(key.clone()), Value::Str(value.clone())]))
        .collect();
    table([
        ("name", Value::Str(section.name.clone())),
        ("entries", Value::List(entries)),
    ])
}

/// Replaces runtime values with tables naming their parts, so the model renders as plain data.
fn spell_out(value: &Value) -> Value {
    match value {
        Value::Runtime(parts) => {
            let parts = parts
                .iter()
                .map(|part| {
                    let (kind, text) = match part {
                        RuntimePart::Text(text) => ("text", text),
                        RuntimePart::Secret(name) => ("secret", name),
                        RuntimePart::QuotedSecret(name) => ("quoted_secret", name),
                    };
                    table([(kind, Value::Str(text.clone()))])
                })
                .collect();
            table([("runtime", Value::List(parts))])
        }
        Value::List(items) => Value::List(items.iter().map(spell_out).collect()),
        Value::Table(entries) => spell_out_entries(entries),
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_) => value.clone(),
    }
}

fn spell_out_entries(entries: &BTreeMap<String, Value>) -> Value {
    Value::Table(
        entries
            .iter()
            .map(|(key, value)| (key.clone(), spell_out(value)))
            .collect(),
    )
}

#[cfg(test)]
mod tests;
