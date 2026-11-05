//! Modules written in Luau.
//!
//! Luau is locked down to behave deterministically. The prelude replaces `pairs` with sorted
//! iteration and removes or wraps everything that exposes addresses, time or randomness.
//! Iterating a table directly (`for k, v in t do`) is rejected before the module runs, because
//! its order can't be controlled. Values a module reads are read-only, and every run has a time
//! and memory limit.
//!
//! A module file returns a table:
//!
//! ```lua
//! return {
//!     name = "syncthing",
//!     options = { ["services.syncthing"] = types.map(instance) },
//!     writes = { "users" },
//!     render = function(ctx) ... end,
//! }
//! ```

mod convert;
mod lint;

use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use distro_eval::{
    ConfigReader, Diagnostic, File, FileId, Level, ListMerge, Module, OptionPath, OptionSpec,
    Output, ReadError, RenderError, Setting, Sources, Span, Type, Unit, Value, formats,
};
use mlua::{AnyUserData, Function, Lua, Table, UserData, Value as LuaValue, VmState};

const PRELUDE: &str = include_str!("prelude.luau");
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
    let text = sources.text(file);
    if let Some(range) = lint::direct_iteration(text) {
        return Err(Diagnostic::error("iteration order is not defined here")
            .label(
                Span::new(file, range),
                "iterate with `pairs(...)` or `ipairs(...)`",
            )
            .note("help: `pairs` visits keys in sorted order"));
    }
    let lines = Lines::new(file, path, text);
    let deadline = Arc::new(Mutex::new(Instant::now() + TIME_LIMIT));
    let lua =
        sandbox(&deadline).map_err(|e| Diagnostic::error(format!("cannot start Luau: {e}")))?;
    let exported: Table = lua
        .load(text)
        .set_name(format!("={path}"))
        .eval()
        .map_err(|e| lines.diagnostic(&e))?;
    let module = LuauModule::from_table(&exported, lua, lines, deadline)
        .map_err(|message| Diagnostic::error(format!("{path}: {message}")))?;
    Ok(Box::new(module))
}

fn sandbox(deadline: &Arc<Mutex<Instant>>) -> mlua::Result<Lua> {
    let lua = Lua::new();
    lua.set_memory_limit(MEMORY_LIMIT)?;
    let deadline = Arc::clone(deadline);
    lua.set_interrupt(move |_| {
        let expired = deadline
            .lock()
            .is_ok_and(|deadline| Instant::now() > *deadline);
        if expired {
            Err(mlua::Error::runtime(format!(
                "the module ran for longer than {} seconds",
                TIME_LIMIT.as_secs()
            )))
        } else {
            Ok(VmState::Continue)
        }
    });
    lua.load(PRELUDE).set_name("=prelude").exec()?;
    install_std(&lua)?;
    lua.sandbox(true)?;
    Ok(lua)
}

/// A type in a module's option declarations.
#[derive(Clone)]
struct LuaType(Type);

impl UserData for LuaType {}

/// An option: a type with a default, documentation and flags.
#[derive(Clone)]
struct LuaOption(OptionSpec);

impl UserData for LuaOption {}

fn type_of(data: &AnyUserData) -> mlua::Result<Type> {
    match data.borrow::<LuaType>() {
        Ok(ty) => Ok(ty.0.clone()),
        Err(_) => Err(mlua::Error::runtime(
            "expected a type such as `types.str`; options only go in records, maps and `options`",
        )),
    }
}

/// A type or an option, as an option.
fn spec_of(data: &AnyUserData) -> mlua::Result<OptionSpec> {
    if let Ok(option) = data.borrow::<LuaOption>() {
        return Ok(option.0.clone());
    }
    type_of(data).map(OptionSpec::new)
}

fn install_std(lua: &Lua) -> mlua::Result<()> {
    let types = lua.create_table()?;
    for (name, ty) in [
        ("bool", Type::Bool),
        ("int", Type::int()),
        ("float", Type::Float),
        ("str", Type::Str),
        ("path", Type::Path),
        ("port", Type::port()),
        ("freeform", Type::Freeform),
    ] {
        types.set(name, LuaType(ty))?;
    }
    for (name, merge) in [
        ("list", ListMerge::Atomic),
        ("set", ListMerge::Set),
        ("ordered", ListMerge::Ordered),
    ] {
        types.set(
            name,
            lua.create_function(move |_, item: AnyUserData| {
                Ok(LuaType(Type::list(type_of(&item)?, merge)))
            })?,
        )?;
    }
    types.set(
        "map",
        lua.create_function(|_, item: AnyUserData| Ok(LuaType(Type::map(spec_of(&item)?))))?,
    )?;
    types.set(
        "record",
        lua.create_function(|_, fields: Table| {
            let mut record = Vec::new();
            for pair in fields.pairs::<String, AnyUserData>() {
                let (name, field) = pair?;
                record.push((name, spec_of(&field)?));
            }
            Ok(LuaType(Type::record(
                record
                    .iter()
                    .map(|(name, spec)| (name.as_str(), spec.clone())),
            )))
        })?,
    )?;
    types.set(
        "enum",
        lua.create_function(|_, choices: Vec<String>| Ok(LuaType(Type::Enum(choices))))?,
    )?;
    types.set(
        "range",
        lua.create_function(|_, (min, max): (i64, i64)| {
            Ok(LuaType(Type::Int {
                min: Some(min),
                max: Some(max),
            }))
        })?,
    )?;
    lua.globals().set("types", types)?;

    lua.globals().set(
        "option",
        lua.create_function(|_, (ty, settings): (AnyUserData, Option<Table>)| {
            let mut spec = OptionSpec::new(type_of(&ty)?);
            let settings = match settings {
                Some(settings) => settings.pairs::<String, LuaValue>().collect::<mlua::Result<Vec<_>>>()?,
                None => Vec::new(),
            };
            for (key, value) in settings {
                match key.as_str() {
                    "default" => {
                        spec.default = Some(convert::from_lua(&value).map_err(mlua::Error::runtime)?);
                    }
                    "optional" => spec.optional = bool_setting(&key, &value)?,
                    "runtime" => spec.runtime = bool_setting(&key, &value)?,
                    "doc" => match value {
                        LuaValue::String(doc) => spec.doc = doc.to_str()?.to_owned(),
                        _ => return Err(mlua::Error::runtime("`doc` must be a string")),
                    },
                    other => {
                        return Err(mlua::Error::runtime(format!(
                            "unknown option setting `{other}`; expected default, optional, runtime or doc"
                        )));
                    }
                }
            }
            Ok(LuaOption(spec))
        })?,
    )?;

    let formats = lua.create_table()?;
    formats.set(
        "json",
        lua.create_function(|_, value: LuaValue| {
            let value = convert::from_lua(&value).map_err(mlua::Error::runtime)?;
            formats::json(&value).map_err(mlua::Error::runtime)
        })?,
    )?;
    lua.globals().set("formats", formats)?;
    Ok(())
}

fn bool_setting(key: &str, value: &LuaValue) -> mlua::Result<bool> {
    match value {
        LuaValue::Boolean(b) => Ok(*b),
        _ => Err(mlua::Error::runtime(format!(
            "`{key}` must be true or false"
        ))),
    }
}

/// Line positions of a module's source, for turning Luau's line numbers into spans.
struct Lines {
    file: FileId,
    /// The chunk name Luau puts in front of error messages, `path:`.
    prefix: String,
    /// Each line's first non-blank byte and end.
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
            prefix: format!("{path}:"),
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

    /// A diagnostic for a Luau error, pointing at the line in the message when there is one.
    fn diagnostic(&self, error: &mlua::Error) -> Diagnostic {
        let message = innermost(error);
        let message = message.lines().next().unwrap_or_default();
        let located = message.strip_prefix(&self.prefix).and_then(|rest| {
            let (line, text) = rest.split_once(": ")?;
            Some((self.span(line.parse().ok()?)?, text))
        });
        match located {
            Some((span, text)) => Diagnostic::error(text).label(span, "here"),
            None => Diagnostic::error(message),
        }
    }
}

fn innermost(error: &mlua::Error) -> String {
    match error {
        mlua::Error::CallbackError { cause, .. } => innermost(cause),
        mlua::Error::RuntimeError(message) | mlua::Error::SyntaxError { message, .. } => {
            message.clone()
        }
        other => other.to_string(),
    }
}

struct LuauModule {
    lua: Lua,
    name: String,
    options: Vec<(OptionPath, OptionSpec)>,
    writes: Vec<OptionPath>,
    render: Function,
    lines: Lines,
    deadline: Arc<Mutex<Instant>>,
}

impl LuauModule {
    fn from_table(
        exported: &Table,
        lua: Lua,
        lines: Lines,
        deadline: Arc<Mutex<Instant>>,
    ) -> Result<Self, String> {
        let mut name = None;
        let mut options = Vec::new();
        let mut writes = Vec::new();
        let mut render = None;
        for pair in exported.pairs::<String, LuaValue>() {
            let (key, value) = pair.map_err(|e| e.to_string())?;
            match (key.as_str(), value) {
                ("name", LuaValue::String(text)) => {
                    name = Some(text.to_str().map_err(|e| e.to_string())?.to_owned());
                }
                ("options", LuaValue::Table(table)) => {
                    for pair in table.pairs::<String, AnyUserData>() {
                        let (path, spec) = pair.map_err(|e| e.to_string())?;
                        let path = OptionPath::parse(&path)?;
                        options.push((path, spec_of(&spec).map_err(|e| e.to_string())?));
                    }
                }
                ("writes", LuaValue::Table(table)) => {
                    for path in table.sequence_values::<String>() {
                        writes.push(OptionPath::parse(&path.map_err(|e| e.to_string())?)?);
                    }
                }
                ("render", LuaValue::Function(function)) => render = Some(function),
                (key @ ("name" | "options" | "writes" | "render"), value) => {
                    return Err(format!("`{key}` has the wrong type: {}", value.type_name()));
                }
                (key, _) => return Err(format!("unknown key `{key}` in the module table")),
            }
        }
        options.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Self {
            lua,
            name: name.ok_or("the module table needs a `name`")?,
            options,
            writes,
            render: render.ok_or("the module table needs a `render` function")?,
            lines,
            deadline,
        })
    }

    /// Where in the module the running callback was called from.
    fn caller(&self, lua: &Lua) -> Option<Span> {
        let line = lua
            .inspect_stack(1, |debug| debug.current_line())
            .flatten()?;
        self.lines.span(line)
    }
}

/// Why a render stopped early from inside a callback.
enum Stop {
    /// Something the module read failed; the error is already reported.
    Failed,
    Error(Diagnostic),
}

#[derive(Default)]
struct RenderState {
    output: Output,
    stop: Option<Stop>,
}

/// Records why a callback failed and returns the Luau error that unwinds the module.
fn stop(state: &RefCell<RenderState>, stop: Stop) -> mlua::Error {
    let message = match &stop {
        Stop::Failed => "stopped because a value it read failed".to_owned(),
        Stop::Error(diagnostic) => diagnostic.message.clone(),
    };
    state.borrow_mut().stop.get_or_insert(stop);
    mlua::Error::runtime(message)
}

impl Module for LuauModule {
    fn name(&self) -> &str {
        &self.name
    }

    fn options(&self) -> Vec<(OptionPath, OptionSpec)> {
        self.options.clone()
    }

    fn writes(&self) -> Vec<OptionPath> {
        self.writes.clone()
    }

    fn render(&self, config: &mut dyn ConfigReader) -> Result<Output, RenderError> {
        if let Ok(mut deadline) = self.deadline.lock() {
            *deadline = Instant::now() + TIME_LIMIT;
        }
        let state = RefCell::new(RenderState::default());
        let state = &state;
        let invalid = |at: Option<Span>, message: String| {
            stop(
                state,
                Stop::Error(Diagnostic::error(message).label_opt(at, "here")),
            )
        };
        let result = self.lua.scope(|scope| {
            let ctx = self.lua.create_table()?;
            ctx.set(
                "read",
                scope.create_function_mut(move |lua, path: String| {
                    let at = self.caller(lua);
                    let path = OptionPath::parse(&path).map_err(|e| invalid(at, e))?;
                    match config.read(&path, at) {
                        Ok(Some(value)) => convert::to_lua(lua, &value),
                        Ok(None) => Ok(LuaValue::Nil),
                        Err(ReadError::Invalid(message)) => Err(invalid(at, message)),
                        Err(ReadError::Failed) => Err(stop(state, Stop::Failed)),
                    }
                })?,
            )?;
            for (name, level, removes) in [
                ("set", Level::Normal, false),
                ("default", Level::Default, false),
                ("remove", Level::Normal, true),
            ] {
                ctx.set(
                    name,
                    scope.create_function(move |lua, (path, value): (String, LuaValue)| {
                        let at = self.caller(lua);
                        let path = OptionPath::parse(&path).map_err(|e| invalid(at, e))?;
                        let mut value = convert::from_lua(&value).map_err(|e| invalid(at, e))?;
                        if removes {
                            value =
                                Value::Table([("remove".to_owned(), value)].into_iter().collect());
                        }
                        state.borrow_mut().output.settings.push(Setting {
                            path,
                            value,
                            level,
                            at,
                        });
                        Ok(())
                    })?,
                )?;
            }
            ctx.set(
                "unit",
                scope.create_function(|lua, (name, sections): (String, LuaValue)| {
                    let at = self.caller(lua);
                    let unit = convert::from_lua(&sections)
                        .and_then(|sections| Unit::from_value(&sections))
                        .map_err(|e| invalid(at, format!("unit `{name}`: {e}")))?;
                    let replaced = state.borrow_mut().output.units.insert(name.clone(), unit);
                    if replaced.is_some() {
                        return Err(invalid(at, format!("unit `{name}` is rendered twice")));
                    }
                    Ok(())
                })?,
            )?;
            ctx.set(
                "file",
                scope.create_function(
                    |lua, (path, text, mode): (String, String, Option<u32>)| {
                        let at = self.caller(lua);
                        let file = File {
                            text,
                            mode: mode.unwrap_or(0o644),
                        };
                        let replaced = state.borrow_mut().output.files.insert(path.clone(), file);
                        if replaced.is_some() {
                            return Err(invalid(at, format!("file `{path}` is rendered twice")));
                        }
                        Ok(())
                    },
                )?,
            )?;
            ctx.set(
                "package",
                scope.create_function(|_, name: String| {
                    state.borrow_mut().output.packages.insert(name);
                    Ok(())
                })?,
            )?;
            self.render.call::<()>(ctx)
        });
        let RenderState { output, stop } = state.take();
        match (stop, result) {
            (Some(Stop::Failed), _) => Err(RenderError::Failed),
            (Some(Stop::Error(diagnostic)), _) => Err(RenderError::Error(diagnostic)),
            (None, Err(e)) => Err(RenderError::Error(self.lines.diagnostic(&e))),
            (None, Ok(())) => Ok(output),
        }
    }
}

#[cfg(test)]
mod tests;
