//! `machines.toml`: the machines a configuration with roles names, each with its role and the
//! values it's given.

use std::collections::BTreeMap;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::diag::Diagnostic;
use crate::fs::ConfigFs;
use crate::source::{FileId, Sources, Span};
use crate::suggest::did_you_mean;

pub(crate) const MACHINES: &str = "machines.toml";
const ROLE: &str = "role";

/// A machine named in `machines.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub role: String,
    /// The values it's given, such as its zone, by the names its role declares.
    pub values: BTreeMap<String, String>,
}

/// Reads `machines.toml`, when there is one. Every machine needs a role in `roles`, and the
/// machines with `role`, the one being evaluated, need exactly the values it declares.
pub(crate) fn machines(
    fs: &dyn ConfigFs,
    sources: &mut Sources,
    roles: &[String],
    role: Option<&str>,
    declared: &BTreeMap<String, Span>,
) -> (BTreeMap<String, Machine>, Vec<Diagnostic>) {
    let Ok(text) = fs.read(MACHINES) else {
        return (BTreeMap::new(), Vec::new());
    };
    let file = sources.add(fs.display(MACHINES), text);
    if roles.is_empty() {
        let diagnostic = Diagnostic::error("`machines.toml` names machines for roles")
            .note("this configuration describes one machine in `system.toml`")
            .note("help: set what differs in the configuration itself, or move to `roles/`");
        return (BTreeMap::new(), vec![diagnostic]);
    }
    let table = match DeTable::parse(sources.text(file)) {
        Ok(table) => table,
        Err(e) => {
            let message = e.message().trim().replace('\n', "; ");
            let diagnostic = Diagnostic::error("invalid TOML")
                .label_opt(e.span().map(|range| Span::new(file, range)), message);
            return (BTreeMap::new(), vec![diagnostic]);
        }
    };
    let check = Check {
        file,
        roles,
        role,
        declared,
        diagnostics: Vec::new(),
    };
    check.run(table.get_ref())
}

struct Check<'a> {
    file: FileId,
    roles: &'a [String],
    role: Option<&'a str>,
    declared: &'a BTreeMap<String, Span>,
    diagnostics: Vec<Diagnostic>,
}

impl Check<'_> {
    fn run(mut self, table: &DeTable<'_>) -> (BTreeMap<String, Machine>, Vec<Diagnostic>) {
        let machines = table
            .iter()
            .filter_map(|(name, entry)| {
                let machine = self.machine(self.span(name), entry)?;
                Some((name.get_ref().to_string(), machine))
            })
            .collect();
        (machines, self.diagnostics)
    }

    fn span(&self, value: &Spanned<impl Sized>) -> Span {
        Span::new(self.file, value.span())
    }

    /// One machine, `w-01 = { role = "worker", zone = "eu-north-1a" }`.
    fn machine(&mut self, name: Span, entry: &Spanned<DeValue<'_>>) -> Option<Machine> {
        let DeValue::Table(entry) = entry.get_ref() else {
            self.diagnostics.push(
                Diagnostic::error("a machine is a table with its role")
                    .label(self.span(entry), "expected `{ role = \"...\" }`"),
            );
            return None;
        };
        let Some(role) = entry.get(ROLE) else {
            self.diagnostics.push(
                Diagnostic::error("a machine needs a role")
                    .label(name, "no `role` here")
                    .note(format!("the roles are {}", self.roles.join(", "))),
            );
            return None;
        };
        let DeValue::String(role_name) = role.get_ref() else {
            self.diagnostics.push(
                Diagnostic::error("a machine's role is a string")
                    .label(self.span(role), "expected the name of a role"),
            );
            return None;
        };
        if !self.roles.iter().any(|known| known == role_name) {
            let mut diagnostic = Diagnostic::error(format!("there's no role `{role_name}`"))
                .label(self.span(role), "named here")
                .note(format!("the roles are {}", self.roles.join(", ")));
            if let Some(note) = did_you_mean(role_name, self.roles) {
                diagnostic = diagnostic.note(note);
            }
            self.diagnostics.push(diagnostic);
            return None;
        }
        let mut values = BTreeMap::new();
        for (key, value) in entry.iter().filter(|(key, _)| key.get_ref() != ROLE) {
            match value.get_ref() {
                DeValue::String(text) => {
                    values.insert(key.get_ref().to_string(), text.to_string());
                }
                other => self.diagnostics.push(
                    Diagnostic::error(format!(
                        "machine values are strings, found {}",
                        other.type_str()
                    ))
                    .label(self.span(value), "expected a string"),
                ),
            }
        }
        if self.role == Some(role_name) {
            self.check_values(name, entry);
        }
        Some(Machine {
            role: role_name.to_string(),
            values,
        })
    }

    /// Checks a machine with the role being evaluated gives exactly the values it declares.
    fn check_values(&mut self, name: Span, entry: &DeTable<'_>) {
        for (key, _) in entry.iter().filter(|(key, _)| key.get_ref() != ROLE) {
            if self.declared.contains_key(key.get_ref().as_ref()) {
                continue;
            }
            let mut diagnostic = Diagnostic::error(format!(
                "the role doesn't declare a machine value `{}`",
                key.get_ref()
            ))
            .label(self.span(key), "given here");
            diagnostic = match did_you_mean(key.get_ref(), self.declared.keys()) {
                Some(note) => diagnostic.note(note),
                None => diagnostic.note("help: declare it in the role's `[machine.values]`"),
            };
            self.diagnostics.push(diagnostic);
        }
        // A machine's name is its key here.
        for (key, at) in self.declared {
            if key != "name" && !entry.contains_key(key.as_str()) {
                self.diagnostics.push(
                    Diagnostic::error(format!("this machine isn't given `{key}`"))
                        .label(name, format!("needs `{key}`"))
                        .label(*at, "declared here"),
                );
            }
        }
    }
}
