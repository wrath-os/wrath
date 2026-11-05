//! The resolved model: a complete description of one machine with no logic, references or
//! defaults left in it. It is what every backend consumes, and its hash identifies the system.
//!
//! The model leaves out provenance (which file or module set what), so moving a value from one
//! file to another doesn't change the system. This is a first draft; the format is versioned.

mod plan;

use std::collections::{BTreeMap, BTreeSet};

use distro_eval::{Evaluation, RuntimePart, Section, Value, formats};

pub use plan::{Change, ChangeKind, Plan, Tier, plan};

/// The version of the model format, part of the hash.
pub const VERSION: i64 = 1;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Model {
    /// Every option with a value, by dotted path.
    pub options: BTreeMap<String, Value>,
    pub packages: BTreeSet<String>,
    pub files: BTreeMap<String, File>,
    pub units: BTreeMap<String, Vec<Section>>,
    /// Machine facts the configuration read, by dotted path.
    pub facts: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub mode: u32,
    pub text: String,
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
            model.files.extend(output.files.iter().map(|(path, file)| {
                let file = File {
                    mode: file.mode,
                    text: file.text.clone(),
                };
                (path.clone(), file)
            }));
            model.units.extend(
                output
                    .units
                    .iter()
                    .map(|(name, unit)| (name.clone(), unit.sections.clone())),
            );
        }
        model
    }

    /// The model as one value, with runtime values spelled out as tables.
    pub fn to_value(&self) -> Value {
        let packages = self
            .packages
            .iter()
            .map(|name| Value::Str(name.clone()))
            .collect();
        let files = self
            .files
            .iter()
            .map(|(path, file)| {
                let file = table([
                    ("mode", Value::Int(i64::from(file.mode))),
                    ("text", Value::Str(file.text.clone())),
                ]);
                (path.clone(), file)
            })
            .collect();
        let units = self
            .units
            .iter()
            .map(|(name, sections)| {
                let sections = sections.iter().map(section_value).collect();
                (name.clone(), Value::List(sections))
            })
            .collect();
        table([
            ("version", Value::Int(VERSION)),
            ("options", spell_out_entries(&self.options)),
            ("packages", Value::List(packages)),
            ("files", Value::Table(files)),
            ("units", Value::Table(units)),
            ("facts", spell_out_entries(&self.facts)),
        ])
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
