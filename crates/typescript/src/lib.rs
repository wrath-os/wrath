//! Modules written in TypeScript, with types stripped and run on QuickJS.
//!
//! Types are erased in place by replacing them with whitespace, so line and column numbers in
//! errors match the TypeScript source. `tsc` checks the modules against `distro.d.ts`, which infers
//! the shape a module reads from its option declarations. At run time the prelude removes `Date`,
//! `Math.random`, `WeakRef` and `FinalizationRegistry`; values a module reads are frozen; and every
//! run has a time and memory limit. Object key order is defined by the language, so it doesn't
//! depend on hashing.
//!
//! A module file's default export is `module({ name, options, writes, render })`.

mod convert;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use distro_eval::{
    ConfigReader, Diagnostic, File, FileId, Level, Module, OptionPath, OptionSpec, Output,
    ReadError, RenderError, Setting, Sources, Span, Unit, Value, formats,
};
use rquickjs::function::{IntoJsFunc, Opt};
use rquickjs::{Context, Ctx, Exception, Function, Object, Persistent, Runtime, Value as JsValue};

const PRELUDE: &str = include_str!("prelude.js");
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
#[cfg(not(test))]
const TIME_LIMIT: Duration = Duration::from_secs(10);
#[cfg(test)]
const TIME_LIMIT: Duration = Duration::from_secs(1);

/// Loads a module from source. `path` is how errors refer to the file.
pub fn load(
    sources: &mut Sources,
    path: &str,
    text: String,
) -> Result<Box<dyn Module>, Diagnostic> {
    let file = sources.add(path, text);
    let lines = Lines::new(file, path, sources.text(file));
    let script = strip_types(path, sources.text(file)).map_err(|(message, line)| {
        Diagnostic::error(message).label_opt(line.and_then(|line| lines.span(line)), "here")
    })?;

    let cannot_start = |e: rquickjs::Error| Diagnostic::error(format!("cannot start QuickJS: {e}"));
    let deadline = Arc::new(Mutex::new(Instant::now() + TIME_LIMIT));
    let runtime = Runtime::new().map_err(cannot_start)?;
    runtime.set_memory_limit(MEMORY_LIMIT);
    let interrupt_deadline = Arc::clone(&deadline);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        interrupt_deadline
            .lock()
            .is_ok_and(|deadline| Instant::now() > *deadline)
    })));
    let context = Context::full(&runtime).map_err(cannot_start)?;

    let declared = context.with(|ctx| -> Result<Declared, Diagnostic> {
        let fail = |e: rquickjs::Error| lines.diagnostic(&ctx, &e);
        ctx.eval::<(), _>(PRELUDE).map_err(fail)?;
        install_formats(&ctx).map_err(fail)?;
        let module = rquickjs::Module::declare(ctx.clone(), path, script).map_err(fail)?;
        let (module, promise) = module.eval().map_err(fail)?;
        promise.finish::<()>().map_err(fail)?;
        let exported: Object = module
            .namespace()
            .and_then(|namespace| namespace.get("default"))
            .map_err(fail)?;
        Declared::from_object(&ctx, &exported)
            .map_err(|message| Diagnostic::error(format!("{path}: {message}")))
    })?;

    Ok(Box::new(TypeScriptModule {
        _runtime: runtime,
        context,
        declared,
        lines: Rc::new(lines),
        deadline,
    }))
}

/// Erases TypeScript syntax, keeping every other character where it was. Errors come with the
/// 1-based line they are on, when known.
fn strip_types(path: &str, source: &str) -> Result<String, (String, Option<usize>)> {
    use swc_common::errors::{DiagnosticBuilder, Emitter, Handler};
    use swc_common::sync::Lrc;
    use swc_common::{GLOBALS, Globals, SourceMap};

    /// An error message and where it starts.
    type Located = (String, Option<swc_common::BytePos>);

    /// Keeps the first error.
    struct First(Arc<Mutex<Option<Located>>>);

    impl Emitter for First {
        fn emit(&mut self, diagnostic: &mut DiagnosticBuilder<'_>) {
            if let Ok(mut first) = self.0.lock() {
                first.get_or_insert_with(|| {
                    (
                        diagnostic.message(),
                        diagnostic.span.primary_span().map(|span| span.lo),
                    )
                });
            }
        }
    }

    let first = Arc::new(Mutex::new(None));
    let map: Lrc<SourceMap> = Lrc::default();
    let handler = Handler::with_emitter(true, false, Box::new(First(Arc::clone(&first))));
    let options = swc_ts_fast_strip::Options {
        module: Some(true),
        filename: Some(path.to_owned()),
        ..Default::default()
    };
    let result = GLOBALS.set(&Globals::new(), || {
        swc_ts_fast_strip::operate(&map, &handler, source.to_owned(), options)
    });
    let located = first.lock().ok().and_then(|first| first.clone());
    match (result, located) {
        (Ok(output), None) => Ok(output.code),
        (_, Some((message, position))) => {
            let line = position.map(|position| map.lookup_char_pos(position).line);
            Err((message, line))
        }
        (Err(error), None) => Err((error.to_string(), None)),
    }
}

fn install_formats(ctx: &Ctx<'_>) -> rquickjs::Result<()> {
    let formats = Object::new(ctx.clone())?;
    formats.set(
        "json",
        Function::new(ctx.clone(), |ctx: Ctx<'_>, value: JsValue<'_>| {
            let value = convert::from_js(&value).map_err(|e| Exception::throw_message(&ctx, &e))?;
            formats::json(&value).map_err(|e| Exception::throw_message(&ctx, &e))
        })?,
    )?;
    ctx.globals().set("formats", formats)
}

/// What a module file declares.
struct Declared {
    name: String,
    options: Vec<(OptionPath, OptionSpec)>,
    writes: Vec<OptionPath>,
    render: Persistent<Function<'static>>,
}

impl Declared {
    fn from_object<'js>(ctx: &Ctx<'js>, exported: &Object<'js>) -> Result<Self, String> {
        let get = |key: &str| convert::get(exported, key);
        let name = get("name")?
            .as_string()
            .and_then(|name| name.to_string().ok())
            .ok_or("the module needs a `name` string")?;
        let options = get("options")?
            .into_object()
            .ok_or("`options` must be an object")?;
        let mut declared = Vec::new();
        for pair in options.props::<String, JsValue<'_>>() {
            let (path, spec) = pair.map_err(|e| e.to_string())?;
            declared.push((OptionPath::parse(&path)?, convert::spec(&spec)?));
        }
        declared.sort_by(|a, b| a.0.cmp(&b.0));
        let writes = convert::strings(&get("writes")?)?
            .iter()
            .map(|path| OptionPath::parse(path))
            .collect::<Result<_, _>>()?;
        let render = get("render")?
            .into_function()
            .ok_or("the module needs a `render` function")?;
        Ok(Self {
            name,
            options: declared,
            writes,
            render: Persistent::save(ctx, render),
        })
    }
}

/// Line positions of a module's source, for turning QuickJS line numbers into spans.
struct Lines {
    file: FileId,
    path: String,
    lines: Vec<(usize, usize)>,
}

impl Lines {
    fn new(file: FileId, path: &str, text: &str) -> Self {
        let mut lines = Vec::new();
        let mut start = 0;
        for line in text.split_inclusive('\n') {
            let content = line.trim_end();
            let indent = content.len() - content.trim_start().len();
            lines.push((start + indent, start + content.len()));
            start += line.len();
        }
        Self {
            file,
            path: path.to_owned(),
            lines,
        }
    }

    fn span(&self, line: usize) -> Option<Span> {
        let (start, end) = *self.lines.get(line.checked_sub(1)?)?;
        Some(Span {
            file: self.file,
            start,
            end,
        })
    }

    /// The first line in a stack trace that is in this module.
    fn line_in_stack(&self, stack: &str) -> Option<usize> {
        let marker = format!("{}:", self.path);
        stack.lines().find_map(|frame| {
            let rest = &frame[frame.find(&marker)? + marker.len()..];
            rest.split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
    }

    /// A diagnostic for a QuickJS error, pointing at the module line the exception came from.
    fn diagnostic(&self, ctx: &Ctx<'_>, error: &rquickjs::Error) -> Diagnostic {
        let rquickjs::Error::Exception = error else {
            return Diagnostic::error(error.to_string());
        };
        let caught = ctx.catch();
        match caught.as_exception() {
            Some(exception) => {
                let message = exception.message().unwrap_or_default();
                let line = exception
                    .stack()
                    .and_then(|stack| self.line_in_stack(&stack));
                Diagnostic::error(message).label_opt(line.and_then(|line| self.span(line)), "here")
            }
            None => Diagnostic::error("the module threw a value that is not an Error"),
        }
    }

    /// Where in this module the running host function was called from.
    fn caller(&self, ctx: &Ctx<'_>) -> Option<Span> {
        let stack: String = ctx.eval("new Error().stack").ok()?;
        self.span(self.line_in_stack(&stack)?)
    }
}

/// Fields drop in order: the saved render function before the context, and the context before the
/// runtime, or QuickJS aborts on values still alive.
struct TypeScriptModule {
    declared: Declared,
    context: Context,
    _runtime: Runtime,
    lines: Rc<Lines>,
    deadline: Arc<Mutex<Instant>>,
}

/// The state host functions share during one run of a module's render function.
///
/// QuickJS host functions must be `'static`, so they can't borrow the evaluator's config reader
/// for the length of a render. Instead a render runs against a snapshot of the values read so
/// far. When it reads something new, the host function records the request and throws; the host
/// reads the value outside JavaScript and runs the render again. Renders are deterministic, so
/// the last run is the same as one uninterrupted run.
#[derive(Default)]
struct RenderState {
    reads: BTreeMap<OptionPath, Option<Value>>,
    /// A read the snapshot doesn't have yet, and where the module made it.
    needed: Option<(OptionPath, Option<Span>)>,
    output: Output,
    error: Option<Diagnostic>,
}

type Shared = Rc<RefCell<RenderState>>;

/// Records an error in a module's use of the host API and returns the exception that unwinds it.
fn invalid(state: &Shared, ctx: &Ctx<'_>, at: Option<Span>, message: String) -> rquickjs::Error {
    let diagnostic = Diagnostic::error(message.clone()).label_opt(at, "here");
    state.borrow_mut().error.get_or_insert(diagnostic);
    Exception::throw_message(ctx, &message)
}

impl TypeScriptModule {
    /// Runs the render function once against the reads in `state`.
    fn run(&self, state: &Shared) -> Result<(), Diagnostic> {
        self.context.with(|ctx| {
            let attempt = || -> rquickjs::Result<()> {
                let host = host_object(&ctx, state, &self.lines)?;
                let render = self.declared.render.clone().restore(&ctx)?;
                render.call::<_, ()>((host,))
            };
            attempt().map_err(|e| self.lines.diagnostic(&ctx, &e))
        })
    }
}

/// The `ctx` object a render function receives.
fn host_object<'js>(
    ctx: &Ctx<'js>,
    state: &Shared,
    lines: &Rc<Lines>,
) -> rquickjs::Result<Object<'js>> {
    let host = Object::new(ctx.clone())?;
    let handles = || (Rc::clone(state), Rc::clone(lines));

    let (state, lines) = handles();
    define(&host, "read", move |ctx: Ctx<'js>, path: String| {
        let at = lines.caller(&ctx);
        let path = OptionPath::parse(&path).map_err(|e| invalid(&state, &ctx, at, e))?;
        let known = state.borrow().reads.get(&path).cloned();
        match known {
            Some(Some(value)) => convert::to_js(&ctx, &value),
            Some(None) => Ok(JsValue::new_undefined(ctx.clone())),
            None => {
                state.borrow_mut().needed.get_or_insert((path, at));
                Err(Exception::throw_message(
                    &ctx,
                    "the host needs to read more configuration",
                ))
            }
        }
    })?;

    for (name, level, removes) in [
        ("set", Level::Normal, false),
        ("default", Level::Default, false),
        ("remove", Level::Normal, true),
    ] {
        let (state, lines) = handles();
        define(
            &host,
            name,
            move |ctx: Ctx<'js>, path: String, value: JsValue<'js>| {
                let at = lines.caller(&ctx);
                let path = OptionPath::parse(&path).map_err(|e| invalid(&state, &ctx, at, e))?;
                let mut value =
                    convert::from_js(&value).map_err(|e| invalid(&state, &ctx, at, e))?;
                if removes {
                    value = Value::Table([("remove".to_owned(), value)].into_iter().collect());
                }
                state.borrow_mut().output.settings.push(Setting {
                    path,
                    value,
                    level,
                    at,
                });
                Ok::<(), rquickjs::Error>(())
            },
        )?;
    }

    let (state, lines) = handles();
    define(
        &host,
        "unit",
        move |ctx: Ctx<'js>, name: String, sections: JsValue<'js>| {
            let at = lines.caller(&ctx);
            let unit = convert::from_js(&sections)
                .and_then(|sections| Unit::from_value(&sections))
                .map_err(|e| invalid(&state, &ctx, at, format!("unit `{name}`: {e}")))?;
            let replaced = state.borrow_mut().output.units.insert(name.clone(), unit);
            if replaced.is_some() {
                let message = format!("unit `{name}` is rendered twice");
                return Err(invalid(&state, &ctx, at, message));
            }
            Ok(())
        },
    )?;

    let (state, lines) = handles();
    define(
        &host,
        "file",
        move |ctx: Ctx<'js>, path: String, text: String, mode: Opt<u32>| {
            let at = lines.caller(&ctx);
            let file = File {
                text,
                mode: mode.0.unwrap_or(0o644),
            };
            let replaced = state.borrow_mut().output.files.insert(path.clone(), file);
            if replaced.is_some() {
                let message = format!("file `{path}` is rendered twice");
                return Err(invalid(&state, &ctx, at, message));
            }
            Ok(())
        },
    )?;

    let (state, _) = handles();
    define(&host, "package", move |name: String| {
        state.borrow_mut().output.packages.insert(name);
    })?;

    Ok(host)
}

/// Sets `name` on `host` to a host function.
fn define<'js, P>(
    host: &Object<'js>,
    name: &str,
    function: impl IntoJsFunc<'js, P> + 'js,
) -> rquickjs::Result<()> {
    host.set(name, Function::new(host.ctx().clone(), function)?)
}

impl Module for TypeScriptModule {
    fn name(&self) -> &str {
        &self.declared.name
    }

    fn options(&self) -> Vec<(OptionPath, OptionSpec)> {
        self.declared.options.clone()
    }

    fn writes(&self) -> Vec<OptionPath> {
        self.declared.writes.clone()
    }

    fn render(&self, config: &mut dyn ConfigReader) -> Result<Output, RenderError> {
        let state: Shared = Rc::default();
        loop {
            if let Ok(mut deadline) = self.deadline.lock() {
                *deadline = Instant::now() + TIME_LIMIT;
            }
            {
                let mut state = state.borrow_mut();
                state.needed = None;
                state.output = Output::default();
            }
            let result = self.run(&state);
            let (needed, error) = {
                let mut state = state.borrow_mut();
                (state.needed.take(), state.error.take())
            };
            if let Some(diagnostic) = error {
                return Err(RenderError::Error(diagnostic));
            }
            let Some((path, at)) = needed else {
                return match result {
                    Ok(()) => Ok(std::mem::take(&mut state.borrow_mut().output)),
                    Err(diagnostic) => Err(RenderError::Error(diagnostic)),
                };
            };
            let value = match config.read(&path, at) {
                Ok(value) => value,
                Err(ReadError::Invalid(message)) => {
                    return Err(RenderError::Error(
                        Diagnostic::error(message).label_opt(at, "here"),
                    ));
                }
                Err(ReadError::Failed) => return Err(RenderError::Failed),
            };
            state.borrow_mut().reads.insert(path, value);
        }
    }
}

#[cfg(test)]
mod tests;
