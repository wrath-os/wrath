//! Modules written in Starlark.
//!
//! Starlark is deterministic by design: dictionaries keep insertion order, there is no I/O,
//! clock or randomness, and values are frozen once a file has been evaluated. A module file
//! calls `module(...)`:
//!
//! ```python
//! module(
//!     name = "syncthing",
//!     options = {"services.syncthing": types.map(instance)},
//!     writes = ["users"],
//!     render = render,
//! )
//! ```

// starlark-rust's derive macros (`ProvidesStaticType`, `starlark_value`) emit `unsafe impl`s.
#![allow(unsafe_code)]

mod convert;
pub mod style;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

use allocative::Allocative;
use distro_eval::{
    Build, ConfigKind, ConfigReader, Diagnostic, EVERY, File, FileId, Level, ListMerge,
    MACHINE_VALUES, Module, OptionPath, OptionSpec, Output, ReadError, RenderError, RuntimePart,
    Secret, Setting, Sources, Span, Template, Type, Unit, UnitAction, Value, Wrapper, formats,
    home_path,
};
use starlark::any::ProvidesStaticType;
use starlark::codemap::FileSpan;
use starlark::collections::SmallMap;
use starlark::environment::{
    FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Methods, MethodsBuilder,
    MethodsStatic, Module as StarModule,
};
use starlark::eval::{Evaluator, ReturnFileLoader};
use starlark::starlark_module;
use starlark::starlark_simple_value;
use starlark::syntax::{AstModule, Dialect, DialectTypes};
use starlark::values::none::{NoneOr, NoneType};
use starlark::values::{
    Heap, NoSerialize, StarlarkPagablePanic, StarlarkValue, Value as StarValue, ValueLike,
};
use starlark_derive::starlark_value;

/// The module variable that holds the render function after `module(...)` runs.
const RENDER: &str = "module_render";

#[cfg(not(test))]
const TIME_LIMIT: Duration = Duration::from_secs(10);
#[cfg(test)]
const TIME_LIMIT: Duration = Duration::from_secs(1);

/// Loads a module from source. `path` is how errors refer to the file, and `root` the modules
/// directory, which libraries the module loads, as `load("lib/merge.star", "merge")`, are in.
pub fn load(
    sources: &mut Sources,
    path: &str,
    text: String,
    root: &Path,
) -> Result<Box<dyn Module>, Diagnostic> {
    let file = sources.add(path, text);
    let locate = Locate {
        file,
        path: path.to_owned(),
    };
    let ast = parse(path, sources.text(file)).map_err(|e| locate.diagnostic(&e))?;
    let globals = globals();
    let mut libraries = Libraries::new();
    load_libraries(
        sources,
        root,
        &ast,
        &globals,
        &mut libraries,
        &mut Vec::new(),
    )
    .map_err(|message| Diagnostic::error(format!("{path}: {message}")))?;
    let declared = RefCell::new(None);
    StarModule::with_temp_heap(|module| {
        let loading = Loading(&declared);
        let loader = loader(&libraries);
        let file_loader = ReturnFileLoader { modules: &loader };
        let mut eval = Evaluator::new(&module);
        eval.enable_static_typechecking(true);
        eval.extra = Some(&loading);
        eval.set_loader(&file_loader);
        set_time_limit(&mut eval);
        eval.eval_module(ast, &globals)
            .map(|_| ())
            .map_err(|e| locate.diagnostic(&e))
    })?;
    let declared = declared.into_inner().ok_or_else(|| {
        Diagnostic::error(format!("{path}: the module never calls `module(...)`"))
    })?;
    Ok(Box::new(StarlarkModule {
        declared,
        source: sources.text(file).to_owned(),
        globals,
        libraries,
        locate,
    }))
}

/// Libraries by the path modules load them by, evaluated once and shared by every render.
type Libraries = HashMap<String, FrozenModule>;

/// The evaluator's `extra` while a library loads, so `module(...)` can refuse.
#[derive(ProvidesStaticType)]
struct LoadingLibrary;

fn loader(libraries: &Libraries) -> HashMap<&str, &FrozenModule> {
    libraries
        .iter()
        .map(|(path, module)| (path.as_str(), module))
        .collect()
}

/// Loads the libraries `ast` loads, and theirs, into `libraries`, and adds each one's text to
/// `sources` once, so an apply keeps the code its modules ran. `stack` holds the libraries being
/// loaded, to catch one that ends up loading itself.
fn load_libraries(
    sources: &mut Sources,
    root: &Path,
    ast: &AstModule,
    globals: &Globals,
    libraries: &mut Libraries,
    stack: &mut Vec<String>,
) -> Result<(), String> {
    for load in ast.loads() {
        let path = load.module_id;
        if libraries.contains_key(path) {
            continue;
        }
        if stack.iter().any(|loading| loading == path) {
            return Err(format!("`{path}` loads itself"));
        }
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative.extension().is_none_or(|e| e != "star")
            || relative.components().any(|c| c == Component::ParentDir)
        {
            return Err(format!(
                "`{path}` isn't a library: libraries are `.star` files in the modules directory"
            ));
        }
        let file = root.join(relative);
        let text =
            std::fs::read_to_string(&file).map_err(|e| format!("cannot load `{path}`: {e}"))?;
        let library = parse(path, &text).map_err(|e| format!("`{path}`: {e}"))?;
        let name = file.display().to_string();
        if !sources.files().any(|id| sources.name(id) == name) {
            sources.add(name, text);
        }
        stack.push(path.to_owned());
        load_libraries(sources, root, &library, globals, libraries, stack)?;
        stack.pop();
        let frozen = StarModule::with_temp_heap(|module| {
            {
                let loader = loader(libraries);
                let file_loader = ReturnFileLoader { modules: &loader };
                let mut eval = Evaluator::new(&module);
                eval.enable_static_typechecking(true);
                eval.extra = Some(&LoadingLibrary);
                eval.set_loader(&file_loader);
                set_time_limit(&mut eval);
                eval.eval_module(library, globals)
                    .map_err(|e| format!("`{path}`: {e}"))?;
            }
            module.freeze().map_err(|e| format!("`{path}`: {e:?}"))
        })?;
        libraries.insert(path.to_owned(), frozen);
    }
    Ok(())
}

fn parse(path: &str, source: &str) -> starlark::Result<AstModule> {
    // Annotations are checked when a function is called. The evaluators also turn on static
    // type checking, so a misspelt or mistyped call fails before anything runs.
    let dialect = Dialect {
        enable_types: DialectTypes::Enable,
        ..Dialect::Standard
    };
    AstModule::parse(path, source.to_owned(), &dialect)
}

fn set_time_limit(eval: &mut Evaluator<'_, '_, '_>) {
    let deadline = Instant::now() + TIME_LIMIT;
    eval.set_check_cancelled(Box::new(move || Instant::now() > deadline));
}

/// What `module(...)` declared.
struct Declared {
    name: String,
    options: Vec<(OptionPath, OptionSpec)>,
    writes: Vec<OptionPath>,
    configurations: Vec<ConfigKind>,
    /// The `enable` option that turns the module on, when it has one.
    switch: Option<OptionPath>,
}

/// The evaluator's `extra` while a module file loads.
#[derive(ProvidesStaticType)]
struct Loading<'a>(&'a RefCell<Option<Declared>>);

/// Turns Starlark locations into spans in the module's source file.
struct Locate {
    file: FileId,
    path: String,
}

impl Locate {
    fn span(&self, span: &FileSpan) -> Option<Span> {
        (span.filename() == self.path).then(|| Span {
            file: self.file,
            start: span.span.begin().get() as usize,
            end: span.span.end().get() as usize,
        })
    }

    fn diagnostic(&self, error: &starlark::Error) -> Diagnostic {
        let message = error.kind().to_string();
        let message = message.lines().next().unwrap_or_default().to_owned();
        Diagnostic::error(message).label_opt(error.span().and_then(|span| self.span(span)), "here")
    }
}

/// A type in a module's option declarations.
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
struct StarType(#[allocative(skip)] Type);

starlark_simple_value!(StarType);

impl fmt::Display for StarType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<type: {}>", self.0.describe())
    }
}

#[starlark_value(type = "type")]
impl<'v> StarlarkValue<'v> for StarType {}

/// An option: a type with a default, documentation and flags.
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
struct StarOption(#[allocative(skip)] OptionSpec);

starlark_simple_value!(StarOption);

impl fmt::Display for StarOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<option: {}>", self.0.ty.describe())
    }
}

#[starlark_value(type = "option")]
impl<'v> StarlarkValue<'v> for StarOption {}

fn type_of(value: StarValue<'_>) -> anyhow::Result<Type> {
    match value.downcast_ref::<StarType>() {
        Some(ty) => Ok(ty.0.clone()),
        None => Err(anyhow::anyhow!(
            "expected a type such as `types.str`, found a {}; options only go in records, maps and `options`",
            value.get_type()
        )),
    }
}

/// A type or an option, as an option.
fn spec_of(value: StarValue<'_>) -> anyhow::Result<OptionSpec> {
    match value.downcast_ref::<StarOption>() {
        Some(option) => Ok(option.0.clone()),
        None => type_of(value).map(OptionSpec::new),
    }
}

/// A list of strings, such as `writes`.
fn strings(value: StarValue<'_>) -> anyhow::Result<Vec<String>> {
    match convert::from_starlark(value) {
        Ok(Value::List(items)) => items
            .into_iter()
            .map(|item| match item {
                Value::Str(text) => Ok(text),
                other => Err(anyhow::anyhow!("expected a string, found {other}")),
            })
            .collect(),
        _ => Err(anyhow::anyhow!(
            "expected a list of strings, found a {}",
            value.get_type()
        )),
    }
}

fn globals() -> Globals {
    base_globals().build()
}

/// What modules and build styles both have: `option`, `types` and `formats`.
fn base_globals() -> GlobalsBuilder {
    GlobalsBuilder::extended_by(&[
        LibraryExtension::StructType,
        LibraryExtension::Json,
        LibraryExtension::Typing,
    ])
    .with(std_globals)
    .with_namespace("types", |builder| {
        types_functions(builder);
        for (name, ty) in [
            ("bool", Type::Bool),
            ("int", Type::int()),
            ("float", Type::Float),
            ("str", Type::Str),
            ("path", Type::Path),
            ("port", Type::port()),
            ("freeform", Type::Freeform),
        ] {
            builder.set(name, StarType(ty));
        }
    })
    .with_namespace("formats", formats_functions)
}

#[starlark_module]
#[starlark_types(Ctx as Context, StarOption as Option)]
fn std_globals(builder: &mut GlobalsBuilder) {
    /// A machine value, such as `machine_value("machine.name")`, filled in on the machine when it
    /// switches. Only options and files filled in there take it, like a secret.
    fn machine_value<'v>(
        #[starlark(require = pos)] name: &str,
        heap: Heap<'v>,
    ) -> anyhow::Result<StarValue<'v>> {
        if !MACHINE_VALUES.contains(&name) {
            anyhow::bail!(
                "unknown machine value `{name}`; the machine values are {}",
                MACHINE_VALUES.join(", ")
            );
        }
        let part = RuntimePart::Machine(name.to_owned());
        Ok(heap.alloc(convert::Runtime(vec![part])))
    }

    /// Declares an option: a type with a default, documentation and flags. A list or table is
    /// empty when nothing sets it, unless it's `optional` or `required`.
    fn option<'v>(
        #[starlark(require = pos)] r#type: StarValue<'v>,
        #[starlark(require = named)] default: Option<StarValue<'v>>,
        #[starlark(require = named, default = false)] optional: bool,
        #[starlark(require = named, default = false)] required: bool,
        #[starlark(require = named, default = false)] runtime: bool,
        #[starlark(require = named, default = false)] account: bool,
        #[starlark(require = named, default = "")] doc: &str,
    ) -> anyhow::Result<StarOption> {
        if required && (optional || default.is_some()) {
            anyhow::bail!("an option that has to be set can't be optional or have a default");
        }
        let mut spec = OptionSpec::new(type_of(r#type)?);
        spec.default = default
            .map(convert::from_starlark)
            .transpose()
            .map_err(anyhow::Error::msg)?;
        if spec.default.is_none() && !optional && !required {
            spec.default = match spec.ty {
                Type::List { .. } => Some(Value::List(Vec::new())),
                Type::Map(_) => Some(Value::Table(Default::default())),
                _ => None,
            };
        }
        spec.optional = optional;
        spec.runtime = runtime;
        spec.account = account;
        spec.doc = doc.to_owned();
        Ok(StarOption(spec))
    }

    /// The lines of `items`, each ending in a newline, as a file holds them.
    fn lines(
        #[starlark(require = pos)] items: starlark::values::list::UnpackList<String>,
    ) -> anyhow::Result<String> {
        Ok(items.items.iter().map(|line| format!("{line}\n")).collect())
    }

    /// Declares the module. Every module file calls this once. `user_options` are options every
    /// user has, under `users.<name>`.
    #[allow(clippy::too_many_arguments)]
    fn module<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] options: SmallMap<String, StarValue<'v>>,
        #[starlark(require = named)] user_options: Option<SmallMap<String, StarValue<'v>>>,
        #[starlark(require = named)] writes: StarValue<'v>,
        #[starlark(require = named)] render: StarValue<'v>,
        #[starlark(require = named)] configurations: Option<StarValue<'v>>,
        #[starlark(require = named)] enable: Option<bool>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let every_user = OptionPath::root().child("users").child(EVERY);
        let parse = |path: &str| OptionPath::parse(path).map_err(anyhow::Error::msg);
        let user_options = user_options
            .unwrap_or_default()
            .into_iter()
            .map(|(path, spec)| {
                let path = parse(&path)?
                    .segments()
                    .iter()
                    .fold(every_user.clone(), |path, segment| {
                        path.child(segment.as_str())
                    });
                Ok((path, spec_of(spec)?))
            });
        let mut options = options
            .into_iter()
            .map(|(path, spec)| Ok((parse(&path)?, spec_of(spec)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let switch = enable.map(|on| add_switch(&mut options, on)).transpose()?;
        options.extend(user_options.collect::<anyhow::Result<Vec<_>>>()?);
        options.sort_by(|a, b| a.0.cmp(&b.0));
        let writes = strings(writes)?
            .iter()
            .map(|path| OptionPath::parse(path))
            .collect::<Result<_, _>>()
            .map_err(anyhow::Error::msg)?;
        let configurations = match configurations {
            None => vec![ConfigKind::System],
            Some(value) => strings(value)?
                .iter()
                .map(|kind| match kind.as_str() {
                    "system" => Ok(ConfigKind::System),
                    "project" => Ok(ConfigKind::Project),
                    "home" => Ok(ConfigKind::Home),
                    other => Err(anyhow::anyhow!(
                        "a module is for \"system\", \"project\" or \"home\" configurations, \
                         not `{other}`"
                    )),
                })
                .collect::<anyhow::Result<_>>()?,
        };
        if eval
            .extra
            .is_some_and(|extra| extra.downcast_ref::<LoadingLibrary>().is_some())
        {
            return Err(anyhow::anyhow!(
                "only a module calls `module(...)`; a library holds helpers for modules"
            ));
        }
        // While rendering, the file runs again; only the render function matters then.
        if let Some(loading) = eval.extra.and_then(|extra| extra.downcast_ref::<Loading>()) {
            if loading.0.borrow().is_some() {
                return Err(anyhow::anyhow!("`module(...)` is called twice"));
            }
            *loading.0.borrow_mut() = Some(Declared {
                name: name.to_owned(),
                options,
                writes,
                configurations,
                switch,
            });
        }
        eval.module().set(RENDER, render);
        Ok(NoneType)
    }
}

/// Adds `enable`, defaulting to `on`, to the module's one table of options, and returns its path.
fn add_switch(options: &mut [(OptionPath, OptionSpec)], on: bool) -> anyhow::Result<OptionPath> {
    let [
        (
            path,
            OptionSpec {
                ty: Type::Record(record),
                ..
            },
        ),
    ] = options
    else {
        anyhow::bail!("`enable` turns on a module with one table of options");
    };
    if record.fields.contains_key(ENABLE) {
        anyhow::bail!("`enable = ...` adds the option, and the module declares `enable` itself");
    }
    let switch = OptionSpec::new(Type::Bool)
        .default(Value::Bool(on))
        .doc("Whether it's on.");
    record.fields.insert(ENABLE.to_owned(), switch);
    Ok(path.child(ENABLE))
}

/// The option that turns a module on.
const ENABLE: &str = "enable";

#[starlark_module]
fn types_functions(builder: &mut GlobalsBuilder) {
    /// A list where one value wins as a whole.
    fn list<'v>(item: StarValue<'v>) -> anyhow::Result<StarType> {
        Ok(StarType(Type::list(type_of(item)?, ListMerge::Atomic)))
    }

    /// A list merged from every definition, sorted and without duplicates.
    fn set<'v>(item: StarValue<'v>) -> anyhow::Result<StarType> {
        Ok(StarType(Type::list(type_of(item)?, ListMerge::Set)))
    }

    /// A list of named items placed with `before` and `after` anchors.
    fn ordered<'v>(item: StarValue<'v>) -> anyhow::Result<StarType> {
        Ok(StarType(Type::list(type_of(item)?, ListMerge::Ordered)))
    }

    /// Named entries chosen by the configuration.
    fn map<'v>(item: StarValue<'v>) -> anyhow::Result<StarType> {
        Ok(StarType(Type::map(spec_of(item)?)))
    }

    /// A table with these fields.
    fn record<'v>(
        #[starlark(kwargs)] fields: SmallMap<String, StarValue<'v>>,
    ) -> anyhow::Result<StarType> {
        let fields = fields
            .iter()
            .map(|(name, field)| Ok((name.as_str(), spec_of(*field)?)))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(StarType(Type::record(fields)))
    }

    /// One of these strings.
    fn one_of<'v>(choices: StarValue<'v>) -> anyhow::Result<StarType> {
        Ok(StarType(Type::Enum(strings(choices)?)))
    }

    /// An integer from `min` to `max`.
    fn range(min: i64, max: i64) -> anyhow::Result<StarType> {
        Ok(StarType(Type::Int {
            min: Some(min),
            max: Some(max),
        }))
    }
}

/// Formats render values with secrets in them too. The result is then a runtime value, text
/// with the secrets left to fill in on the machine, which only `ctx.file` takes.
#[starlark_module]
fn formats_functions(builder: &mut GlobalsBuilder) {
    /// Pretty-printed JSON with sorted keys.
    fn json<'v>(value: StarValue<'v>, heap: Heap<'v>) -> anyhow::Result<StarValue<'v>> {
        render(value, heap, formats::json_parts)
    }

    /// A systemd-style file, such as a `.network` file, from a dictionary of sections. A list
    /// value repeats its key.
    fn systemd<'v>(sections: StarValue<'v>, heap: Heap<'v>) -> anyhow::Result<StarValue<'v>> {
        render(sections, heap, formats::systemd_parts)
    }

    /// A TOML document from a dictionary, with keys sorted.
    fn toml<'v>(value: StarValue<'v>, heap: Heap<'v>) -> anyhow::Result<StarValue<'v>> {
        render(value, heap, formats::toml_parts)
    }
}

/// `value` rendered by `format`: a string, or a runtime value when it has secrets.
fn render<'v>(
    value: StarValue<'v>,
    heap: Heap<'v>,
    format: fn(&Value) -> Result<Vec<RuntimePart>, String>,
) -> anyhow::Result<StarValue<'v>> {
    let value = convert::from_starlark(value).map_err(anyhow::Error::msg)?;
    let parts = format(&value).map_err(anyhow::Error::msg)?;
    Ok(match parts.as_slice() {
        [] => heap.alloc(""),
        [RuntimePart::Text(text)] => heap.alloc(text.as_str()),
        _ => heap.alloc(convert::Runtime(parts)),
    })
}

/// A loaded module. Each render evaluates the file again on a fresh heap and calls the render
/// function it declares, so no Starlark values outlive a render.
struct StarlarkModule {
    declared: Declared,
    source: String,
    globals: Globals,
    libraries: Libraries,
    locate: Locate,
}

/// Why a render stopped early from inside a method.
enum Stop {
    /// Something the module read failed; the error is already reported.
    Failed,
    Error(Diagnostic),
}

/// The evaluator's `extra` while a module renders.
#[derive(ProvidesStaticType)]
struct Rendering<'a> {
    config: RefCell<&'a mut dyn ConfigReader>,
    output: RefCell<Output>,
    stop: RefCell<Option<Stop>>,
    locate: &'a Locate,
}

impl Rendering<'_> {
    /// Records why a method failed and returns the error that unwinds the module.
    fn stop(&self, stop: Stop) -> anyhow::Error {
        let message = match &stop {
            Stop::Failed => "stopped because a value it read failed".to_owned(),
            Stop::Error(diagnostic) => diagnostic.message.clone(),
        };
        self.stop.borrow_mut().get_or_insert(stop);
        anyhow::Error::msg(message)
    }

    fn invalid(&self, at: Option<Span>, message: impl Into<String>) -> anyhow::Error {
        self.stop(Stop::Error(
            Diagnostic::error(message).label_opt(at, "here"),
        ))
    }
}

/// The current render and where in the module the running method was called from.
fn rendering<'a, 'e>(
    eval: &'a Evaluator<'_, 'a, 'e>,
) -> anyhow::Result<(&'a Rendering<'e>, Option<Span>)> {
    let rendering = eval
        .extra
        .and_then(|extra| extra.downcast_ref::<Rendering>())
        .ok_or_else(|| anyhow::anyhow!("`ctx` can only be used while a module renders"))?;
    let at = eval
        .call_stack_top_location()
        .and_then(|span| rendering.locate.span(&span));
    Ok((rendering, at))
}

/// The `ctx` argument of a module's render function.
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
struct Ctx;

starlark_simple_value!(Ctx);

impl fmt::Display for Ctx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<ctx>")
    }
}

#[starlark_value(type = "Context")]
impl<'v> StarlarkValue<'v> for Ctx {
    fn get_methods() -> Option<&'static Methods> {
        static METHODS: MethodsStatic = MethodsStatic::new("ctx", ctx_methods);
        Some(METHODS.methods())
    }
}

fn set_option(
    eval: &mut Evaluator<'_, '_, '_>,
    path: &str,
    value: StarValue<'_>,
    level: Level,
    removes: bool,
) -> anyhow::Result<NoneType> {
    let (rendering, at) = rendering(eval)?;
    let path = OptionPath::parse(path).map_err(|e| rendering.invalid(at, e))?;
    let mut value = convert::from_starlark(value).map_err(|e| rendering.invalid(at, e))?;
    if removes {
        value = Value::Table([("remove".to_owned(), value)].into_iter().collect());
    }
    rendering.output.borrow_mut().settings.push(Setting {
        path,
        value,
        level,
        at,
    });
    Ok(NoneType)
}

/// Inserts a keyed setting into the map of the output that `pick` selects. Each key can be set
/// once per module.
fn insert_setting(
    eval: &mut Evaluator<'_, '_, '_>,
    what: &str,
    key: &str,
    value: StarValue<'_>,
    pick: fn(&mut Output) -> &mut std::collections::BTreeMap<String, Value>,
) -> anyhow::Result<NoneType> {
    let (rendering, at) = rendering(eval)?;
    let value = convert::from_starlark(value)
        .map_err(|e| rendering.invalid(at, format!("{what} `{key}`: {e}")))?;
    let replaced = pick(&mut rendering.output.borrow_mut()).insert(key.to_owned(), value);
    if replaced.is_some() {
        return Err(rendering.invalid(at, format!("{what} `{key}` is set twice")));
    }
    Ok(NoneType)
}

fn insert_trigger(
    eval: &mut Evaluator<'_, '_, '_>,
    unit: &str,
    action: UnitAction,
    on: StarValue<'_>,
) -> anyhow::Result<NoneType> {
    let (rendering, at) = rendering(eval)?;
    let paths = strings(on)
        .map_err(|e| rendering.invalid(at, format!("{} `{unit}`: on: {e}", action.as_str())))?;
    rendering
        .output
        .borrow_mut()
        .triggers
        .entry((unit.to_owned(), action))
        .or_default()
        .extend(paths);
    Ok(NoneType)
}

#[starlark_module]
fn ctx_methods(builder: &mut MethodsBuilder) {
    /// The value of an option, or a dictionary of everything under a table of options.
    fn read<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] path: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<StarValue<'v>> {
        let heap = eval.heap();
        let (rendering, at) = rendering(eval)?;
        let path = OptionPath::parse(path).map_err(|e| rendering.invalid(at, e))?;
        let read = rendering.config.borrow_mut().read(&path, at);
        match read {
            Ok(Some(value)) => Ok(convert::to_starlark(heap, &value)),
            Ok(None) => Ok(StarValue::new_none()),
            Err(ReadError::Invalid(message)) => Err(rendering.invalid(at, message)),
            Err(ReadError::Failed) => Err(rendering.stop(Stop::Failed)),
        }
    }

    /// The uids and gids the lock keeps, as `{"users": {name: uid}, "groups": {name: gid}}`. A
    /// module that hands out new ones records them all with `ctx.base("ids", ...)`, and the lock
    /// keeps them from then on.
    fn locked_ids<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<StarValue<'v>> {
        let heap = eval.heap();
        let (rendering, _) = rendering(eval)?;
        let ids = rendering.config.borrow().locked_ids();
        Ok(convert::to_starlark(heap, &ids))
    }

    /// Sets an option.
    fn set<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] path: &str,
        #[starlark(require = pos)] value: StarValue<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        set_option(eval, path, value, Level::Normal, false)
    }

    /// Sets an option at the default level, so configuration overrides it.
    fn default<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] path: &str,
        #[starlark(require = pos)] value: StarValue<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        set_option(eval, path, value, Level::ModuleDefault, false)
    }

    /// Removes items from a set or ordered list.
    fn remove<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] path: &str,
        #[starlark(require = pos)] items: StarValue<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        set_option(eval, path, items, Level::Normal, true)
    }

    /// Renders a systemd unit from a dictionary of sections. `restart_on` lists files the unit
    /// reads, so it restarts when one of them changes. With `user`, the unit runs in that
    /// user's systemd user manager.
    fn unit<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] name: &str,
        #[starlark(require = pos)] sections: StarValue<'v>,
        #[starlark(require = named)] restart_on: Option<StarValue<'v>>,
        #[starlark(require = named)] user: Option<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let (rendering, at) = rendering(eval)?;
        let mut unit = convert::from_starlark(sections)
            .and_then(|sections| Unit::from_value(&sections))
            .map_err(|e| rendering.invalid(at, format!("unit `{name}`: {e}")))?;
        if let Some(paths) = restart_on {
            if user.is_some() {
                return Err(rendering.invalid(
                    at,
                    format!("unit `{name}`: user units can't restart_on files yet"),
                ));
            }
            unit.restart_on = strings(paths)
                .map_err(|e| rendering.invalid(at, format!("unit `{name}`: restart_on: {e}")))?
                .into_iter()
                .collect();
        }
        let mut output = rendering.output.borrow_mut();
        let (units, shown) = match user {
            Some(user) => (
                &mut output.homes.entry(user.to_owned()).or_default().units,
                format!("{name} (for {user})"),
            ),
            None => (&mut output.units, name.to_owned()),
        };
        if units.insert(name.to_owned(), unit).is_some() {
            return Err(rendering.invalid(at, format!("unit `{shown}` is rendered twice")));
        }
        Ok(NoneType)
    }

    /// Reloads a service the system provides, such as `systemd-networkd.service`, when one of
    /// the files in `on` changes.
    fn reload<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] unit: &str,
        #[starlark(require = named)] on: StarValue<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        insert_trigger(eval, unit, UnitAction::Reload, on)
    }

    /// Restarts a service the system provides when one of the files in `on` changes.
    fn restart<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] unit: &str,
        #[starlark(require = named)] on: StarValue<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        insert_trigger(eval, unit, UnitAction::Restart, on)
    }

    /// Renders a file. With `user`, the path starts with `~/` and the file goes in that user's
    /// home. Text with secrets in it, a runtime value, makes a file the machine fills in,
    /// owned by `owner` and `group` with `mode` (0400 by default); other files are links into
    /// the store that anyone can read. `next_login` says the file is only read when a user logs
    /// in, so changing it takes effect at the next login.
    #[allow(clippy::too_many_arguments)]
    fn file<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] path: &str,
        #[starlark(require = pos)] text: StarValue<'v>,
        #[starlark(require = pos, default = NoneOr::None)] mode: NoneOr<u32>,
        #[starlark(require = named)] user: Option<&str>,
        #[starlark(require = named, default = NoneOr::None)] owner: NoneOr<&str>,
        #[starlark(require = named, default = NoneOr::None)] group: NoneOr<&str>,
        #[starlark(require = named, default = false)] next_login: bool,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let (rendering, at) = rendering(eval)?;
        let (mode, owner, group) = (mode.into_option(), owner.into_option(), group.into_option());
        let mut guard = rendering.output.borrow_mut();
        let output = &mut *guard;
        if user.is_some() && !path.starts_with("~/") {
            return Err(rendering.invalid(
                at,
                format!("file `{path}` is in a home, so it starts with `~/`"),
            ));
        }
        if let Some(runtime) = text.downcast_ref::<convert::Runtime>() {
            let template = Template {
                parts: runtime.0.clone(),
                owner: owner.or(user).unwrap_or("root").to_owned(),
                group: group.map(str::to_owned),
                mode: mode.unwrap_or(0o400),
            };
            let (files, templates, shown) = match user {
                Some(user) => {
                    let home = output.homes.entry(user.to_owned()).or_default();
                    (&home.files, &mut home.templates, home_path(user, path))
                }
                None => (&output.files, &mut output.templates, path.to_owned()),
            };
            if files.contains_key(path) || templates.insert(path.to_owned(), template).is_some() {
                return Err(rendering.invalid(at, format!("file `{shown}` is rendered twice")));
            }
            if next_login {
                output.next_login.insert(shown);
            }
            return Ok(NoneType);
        }
        let Some(text) = text.unpack_str() else {
            return Err(rendering.invalid(
                at,
                format!(
                    "file `{path}`: the text is {}, not a string",
                    text.get_type()
                ),
            ));
        };
        if owner.is_some() || group.is_some() {
            return Err(rendering.invalid(
                at,
                format!(
                    "file `{path}`: only a file with secrets in it has an owner; others are \
                     links into the store that anyone can read"
                ),
            ));
        }
        let file = File {
            text: text.to_owned(),
            mode: mode.unwrap_or(0o644),
        };
        let (files, templates, shown) = match user {
            Some(user) => {
                let home = output.homes.entry(user.to_owned()).or_default();
                (&mut home.files, &home.templates, home_path(user, path))
            }
            None => (&mut output.files, &output.templates, path.to_owned()),
        };
        if templates.contains_key(path) || files.insert(path.to_owned(), file).is_some() {
            return Err(rendering.invalid(at, format!("file `{shown}` is rendered twice")));
        }
        if next_login {
            output.next_login.insert(shown);
        }
        Ok(NoneType)
    }

    /// Stops the render with `message`, pointing at where the configuration sets the option at
    /// `path`, or the first option under it, so the error shows what to fix. `path` is dotted,
    /// or a list of keys for keys with dots in them. Without such a place, the error points at
    /// the module.
    fn error<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] path: StarValue<'v>,
        #[starlark(require = pos)] message: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let (rendering, at) = rendering(eval)?;
        let option = match path.unpack_str() {
            Some(dotted) => OptionPath::parse(dotted).map_err(|e| rendering.invalid(at, e))?,
            None => strings(path)?
                .iter()
                .fold(OptionPath::root(), |path, key| path.child(key.as_str())),
        };
        let located = rendering.config.borrow().locate(&option);
        Err(match located {
            Some(span) => rendering.stop(Stop::Error(
                Diagnostic::error(message)
                    .label(span, format!("`{option}` is set here"))
                    .note(format!("reported by {}", rendering.locate.path)),
            )),
            None => rendering.invalid(at, message),
        })
    }

    /// Declares a secret, kept encrypted in `file` and decrypted on the machine to
    /// `/run/secrets/<name>`, owned by `owner` and `group` with `mode`, and linked from `path`.
    #[allow(clippy::too_many_arguments)]
    fn secret<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] name: &str,
        #[starlark(require = named)] file: &str,
        #[starlark(require = named, default = "root")] owner: &str,
        #[starlark(require = named, default = NoneOr::None)] group: NoneOr<&str>,
        #[starlark(require = named, default = 0o400)] mode: u32,
        #[starlark(require = named, default = NoneOr::None)] path: NoneOr<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let (rendering, at) = rendering(eval)?;
        let secret = Secret {
            file: file.to_owned(),
            owner: owner.to_owned(),
            group: group.into_option().map(str::to_owned),
            mode,
            link: path.into_option().map(str::to_owned),
        };
        let replaced = rendering
            .output
            .borrow_mut()
            .secrets
            .insert(name.to_owned(), secret);
        if replaced.is_some() {
            return Err(rendering.invalid(at, format!("secret `{name}` is declared twice")));
        }
        Ok(NoneType)
    }

    /// Sets one top-level setting of the base system the backend provides, such as `audio`.
    fn base<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] key: &str,
        #[starlark(require = pos)] value: StarValue<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        insert_setting(eval, "base setting", key, value, |output| &mut output.base)
    }

    /// Adds a package whose units, users, tmpfiles, udev rules and D-Bus services join the
    /// system, such as `dbus`. Its programs are added too.
    fn service_package<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] name: &str,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let (rendering, _) = rendering(eval)?;
        let mut output = rendering.output.borrow_mut();
        output.packages.insert(name.to_owned());
        output.service_packages.insert(name.to_owned());
        Ok(NoneType)
    }

    /// Enables a unit a service package ships, as `systemctl enable` does: by its [Install]
    /// section, or wanted by the targets in `wanted_by` instead. With `user`, a user unit, for
    /// every user.
    fn enable<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] unit: &str,
        #[starlark(require = named, default = false)] user: bool,
        #[starlark(require = named)] wanted_by: Option<StarValue<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let (rendering, at) = rendering(eval)?;
        let targets = match wanted_by {
            None => Vec::new(),
            Some(value) => match value.unpack_str() {
                Some(target) => vec![target.to_owned()],
                None => strings(value)
                    .map_err(|e| rendering.invalid(at, format!("unit `{unit}`: wanted_by: {e}")))?,
            },
        };
        let mut output = rendering.output.borrow_mut();
        let enabled = if user {
            &mut output.enabled_user
        } else {
            &mut output.enabled
        };
        enabled.entry(unit.to_owned()).or_default().extend(targets);
        Ok(NoneType)
    }

    /// Puts a copy of `source` in `/run/wrappers/bin/<name>` with its own owner and mode, such
    /// as setuid root, for programs that run with more privilege than their caller.
    fn wrapper<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] name: &str,
        #[starlark(require = named)] source: &str,
        #[starlark(require = named)] owner: Option<&str>,
        #[starlark(require = named)] group: Option<&str>,
        #[starlark(require = named)] mode: Option<i32>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let (rendering, at) = rendering(eval)?;
        if name.contains('/') || !source.starts_with('/') {
            return Err(rendering.invalid(
                at,
                format!("wrapper `{name}`: a name has no `/`, and the source is an absolute path"),
            ));
        }
        let wrapper = Wrapper {
            source: source.to_owned(),
            owner: owner.unwrap_or("root").to_owned(),
            group: group.unwrap_or("root").to_owned(),
            mode: mode.map_or(0o4755, |mode| mode as u32),
        };
        let mut output = rendering.output.borrow_mut();
        if output.wrappers.insert(name.to_owned(), wrapper).is_some() {
            return Err(rendering.invalid(at, format!("wrapper `{name}` is made twice")));
        }
        Ok(NoneType)
    }

    /// Makes `name` a package built from `package`, by default the one called `name`, with other
    /// values for its build's `options` and the `patches` applied, paths in the configuration.
    fn build<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] name: &str,
        #[starlark(require = named, default = NoneOr::None)] package: NoneOr<&str>,
        #[starlark(require = named)] options: Option<StarValue<'v>>,
        #[starlark(require = named)] patches: Option<StarValue<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let (rendering, at) = rendering(eval)?;
        let options = match options.map(convert::from_starlark).transpose() {
            Ok(None) => std::collections::BTreeMap::new(),
            Ok(Some(Value::Table(options))) => options,
            Ok(Some(_)) => {
                let message = format!("build `{name}`: options must be a dictionary");
                return Err(rendering.invalid(at, message));
            }
            Err(e) => return Err(rendering.invalid(at, format!("build `{name}`: {e}"))),
        };
        let patches = patches.map(strings).transpose()?.unwrap_or_default();
        let build = Build {
            package: package.into_option().unwrap_or(name).to_owned(),
            options,
            patches,
        };
        let replaced = rendering
            .output
            .borrow_mut()
            .builds
            .insert(name.to_owned(), build);
        if replaced.is_some() {
            return Err(rendering.invalid(at, format!("build `{name}` is declared twice")));
        }
        Ok(NoneType)
    }

    /// Adds a package to the system, or with `user`, to that user's packages. `stable#git` is
    /// `git` from the source `stable`.
    fn package<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] name: &str,
        #[starlark(require = named)] user: Option<&str>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let (rendering, _) = rendering(eval)?;
        let mut output = rendering.output.borrow_mut();
        let packages = match user {
            Some(user) => &mut output.homes.entry(user.to_owned()).or_default().packages,
            None => &mut output.packages,
        };
        packages.insert(name.to_owned());
        Ok(NoneType)
    }
}

impl Module for StarlarkModule {
    fn name(&self) -> &str {
        &self.declared.name
    }

    fn options(&self) -> Vec<(OptionPath, OptionSpec)> {
        self.declared.options.clone()
    }

    fn writes(&self) -> Vec<OptionPath> {
        self.declared.writes.clone()
    }

    fn configurations(&self) -> &[ConfigKind] {
        &self.declared.configurations
    }

    fn render(&self, config: &mut dyn ConfigReader) -> Result<Output, RenderError> {
        if let Some(switch) = &self.declared.switch {
            match config.read(switch, None) {
                Ok(Some(Value::Bool(false))) => return Ok(Output::default()),
                Ok(_) => {}
                Err(ReadError::Failed) => return Err(RenderError::Failed),
                Err(ReadError::Invalid(message)) => {
                    return Err(RenderError::Error(Diagnostic::error(message)));
                }
            }
        }
        let rendering = Rendering {
            config: RefCell::new(config),
            output: RefCell::new(Output::default()),
            stop: RefCell::new(None),
            locate: &self.locate,
        };
        let ast = parse(&self.locate.path, &self.source);
        let result = StarModule::with_temp_heap(|module| {
            let loader = loader(&self.libraries);
            let file_loader = ReturnFileLoader { modules: &loader };
            let mut eval = Evaluator::new(&module);
            eval.enable_static_typechecking(true);
            eval.extra = Some(&rendering);
            eval.set_loader(&file_loader);
            set_time_limit(&mut eval);
            eval.eval_module(ast?, &self.globals)?;
            let render = module.get(RENDER).ok_or_else(|| {
                starlark::Error::new_other(anyhow::anyhow!(
                    "the module no longer calls `module(...)`"
                ))
            })?;
            let ctx = module.heap().alloc(Ctx);
            eval.eval_function(render, &[ctx], &[]).map(|_| ())
        });
        let Rendering { output, stop, .. } = rendering;
        match (stop.into_inner(), result) {
            (Some(Stop::Failed), _) => Err(RenderError::Failed),
            (Some(Stop::Error(diagnostic)), _) => Err(RenderError::Error(diagnostic)),
            (None, Err(e)) => Err(RenderError::Error(self.locate.diagnostic(&e))),
            (None, Ok(())) => Ok(output.into_inner()),
        }
    }
}

#[cfg(test)]
mod tests;
