//! Merging definitions and resolving references and modules into one value per option.
//!
//! Options are resolved on demand. Before anything under a prefix is read, every module that
//! may set something under it renders, so its settings, and the map entries they create, are in
//! place. A cycle between options and modules is an error naming each step.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::rc::Rc;

use crate::caches::DeclaredCache;
use crate::diag::{self, Diagnostic};
use crate::fs::ConfigFs;
use crate::load::{self, ConfigKind, DeclaredSource};
use crate::machines::{self, Machine};
use crate::module::{
    ConfigReader, HOME_SECRETS_DIR, Module, ModuleId, Output, ReadError, RenderError, SECRETS_DIR,
    Setting, WRATH_SOURCE, home_path, package_source,
};
use crate::ordered::{self, OrderError};
use crate::path::OptionPath;
use crate::platform::Platform;
use crate::requires::Requirement;
use crate::schema::{Child, EVERY, ListMerge, OptionSpec, Schema, Type};
use crate::source::{FileId, Sources, Span};
use crate::suggest::{closest, did_you_mean};
use crate::template::Part;
use crate::value::{RuntimePart, Value};
use crate::walk::{self, Action, Definition, Level, MACHINE, REMOVE, Raw, Site, mark_instances};
use crate::writers::{Entry, Writers};

/// The roots of references to machine values, with [`MACHINE`]: `${facts.cpu.count}`,
/// `${machine.name}`.
const FACTS: &str = "facts";

/// The machine values a configuration can refer to: facts the machine detects, and values it's
/// given. They're filled in on the machine when it switches, so the build doesn't depend on them.
pub const MACHINE_VALUES: [&str; 2] = ["facts.cpu.count", "machine.name"];

/// References to a secret's contents start with this, as `${secret.api-token}`.
const SECRET: &str = "secret";

pub struct Input<'a> {
    /// Sources so far, normally the modules' own files, so module spans resolve.
    pub sources: Sources,
    pub config: &'a dyn ConfigFs,
    pub kind: ConfigKind,
    /// The user a home configuration belongs to.
    pub user: Option<&'a str>,
    /// The role to evaluate, for a configuration with `roles/`.
    pub role: Option<&'a str>,
    /// Options declared outside modules. Module options are added to it.
    pub schema: Schema,
    pub modules: &'a [Box<dyn Module>],
}

/// The result of evaluating a configuration: one resolved value per option that has one, and
/// what each module rendered.
#[derive(Debug)]
pub struct Evaluation {
    sources: Sources,
    /// The files the configuration starts from, as opposed to files only reached through an
    /// include.
    starting_files: HashSet<FileId>,
    options: BTreeMap<OptionPath, Resolved>,
    machine_values: BTreeSet<OptionPath>,
    declared: BTreeMap<String, Span>,
    machines: BTreeMap<String, Machine>,
    requirements: Vec<Requirement>,
    declared_sources: BTreeMap<String, DeclaredSource>,
    declared_caches: BTreeMap<String, DeclaredCache>,
    modules: Vec<Rendered>,
    secret_files: BTreeMap<String, SecretFile>,
    patches: BTreeMap<String, String>,
    platform: Platform,
}

/// The encrypted file of a secret as read when evaluating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretFile {
    /// Where the file is, as the configuration's files are named.
    pub source: String,
    /// The BLAKE3 hash of the encrypted contents, in hex.
    pub hash: String,
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
    /// Removals of items from a set or ordered list.
    pub removed_by: Vec<Contribution>,
    /// Options and machine values the value's references read.
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
}

impl Evaluation {
    pub fn sources(&self) -> &Sources {
        &self.sources
    }

    /// Whether the configuration starts from `file`, rather than only reaching it through an
    /// include. Commands edit starting files and leave shared ones alone.
    pub fn is_starting_file(&self, file: FileId) -> bool {
        self.starting_files.contains(&file)
    }

    pub fn options(&self) -> &BTreeMap<OptionPath, Resolved> {
        &self.options
    }

    /// The value of an option, by its dotted path.
    pub fn get(&self, path: &str) -> Option<&Value> {
        let path = OptionPath::parse(path).ok()?;
        self.options.get(&path).map(|resolved| &resolved.value)
    }

    /// The machine values the configuration refers to, such as `facts.cpu.count`.
    pub fn machine_values_used(&self) -> &BTreeSet<OptionPath> {
        &self.machine_values
    }

    /// Where packages and included files come from, by name, as `[sources]` declares them.
    pub fn declared_sources(&self) -> &BTreeMap<String, DeclaredSource> {
        &self.declared_sources
    }

    /// The caches to fetch builds from, by name, as `[caches]` declares them.
    pub fn declared_caches(&self) -> &BTreeMap<String, DeclaredCache> {
        &self.declared_caches
    }

    /// What a machine needs to run the configuration, from `[machine.requires]`.
    pub fn requirements(&self) -> &[Requirement] {
        &self.requirements
    }

    /// The machines `machines.toml` names, with their roles and values.
    pub fn machines(&self) -> &BTreeMap<String, Machine> {
        &self.machines
    }

    /// The values each machine is given, declared in `[machine.values]`, and where.
    pub fn machine_values_declared(&self) -> &BTreeMap<String, Span> {
        &self.declared
    }

    /// What each module rendered, in the order the modules were given.
    pub fn modules(&self) -> &[Rendered] {
        &self.modules
    }

    pub fn module_name(&self, module: ModuleId) -> &str {
        &self.modules[module.index()].name
    }

    /// The encrypted file of each secret, by name.
    pub fn secret_files(&self) -> &BTreeMap<String, SecretFile> {
        &self.secret_files
    }

    /// The text of each patch builds use, by its path in the configuration.
    pub fn patches(&self) -> &BTreeMap<String, String> {
        &self.patches
    }

    /// The platform the configuration builds on.
    pub fn platform(&self) -> &Platform {
        &self.platform
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
        kind,
        user,
        role,
        mut schema,
        modules,
    } = input;
    let mut diagnostics = Vec::new();
    // Options in every entry of a map come last, so the map is declared whatever the order.
    let mut options: Vec<(&dyn Module, OptionPath, OptionSpec)> = modules
        .iter()
        .flat_map(|module| {
            module
                .options()
                .into_iter()
                .map(move |(path, spec)| (module.as_ref(), path, spec))
        })
        .collect();
    options.sort_by_key(|(_, path, _)| path.segments().iter().any(|segment| segment == EVERY));
    for (module, path, spec) in options {
        if let Err(e) = schema.declare_at(&path, spec) {
            diagnostics.push(Diagnostic::error(format!(
                "module `{}`: {e}",
                module.name()
            )));
        }
    }
    diagnostics.extend(undeclared_writes(&schema, modules));
    let load::Loaded {
        files,
        diagnostics: load_diagnostics,
        role,
        sources: declared_sources,
        caches: declared_caches,
        platform,
    } = load::load(config, &mut sources, kind, user, role);
    diagnostics.extend(load_diagnostics);
    let walked = walk::walk(&schema, &sources, &files, kind == ConfigKind::Home);
    let starting_files = files
        .iter()
        .filter(|loaded| loaded.start)
        .map(|loaded| loaded.file)
        .collect();
    diagnostics.extend(walked.diagnostics);
    let (machines, machine_diagnostics) = match kind {
        ConfigKind::System => machines::machines(
            config,
            &mut sources,
            &load::roles(config).unwrap_or_default(),
            role.as_deref(),
            &walked.machine_values,
        ),
        ConfigKind::Project | ConfigKind::Home => (BTreeMap::new(), Vec::new()),
    };
    diagnostics.extend(machine_diagnostics);

    let ids = match config.read(crate::LOCK_FILE) {
        Ok(text) => crate::Ids::from_lock(&text).unwrap_or_else(|e| {
            diagnostics.push(Diagnostic::error(format!("{}: {e}", crate::LOCK_FILE)));
            crate::Ids::default()
        }),
        Err(_) => crate::Ids::default(),
    };
    let mut definitions: BTreeMap<OptionPath, Vec<Rc<Definition>>> = BTreeMap::new();
    for definition in walked.definitions {
        definitions
            .entry(definition.path.clone())
            .or_default()
            .push(Rc::new(definition));
    }
    let writes: Vec<Vec<OptionPath>> = modules.iter().map(|module| module.writes()).collect();
    let writers = Writers::new(&writes);
    let mut resolver = Resolver {
        schema: &schema,
        modules,
        settled: vec![None; writers.len()],
        writers,
        writes,
        // A definition that failed to load may be the one that sets a required option, so
        // missing options are only reported when everything loaded.
        report_missing: diagnostics.is_empty(),
        definitions,
        included_by: files
            .iter()
            .map(|loaded| {
                let includers = loaded.inclusions.iter().map(|inclusion| inclusion.at.file);
                (loaded.file, includers.collect())
            })
            .collect(),
        instances: walked.instances,
        state: HashMap::new(),
        modules_state: vec![ModuleState::NotRun; modules.len()],
        rendered: vec![None; modules.len()],
        stack: Vec::new(),
        options: BTreeMap::new(),
        machine_values: BTreeSet::new(),
        declared: walked.machine_values,
        secrets_dir: match kind {
            ConfigKind::Home => HOME_SECRETS_DIR,
            ConfigKind::System | ConfigKind::Project => SECRETS_DIR,
        },
        ids,
        config,
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
    if diagnostics.is_empty() {
        diagnostics.extend(shared_outputs(&resolver.rendered));
        diagnostics.extend(bad_triggers(&resolver.rendered));
        diagnostics.extend(unknown_sources(&resolver.rendered, &declared_sources));
        diagnostics.extend(undeclared_secrets(&resolver.rendered, &resolver.options));
    }
    let patches = if diagnostics.is_empty() {
        read_patches(&resolver.rendered, config, &mut diagnostics)
    } else {
        BTreeMap::new()
    };
    let secret_files = if diagnostics.is_empty() {
        read_secrets(&resolver.rendered, config, &mut diagnostics)
    } else {
        BTreeMap::new()
    };

    if !diagnostics.is_empty() {
        return Err(EvalError {
            sources,
            diagnostics,
        });
    }
    Ok(Evaluation {
        sources,
        starting_files,
        options: resolver.options,
        machine_values: resolver.machine_values,
        declared: resolver.declared,
        machines,
        requirements: walked.requirements,
        declared_sources,
        declared_caches,
        modules: resolver
            .rendered
            .into_iter()
            .map(|rendered| rendered.expect("every module rendered without errors"))
            .collect(),
        secret_files,
        patches,
        platform,
    })
}

/// Options modules say they write that no module declares, such as a firewall port when the
/// platform leaves out the firewall. What they'd write would go nowhere, so it's an error. A path
/// inside a freeform option is declared by it.
fn undeclared_writes(schema: &Schema, modules: &[Box<dyn Module>]) -> Vec<Diagnostic> {
    let declared = |path: &OptionPath| {
        let mut prefix = OptionPath::root();
        for segment in path.segments() {
            prefix = prefix.child(segment.as_str());
            match schema.lookup(&prefix).map(|spec| &spec.ty) {
                None => return false,
                Some(Type::Freeform) => return true,
                Some(_) => {}
            }
        }
        true
    };
    modules
        .iter()
        .flat_map(|module| {
            module
                .writes()
                .into_iter()
                .filter(|path| !declared(path))
                .map(move |path| {
                    Diagnostic::error(format!(
                        "module `{}` writes `{path}`, which no module declares",
                        module.name()
                    ))
                    .note(
                        "help: the module that declares it is missing; add it to the platform, \
                         or leave out the module that writes it",
                    )
                })
        })
        .collect()
}

/// References to secrets that no module declares.
fn undeclared_secrets(
    rendered: &[Option<Rendered>],
    options: &BTreeMap<OptionPath, Resolved>,
) -> Vec<Diagnostic> {
    let declared: BTreeSet<&String> = rendered
        .iter()
        .flatten()
        .flat_map(|rendered| rendered.output.secrets.keys())
        .collect();
    let mut diagnostics = Vec::new();
    for (path, resolved) in options {
        for contribution in &resolved.set_by {
            for name in secret_names(&contribution.value) {
                if declared.contains(name) {
                    continue;
                }
                let mut diagnostic = Diagnostic::error(format!("there is no secret `{name}`"));
                if let Some(span) = contribution.site.span() {
                    diagnostic = diagnostic.label(span, format!("`{path}` refers to it"));
                }
                let note = did_you_mean(name, declared.iter().copied())
                    .unwrap_or_else(|| format!("help: declare it under `[secrets.{name}]`"));
                diagnostics.push(diagnostic.note(note));
            }
        }
    }
    diagnostics
}

/// The value a module reads: a secret in a path option is the path it's decrypted to, in `dir`.
fn secret_paths(dir: &str, ty: &Type, value: Value) -> Value {
    match (ty, value) {
        (Type::Path, Value::Runtime(parts)) => match parts.as_slice() {
            [RuntimePart::Secret(name)] => Value::Str(format!("{dir}/{name}")),
            _ => Value::Runtime(parts),
        },
        (Type::List { item, .. }, Value::List(items)) => Value::List(
            items
                .into_iter()
                .map(|value| secret_paths(dir, item, value))
                .collect(),
        ),
        (Type::Map(spec), Value::Table(entries)) => Value::Table(
            entries
                .into_iter()
                .map(|(key, value)| (key, secret_paths(dir, &spec.ty, value)))
                .collect(),
        ),
        (Type::Record(record), Value::Table(entries)) => Value::Table(
            entries
                .into_iter()
                .map(|(key, value)| {
                    let value = match record.fields.get(&key) {
                        Some(spec) => secret_paths(dir, &spec.ty, value),
                        None => value,
                    };
                    (key, value)
                })
                .collect(),
        ),
        (_, value) => value,
    }
}

fn secret_names(value: &Value) -> Vec<&String> {
    match value {
        Value::Runtime(parts) => parts
            .iter()
            .filter_map(|part| match part {
                RuntimePart::Secret(name) | RuntimePart::QuotedSecret(name) => Some(name),
                RuntimePart::Text(_) | RuntimePart::Machine(_) | RuntimePart::QuotedMachine(_) => {
                    None
                }
            })
            .collect(),
        Value::List(items) => items.iter().flat_map(secret_names).collect(),
        Value::Table(entries) => entries.values().flat_map(secret_names).collect(),
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_) => Vec::new(),
    }
}

/// Reads the patches builds use, so the model holds what they say.
fn read_patches(
    rendered: &[Option<Rendered>],
    config: &dyn ConfigFs,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<String, String> {
    let mut patches = BTreeMap::new();
    for (name, build) in rendered
        .iter()
        .flatten()
        .flat_map(|rendered| &rendered.output.builds)
    {
        for path in &build.patches {
            match config.read(path) {
                Ok(text) => {
                    patches.insert(path.clone(), text);
                }
                Err(e) => diagnostics.push(Diagnostic::error(format!(
                    "cannot read patch `{path}` of build `{name}`: {}: {e}",
                    config.display(path)
                ))),
            }
        }
    }
    patches
}

/// Reads and hashes each secret's encrypted file.
fn read_secrets(
    rendered: &[Option<Rendered>],
    config: &dyn ConfigFs,
    diagnostics: &mut Vec<Diagnostic>,
) -> BTreeMap<String, SecretFile> {
    let mut files = BTreeMap::new();
    for (name, secret) in rendered
        .iter()
        .flatten()
        .flat_map(|rendered| &rendered.output.secrets)
    {
        let source = config.display(&secret.file);
        match config.read_bytes(&secret.file) {
            Ok(bytes) => {
                let hash = blake3::hash(&bytes).to_hex().to_string();
                files.insert(name.clone(), SecretFile { source, hash });
            }
            Err(e) => diagnostics.push(
                Diagnostic::error(format!("cannot read secret `{name}` from {source}: {e}"))
                    .note("help: encrypt it with age to the machine's key, or set its `file`"),
            ),
        }
    }
    files
}

/// Files, units and base settings that more than one module renders. Each has one owner, so
/// the modules have to settle it between them.
fn shared_outputs(rendered: &[Option<Rendered>]) -> Vec<Diagnostic> {
    let mut owners: BTreeMap<(&str, String), Vec<&str>> = BTreeMap::new();
    for rendered in rendered.iter().flatten() {
        let output = &rendered.output;
        let home_files = output
            .homes
            .iter()
            .flat_map(|(user, home)| home.files.keys().map(move |path| home_path(user, path)))
            .collect();
        let user_units = output
            .homes
            .iter()
            .flat_map(|(user, home)| {
                home.units
                    .keys()
                    .map(move |name| format!("{name} (for {user})"))
            })
            .collect();
        let outputs: [(&str, Vec<String>); 8] = [
            ("file", output.files.keys().cloned().collect()),
            ("file", output.templates.keys().cloned().collect()),
            ("file", home_files),
            ("unit", output.units.keys().cloned().collect()),
            ("unit", user_units),
            ("base setting", output.base.keys().cloned().collect()),
            ("secret", output.secrets.keys().cloned().collect()),
            ("build", output.builds.keys().cloned().collect()),
        ];
        for (kind, keys) in outputs {
            for key in keys {
                owners.entry((kind, key)).or_default().push(&rendered.name);
            }
        }
    }
    owners
        .into_iter()
        .filter(|(_, modules)| modules.len() > 1)
        .map(|((kind, key), modules)| {
            let modules: Vec<String> = modules.iter().map(|name| format!("`{name}`")).collect();
            Diagnostic::error(format!(
                "{kind} `{key}` is rendered by modules {}",
                modules.join(" and ")
            ))
            .note("each one belongs to a single module; the others can set its options instead")
        })
        .collect()
}

/// Packages from sources nothing declares. The default source is always there.
fn unknown_sources(
    rendered: &[Option<Rendered>],
    sources: &BTreeMap<String, DeclaredSource>,
) -> Vec<Diagnostic> {
    let rendered: Vec<&Rendered> = rendered.iter().flatten().collect();
    let declared: BTreeSet<&str> = sources
        .keys()
        .map(String::as_str)
        .chain([WRATH_SOURCE])
        .collect();
    let mut diagnostics = Vec::new();
    for module in rendered {
        let output = &module.output;
        let homes = output.homes.values().flat_map(|home| &home.packages);
        for package in output.packages.iter().chain(homes) {
            let (source, _) = package_source(package);
            if !declared.contains(source) {
                diagnostics.push(
                    Diagnostic::error(format!(
                        "package `{package}` comes from source `{source}`, which isn't declared"
                    ))
                    .note(format!(
                        "help: declare it with `[sources.{source}]` and the `url` it follows"
                    )),
                );
            }
        }
    }
    diagnostics
}

/// Units that restart or reload on files no module renders, which is most likely a typo, and
/// triggers on units that aren't the system's services.
fn bad_triggers(rendered: &[Option<Rendered>]) -> Vec<Diagnostic> {
    let rendered: Vec<&Rendered> = rendered.iter().flatten().collect();
    let files: BTreeSet<&String> = rendered
        .iter()
        .flat_map(|rendered| rendered.output.files.keys())
        .collect();
    let units: BTreeSet<&String> = rendered
        .iter()
        .flat_map(|rendered| rendered.output.units.keys())
        .collect();

    let mut diagnostics = Vec::new();
    for module in &rendered {
        let output = &module.output;
        let on_files = output
            .units
            .iter()
            .flat_map(|(unit, spec)| {
                spec.restart_on
                    .iter()
                    .map(move |path| (unit, "restarts", path))
            })
            .chain(output.triggers.iter().flat_map(|((unit, action), paths)| {
                paths.iter().map(move |path| (unit, action.verb(), path))
            }));
        for (unit, verb, path) in on_files.filter(|(_, _, path)| !files.contains(path)) {
            diagnostics.push(Diagnostic::error(format!(
                "unit `{unit}` from module `{}` {verb} on `{path}`, which no module renders",
                module.name
            )));
        }
        // A rendered unit's restart becomes part of the unit, like its own `restart_on`, and its
        // reload stays a trigger, as the system's services' do.
        for unit in output.triggers.keys().map(|(unit, _)| unit) {
            if !units.contains(unit) && !unit.ends_with(".service") {
                diagnostics.push(Diagnostic::error(format!(
                    "module `{}` triggers `{unit}`; only services reload or restart",
                    module.name
                )));
            }
        }
    }
    diagnostics
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
    writers: Writers,
    /// Written paths whose modules have all finished, with how that went.
    settled: Vec<Option<Result<(), Failed>>>,
    report_missing: bool,
    definitions: BTreeMap<OptionPath, Vec<Rc<Definition>>>,
    /// The files that include each file.
    included_by: HashMap<FileId, Vec<FileId>>,
    instances: BTreeSet<OptionPath>,
    state: HashMap<OptionPath, State>,
    modules_state: Vec<ModuleState>,
    rendered: Vec<Option<Rendered>>,
    stack: Vec<Frame>,
    options: BTreeMap<OptionPath, Resolved>,
    machine_values: BTreeSet<OptionPath>,
    /// The values each machine is given, declared in `[machine.values]`.
    declared: BTreeMap<String, Span>,
    /// Where secrets are decrypted to: the system's, or a home's own.
    secrets_dir: &'static str,
    /// The uids and gids the lock next to the configuration keeps.
    ids: crate::Ids,
    /// The configuration's files, which file options name.
    config: &'a dyn ConfigFs,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Resolver<'a> {
    fn module_name(&self, module: ModuleId) -> &'a str {
        self.modules[module.index()].name()
    }

    fn writes(&self, module: ModuleId) -> &[OptionPath] {
        &self.writes[module.index()]
    }

    /// Renders every module that may set something under `path`. A written path whose modules
    /// have all finished can't change, so thousands of modules writing one parent aren't walked
    /// again for each option under it.
    fn run_writers(&mut self, path: &OptionPath) -> Result<(), Failed> {
        let entries = self.writers.entries(path);
        let mut earlier = Ok(());
        let mut pending = Vec::new();
        for entry in &entries {
            match self.settled[entry.index()] {
                Some(settled) => earlier = earlier.and(settled),
                None => pending.extend_from_slice(self.writers.modules(*entry)),
            }
        }
        pending.sort_unstable();
        pending.dedup();
        let ran = all(pending.into_iter().map(|id| self.run(id)));
        for entry in entries {
            if self.settled[entry.index()].is_none() {
                self.settled[entry.index()] = self.finished(entry);
            }
        }
        earlier.and(ran)
    }

    /// How `entry`'s modules went, if they have all finished.
    fn finished(&self, entry: Entry) -> Option<Result<(), Failed>> {
        let mut result = Ok(());
        for id in self.writers.modules(entry) {
            match self.modules_state[id.index()] {
                ModuleState::Done(done) => result = result.and(done),
                ModuleState::NotRun | ModuleState::Running => return None,
            }
        }
        Some(result)
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
                | Type::File
                | Type::Enum(_)
                | Type::List { .. }
                | Type::Freeform,
                value,
            ) => {
                // A module removes items the same way config does, with `{ remove = [...] }`.
                let removal = match value {
                    Value::Table(entries) if entries.len() == 1 && spec.ty.merges_items() => {
                        entries.get(REMOVE)
                    }
                    _ => None,
                };
                let (value, action) = match removal {
                    Some(items) => (items.clone(), Action::Remove),
                    None => (value.clone(), Action::Set),
                };
                self.definitions
                    .entry(path.clone())
                    .or_default()
                    .push(Rc::new(Definition {
                        path,
                        value: Raw::Literal(value),
                        level,
                        site,
                        reason: None,
                        action,
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

    /// The entries that exist directly under the map option at `path`.
    fn map_entries<'p>(&'p self, path: &'p OptionPath) -> impl Iterator<Item = &'p OptionPath> {
        self.instances
            .range(path.clone()..)
            .take_while(move |instance| instance.starts_with(path))
            .filter(move |instance| instance.len() == path.len() + 1)
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
                for entry in self.map_entries(&path) {
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
            | Type::File
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

    /// The value of the option at `path` as a module reads it: a secret in a path is where it's
    /// decrypted to, and a file is its text, which an error points at where the configuration
    /// names it when it can't be read.
    fn read_as(&mut self, path: &OptionPath, ty: &Type, value: Value) -> Result<Value, Failed> {
        let value = secret_paths(self.secrets_dir, ty, value);
        let (Type::File, Value::Str(file)) = (ty, &value) else {
            return Ok(value);
        };
        let e = match self.config.read(file) {
            Ok(text) => return Ok(Value::Str(text)),
            Err(e) => e,
        };
        let reason = match e.kind() {
            std::io::ErrorKind::NotFound => "there's no such file in the configuration".into(),
            _ => e.to_string(),
        };
        let site = self.options[path]
            .set_by
            .first()
            .map_or(Site::Default, |c| c.site);
        let diagnostic = Diagnostic::error(format!("cannot read `{file}`"));
        let diagnostic = self.locate(diagnostic, site, reason);
        self.diagnostics.push(diagnostic);
        Err(Failed)
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
            Known::Leaf(spec) => match self.resolve(path, &spec, at) {
                Ok(Some(value)) => self
                    .read_as(path, &spec.ty, value)
                    .map(Some)
                    .map_err(|Failed| ReadError::Failed),
                Ok(None) => Ok(None),
                Err(Failed) => Err(ReadError::Failed),
            },
            Known::Table(spec) => {
                let mut table = self.skeleton(&spec, path);
                let mut failed = false;
                for (leaf, spec) in self.leaves_under(&spec, path.clone()) {
                    match self.resolve(&leaf, &spec, at) {
                        Ok(Some(value)) => match self.read_as(&leaf, &spec.ty, value) {
                            Ok(value) => {
                                insert_nested(&mut table, &leaf.segments()[path.len()..], value)
                            }
                            Err(Failed) => failed = true,
                        },
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

    /// Empty tables for every record and map entry under a table of options, so a module reading
    /// it sees `{}` for an empty map or an unset record instead of nothing.
    fn skeleton(&self, spec: &OptionSpec, path: &OptionPath) -> BTreeMap<String, Value> {
        let children: Vec<(String, &OptionSpec)> = match &spec.ty {
            Type::Record(record) => record
                .fields
                .iter()
                .map(|(name, field)| (name.clone(), field))
                .collect(),
            Type::Map(item) => self
                .map_entries(path)
                .map(|entry| (entry.segments()[path.len()].clone(), item.as_ref()))
                .collect(),
            Type::Bool
            | Type::Int { .. }
            | Type::Float
            | Type::Str
            | Type::Path
            | Type::File
            | Type::Enum(_)
            | Type::List { .. }
            | Type::Freeform => Vec::new(),
        };
        children
            .into_iter()
            .filter(|(_, child)| child.ty.is_table())
            .map(|(name, child)| {
                let inner = self.skeleton(child, &path.child(name.as_str()));
                (name, Value::Table(inner))
            })
            .collect()
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
        let resolution = self.resolve_definitions(path, spec);
        self.stack.pop();
        self.state
            .insert(path.clone(), State::Done(resolution.clone()));
        resolution
    }

    fn resolve_definitions(&mut self, path: &OptionPath, spec: &OptionSpec) -> Resolution {
        self.run_writers(path)?;
        let mut definitions = self.definitions.get(path).cloned().unwrap_or_default();
        if let Some(default) = &spec.default {
            definitions.push(Rc::new(Definition {
                path: path.clone(),
                value: Raw::Literal(default.clone()),
                level: Level::Declared,
                site: Site::Default,
                reason: None,
                action: Action::Set,
            }));
        }
        if definitions.is_empty() {
            if spec.optional {
                return Ok(None);
            }
            if self.report_missing {
                self.diagnostics
                    .push(Diagnostic::error(format!("`{path}` is not set")).note(
                        "help: this option has no default, so the configuration must set it",
                    ));
            }
            return Err(Failed);
        }

        let wins = self.winners(spec, &definitions);
        let mut set_by = Vec::new();
        let mut overridden = Vec::new();
        let mut removed_by = Vec::new();
        let mut uses = Vec::new();
        let mut failed = false;
        for (definition, wins) in definitions.iter().zip(wins) {
            let mut definition_uses = Vec::new();
            let Some(value) = self.eval(&definition.value, &mut definition_uses) else {
                failed = true;
                continue;
            };
            let value = spec.ty.normalize(value);
            let checked = match definition.action {
                Action::Set => check(path, spec, &value),
                Action::Remove => check_removal(spec, &value),
            };
            if let Err(message) = checked {
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
            match definition.action {
                Action::Remove => {
                    uses.extend(definition_uses);
                    removed_by.push(contribution);
                }
                Action::Set if wins => {
                    uses.extend(definition_uses);
                    set_by.push(contribution);
                }
                Action::Set => overridden.push(contribution),
            }
        }
        if failed {
            return Err(Failed);
        }
        let value = self.merge(path, spec, &set_by, &removed_by).ok_or(Failed)?;
        self.options.insert(
            path.clone(),
            Resolved {
                value: value.clone(),
                set_by,
                overridden,
                removed_by,
                uses,
            },
        );
        Ok(Some(value))
    }

    /// Which definitions make up the value. The strongest level wins, except that set and
    /// ordered lists merge everything from module defaults to set values. Between defaults in
    /// configuration files, a file's default beats those of the files it includes. A declared
    /// default only applies when nothing else sets the option. Removals apply whatever level
    /// added the items, so they don't compete.
    fn winners(&self, spec: &OptionSpec, definitions: &[Rc<Definition>]) -> Vec<bool> {
        let level = |definition: &Definition| {
            (definition.action == Action::Set).then_some(definition.level)
        };
        let Some(top) = definitions.iter().filter_map(|d| level(d)).max() else {
            return vec![false; definitions.len()];
        };
        let merged = Level::ModuleDefault..=Level::Normal;
        if spec.ty.merges_items() && merged.contains(&top) {
            return definitions
                .iter()
                .map(|d| level(d).is_some_and(|level| merged.contains(&level)))
                .collect();
        }
        let file = |definition: &Definition| match definition.site {
            Site::Config(span) if definition.level == Level::Default => Some(span.file),
            Site::Config(_) | Site::Default | Site::Module { .. } => None,
        };
        let top_files: Vec<FileId> = definitions
            .iter()
            .filter(|d| level(d) == Some(top))
            .filter_map(|d| file(d))
            .collect();
        definitions
            .iter()
            .map(|d| {
                level(d) == Some(top)
                    && file(d).is_none_or(|own| {
                        !top_files
                            .iter()
                            .any(|&other| other != own && self.includes(other, own))
                    })
            })
            .collect()
    }

    /// Whether `ancestor` includes `file`, directly or through other files.
    fn includes(&self, ancestor: FileId, file: FileId) -> bool {
        let mut seen = HashSet::new();
        let mut pending = vec![file];
        while let Some(file) = pending.pop() {
            for &parent in self.included_by.get(&file).into_iter().flatten() {
                if parent == ancestor {
                    return true;
                }
                if seen.insert(parent) {
                    pending.push(parent);
                }
            }
        }
        false
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
        removed_by: &[Contribution],
    ) -> Option<Value> {
        if let Type::List { merge, .. } = spec.ty {
            match merge {
                ListMerge::Set => return self.merge_set(path, set_by, removed_by),
                ListMerge::Ordered => return self.merge_ordered(path, set_by, removed_by),
                ListMerge::Atomic => {}
            }
        }
        let first = &set_by.first()?.value;
        if set_by
            .iter()
            .all(|contribution| &contribution.value == first)
        {
            return Some(first.clone());
        }
        // Every contribution is at the same level, so the first one says which.
        let defaults = set_by[0].level == Level::Default;
        let (what, help) = if defaults {
            (
                "defaults",
                "help: neither file includes the other, so neither default wins; remove one, or \
                 set the value",
            )
        } else if set_by
            .iter()
            .any(|contribution| matches!(contribution.site, Site::Config(_)))
        {
            (
                "values",
                "help: remove all but one, make one a default others may change with \
                 `{ default = ... }`, or force one with `{ value = ..., force = \"reason\" }`",
            )
        } else {
            (
                "values",
                "help: remove all but one, or force one with `{ value = ..., force = \"reason\" }`",
            )
        };
        let mut diagnostic = Diagnostic::error(format!("conflicting {what} for `{path}`"));
        let verb = if defaults { "default" } else { "set to" };
        for contribution in set_by {
            let label = format!("{verb} {} here", contribution.value);
            diagnostic = self.locate(diagnostic, contribution.site, label);
        }
        self.diagnostics.push(diagnostic.note(help));
        None
    }

    /// The union of every contribution, sorted, without duplicates and without removed items.
    fn merge_set(
        &mut self,
        path: &OptionPath,
        set_by: &[Contribution],
        removed_by: &[Contribution],
    ) -> Option<Value> {
        let mut items: BTreeMap<String, Value> = set_by
            .iter()
            .flat_map(|contribution| list_items(&contribution.value))
            .map(|item| (item.to_string(), item.clone()))
            .collect();
        let mut failed = false;
        for removal in removed_by {
            for item in list_items(&removal.value) {
                let key = item.to_string();
                if items.remove(&key).is_none() {
                    let present: Vec<String> = items.keys().cloned().collect();
                    self.missing_removal(path, &key, &present, removal.site);
                    failed = true;
                }
            }
        }
        (!failed).then(|| {
            let mut items: Vec<Value> = items.into_values().collect();
            items.sort_by(Value::canonical_cmp);
            Value::List(items)
        })
    }

    /// Orders named items by their anchors and drops removed ones.
    fn merge_ordered(
        &mut self,
        path: &OptionPath,
        set_by: &[Contribution],
        removed_by: &[Contribution],
    ) -> Option<Value> {
        let mut items: BTreeMap<String, (ordered::Item, Site)> = BTreeMap::new();
        let mut failed = false;
        for contribution in set_by {
            for value in list_items(&contribution.value) {
                let item = ordered::split(value).expect("items were checked");
                match items.get(&item.name) {
                    Some((existing, _)) if *existing == item => {}
                    Some((_, site)) => {
                        let diagnostic = Diagnostic::error(format!(
                            "two different items named `{}` in `{path}`",
                            item.name
                        ));
                        let diagnostic = self.locate(diagnostic, *site, "one here");
                        let diagnostic = self.locate(diagnostic, contribution.site, "another here");
                        self.diagnostics.push(diagnostic);
                        failed = true;
                    }
                    None => {
                        items.insert(item.name.clone(), (item, contribution.site));
                    }
                }
            }
        }
        let mut removed = BTreeSet::new();
        for removal in removed_by {
            for name in list_items(&removal.value) {
                let Value::Str(name) = name else {
                    unreachable!("removals were checked");
                };
                removed.insert(name.clone());
                if !items.contains_key(name) {
                    let present: Vec<String> =
                        items.keys().map(|name| format!("\"{name}\"")).collect();
                    self.missing_removal(path, &format!("\"{name}\""), &present, removal.site);
                    failed = true;
                }
            }
        }
        if failed {
            return None;
        }
        let (list, sites): (Vec<ordered::Item>, BTreeMap<String, Site>) = items
            .into_iter()
            .map(|(name, (item, site))| (item, (name, site)))
            .unzip();
        match ordered::order(&list, &removed) {
            Ok(values) => Some(Value::List(values)),
            Err(OrderError::UnknownAnchor { item, anchor }) => {
                let diagnostic = Diagnostic::error(format!(
                    "`{item}` is placed relative to `{anchor}`, which is not in `{path}`"
                ));
                let diagnostic = self.locate(diagnostic, sites[&item], "placed here");
                self.diagnostics.push(diagnostic);
                None
            }
            Err(OrderError::Cycle(names)) => {
                let mut diagnostic = Diagnostic::error(format!(
                    "the `before` and `after` anchors in `{path}` contradict each other"
                ));
                for name in &names {
                    diagnostic =
                        self.locate(diagnostic, sites[name], format!("`{name}` placed here"));
                }
                self.diagnostics.push(diagnostic);
                None
            }
        }
    }

    fn missing_removal(&mut self, path: &OptionPath, item: &str, present: &[String], site: Site) {
        let diagnostic = Diagnostic::error(format!(
            "cannot remove {item} from `{path}`, it is not there"
        ));
        let mut diagnostic = self.locate(diagnostic, site, "removed here");
        if let Some(note) = did_you_mean(item, present) {
            diagnostic = diagnostic.note(note);
        }
        self.diagnostics.push(diagnostic);
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
                                filled @ (RuntimePart::Secret(_)
                                | RuntimePart::QuotedSecret(_)
                                | RuntimePart::Machine(_)
                                | RuntimePart::QuotedMachine(_)) => out.push(filled),
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
        match path
            .segments()
            .split_first()
            .map(|(first, rest)| (first.as_str(), rest))
        {
            Some((FACTS | MACHINE, _)) => return self.machine_value(path, span, uses),
            Some((SECRET, [name])) => {
                return Some(Value::Runtime(vec![RuntimePart::Secret(name.clone())]));
            }
            Some((SECRET, _)) => {
                self.diagnostics.push(
                    Diagnostic::error(format!("`{path}` doesn't name a secret"))
                        .label(span, "referenced here")
                        .note("help: refer to a secret as `${secret.NAME}`"),
                );
                return None;
            }
            _ => {}
        }
        self.run_writers(path).ok()?;
        let diagnostic = match self.known(path) {
            Known::Leaf(spec) => match self.resolve(path, &spec, Some(span)) {
                Ok(Some(value)) => {
                    uses.push(Use { path: path.clone() });
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

    /// A machine value, filled in on the machine: one of [`MACHINE_VALUES`], or a value declared
    /// in `[machine.values]`.
    fn machine_value(
        &mut self,
        path: &OptionPath,
        span: Span,
        uses: &mut Vec<Use>,
    ) -> Option<Value> {
        let name = path.to_string();
        let declared = match path.segments() {
            [first, key] if first == MACHINE => self.declared.contains_key(key),
            _ => false,
        };
        if declared || MACHINE_VALUES.contains(&name.as_str()) {
            self.machine_values.insert(path.clone());
            uses.push(Use { path: path.clone() });
            return Some(Value::Runtime(vec![RuntimePart::Machine(name)]));
        }
        // A given value that's close to a declared one is likely a typo, and otherwise likely
        // missing its declaration, even when it's close to `machine.name`.
        let note = if path.segments()[0] == MACHINE {
            let declared: Vec<String> = self
                .declared
                .keys()
                .map(|key| format!("{MACHINE}.{key}"))
                .collect();
            match closest(&name, declared.iter().map(String::as_str)) {
                Some(known) => format!("help: did you mean `{known}`?"),
                None => {
                    "help: declare it in `[machine.values]`, so each machine is given it".to_owned()
                }
            }
        } else {
            match closest(&name, MACHINE_VALUES) {
                Some(known) => format!("help: did you mean `{known}`?"),
                None => format!("the machine values are {}", MACHINE_VALUES.join(", ")),
            }
        };
        self.diagnostics.push(
            Diagnostic::error(format!("unknown machine value `{path}`"))
                .label(span, "referenced here")
                .note(note),
        );
        None
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

    fn locate(&self, path: &OptionPath) -> Option<Span> {
        self.resolver
            .definitions
            .range(path.clone()..)
            .take_while(|(defined, _)| defined.starts_with(path))
            .flat_map(|(_, definitions)| definitions)
            .find_map(|definition| match definition.site {
                Site::Config(span) => Some(span),
                Site::Default | Site::Module { .. } => None,
            })
    }

    fn locked_ids(&self) -> Value {
        self.resolver.ids.to_value()
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

fn list_items(value: &Value) -> &[Value] {
    match value {
        Value::List(items) => items,
        _ => &[],
    }
}

/// Checks the items of `{ remove = [...] }`: values for a set, names for an ordered list.
fn check_removal(spec: &OptionSpec, value: &Value) -> Result<(), String> {
    match (&spec.ty, value) {
        (
            Type::List {
                merge: ListMerge::Ordered,
                ..
            },
            Value::List(names),
        ) => names.iter().try_for_each(|name| match name {
            Value::Str(_) => Ok(()),
            other => Err(format!(
                "ordered list items are removed by name, found {other}"
            )),
        }),
        (
            Type::List {
                item,
                merge: ListMerge::Set,
            },
            Value::List(items),
        ) => items.iter().try_for_each(|value| item.check(value)),
        (_, other) => Err(format!("expected a list of items to remove, found {other}")),
    }
}
