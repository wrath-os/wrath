//! Turning parsed configuration files into option definitions, checked against the schema.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::diag::Diagnostic;
use crate::load::{INCLUDE, LoadedFile};
use crate::path::OptionPath;
use crate::schema::{OptionSpec, Schema, Type};
use crate::source::{FileId, Sources, Span};
use crate::suggest::did_you_mean;
use crate::template::{self, Part};
use crate::value::Value;

/// One place that sets an option.
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub path: OptionPath,
    pub value: Raw,
    pub level: Level,
    pub site: Site,
    /// Why a forced value is forced.
    pub reason: Option<String>,
}

/// How strongly a definition holds. A higher level replaces lower ones, and two different values
/// at the same level conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Default,
    Normal,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    Config(Span),
    /// The option's declared default.
    Default,
}

impl Site {
    pub fn span(self) -> Option<Span> {
        match self {
            Self::Config(span) => Some(span),
            Self::Default => None,
        }
    }
}

/// A value as written, before references are resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    Literal(Value),
    Template(Vec<Part>),
    List(Vec<Raw>),
    Table(BTreeMap<String, Raw>),
    Secret(String),
}

#[derive(Debug, Default)]
pub(crate) struct Walked {
    pub definitions: Vec<Definition>,
    /// Map entries the configuration names, such as `services.nginx.main`.
    pub instances: BTreeSet<OptionPath>,
    pub diagnostics: Vec<Diagnostic>,
}

pub(crate) fn walk(schema: &Schema, sources: &Sources, files: &[LoadedFile]) -> Walked {
    let mut walker = Walker {
        sources,
        out: Walked::default(),
    };
    for file in files {
        walker.file(schema, file);
    }
    walker.out
}

struct Walker<'a> {
    sources: &'a Sources,
    out: Walked,
}

impl Walker<'_> {
    fn file(&mut self, schema: &Schema, loaded: &LoadedFile) {
        // Files that don't parse were reported while loading.
        let Ok(table) = DeTable::parse(self.sources.text(loaded.file)) else {
            return;
        };
        let Some(spec) = schema.lookup(&loaded.root) else {
            self.out.diagnostics.push(Diagnostic::error(format!(
                "no module declares `{}`, so `{}` has nothing to configure",
                loaded.root,
                self.sources.name(loaded.file)
            )));
            return;
        };
        let mut spec_at = schema.root();
        let mut path = OptionPath::root();
        for segment in loaded.root.segments() {
            path = path.child(segment.as_str());
            spec_at = match &spec_at.ty {
                Type::Map(item) => {
                    self.out.instances.insert(path.clone());
                    item
                }
                Type::Record(record) => &record.fields[segment],
                Type::Bool
                | Type::Int { .. }
                | Type::Float
                | Type::Str
                | Type::Path
                | Type::Enum(_)
                | Type::List { .. }
                | Type::Freeform => unreachable!("the lookup above found a table"),
            };
        }
        if !spec.ty.is_table() {
            self.out.diagnostics.push(Diagnostic::error(format!(
                "`{}` is not a table of options, so `{}` cannot configure it",
                loaded.root,
                self.sources.name(loaded.file)
            )));
            return;
        }
        self.table(spec, &loaded.root, table.get_ref(), loaded.file, true);
    }

    fn table(
        &mut self,
        spec: &OptionSpec,
        path: &OptionPath,
        table: &DeTable<'_>,
        file: FileId,
        top: bool,
    ) {
        for (key, value) in table.iter() {
            let name: &str = key.get_ref();
            if top && name == INCLUDE {
                continue;
            }
            let child = path.child(name);
            let child_spec = match &spec.ty {
                Type::Record(record) => match record.fields.get(name) {
                    Some(field) => Cow::Borrowed(field),
                    None => {
                        let mut diagnostic = Diagnostic::error(format!("unknown option `{child}`"))
                            .label(Span::new(file, key.span()), "no option with this name");
                        if let Some(note) = did_you_mean(name, record.fields.keys()) {
                            diagnostic = diagnostic.note(note);
                        }
                        self.out.diagnostics.push(diagnostic);
                        continue;
                    }
                },
                Type::Map(item) => {
                    self.out.instances.insert(child.clone());
                    Cow::Borrowed(item.as_ref())
                }
                Type::Freeform => Cow::Owned(OptionSpec::freeform()),
                Type::Bool
                | Type::Int { .. }
                | Type::Float
                | Type::Str
                | Type::Path
                | Type::Enum(_)
                | Type::List { .. } => unreachable!("only tables of options are walked"),
            };
            self.node(&child_spec, child, value, file);
        }
    }

    fn node(
        &mut self,
        spec: &OptionSpec,
        path: OptionPath,
        value: &Spanned<DeValue<'_>>,
        file: FileId,
    ) {
        let span = Span::new(file, value.span());
        match value.get_ref() {
            DeValue::Table(table) if spec.ty.is_table() => {
                self.table(spec, &path, table, file, false);
            }
            other if spec.ty.is_table() => self.out.diagnostics.push(
                Diagnostic::error(format!(
                    "`{path}` is a table of options, found {}",
                    other.type_str()
                ))
                .label(span, "expected a table"),
            ),
            DeValue::Table(table)
                if matches!(spec.ty, Type::Freeform)
                    && !table.contains_key("secret")
                    && !table.contains_key("force") =>
            {
                self.table(spec, &path, table, file, false);
            }
            _ => match self.leaf(value, file) {
                Ok((raw, reason)) => self.out.definitions.push(Definition {
                    path,
                    value: raw,
                    level: if reason.is_some() {
                        Level::Force
                    } else {
                        Level::Normal
                    },
                    site: Site::Config(span),
                    reason,
                }),
                Err(diagnostic) => self.out.diagnostics.push(diagnostic),
            },
        }
    }

    /// A value, or a forced one written `{ value = ..., force = "reason" }`.
    fn leaf(
        &self,
        value: &Spanned<DeValue<'_>>,
        file: FileId,
    ) -> Result<(Raw, Option<String>), Diagnostic> {
        let DeValue::Table(table) = value.get_ref() else {
            return Ok((self.raw(value, file)?, None));
        };
        let Some(force) = table.get("force") else {
            return Ok((self.raw(value, file)?, None));
        };
        let span = Span::new(file, value.span());
        if let Some((key, _)) = table
            .iter()
            .find(|(key, _)| !matches!(key.get_ref().as_ref(), "force" | "value"))
        {
            return Err(Diagnostic::error(format!(
                "unexpected `{}` in a forced value",
                key.get_ref()
            ))
            .label(
                Span::new(file, key.span()),
                "only `value` and `force` belong here",
            ));
        }
        let reason = match force.get_ref() {
            DeValue::String(reason) if !reason.trim().is_empty() => reason.to_string(),
            _ => {
                return Err(Diagnostic::error("`force` needs a reason")
                    .label(Span::new(file, force.span()), "expected a non-empty string")
                    .note("help: for example `force = \"the vendor default breaks suspend\"`"));
            }
        };
        let Some(inner) = table.get("value") else {
            return Err(
                Diagnostic::error("a forced value needs `value`").label(span, "add `value = ...`")
            );
        };
        Ok((self.raw(inner, file)?, Some(reason)))
    }

    fn raw(&self, value: &Spanned<DeValue<'_>>, file: FileId) -> Result<Raw, Diagnostic> {
        let span = Span::new(file, value.span());
        Ok(match value.get_ref() {
            DeValue::String(text) => Raw::Template(self.template(text, span)?),
            DeValue::Integer(int) => {
                let int = i64::from_str_radix(int.as_str(), int.radix()).map_err(|_| {
                    Diagnostic::error("integer out of range").label(span, "does not fit in 64 bits")
                })?;
                Raw::Literal(Value::Int(int))
            }
            DeValue::Float(float) => {
                let float = float
                    .as_str()
                    .parse()
                    .map_err(|_| Diagnostic::error("invalid float").label(span, "not a number"))?;
                Raw::Literal(Value::Float(float))
            }
            DeValue::Boolean(b) => Raw::Literal(Value::Bool(*b)),
            DeValue::Datetime(datetime) => Raw::Literal(Value::Str(datetime.to_string())),
            DeValue::Array(items) => Raw::List(
                items
                    .iter()
                    .map(|item| self.raw(item, file))
                    .collect::<Result<_, _>>()?,
            ),
            DeValue::Table(table) => match table.get("secret") {
                Some(name) => match name.get_ref() {
                    DeValue::String(name) if table.len() == 1 => Raw::Secret(name.to_string()),
                    _ => {
                        return Err(Diagnostic::error("invalid secret")
                            .label(span, "expected `{ secret = \"name\" }`"));
                    }
                },
                None => Raw::Table(
                    table
                        .iter()
                        .map(|(key, value)| Ok((key.get_ref().to_string(), self.raw(value, file)?)))
                        .collect::<Result<_, Diagnostic>>()?,
                ),
            },
        })
    }

    fn template(&self, text: &str, span: Span) -> Result<Vec<Part>, Diagnostic> {
        let source = &self.sources.text(span.file)[span.start..span.end];
        // Quotes are one byte, so this only matches strings written without escapes.
        let exact = source.len() == text.len() + 2 && &source[1..source.len() - 1] == text;
        template::parse(text, exact.then_some(span.start + 1), span)
            .map_err(|(message, span)| Diagnostic::error(message).label(span, "in this reference"))
    }
}
