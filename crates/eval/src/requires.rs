//! `[machine.requires]`: what a machine needs to run a configuration, such as a GPU or enough
//! memory. Each machine checks it before it switches.

use std::fmt;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::diag::Diagnostic;
use crate::source::{FileId, Span};
use crate::suggest::closest;
use crate::walk::only_keys;

/// What a requirement can be about: facts a machine detects, and whether each is a number.
pub const FACTS: [(&str, Kind); 4] = [
    ("arch", Kind::Name),
    ("cpu.count", Kind::Number),
    ("gpu", Kind::Name),
    ("memory_gb", Kind::Number),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Such as `x86_64`, required as it is or `within` a list.
    Name,
    /// Such as a count, required as it is or `at_least` some amount.
    Number,
}

/// One thing a machine needs, written in `[machine.requires]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// The fact it's about, one of [`FACTS`].
    pub fact: &'static str,
    pub check: Check,
    pub at: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    Is(String),
    AtLeast(i64),
    Within(Vec<String>),
}

impl Check {
    /// Whether a machine with `values` for the fact meets it. A machine can have several, such
    /// as GPUs, and meets it when one does.
    pub fn met_by(&self, values: &[String]) -> bool {
        values.iter().any(|value| match self {
            Self::Is(wanted) => value == wanted,
            Self::AtLeast(least) => value.parse::<i64>().is_ok_and(|value| value >= *least),
            Self::Within(names) => names.contains(value),
        })
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Is(value) => write!(f, "{value}"),
            Self::AtLeast(least) => write!(f, "at least {least}"),
            Self::Within(names) => write!(f, "one of {}", names.join(", ")),
        }
    }
}

const AT_LEAST: &str = "at_least";
const WITHIN: &str = "within";

/// Reads `[machine.requires]`, adding each requirement to `out`. Dotted keys name a fact, so
/// `cpu.count = { at_least = 8 }` is about `cpu.count`.
pub(crate) fn read(
    table: &DeTable<'_>,
    file: FileId,
    out: &mut Vec<Requirement>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    read_under("", table, file, out, diagnostics);
}

fn read_under(
    prefix: &str,
    table: &DeTable<'_>,
    file: FileId,
    out: &mut Vec<Requirement>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (key, value) in table.iter() {
        let name = format!("{prefix}{}", key.get_ref());
        let key_span = Span::new(file, key.span());
        let fact = FACTS.iter().find(|(fact, _)| *fact == name);
        match (fact, value.get_ref()) {
            (None, DeValue::Table(inner))
                if FACTS
                    .iter()
                    .any(|(fact, _)| fact.starts_with(&format!("{name}."))) =>
            {
                read_under(&format!("{name}."), inner, file, out, diagnostics);
            }
            (None, _) => {
                let diagnostic = Diagnostic::error(format!("unknown fact `{name}`"))
                    .label(key_span, "required here");
                let diagnostic = match closest(&name, FACTS.iter().map(|(fact, _)| *fact)) {
                    Some(fact) => diagnostic.note(format!("help: did you mean `{fact}`?")),
                    None => diagnostic.note(format!(
                        "a machine can be required to have {}",
                        FACTS.map(|(fact, _)| fact).join(", ")
                    )),
                };
                diagnostics.push(diagnostic);
            }
            (Some(&(fact, kind)), _) => match check(fact, kind, value, file) {
                Ok(check) => out.push(Requirement {
                    fact,
                    check,
                    at: key_span,
                }),
                Err(diagnostic) => diagnostics.push(diagnostic),
            },
        }
    }
}

/// The check written for `fact`: a value it must be, or `{ at_least = N }` for a number, or
/// `{ within = [...] }` for a name.
fn check(
    fact: &str,
    kind: Kind,
    value: &Spanned<DeValue<'_>>,
    file: FileId,
) -> Result<Check, Diagnostic> {
    let span = Span::new(file, value.span());
    let (allowed, example) = match kind {
        Kind::Name => (WITHIN, "a name, or `{ within = [...] }`"),
        Kind::Number => (AT_LEAST, "a number, or `{ at_least = N }`"),
    };
    let wrong = || {
        Diagnostic::error(format!("`{fact}` is required as {example}"))
            .label(span, format!("expected {example}"))
    };
    let table = match (kind, value.get_ref()) {
        (Kind::Name, DeValue::String(name)) => return Ok(Check::Is(name.to_string())),
        (Kind::Number, DeValue::Integer(number)) => {
            return number_of(number, span).map(|n| Check::Is(n.to_string()));
        }
        (_, DeValue::Table(table)) => table,
        _ => return Err(wrong()),
    };
    only_keys(
        table,
        &[allowed],
        &format!("a requirement on `{fact}`"),
        file,
    )?;
    let inner = table.get(allowed).ok_or_else(wrong)?;
    let inner_span = Span::new(file, inner.span());
    match (kind, inner.get_ref()) {
        (Kind::Number, DeValue::Integer(number)) => {
            number_of(number, inner_span).map(Check::AtLeast)
        }
        (Kind::Name, DeValue::Array(items)) => items
            .iter()
            .map(|item| match item.get_ref() {
                DeValue::String(name) => Ok(name.to_string()),
                _ => Err(Diagnostic::error(format!("`{fact}` is a name"))
                    .label(Span::new(file, item.span()), "expected a string")),
            })
            .collect::<Result<_, _>>()
            .map(Check::Within),
        _ => Err(wrong()),
    }
}

fn number_of(number: &toml::de::DeInteger<'_>, span: Span) -> Result<i64, Diagnostic> {
    i64::from_str_radix(number.as_str(), number.radix()).map_err(|_| {
        Diagnostic::error("integer out of range").label(span, "does not fit in 64 bits")
    })
}
