//! Turning parsed configuration files into option definitions, checked against the schema.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::diag::Diagnostic;
use crate::load::{INCLUDE, LoadedFile};
use crate::module::ModuleId;
use crate::path::OptionPath;
use crate::schema::{Child, OptionSpec, Schema, Type};
use crate::source::{FileId, Sources, Span};
use crate::suggest::did_you_mean;
use crate::template::{self, Part};
use crate::value::Value;

/// The keys of a change to a list, `{ add = [...], remove = [...] }`.
pub(crate) const ADD: &str = "add";
pub(crate) const REMOVE: &str = "remove";

/// `[machine.values]`, where a configuration declares what each machine is given.
pub(crate) const MACHINE: &str = "machine";
const VALUES: &str = "values";

/// One place that sets an option.
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub path: OptionPath,
    pub value: Raw,
    pub level: Level,
    pub site: Site,
    /// Why a forced value is forced.
    pub reason: Option<String>,
    pub action: Action,
}

/// What a definition does to its option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Set,
    /// Removes items from a set or ordered list, whatever level added them. Written
    /// `{ remove = [...] }`; ordered list items are removed by name.
    Remove,
}

/// How strongly a definition holds. A higher level replaces lower ones, and two different values
/// at the same level conflict. Set and ordered lists merge everything from `ModuleDefault` to
/// `Normal`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// An option's declared default, used only when nothing else sets the option.
    Declared,
    /// Set with `ctx.default` in a module.
    ModuleDefault,
    /// Written `{ default = ... }` in a configuration file. Between two of these, the file that
    /// includes the other wins.
    Default,
    Normal,
    Force,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    Config(Span),
    /// The option's declared default.
    Default,
    /// Set by a module while rendering. `at` is where in the module, when the language knows.
    Module {
        module: ModuleId,
        at: Option<Span>,
    },
}

impl Site {
    pub fn span(self) -> Option<Span> {
        match self {
            Self::Config(span) => Some(span),
            Self::Module { at, .. } => at,
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
    /// Values each machine is given, declared in `[machine.values]`, and where.
    pub machine_values: BTreeMap<String, Span>,
    pub diagnostics: Vec<Diagnostic>,
}

pub(crate) fn walk(schema: &Schema, sources: &Sources, files: &[LoadedFile]) -> Walked {
    let mut walker = Walker {
        sources,
        defaults: false,
        out: Walked::default(),
    };
    for file in files {
        walker.file(schema, file);
    }
    walker.out
}

struct Walker<'a> {
    sources: &'a Sources,
    /// Whether the file being walked gives defaults.
    defaults: bool,
    out: Walked,
}

impl Walker<'_> {
    fn file(&mut self, schema: &Schema, loaded: &LoadedFile) {
        self.defaults = loaded.defaults;
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
        mark_instances(schema, &loaded.root, &mut self.out.instances);
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

    /// `[machine.values]`: the values each machine is given, such as its zone.
    fn machine(&mut self, value: &Spanned<DeValue<'_>>, file: FileId) {
        let DeValue::Table(machine) = value.get_ref() else {
            return self.out.diagnostics.push(
                Diagnostic::error(format!(
                    "`machine` is a table, found {}",
                    value.get_ref().type_str()
                ))
                .label(Span::new(file, value.span()), "expected `[machine.values]`"),
            );
        };
        if let Err(diagnostic) = only_keys(machine, &[VALUES], "`[machine]`", file) {
            self.out.diagnostics.push(diagnostic);
        }
        let Some(declarations) = machine.get(VALUES) else {
            return;
        };
        let DeValue::Table(declarations) = declarations.get_ref() else {
            return self.out.diagnostics.push(
                Diagnostic::error("`machine.values` is a table of declarations").label(
                    Span::new(file, declarations.span()),
                    "expected a table, such as `zone = { type = \"string\" }`",
                ),
            );
        };
        for (key, declaration) in declarations.iter() {
            match machine_value(declaration, file) {
                Ok(()) => {
                    self.out
                        .machine_values
                        .entry(key.get_ref().to_string())
                        .or_insert(Span::new(file, key.span()));
                }
                Err(diagnostic) => self.out.diagnostics.push(diagnostic),
            }
        }
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
            if top && path.is_empty() && name == MACHINE {
                self.machine(value, file);
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
            DeValue::Table(table) if table.contains_key(ADD) || table.contains_key(REMOVE) => {
                if let Err(diagnostic) = self.list_change(spec, &path, span, table, file) {
                    self.out.diagnostics.push(diagnostic);
                }
            }
            _ => match self.leaf(value, file) {
                Ok((_, Level::Default, _)) if spec.ty.merges_items() => {
                    self.out.diagnostics.push(
                        Diagnostic::error(format!(
                            "`{path}` merges with other files, so it has no default"
                        ))
                        .label(span, "written as a default")
                        .note(
                            "help: write the list itself; others add to it, and take items out \
                             with `{ remove = [...] }`",
                        ),
                    );
                }
                Ok((_, Level::Force, _)) if self.defaults => self.out.diagnostics.push(
                    Diagnostic::error("a file included as defaults can't force a value")
                        .label(span, "forced here")
                        .note("help: force it in the file that includes this one"),
                ),
                Ok((raw, level, reason)) => self.out.definitions.push(Definition {
                    path,
                    value: raw,
                    level: if self.defaults { Level::Default } else { level },
                    site: Site::Config(span),
                    reason,
                    action: Action::Set,
                }),
                Err(diagnostic) => self.out.diagnostics.push(diagnostic),
            },
        }
    }

    /// `{ add = [...], remove = [...] }` on a set or ordered list, with either key or both.
    fn list_change(
        &mut self,
        spec: &OptionSpec,
        path: &OptionPath,
        span: Span,
        table: &DeTable<'_>,
        file: FileId,
    ) -> Result<(), Diagnostic> {
        if !spec.ty.merges_items() {
            return Err(
                Diagnostic::error("only set and ordered lists support `add` and `remove`")
                    .label(span, format!("this option is {}", spec.ty.describe())),
            );
        }
        only_keys(table, &[ADD, REMOVE], "a list change", file)?;
        if let Some(items) = table.get(ADD) {
            self.out.definitions.push(Definition {
                path: path.clone(),
                value: self.raw(items, file)?,
                level: if self.defaults {
                    Level::Default
                } else {
                    Level::Normal
                },
                site: Site::Config(Span::new(file, items.span())),
                reason: None,
                action: Action::Set,
            });
        }
        if let Some(items) = table.get(REMOVE) {
            self.out.definitions.push(Definition {
                path: path.clone(),
                value: self.raw(items, file)?,
                level: Level::Normal,
                site: Site::Config(span),
                reason: None,
                action: Action::Remove,
            });
        }
        Ok(())
    }

    /// A value, a default written `{ default = ... }`, or a forced value written
    /// `{ value = ..., force = "reason" }`, with its level and the reason for forcing it.
    fn leaf(
        &self,
        value: &Spanned<DeValue<'_>>,
        file: FileId,
    ) -> Result<(Raw, Level, Option<String>), Diagnostic> {
        let DeValue::Table(table) = value.get_ref() else {
            return Ok((self.raw(value, file)?, Level::Normal, None));
        };
        if let Some(default) = table.get("default") {
            only_keys(table, &["default"], "a default", file)?;
            return Ok((self.raw(default, file)?, Level::Default, None));
        }
        let Some(force) = table.get("force") else {
            return Ok((self.raw(value, file)?, Level::Normal, None));
        };
        let span = Span::new(file, value.span());
        only_keys(table, &["value", "force"], "a forced value", file)?;
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
        Ok((self.raw(inner, file)?, Level::Force, Some(reason)))
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

/// Records the map entries along `path`, such as `services.nginx.main` in
/// `services.nginx.main.port`, so they exist even when nothing under them is set.
pub(crate) fn mark_instances(
    schema: &Schema,
    path: &OptionPath,
    instances: &mut BTreeSet<OptionPath>,
) {
    let mut spec = schema.root();
    let mut prefix = OptionPath::root();
    for segment in path.segments() {
        prefix = prefix.child(segment.as_str());
        spec = match spec.ty.child(segment) {
            Child::Field(field) => field,
            Child::Entry(item) => {
                instances.insert(prefix.clone());
                item
            }
            Child::Freeform | Child::None => return,
        };
    }
}

/// Checks one declaration in `[machine.values]`: `{ type = "string" }`, with an optional `doc`.
fn machine_value(declaration: &Spanned<DeValue<'_>>, file: FileId) -> Result<(), Diagnostic> {
    let span = Span::new(file, declaration.span());
    let DeValue::Table(table) = declaration.get_ref() else {
        return Err(
            Diagnostic::error("a machine value is declared with its type")
                .label(span, "expected `{ type = \"string\" }`"),
        );
    };
    only_keys(table, &["type", "doc"], "a machine value", file)?;
    let Some(ty) = table.get("type") else {
        return Err(
            Diagnostic::error("a machine value is declared with its type")
                .label(span, "expected `type = \"string\"`"),
        );
    };
    if !matches!(ty.get_ref(), DeValue::String(name) if name == "string") {
        return Err(Diagnostic::error("machine values are strings")
            .label(Span::new(file, ty.span()), "expected `\"string\"`"));
    }
    match table.get("doc") {
        Some(doc) if !matches!(doc.get_ref(), DeValue::String(_)) => {
            Err(Diagnostic::error("a machine value's `doc` is a string")
                .label(Span::new(file, doc.span()), "expected a string"))
        }
        _ => Ok(()),
    }
}

/// Refuses keys other than `allowed` in a table such as `{ default = ... }`. `what` names the
/// table in the error.
pub(crate) fn only_keys(
    table: &DeTable<'_>,
    allowed: &[&str],
    what: &str,
    file: FileId,
) -> Result<(), Diagnostic> {
    let Some((key, _)) = table
        .iter()
        .find(|(key, _)| !allowed.contains(&key.get_ref().as_ref()))
    else {
        return Ok(());
    };
    let names: Vec<String> = allowed.iter().map(|name| format!("`{name}`")).collect();
    let verb = if names.len() == 1 {
        "belongs"
    } else {
        "belong"
    };
    Err(
        Diagnostic::error(format!("unexpected `{}` in {what}", key.get_ref())).label(
            Span::new(file, key.span()),
            format!("only {} {verb} here", names.join(" and ")),
        ),
    )
}
