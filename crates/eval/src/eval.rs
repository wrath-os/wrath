//! Merging definitions and resolving references and modules into one value per option.
//!
//! Options are resolved on demand. Before anything under a prefix is read, every module that
//! may set something under it renders, so its settings, and the map entries they create, are in
//! place. A cycle between options and modules is an error naming each step.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::rc::Rc;

use crate::diag::{self, Diagnostic};
use crate::fs::ConfigFs;
use crate::load;
use crate::module::{ConfigReader, Module, ModuleId, Output, ReadError, RenderError, Setting};
use crate::path::OptionPath;
use crate::schema::{Child, ListMerge, OptionSpec, Schema, Type};
use crate::source::{Sources, Span};
use crate::suggest::did_you_mean;
use crate::template::Part;
use crate::value::{RuntimePart, Value};
use crate::walk::{self, Definition, Level, Raw, Site, mark_instances};

/// The root of `${facts.…}` references.
const FACTS: &str = "facts";

pub struct Input<'a> {
    /// Sources so far, normally the modules' own files, so module spans resolve.
    pub sources: Sources,
    pub config: &'a dyn ConfigFs,
    /// Options declared outside modules. Module options are added to it.
    pub schema: Schema,
    pub modules: &'a [Box<dyn Module>],
    /// A table of machine facts.
    pub facts: &'a Value,
}

/// The result of evaluating a configuration: one resolved value per option that has one, and
/// what each module rendered.
#[derive(Debug)]
pub struct Evaluation {
    sources: Sources,
    options: BTreeMap<OptionPath, Resolved>,
    facts: BTreeMap<OptionPath, Value>,
    modules: Vec<Rendered>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub name: String,
    /// The module's output, without its settings, which are part of the options.
    pub output: Output,
    /// Options and tables of options the module read.
    pub reads: BTreeSet<OptionPath>,
}

/// An option's value and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub value: Value,
    /// The definitions that make up the value: one, or several that agree or merge.
    pub set_by: Vec<Contribution>,
    /// Definitions that lost to a stronger one.
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

    /// What each module rendered, in the order the modules were given.
    pub fn modules(&self) -> &[Rendered] {
        &self.modules
    }

    pub fn module_name(&self, module: ModuleId) -> &str {
        &self.modules[module.index()].name
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

pub fn evaluate(input: Input<'_>) -> Result<Evaluation, EvalError> {
    let Input {
        mut sources,
        config,
        mut schema,
        modules,
        facts,
    } = input;
    let mut diagnostics = Vec::new();
    for module in modules {
        for (path, spec) in module.options() {
            if let Err(e) = schema.declare_at(&path, spec) {
                diagnostics.push(Diagnostic::error(format!(
                    "module `{}`: {e}",
                    module.name()
                )));
            }
        }
    }
    let (files, load_diagnostics) = load::load(config, &mut sources);
    diagnostics.extend(load_diagnostics);
    let walked = walk::walk(&schema, &sources, &files);
    diagnostics.extend(walked.diagnostics);

    let mut definitions: BTreeMap<OptionPath, Vec<Rc<Definition>>> = BTreeMap::new();
    for definition in walked.definitions {
        definitions
            .entry(definition.path.clone())
            .or_default()
            .push(Rc::new(definition));
    }
    let mut resolver = Resolver {
        schema: &schema,
        modules,
        writes: modules.iter().map(|module| module.writes()).collect(),
        facts,
        // A definition that failed to load may be the one that sets a required option, so
        // missing options are only reported when everything loaded.
        report_missing: diagnostics.is_empty(),
        definitions,
        instances: walked.instances,
        state: HashMap::new(),
        modules_state: vec![ModuleState::NotRun; modules.len()],
        rendered: vec![None; modules.len()],
        stack: Vec::new(),
        options: BTreeMap::new(),
        facts_used: BTreeMap::new(),
        diagnostics: Vec::new(),
    };
    for id in 0..modules.len() {
        // Failures are reported by the resolver.
        let _ = resolver.run(ModuleId::from_index(id));
    }
    for (path, spec) in resolver.leaves_under(schema.root(), OptionPath::root()) {
        let _ = resolver.resolve(&path, &spec, None);
    }
    diagnostics.extend(resolver.diagnostics);

    if !diagnostics.is_empty() {
        return Err(EvalError {
            sources,
            diagnostics,
        });
    }
    Ok(Evaluation {
        sources,
        options: resolver.options,
        facts: resolver.facts_used,
        modules: resolver
            .rendered
            .into_iter()
            .map(|rendered| rendered.expect("every module rendered without errors"))
            .collect(),
    })
}

fn overlaps(a: &OptionPath, b: &OptionPath) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// The option, or the failure that is already reported.
type Resolution = Result<Option<Value>, Failed>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Failed;

/// Fails if any step failed. Every step runs, so each failure is reported.
fn all(steps: impl IntoIterator<Item = Result<(), Failed>>) -> Result<(), Failed> {
    steps.into_iter().fold(Ok(()), Result::and)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModuleState {
    NotRun,
    Running,
    Done(Result<(), Failed>),
}

enum State {
    InProgress,
    Done(Resolution),
}

/// A step in a chain of dependencies, for reporting cycles.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Frame {
    Option(OptionPath),
    Module(ModuleId),
}

/// What a path names, given the map entries and freeform values that exist.
enum Known<'s> {
    Leaf(Cow<'s, OptionSpec>),
    Table(Cow<'s, OptionSpec>),
    /// A map entry or freeform value that the configuration doesn't have.
    Absent,
    Unknown,
}

struct Resolver<'a> {
    schema: &'a Schema,
    modules: &'a [Box<dyn Module>],
    writes: Vec<Vec<OptionPath>>,
    facts: &'a Value,
    report_missing: bool,
    definitions: BTreeMap<OptionPath, Vec<Rc<Definition>>>,
    instances: BTreeSet<OptionPath>,
    state: HashMap<OptionPath, State>,
    modules_state: Vec<ModuleState>,
    rendered: Vec<Option<Rendered>>,
    stack: Vec<Frame>,
    options: BTreeMap<OptionPath, Resolved>,
    facts_used: BTreeMap<OptionPath, Value>,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Resolver<'a> {
    fn module_name(&self, module: ModuleId) -> &'a str {
        self.modules[module.index()].name()
    }

    fn writes(&self, module: ModuleId) -> &[OptionPath] {
        &self.writes[module.index()]
    }

    /// Renders every module that may set something under `path`.
    fn run_writers(&mut self, path: &OptionPath) -> Result<(), Failed> {
        let writers: Vec<ModuleId> = (0..self.writes.len())
            .map(ModuleId::from_index)
            .filter(|&id| self.writes(id).iter().any(|write| overlaps(write, path)))
            .collect();
        all(writers.into_iter().map(|writer| self.run(writer)))
    }

    fn run(&mut self, id: ModuleId) -> Result<(), Failed> {
        let index = id.index();
        match self.modules_state[index] {
            ModuleState::Done(result) => return result,
            ModuleState::Running => {
                self.cycle(&Frame::Module(id), None);
                return Err(Failed);
            }
            ModuleState::NotRun => {}
        }
        self.modules_state[index] = ModuleState::Running;
        self.stack.push(Frame::Module(id));
        let mut reads = BTreeSet::new();
        let modules = self.modules;
        let module = &modules[index];
        let rendered = module.render(&mut Reader {
            resolver: self,
            module: id,
            reads: &mut reads,
        });
        self.stack.pop();
        let result = match rendered {
            Ok(mut output) => {
                let settings = std::mem::take(&mut output.settings);
                let applied = self.apply(id, settings);
                self.rendered[index] = Some(Rendered {
                    name: module.name().to_owned(),
                    output,
                    reads,
                });
                applied
            }
            Err(RenderError::Failed) => Err(Failed),
            Err(RenderError::Error(diagnostic)) => {
                self.diagnostics.push(diagnostic);
                Err(Failed)
            }
        };
        self.modules_state[index] = ModuleState::Done(result);
        result
    }

    /// Adds a module's settings as definitions.
    fn apply(&mut self, id: ModuleId, settings: Vec<Setting>) -> Result<(), Failed> {
        all(settings
            .into_iter()
            .map(|setting| self.apply_setting(id, setting)))
    }

    fn apply_setting(&mut self, id: ModuleId, setting: Setting) -> Result<(), Failed> {
        let writes = self.writes(id);
        if !writes.iter().any(|write| setting.path.starts_with(write)) {
            let declared = if writes.is_empty() {
                "nothing".to_owned()
            } else {
                let writes: Vec<String> = writes.iter().map(|write| format!("`{write}`")).collect();
                writes.join(", ")
            };
            self.diagnostics.push(
                Diagnostic::error(format!(
                    "module `{}` sets `{}`, but only declares that it writes {declared}",
                    self.module_name(id),
                    setting.path,
                ))
                .label_opt(setting.at, "set here"),
            );
            return Err(Failed);
        }
        let Some(spec) = spec_at(self.schema, &setting.path) else {
            self.diagnostics.push(
                Diagnostic::error(format!(
                    "module `{}` sets unknown option `{}`",
                    self.module_name(id),
                    setting.path
                ))
                .label_opt(setting.at, "set here"),
            );
            return Err(Failed);
        };
        mark_instances(self.schema, &setting.path, &mut self.instances);
        let site = Site::Module {
            module: id,
            at: setting.at,
        };
        self.flatten(&spec, setting.path, &setting.value, site, setting.level)
    }

    /// Splits a value for a table of options into definitions of the options in it.
    fn flatten(
        &mut self,
        spec: &OptionSpec,
        path: OptionPath,
        value: &Value,
        site: Site,
        level: Level,
    ) -> Result<(), Failed> {
        match (&spec.ty, value) {
            (Type::Record(record), Value::Table(entries)) => {
                all(entries.iter().map(|(key, value)| {
                    let child = path.child(key.as_str());
                    if let Some(field) = record.fields.get(key) {
                        return self.flatten(field, child, value, site, level);
                    }
                    let mut diagnostic = Diagnostic::error(format!("unknown option `{child}`"));
                    diagnostic = self.locate(diagnostic, site, "set here");
                    if let Some(note) = did_you_mean(key, record.fields.keys()) {
                        diagnostic = diagnostic.note(note);
                    }
                    self.diagnostics.push(diagnostic);
                    Err(Failed)
                }))
            }
            (Type::Map(item), Value::Table(entries)) => all(entries.iter().map(|(key, value)| {
                let child = path.child(key.as_str());
                self.instances.insert(child.clone());
                self.flatten(item, child, value, site, level)
            })),
            (Type::Freeform, Value::Table(entries)) => {
                let freeform = OptionSpec::freeform();
                all(entries.iter().map(|(key, value)| {
                    self.flatten(&freeform, path.child(key.as_str()), value, site, level)
                }))
            }
            (Type::Record(_) | Type::Map(_), other) => {
                let diagnostic = Diagnostic::error(format!(
                    "`{path}` is a table of options, found {}",
                    other.kind()
                ));
                let diagnostic = self.locate(diagnostic, site, "set here");
                self.diagnostics.push(diagnostic);
                Err(Failed)
            }
            (
                Type::Bool
                | Type::Int { .. }
                | Type::Float
                | Type::Str
                | Type::Path
                | Type::Enum(_)
                | Type::List { .. }
                | Type::Freeform,
                value,
            ) => {
                self.definitions
                    .entry(path.clone())
                    .or_default()
                    .push(Rc::new(Definition {
                        path,
                        value: Raw::Literal(value.clone()),
                        level,
                        site,
                        reason: None,
                    }));
                Ok(())
            }
        }
    }

    /// Every option under `path` that can hold a value: scalars and lists in records, in the map
    /// entries that exist, and every value set inside freeform options.
    fn leaves_under(&self, spec: &OptionSpec, path: OptionPath) -> Vec<(OptionPath, OptionSpec)> {
        let mut out = Vec::new();
        self.collect_leaves(spec, path, &mut out);
        out
    }

    fn collect_leaves(
        &self,
        spec: &OptionSpec,
        path: OptionPath,
        out: &mut Vec<(OptionPath, OptionSpec)>,
    ) {
        match &spec.ty {
            Type::Record(record) => {
                for (name, field) in &record.fields {
                    self.collect_leaves(field, path.child(name.as_str()), out);
                }
            }
            Type::Map(item) => {
                for entry in self
                    .instances
                    .range(path.clone()..)
                    .take_while(|instance| instance.starts_with(&path))
                    .filter(|instance| instance.len() == path.len() + 1)
                {
                    self.collect_leaves(item, entry.clone(), out);
                }
            }
            Type::Freeform => {
                let start = out.len();
                out.extend(
                    self.defined_under(&path)
                        .map(|leaf| (leaf.clone(), OptionSpec::freeform())),
                );
                if out.len() == start && spec.default.is_some() {
                    out.push((path, spec.clone()));
                }
            }
            Type::Bool
            | Type::Int { .. }
            | Type::Float
            | Type::Str
            | Type::Path
            | Type::Enum(_)
            | Type::List { .. } => out.push((path, spec.clone())),
        }
    }

    /// Options defined at or under `path`, in order.
    fn defined_under<'s>(&'s self, path: &'s OptionPath) -> impl Iterator<Item = &'s OptionPath> {
        self.definitions
            .range(path.clone()..)
            .map(|(defined, _)| defined)
            .take_while(move |defined| defined.starts_with(path))
    }

    /// What `path` names. Writers under it must have run.
    fn known(&self, path: &OptionPath) -> Known<'a> {
        let mut spec = self.schema.root();
        let mut prefix = OptionPath::root();
        for segment in path.segments() {
            prefix = prefix.child(segment.as_str());
            spec = match spec.ty.child(segment) {
                Child::Field(field) => field,
                Child::Entry(item) => {
                    if !self.instances.contains(&prefix) {
                        return Known::Absent;
                    }
                    item
                }
                Child::Freeform => {
                    return if self.definitions.contains_key(path) {
                        Known::Leaf(Cow::Owned(OptionSpec::freeform()))
                    } else if self.defined_under(path).next().is_some() {
                        Known::Table(Cow::Owned(OptionSpec::freeform()))
                    } else {
                        Known::Absent
                    };
                }
                Child::None => return Known::Unknown,
            };
        }
        if spec.ty.is_table() {
            Known::Table(Cow::Borrowed(spec))
        } else {
            Known::Leaf(Cow::Borrowed(spec))
        }
    }

    /// Reads an option or a table of options for a module.
    fn module_read(
        &mut self,
        module: ModuleId,
        path: &OptionPath,
        at: Option<Span>,
        reads: &mut BTreeSet<OptionPath>,
    ) -> Result<Option<Value>, ReadError> {
        let name = self.module_name(module);
        if let Some(write) = self
            .writes(module)
            .iter()
            .find(|write| overlaps(write, path))
        {
            return Err(ReadError::Invalid(format!(
                "module `{name}` sets `{write}`, so it cannot read `{path}`"
            )));
        }
        self.run_writers(path).map_err(|Failed| ReadError::Failed)?;
        reads.insert(path.clone());
        match self.known(path) {
            Known::Leaf(spec) => self
                .resolve(path, &spec, at)
                .map_err(|Failed| ReadError::Failed),
            Known::Table(spec) => {
                let mut table = BTreeMap::new();
                let mut failed = false;
                for (leaf, spec) in self.leaves_under(&spec, path.clone()) {
                    match self.resolve(&leaf, &spec, at) {
                        Ok(Some(value)) => {
                            insert_nested(&mut table, &leaf.segments()[path.len()..], value)
                        }
                        Ok(None) => {}
                        Err(Failed) => failed = true,
                    }
                }
                if failed {
                    Err(ReadError::Failed)
                } else {
                    Ok(Some(Value::Table(table)))
                }
            }
            Known::Absent => Ok(None),
            Known::Unknown => {
                let mut message = format!("unknown option `{path}`");
                if let Some(note) = self.suggest(path) {
                    message = format!("{message}; {}", note.trim_start_matches("help: "));
                }
                Err(ReadError::Invalid(message))
            }
        }
    }

    fn suggest(&self, path: &OptionPath) -> Option<String> {
        let names: Vec<String> = self
            .leaves_under(self.schema.root(), OptionPath::root())
            .into_iter()
            .map(|(leaf, _)| leaf.to_string())
            .collect();
        did_you_mean(&path.to_string(), &names)
    }

    /// Resolves one option. `at` is the reference or read that needs it, for errors.
    fn resolve(&mut self, path: &OptionPath, spec: &OptionSpec, at: Option<Span>) -> Resolution {
        match self.state.get(path) {
            Some(State::Done(resolution)) => return resolution.clone(),
            Some(State::InProgress) => {
                self.cycle(&Frame::Option(path.clone()), at);
                return Err(Failed);
            }
            None => {}
        }
        self.state.insert(path.clone(), State::InProgress);
        self.stack.push(Frame::Option(path.clone()));
        let resolution = self.resolve_definitions(path, spec, at);
        self.stack.pop();
        self.state
            .insert(path.clone(), State::Done(resolution.clone()));
        resolution
    }

    fn resolve_definitions(
        &mut self,
        path: &OptionPath,
        spec: &OptionSpec,
        at: Option<Span>,
    ) -> Resolution {
        self.run_writers(path)?;
        let mut definitions = self.definitions.get(path).cloned().unwrap_or_default();
        if let Some(default) = &spec.default {
            definitions.push(Rc::new(Definition {
                path: path.clone(),
                value: Raw::Literal(default.clone()),
                level: Level::Default,
                site: Site::Default,
                reason: None,
            }));
        }
        if definitions.is_empty() {
            if spec.optional {
                return Ok(None);
            }
            if self.report_missing {
                self.diagnostics.push(
                    Diagnostic::error(format!("`{path}` is not set"))
                        .label_opt(at, "needed here")
                        .note("help: this option has no default, so the configuration must set it"),
                );
            }
            return Err(Failed);
        }

        // A declared default only applies when nothing else sets the option.
        let rank =
            |definition: &Definition| (definition.level, !matches!(definition.site, Site::Default));
        let top = definitions.iter().map(|definition| rank(definition)).max();
        let mut set_by = Vec::new();
        let mut overridden = Vec::new();
        let mut uses = Vec::new();
        let mut failed = false;
        for definition in &definitions {
            let mut definition_uses = Vec::new();
            let Some(value) = self.eval(&definition.value, &mut definition_uses) else {
                failed = true;
                continue;
            };
            if let Err(message) = check(path, spec, &value) {
                let diagnostic = Diagnostic::error(format!("invalid value for `{path}`"));
                let diagnostic = self.locate(diagnostic, definition.site, message);
                self.diagnostics.push(diagnostic);
                failed = true;
                continue;
            }
            let contribution = Contribution {
                site: definition.site,
                level: definition.level,
                reason: definition.reason.clone(),
                value,
            };
            if Some(rank(definition)) == top {
                uses.extend(definition_uses);
                set_by.push(contribution);
            } else {
                overridden.push(contribution);
            }
        }
        if failed {
            return Err(Failed);
        }
        let value = self.merge(path, spec, &set_by).ok_or(Failed)?;
        self.options.insert(
            path.clone(),
            Resolved {
                value: value.clone(),
                set_by,
                overridden,
                uses,
            },
        );
        Ok(Some(value))
    }

    /// Points a diagnostic at a definition's site: its span when there is one, otherwise a note
    /// naming the module.
    fn locate(&self, diagnostic: Diagnostic, site: Site, message: impl Into<String>) -> Diagnostic {
        let message = message.into();
        match site {
            Site::Config(span) => diagnostic.label(span, message),
            Site::Module {
                module,
                at: Some(span),
            } => diagnostic.label(
                span,
                format!("{message} (module `{}`)", self.module_name(module)),
            ),
            Site::Module { module, at: None } => {
                diagnostic.note(format!("module `{}`: {message}", self.module_name(module)))
            }
            Site::Default => diagnostic.note(format!("the declared default: {message}")),
        }
    }

    fn cycle(&mut self, closing: &Frame, at: Option<Span>) {
        let start = self
            .stack
            .iter()
            .position(|frame| frame == closing)
            .unwrap_or(0);
        let steps: Vec<String> = self.stack[start..]
            .iter()
            .chain(std::iter::once(closing))
            .map(|frame| match frame {
                Frame::Option(path) => path.to_string(),
                Frame::Module(module) => format!("module `{}`", self.module_name(*module)),
            })
            .collect();
        self.diagnostics.push(
            Diagnostic::error(format!("cycle: {}", steps.join(" -> ")))
                .label_opt(at, "this closes the cycle"),
        );
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
            diagnostic = self.locate(
                diagnostic,
                contribution.site,
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
            // Every item is evaluated, even after one fails, so each failure is reported.
            Raw::List(items) => {
                let values: Vec<Option<Value>> =
                    items.iter().map(|item| self.eval(item, uses)).collect();
                values.into_iter().collect::<Option<_>>().map(Value::List)
            }
            Raw::Table(entries) => {
                let values: Vec<Option<(String, Value)>> = entries
                    .iter()
                    .map(|(key, item)| Some((key.clone(), self.eval(item, uses)?)))
                    .collect();
                values.into_iter().collect::<Option<_>>().map(Value::Table)
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
        self.run_writers(path).ok()?;
        let diagnostic = match self.known(path) {
            Known::Leaf(spec) => match self.resolve(path, &spec, Some(span)) {
                Ok(Some(value)) => {
                    uses.push(Use {
                        path: path.clone(),
                        span,
                    });
                    return Some(value);
                }
                Ok(None) => Diagnostic::error(format!("`{path}` is not set"))
                    .label(span, "referenced here")
                    .note("help: set it, or stop referencing it"),
                Err(Failed) => return None,
            },
            Known::Table(_) => Diagnostic::error(format!("`{path}` is a table of options"))
                .label(span, "a reference must name a single option"),
            Known::Absent | Known::Unknown => {
                let diagnostic = Diagnostic::error(format!("unknown option `{path}`"))
                    .label(span, "referenced here");
                match self.suggest(path) {
                    Some(note) => diagnostic.note(note),
                    None => diagnostic,
                }
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

/// A module's view of the configuration while it renders.
struct Reader<'r, 'a> {
    resolver: &'r mut Resolver<'a>,
    module: ModuleId,
    reads: &'r mut BTreeSet<OptionPath>,
}

impl ConfigReader for Reader<'_, '_> {
    fn read(&mut self, path: &OptionPath, at: Option<Span>) -> Result<Option<Value>, ReadError> {
        self.resolver.module_read(self.module, path, at, self.reads)
    }
}

/// The spec of the option at `path`. Inside freeform options every path is a freeform option.
fn spec_at<'s>(schema: &'s Schema, path: &OptionPath) -> Option<Cow<'s, OptionSpec>> {
    let mut spec = schema.root();
    for segment in path.segments() {
        spec = match spec.ty.child(segment) {
            Child::Field(spec) | Child::Entry(spec) => spec,
            Child::Freeform => return Some(Cow::Owned(OptionSpec::freeform())),
            Child::None => return None,
        };
    }
    Some(Cow::Borrowed(spec))
}

fn insert_nested(table: &mut BTreeMap<String, Value>, segments: &[String], value: Value) {
    match segments {
        [] => {}
        [last] => {
            table.insert(last.clone(), value);
        }
        [first, rest @ ..] => {
            let entry = table
                .entry(first.clone())
                .or_insert_with(|| Value::Table(BTreeMap::new()));
            if let Value::Table(inner) = entry {
                insert_nested(inner, rest, value);
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
