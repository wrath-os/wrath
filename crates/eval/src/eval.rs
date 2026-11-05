//! Merging definitions and resolving references into one value per option.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use crate::diag::{self, Diagnostic};
use crate::fs::ConfigFs;
use crate::load;
use crate::path::OptionPath;
use crate::schema::{ListMerge, OptionSpec, Schema, Type};
use crate::source::{Sources, Span};
use crate::suggest::did_you_mean;
use crate::template::Part;
use crate::value::{RuntimePart, Value};
use crate::walk::{self, Definition, Level, Raw, Site};

/// The root of `${facts.…}` references.
const FACTS: &str = "facts";

/// The result of evaluating a configuration: one resolved value per option that has one.
#[derive(Debug)]
pub struct Evaluation {
    sources: Sources,
    options: BTreeMap<OptionPath, Resolved>,
    facts: BTreeMap<OptionPath, Value>,
}

/// An option's value and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub value: Value,
    /// The definitions that make up the value: one, or several that agree or merge.
    pub set_by: Vec<Contribution>,
    /// Definitions at a lower level that lost.
    pub overridden: Vec<Contribution>,
    /// Options and facts the value's references read.
    pub uses: Vec<Use>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Contribution {
    pub site: Site,
    pub level: Level,
    pub reason: Option<String>,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Use {
    pub path: OptionPath,
    pub span: Span,
}

impl Evaluation {
    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    pub fn options(&self) -> &BTreeMap<OptionPath, Resolved> {
        &self.options
    }

    /// The value of an option, by its dotted path.
    pub fn get(&self, path: &str) -> Option<&Value> {
        let path = OptionPath::parse(path).ok()?;
        self.options.get(&path).map(|resolved| &resolved.value)
    }

    /// Machine facts the configuration read, by path under `facts`.
    pub fn facts_used(&self) -> &BTreeMap<OptionPath, Value> {
        &self.facts
    }
}

#[derive(Debug)]
pub struct EvalError {
    pub sources: Sources,
    pub diagnostics: Vec<Diagnostic>,
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&diag::render(&self.diagnostics, &self.sources))
    }
}

impl std::error::Error for EvalError {}

/// Evaluates the configuration in `fs` against `schema`. `facts` is a table of machine facts.
pub fn evaluate(
    fs: &dyn ConfigFs,
    schema: &Schema,
    facts: &Value,
) -> Result<Evaluation, EvalError> {
    let mut sources = Sources::default();
    let (files, mut diagnostics) = load::load(fs, &mut sources);
    let walked = walk::walk(schema, &sources, &files);
    diagnostics.extend(walked.diagnostics);

    let defined: BTreeSet<OptionPath> = walked
        .definitions
        .iter()
        .map(|definition| definition.path.clone())
        .collect();
    let mut specs = BTreeMap::new();
    collect_leaves(
        schema.root(),
        OptionPath::root(),
        &defined,
        &walked.instances,
        &mut specs,
    );

    let mut grouped: BTreeMap<OptionPath, Vec<Definition>> = BTreeMap::new();
    for definition in walked.definitions {
        grouped
            .entry(definition.path.clone())
            .or_default()
            .push(definition);
    }
    // A definition that failed to load may be the one that sets a required option, so missing
    // options are only reported when everything loaded.
    let report_missing = diagnostics.is_empty();
    let mut leaves = BTreeMap::new();
    for (path, spec) in specs {
        let mut definitions = grouped.remove(&path).unwrap_or_default();
        if let Some(default) = &spec.default {
            definitions.push(Definition {
                path: path.clone(),
                value: Raw::Literal(default.clone()),
                level: Level::Default,
                site: Site::Default,
                reason: None,
            });
        }
        if definitions.is_empty() {
            if report_missing && !spec.optional {
                diagnostics
                    .push(Diagnostic::error(format!("`{path}` is not set")).note(
                        "help: this option has no default, so the configuration must set it",
                    ));
            }
            continue;
        }
        leaves.insert(path, Leaf { spec, definitions });
    }

    let mut resolver = Resolver {
        leaves: &leaves,
        facts,
        state: HashMap::new(),
        stack: Vec::new(),
        options: BTreeMap::new(),
        facts_used: BTreeMap::new(),
        diagnostics: Vec::new(),
    };
    for path in leaves.keys() {
        resolver.resolve(path);
    }
    diagnostics.extend(resolver.diagnostics);

    if diagnostics.is_empty() {
        Ok(Evaluation {
            sources,
            options: resolver.options,
            facts: resolver.facts_used,
        })
    } else {
        Err(EvalError {
            sources,
            diagnostics,
        })
    }
}

/// Finds every option that can hold a value: scalars and lists in records, in the map entries the
/// configuration names, and every value set inside freeform options.
fn collect_leaves(
    spec: &OptionSpec,
    path: OptionPath,
    defined: &BTreeSet<OptionPath>,
    instances: &BTreeSet<OptionPath>,
    out: &mut BTreeMap<OptionPath, OptionSpec>,
) {
    let under = |set: &'_ BTreeSet<OptionPath>, path: &OptionPath| {
        set.range(path.clone()..)
            .take_while(|candidate| candidate.starts_with(path))
            .cloned()
            .collect::<Vec<_>>()
    };
    match &spec.ty {
        Type::Record(record) => {
            for (name, field) in &record.fields {
                collect_leaves(field, path.child(name.as_str()), defined, instances, out);
            }
        }
        Type::Map(item) => {
            for instance in under(instances, &path) {
                if instance.len() == path.len() + 1 {
                    collect_leaves(item, instance, defined, instances, out);
                }
            }
        }
        Type::Freeform => {
            let set = under(defined, &path);
            if set.is_empty() {
                if spec.default.is_some() {
                    out.insert(path, spec.clone());
                }
            } else {
                out.extend(set.into_iter().map(|leaf| (leaf, OptionSpec::freeform())));
            }
        }
        Type::Bool
        | Type::Int { .. }
        | Type::Float
        | Type::Str
        | Type::Path
        | Type::Enum(_)
        | Type::List { .. } => {
            out.insert(path, spec.clone());
        }
    }
}

struct Leaf {
    spec: OptionSpec,
    definitions: Vec<Definition>,
}

enum State {
    InProgress,
    /// `None` when the option failed; the failure is already reported.
    Done(Option<Value>),
}

struct Resolver<'a> {
    leaves: &'a BTreeMap<OptionPath, Leaf>,
    facts: &'a Value,
    state: HashMap<OptionPath, State>,
    /// Options being resolved, outermost first, for reporting cycles.
    stack: Vec<OptionPath>,
    options: BTreeMap<OptionPath, Resolved>,
    facts_used: BTreeMap<OptionPath, Value>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Resolver<'a> {
    fn resolve(&mut self, path: &OptionPath) -> Option<Value> {
        if let Some(State::Done(value)) = self.state.get(path) {
            return value.clone();
        }
        let leaf: &'a Leaf = &self.leaves[path];
        self.state.insert(path.clone(), State::InProgress);
        self.stack.push(path.clone());

        let top = leaf.definitions.iter().map(|d| d.level).max();
        let mut set_by = Vec::new();
        let mut overridden = Vec::new();
        let mut uses = Vec::new();
        let mut failed = false;
        for definition in &leaf.definitions {
            let mut definition_uses = Vec::new();
            let Some(value) = self.eval(&definition.value, &mut definition_uses) else {
                failed = true;
                continue;
            };
            if let Err(message) = check(path, &leaf.spec, &value) {
                self.diagnostics.push(
                    Diagnostic::error(format!("invalid value for `{path}`"))
                        .label_opt(definition.site.span(), message),
                );
                failed = true;
                continue;
            }
            let contribution = Contribution {
                site: definition.site,
                level: definition.level,
                reason: definition.reason.clone(),
                value,
            };
            if Some(definition.level) == top {
                uses.extend(definition_uses);
                set_by.push(contribution);
            } else {
                overridden.push(contribution);
            }
        }
        let value = if failed {
            None
        } else {
            self.merge(path, &leaf.spec, &set_by)
        };

        self.stack.pop();
        self.state.insert(path.clone(), State::Done(value.clone()));
        if let Some(value) = &value {
            self.options.insert(
                path.clone(),
                Resolved {
                    value: value.clone(),
                    set_by,
                    overridden,
                    uses,
                },
            );
        }
        value
    }

    /// Combines the definitions at the winning level.
    fn merge(
        &mut self,
        path: &OptionPath,
        spec: &OptionSpec,
        set_by: &[Contribution],
    ) -> Option<Value> {
        if let Type::List {
            merge: ListMerge::Set,
            ..
        } = spec.ty
        {
            let mut items: Vec<(String, Value)> = set_by
                .iter()
                .flat_map(|contribution| match &contribution.value {
                    Value::List(items) => items.clone(),
                    other => vec![other.clone()],
                })
                .map(|item| (item.to_string(), item))
                .collect();
            items.sort_by(|a, b| a.0.cmp(&b.0));
            items.dedup_by(|a, b| a.0 == b.0);
            return Some(Value::List(
                items.into_iter().map(|(_, item)| item).collect(),
            ));
        }
        let first = &set_by.first()?.value;
        if set_by
            .iter()
            .all(|contribution| &contribution.value == first)
        {
            return Some(first.clone());
        }
        let mut diagnostic = Diagnostic::error(format!("conflicting values for `{path}`"));
        for contribution in set_by {
            diagnostic = diagnostic.label_opt(
                contribution.site.span(),
                format!("set to {} here", contribution.value),
            );
        }
        self.diagnostics.push(diagnostic.note(
            "help: remove all but one, or force one with `{ value = ..., force = \"reason\" }`",
        ));
        None
    }

    fn eval(&mut self, raw: &Raw, uses: &mut Vec<Use>) -> Option<Value> {
        match raw {
            Raw::Literal(value) => Some(value.clone()),
            Raw::Secret(name) => Some(Value::Runtime(vec![RuntimePart::Secret(name.clone())])),
            Raw::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                let mut failed = false;
                for item in items {
                    match self.eval(item, uses) {
                        Some(value) => values.push(value),
                        None => failed = true,
                    }
                }
                (!failed).then_some(Value::List(values))
            }
            Raw::Table(entries) => {
                let mut values = BTreeMap::new();
                let mut failed = false;
                for (key, item) in entries {
                    match self.eval(item, uses) {
                        Some(value) => {
                            values.insert(key.clone(), value);
                        }
                        None => failed = true,
                    }
                }
                (!failed).then_some(Value::Table(values))
            }
            Raw::Template(parts) => self.template(parts, uses),
        }
    }

    /// A string with references. A string that is only a reference takes the referenced value
    /// with its type; otherwise every reference must be a scalar and the result is a string.
    fn template(&mut self, parts: &[Part], uses: &mut Vec<Use>) -> Option<Value> {
        if let [Part::Ref { path, span }] = parts {
            return self.reference(path, *span, uses);
        }
        let mut out: Vec<RuntimePart> = Vec::new();
        let push_text = |out: &mut Vec<RuntimePart>, text: &str| match out.last_mut() {
            Some(RuntimePart::Text(last)) => last.push_str(text),
            _ => out.push(RuntimePart::Text(text.to_owned())),
        };
        let mut failed = false;
        for part in parts {
            match part {
                Part::Text(text) => push_text(&mut out, text),
                Part::Ref { path, span } => match self.reference(path, *span, uses) {
                    Some(Value::Str(text)) => push_text(&mut out, &text),
                    Some(Value::Int(int)) => push_text(&mut out, &int.to_string()),
                    Some(Value::Float(float)) => push_text(&mut out, &float.to_string()),
                    Some(Value::Bool(b)) => push_text(&mut out, &b.to_string()),
                    Some(Value::Runtime(parts)) => {
                        for part in parts {
                            match part {
                                RuntimePart::Text(text) => push_text(&mut out, &text),
                                secret @ RuntimePart::Secret(_) => out.push(secret),
                            }
                        }
                    }
                    Some(value @ (Value::List(_) | Value::Table(_))) => {
                        self.diagnostics.push(
                            Diagnostic::error(format!("cannot put {} into a string", value.kind()))
                                .label(*span, format!("`{path}` is {value}"))
                                .note("help: a reference that is the whole string keeps its type"),
                        );
                        failed = true;
                    }
                    None => failed = true,
                },
            }
        }
        if failed {
            return None;
        }
        Some(match out.as_slice() {
            [] => Value::Str(String::new()),
            [RuntimePart::Text(text)] => Value::Str(text.clone()),
            _ => Value::Runtime(out),
        })
    }

    fn reference(&mut self, path: &OptionPath, span: Span, uses: &mut Vec<Use>) -> Option<Value> {
        if let Some((FACTS, fact)) = path
            .segments()
            .split_first()
            .map(|(first, rest)| (first.as_str(), rest))
        {
            return self.fact(path, fact, span, uses);
        }
        if matches!(self.state.get(path), Some(State::InProgress)) {
            let start = self
                .stack
                .iter()
                .position(|candidate| candidate == path)
                .unwrap_or(0);
            let cycle: Vec<String> = self.stack[start..]
                .iter()
                .chain(std::iter::once(path))
                .map(ToString::to_string)
                .collect();
            self.diagnostics.push(
                Diagnostic::error(format!("reference cycle: {}", cycle.join(" -> ")))
                    .label(span, "this reference closes the cycle"),
            );
            return None;
        }
        if self.leaves.contains_key(path) {
            let value = self.resolve(path)?;
            uses.push(Use {
                path: path.clone(),
                span,
            });
            return Some(value);
        }
        let diagnostic = if self.leaves.keys().any(|leaf| leaf.starts_with(path)) {
            Diagnostic::error(format!("`{path}` is a table of options"))
                .label(span, "a reference must name a single option")
        } else {
            let names: Vec<String> = self.leaves.keys().map(ToString::to_string).collect();
            let diagnostic = Diagnostic::error(format!("unknown option `{path}`"))
                .label(span, "referenced here");
            match did_you_mean(&path.to_string(), &names) {
                Some(note) => diagnostic.note(note),
                None => diagnostic,
            }
        };
        self.diagnostics.push(diagnostic);
        None
    }

    fn fact(
        &mut self,
        path: &OptionPath,
        fact: &[String],
        span: Span,
        uses: &mut Vec<Use>,
    ) -> Option<Value> {
        let found = fact
            .iter()
            .try_fold(self.facts, |value, segment| match value {
                Value::Table(entries) => entries.get(segment),
                _ => None,
            });
        match found {
            Some(value) => {
                self.facts_used.insert(path.clone(), value.clone());
                uses.push(Use {
                    path: path.clone(),
                    span,
                });
                Some(value.clone())
            }
            None => {
                self.diagnostics.push(
                    Diagnostic::error(format!("unknown machine fact `{path}`"))
                        .label(span, "referenced here"),
                );
                None
            }
        }
    }
}

fn check(path: &OptionPath, spec: &OptionSpec, value: &Value) -> Result<(), String> {
    spec.ty.check(value)?;
    if value.is_runtime() && !spec.runtime {
        return Err(format!(
            "`{path}` is fixed when the system is built, so it cannot hold a value only known at \
             service start"
        ));
    }
    Ok(())
}
