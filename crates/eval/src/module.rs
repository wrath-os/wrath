//! The host API between the evaluator and modules, independent of the language modules are
//! written in.
//!
//! A module declares options, names the option prefixes it may set, and renders: it reads
//! options through a [`ConfigReader`] and returns settings for other options plus the files,
//! units and packages it contributes. A module cannot read anything under the prefixes it sets,
//! so contributions only ever flow one way.

use std::collections::{BTreeMap, BTreeSet};

use crate::diag::Diagnostic;
use crate::load::ConfigKind;
use crate::path::OptionPath;
use crate::schema::OptionSpec;
use crate::source::Span;
use crate::value::{RuntimePart, Value};
use crate::walk::Level;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(pub(crate) u32);

impl ModuleId {
    pub(crate) fn from_index(index: usize) -> Self {
        Self(u32::try_from(index).expect("fewer than 2^32 modules"))
    }

    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

pub trait Module {
    fn name(&self) -> &str;

    /// Options the module declares, by absolute path.
    fn options(&self) -> Vec<(OptionPath, OptionSpec)>;

    /// Option prefixes the module may set.
    fn writes(&self) -> Vec<OptionPath>;

    /// The kinds of configuration the module belongs to.
    fn configurations(&self) -> &[ConfigKind] {
        &[ConfigKind::System]
    }

    fn render(&self, config: &mut dyn ConfigReader) -> Result<Output, RenderError>;
}

pub trait ConfigReader {
    /// The value of an option, or a table of everything set under a table of options. `None`
    /// when an option without a default isn't set. `at` is where the module reads it.
    fn read(&mut self, path: &OptionPath, at: Option<Span>) -> Result<Option<Value>, ReadError>;

    /// Where the configuration sets the option at `path`, or the first option under it.
    fn locate(&self, path: &OptionPath) -> Option<Span>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    /// The read itself is wrong, such as an unknown option. The module should fail with this.
    Invalid(String),
    /// The option failed to evaluate and the error is already reported. The module should stop
    /// with [`RenderError::Failed`].
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderError {
    /// Stopped because something it read failed; already reported.
    Failed,
    Error(Diagnostic),
}

/// The source distro itself comes from, pinned in the lock like the others. Packages come from
/// it when their name doesn't say otherwise, and so does the base system.
pub const DISTRO_SOURCE: &str = "distro";

/// The source a package comes from and its name there: `stable#git` is `git` from `stable`, and
/// a name without a source comes from [`DISTRO_SOURCE`].
pub fn package_source(name: &str) -> (&str, &str) {
    name.split_once('#').unwrap_or((DISTRO_SOURCE, name))
}

/// What a module contributes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Output {
    pub settings: Vec<Setting>,
    pub packages: BTreeSet<String>,
    /// Files by absolute path.
    pub files: BTreeMap<String, File>,
    /// systemd units by name, such as `syncthing.service`.
    pub units: BTreeMap<String, Unit>,
    /// Settings for the base system the backend provides, by top-level key, such as `audio`.
    /// Each key belongs to one module.
    pub base: BTreeMap<String, Value>,
    /// Services the system provides that act when files change, such as networkd reloading
    /// when a `.network` file does. Several modules can trigger the same service.
    pub triggers: BTreeMap<(String, UnitAction), BTreeSet<String>>,
    /// Secrets decrypted on the machine, by name.
    pub secrets: BTreeMap<String, Secret>,
    /// What goes in users' homes, by user.
    pub homes: BTreeMap<String, Home>,
    /// Files with secrets in them, by absolute path, filled in on the machine.
    pub templates: BTreeMap<String, Template>,
    /// Files read when a user logs in, so changing them takes effect at the next login, by path
    /// as plans show it: absolute, or as `~user/.profile`.
    pub next_login: BTreeSet<String>,
    /// Packages whose units, users, tmpfiles, udev rules and D-Bus services join the system, such
    /// as `dbus`. Other packages only add programs.
    pub service_packages: BTreeSet<String>,
    /// Units the service packages ship that the system enables, each with the targets that want
    /// it, or none to enable it as `systemctl enable` does, by its [Install] section.
    pub enabled: BTreeMap<String, BTreeSet<String>>,
    /// Likewise for user units, enabled for every user.
    pub enabled_user: BTreeMap<String, BTreeSet<String>>,
    /// Programs that run with more privilege than their caller, in `/run/wrappers/bin`, by name.
    pub wrappers: BTreeMap<String, Wrapper>,
    /// Packages built differently from how their source builds them, by the name they go by.
    pub builds: BTreeMap<String, Build>,
}

/// A package built with other options or patches than its source's.
#[derive(Debug, Clone, PartialEq)]
pub struct Build {
    /// The package it starts from, such as `whisper-cpp` or `stable#jq`.
    pub package: String,
    /// Values for the options the package's build takes, by name.
    pub options: BTreeMap<String, Value>,
    /// Patches to apply, as paths inside the configuration.
    pub patches: Vec<String>,
}

/// A program copied into `/run/wrappers/bin` with its own owner and mode, such as setuid root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wrapper {
    /// The program, such as `/run/system/bin/sudo`.
    pub source: String,
    pub owner: String,
    pub group: String,
    pub mode: u32,
}

/// A file with secrets in it. The text around the secrets is known when the system is built;
/// the machine fills in the secrets and keeps the result off disk, readable by its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub parts: Vec<RuntimePart>,
    pub owner: String,
    /// The owner's group when unset.
    pub group: Option<String>,
    pub mode: u32,
}

/// A user's files, packages and the units their systemd user manager runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Home {
    /// Files by path relative to the home directory, as `~/.config/git/config`.
    pub files: BTreeMap<String, File>,
    pub packages: BTreeSet<String>,
    /// User units by name, such as `notes-sync.service`.
    pub units: BTreeMap<String, Unit>,
    /// Files with secrets in them, by path as `~/.netrc`, filled in on the machine.
    pub templates: BTreeMap<String, Template>,
}

/// A home file's path as `~user/.config/git/config`, from `~/.config/git/config`.
pub fn home_path(user: &str, path: &str) -> String {
    format!("~{user}{}", &path[1..])
}

/// Where decrypted secrets are on the machine, one file per secret.
pub const SECRETS_DIR: &str = "/run/secrets";

/// Where a home of its own decrypts its secrets, which the user can do without root.
pub const HOME_SECRETS_DIR: &str = "~/.local/state/distro/secrets";

/// A secret kept encrypted in the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secret {
    /// The encrypted file, relative to the configuration or absolute.
    pub file: String,
    pub owner: String,
    /// The owner's group when unset.
    pub group: Option<String>,
    pub mode: u32,
    /// Another path linked to the decrypted file.
    pub link: Option<String>,
}

/// What a service does when a file it reads changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnitAction {
    Reload,
    Restart,
}

impl UnitAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reload => "reload",
            Self::Restart => "restart",
        }
    }

    /// The action as a verb in a sentence, such as "reloads".
    pub fn verb(self) -> &'static str {
        match self {
            Self::Reload => "reloads",
            Self::Restart => "restarts",
        }
    }
}

/// A value a module sets for an option, or for a table of options.
#[derive(Debug, Clone, PartialEq)]
pub struct Setting {
    pub path: OptionPath,
    pub value: Value,
    pub level: Level,
    pub at: Option<Span>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub text: String,
    pub mode: u32,
}

/// A unit file as ordered sections of ordered entries. Keys may repeat, as in `ExecStartPre`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unit {
    pub sections: Vec<Section>,
    /// Files the unit reads. When one changes, the unit restarts even if its own file didn't.
    pub restart_on: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Section {
    pub name: String,
    pub entries: Vec<(String, String)>,
}

impl Unit {
    /// The unit file's text, with a blank line between sections.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for section in &self.sections {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("[{}]\n", section.name));
            for (key, value) in &section.entries {
                out.push_str(&format!("{key}={value}\n"));
            }
        }
        out
    }

    /// A unit from a table of sections, each a table of keys. A list value repeats its key.
    /// `[Unit]` (or `[Match]`, in network files) comes first and `[Install]` last; other sections
    /// and all keys are sorted, so the result doesn't depend on how a module built the table.
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let Value::Table(sections) = value else {
            return Err(format!("a unit is a table of sections, found {value}"));
        };
        let rank = |name: &str| match name {
            "Unit" | "Match" => 0,
            "Install" => 2,
            _ => 1,
        };
        let mut names: Vec<&String> = sections.keys().collect();
        names.sort_by_key(|name| (rank(name), name.as_str()));
        let sections = names
            .into_iter()
            .map(|name| {
                let Value::Table(keys) = &sections[name] else {
                    return Err(format!("section `{name}` must be a table of keys"));
                };
                let mut entries = Vec::new();
                for (key, value) in keys {
                    let values = match value {
                        Value::List(values) => values.as_slice(),
                        single => std::slice::from_ref(single),
                    };
                    for value in values {
                        entries.push((
                            key.clone(),
                            unit_value(value).map_err(|e| format!("`{name}.{key}`: {e}"))?,
                        ));
                    }
                }
                Ok(Section {
                    name: name.clone(),
                    entries,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            sections,
            restart_on: BTreeSet::new(),
        })
    }
}

fn unit_value(value: &Value) -> Result<String, String> {
    match value {
        Value::Str(text) => Ok(text.clone()),
        Value::Int(int) => Ok(int.to_string()),
        Value::Float(float) => Ok(float.to_string()),
        Value::Bool(b) => Ok(if *b { "yes" } else { "no" }.to_owned()),
        Value::List(_) | Value::Table(_) | Value::Runtime(_) => Err(format!(
            "expected a string, number or boolean, found {value}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn table<const N: usize>(entries: [(&str, Value); N]) -> Value {
        Value::Table(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn units_have_a_canonical_section_and_key_order() {
        let value = table([
            (
                "Install",
                table([("WantedBy", Value::Str("multi-user.target".into()))]),
            ),
            (
                "Service",
                table([
                    (
                        "ExecStartPre",
                        Value::List(vec![Value::Str("a".into()), Value::Str("b".into())]),
                    ),
                    ("Restart", Value::Bool(true)),
                ]),
            ),
            ("Unit", table([("Description", Value::Str("x".into()))])),
        ]);

        let unit = Unit::from_value(&value).unwrap();
        let rendered: Vec<String> = unit
            .sections
            .iter()
            .flat_map(|section| {
                section
                    .entries
                    .iter()
                    .map(move |(key, value)| format!("{}.{key}={value}", section.name))
            })
            .collect();
        assert_eq!(
            rendered,
            [
                "Unit.Description=x",
                "Service.ExecStartPre=a",
                "Service.ExecStartPre=b",
                "Service.Restart=yes",
                "Install.WantedBy=multi-user.target",
            ]
        );
        assert_eq!(
            Unit::from_value(&table([("Service", table([("X", table([]))]))])).unwrap_err(),
            "`Service.X`: expected a string, number or boolean, found {}"
        );
    }
}
