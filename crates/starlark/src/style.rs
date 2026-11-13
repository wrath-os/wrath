//! Build styles (decision 0025): a typed Starlark function that makes a recipe's script, extra
//! dependencies and sources from the recipe, its options and what it reads from its source.
//!
//! ```python
//! def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
//!     lock = decode.toml(recipe.read("Cargo.lock"))
//!     return {"script": "cargo build --release\n", "deps": ["rust-bin"], "sources": []}
//!
//! style(name = "cargo", locks = ["Cargo.lock"], options = {...}, build = build)
//! ```

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::collections::SmallMap;
use starlark::environment::{
    FrozenModule, Globals, GlobalsBuilder, Methods, MethodsBuilder, MethodsStatic,
    Module as StarModule,
};
use starlark::eval::Evaluator;
use starlark::starlark_module;
use starlark::starlark_simple_value;
use starlark::values::none::NoneType;
use starlark::values::{
    Heap, NoSerialize, StarlarkPagablePanic, StarlarkValue, Value as StarValue,
};
use starlark_derive::starlark_value;
use wrath_eval::{OptionSpec, Value};

use crate::{base_globals, convert, parse, set_time_limit, spec_of, strings};

/// The module variable that holds the build function after `style(...)` runs.
const BUILD: &str = "style_build";

/// A loaded style, evaluated once and run for each recipe that uses it.
pub struct Style {
    name: String,
    locks: Vec<String>,
    files: Vec<String>,
    options: Vec<(String, OptionSpec)>,
    module: FrozenModule,
}

/// What a style made for one recipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub script: String,
    /// Packages the build needs, added to the recipe's own.
    pub deps: Vec<String>,
    /// Sources added after the recipe's own, such as one per crate of a lock.
    pub sources: Vec<Source>,
    /// The paths it read from the recipe's first source, in the order it read them.
    pub reads: Vec<String>,
}

/// A source a style adds, as a recipe's `sources` entries are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub url: String,
    pub into: Option<String>,
    pub sha256: Option<String>,
    pub file: bool,
}

/// What `style(...)` declared.
struct Declared {
    name: String,
    locks: Vec<String>,
    files: Vec<String>,
    options: Vec<(String, OptionSpec)>,
}

/// The evaluator's `extra` while a style file loads.
#[derive(ProvidesStaticType)]
struct Loading(RefCell<Option<Declared>>);

/// The evaluator's `extra` while a style runs for a recipe.
#[derive(ProvidesStaticType)]
struct Running<'a> {
    read: &'a dyn Fn(&str) -> Result<String, String>,
    reads: RefCell<Vec<String>>,
}

/// The recipe a style runs for.
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
struct Recipe {
    id: String,
    version: String,
}

starlark_simple_value!(Recipe);

impl fmt::Display for Recipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<recipe>")
    }
}

#[starlark_value(type = "Recipe")]
impl<'v> StarlarkValue<'v> for Recipe {
    fn get_methods() -> Option<&'static Methods> {
        static METHODS: MethodsStatic = MethodsStatic::new("recipe", recipe_methods);
        Some(METHODS.methods())
    }
}

fn running<'a>(eval: &'a Evaluator<'_, '_, '_>) -> anyhow::Result<&'a Running<'a>> {
    eval.extra
        .and_then(|extra| extra.downcast_ref::<Running>())
        .ok_or_else(|| anyhow::anyhow!("a recipe is only there while a style builds"))
}

#[starlark_module]
fn recipe_methods(builder: &mut MethodsBuilder) {
    /// The recipe's name.
    #[starlark(attribute)]
    fn id(#[starlark(this)] this: &Recipe) -> anyhow::Result<String> {
        Ok(this.id.clone())
    }

    /// The recipe's version, or "" when it has none.
    #[starlark(attribute)]
    fn version(#[starlark(this)] this: &Recipe) -> anyhow::Result<String> {
        Ok(this.version.clone())
    }

    /// The text of a file in the recipe's first source, by its path under the top directory, or
    /// `default` when the source has no such file and one is given.
    fn read<'v>(
        #[starlark(this)] _this: StarValue<'v>,
        #[starlark(require = pos)] path: &str,
        #[starlark(require = named)] default: Option<StarValue<'v>>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<StarValue<'v>> {
        let running = running(eval)?;
        running.reads.borrow_mut().push(path.to_owned());
        match ((running.read)(path), default) {
            (Ok(text), _) => Ok(eval.heap().alloc(text)),
            (Err(_), Some(default)) => Ok(default),
            (Err(e), None) => Err(anyhow::Error::msg(e)),
        }
    }
}

#[starlark_module]
#[starlark_types(Recipe as Recipe)]
fn style_globals(builder: &mut GlobalsBuilder) {
    /// Declares the style. Every style file calls this once. `locks` and `files` are the files at
    /// the top of a source that pick this style for a recipe that names none; a lock wins over a
    /// build file.
    fn style<'v>(
        #[starlark(require = named)] name: &str,
        #[starlark(require = named)] locks: Option<StarValue<'v>>,
        #[starlark(require = named)] files: Option<StarValue<'v>>,
        #[starlark(require = named)] options: Option<SmallMap<String, StarValue<'v>>>,
        #[starlark(require = named)] build: StarValue<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let list = |value: Option<StarValue<'v>>| value.map_or(Ok(Vec::new()), strings);
        let declared = Declared {
            name: name.to_owned(),
            locks: list(locks)?,
            files: list(files)?,
            options: options
                .unwrap_or_default()
                .into_iter()
                .map(|(name, spec)| Ok((name, spec_of(spec)?)))
                .collect::<anyhow::Result<_>>()?,
        };
        let loading = eval
            .extra
            .and_then(|extra| extra.downcast_ref::<Loading>())
            .ok_or_else(|| anyhow::anyhow!("`style(...)` is only called by a style file"))?;
        if loading.0.borrow().is_some() {
            anyhow::bail!("`style(...)` is called twice");
        }
        *loading.0.borrow_mut() = Some(declared);
        eval.module().set(BUILD, build);
        Ok(NoneType)
    }
}

#[starlark_module]
fn decode_functions(builder: &mut GlobalsBuilder) {
    /// A TOML document as dictionaries and lists. Dates and times come as their text.
    fn toml<'v>(
        #[starlark(require = pos)] text: &str,
        heap: Heap<'v>,
    ) -> anyhow::Result<StarValue<'v>> {
        let table: toml::Table = text.parse().map_err(|e| anyhow::anyhow!("TOML: {e}"))?;
        Ok(convert::to_starlark(
            heap,
            &from_toml(toml::Value::Table(table)),
        ))
    }

    /// A JSON document as dictionaries and lists.
    fn json<'v>(
        #[starlark(require = pos)] text: &str,
        heap: Heap<'v>,
    ) -> anyhow::Result<StarValue<'v>> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| anyhow::anyhow!("JSON: {e}"))?;
        Ok(convert::to_starlark(heap, &from_json(value)?))
    }
}

fn from_toml(value: toml::Value) -> Value {
    match value {
        toml::Value::String(text) => Value::Str(text),
        toml::Value::Integer(int) => Value::Int(int),
        toml::Value::Float(float) => Value::Float(float),
        toml::Value::Boolean(b) => Value::Bool(b),
        toml::Value::Datetime(when) => Value::Str(when.to_string()),
        toml::Value::Array(items) => Value::List(items.into_iter().map(from_toml).collect()),
        toml::Value::Table(table) => Value::Table(
            table
                .into_iter()
                .map(|(key, item)| (key, from_toml(item)))
                .collect(),
        ),
    }
}

/// JSON as a value. Its `null` has no counterpart, so a style never sees one.
pub fn from_json(value: serde_json::Value) -> anyhow::Result<Value> {
    Ok(match value {
        serde_json::Value::Null => anyhow::bail!("JSON: `null` has no value here"),
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(int) => Value::Int(int),
            None => Value::Float(number.as_f64().unwrap_or_default()),
        },
        serde_json::Value::String(text) => Value::Str(text),
        serde_json::Value::Array(items) => Value::List(
            items
                .into_iter()
                .map(from_json)
                .collect::<anyhow::Result<_>>()?,
        ),
        serde_json::Value::Object(entries) => Value::Table(
            entries
                .into_iter()
                .filter(|(_, item)| !item.is_null())
                .map(|(key, item)| Ok((key, from_json(item)?)))
                .collect::<anyhow::Result<_>>()?,
        ),
    })
}

fn globals() -> Globals {
    base_globals()
        .with(style_globals)
        .with_namespace("decode", decode_functions)
        .build()
}

/// Loads a style from its source. `path` is how errors refer to the file.
pub fn load(path: &str, text: &str) -> Result<Style, String> {
    let ast = parse(path, text).map_err(|e| format!("{e}"))?;
    let globals = globals();
    let loading = Loading(RefCell::new(None));
    let module = StarModule::with_temp_heap(|module| {
        {
            let mut eval = Evaluator::new(&module);
            eval.enable_static_typechecking(true);
            eval.extra = Some(&loading);
            set_time_limit(&mut eval);
            eval.eval_module(ast, &globals)
                .map_err(|e| format!("{e}"))?;
        }
        module.freeze().map_err(|e| format!("{path}: {e:?}"))
    })?;
    let declared = loading
        .0
        .into_inner()
        .ok_or_else(|| format!("{path}: the style never calls `style(...)`"))?;
    Ok(Style {
        name: declared.name,
        locks: declared.locks,
        files: declared.files,
        options: declared.options,
        module,
    })
}

impl Style {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The locks that pick this style for a source that has one at its top.
    pub fn locks(&self) -> &[String] {
        &self.locks
    }

    /// The build files that pick this style for a source with no lock.
    pub fn files(&self) -> &[String] {
        &self.files
    }

    pub fn options(&self) -> &[(String, OptionSpec)] {
        &self.options
    }

    /// The options `set` gives, checked against what the style declares, with the defaults of
    /// the rest. `id` names the recipe in errors.
    pub fn options_for(
        &self,
        id: &str,
        set: &BTreeMap<String, Value>,
    ) -> Result<BTreeMap<String, Value>, String> {
        if let Some(unknown) = set.keys().find(|name| !self.declares(name)) {
            let names: Vec<String> = self.options.iter().map(|(n, _)| format!("`{n}`")).collect();
            return Err(if names.is_empty() {
                format!("`{id}`: the {} style takes no options", self.name)
            } else {
                format!(
                    "`{id}`: the {} style has no option `{unknown}`; it has {}",
                    self.name,
                    names.join(", ")
                )
            });
        }
        let mut options = BTreeMap::new();
        for (name, spec) in &self.options {
            let value = match (set.get(name), &spec.default) {
                (Some(value), _) => {
                    spec.ty
                        .check(value)
                        .map_err(|e| format!("`{id}`'s {} option `{name}`: {e}", self.name))?;
                    value.clone()
                }
                (None, Some(default)) => default.clone(),
                (None, None) if spec.optional => continue,
                (None, None) => {
                    return Err(format!(
                        "`{id}` has to set the {} option `{name}`",
                        self.name
                    ));
                }
            };
            options.insert(name.clone(), value);
        }
        Ok(options)
    }

    fn declares(&self, name: &str) -> bool {
        self.options.iter().any(|(declared, _)| declared == name)
    }

    /// Runs the style for the recipe `id`, with the options it sets and `read` for files of its
    /// first source.
    pub fn run(
        &self,
        id: &str,
        version: &str,
        set: &BTreeMap<String, Value>,
        read: &dyn Fn(&str) -> Result<String, String>,
    ) -> Result<Record, String> {
        let options = self.options_for(id, set)?;
        let running = Running {
            read,
            reads: RefCell::new(Vec::new()),
        };
        let build = self
            .module
            .get(BUILD)
            .map_err(|e| format!("the {} style: {e}", self.name))?;
        let made = StarModule::with_temp_heap(|module| {
            let build = module.heap().access_owned_frozen_value(&build);
            let recipe = module.heap().alloc(Recipe {
                id: id.to_owned(),
                version: version.to_owned(),
            });
            let options = convert::to_starlark(module.heap(), &Value::Table(options));
            let mut eval = Evaluator::new(&module);
            eval.extra = Some(&running);
            set_time_limit(&mut eval);
            let made = eval
                .eval_function(build, &[recipe, options], &[])
                .map_err(|e| {
                    failed(&e.to_string())
                        .unwrap_or_else(|| format!("`{id}`, in the {} style: {e}", self.name))
                })?;
            convert::from_starlark(made)
        })?;
        record(made, running.reads.into_inner())
            .map_err(|e| format!("`{id}`, in the {} style: {e}", self.name))
    }
}

/// The message a style gave `fail()`, which is meant for whoever wrote the recipe, without the
/// traceback through the style around it.
fn failed(error: &str) -> Option<String> {
    let message = error.split_once("error: fail: ")?.1;
    Some(message.lines().next().unwrap_or(message).to_owned())
}

/// The record a build function returned: `script`, and optionally `deps` and `sources`.
fn record(made: Value, reads: Vec<String>) -> Result<Record, String> {
    let Value::Table(mut fields) = made else {
        return Err(format!("a build returns a dictionary, not {made}"));
    };
    let script = match fields.remove("script") {
        Some(Value::Str(script)) => script,
        Some(other) => return Err(format!("`script` is text, not {other}")),
        None => return Err("a build returns a `script`".to_owned()),
    };
    let deps = match fields.remove("deps") {
        None => Vec::new(),
        Some(value) => texts(value).map_err(|e| format!("`deps`: {e}"))?,
    };
    let sources = match fields.remove("sources") {
        None => Vec::new(),
        Some(Value::List(items)) => items.into_iter().map(source).collect::<Result<_, _>>()?,
        Some(other) => return Err(format!("`sources` is a list, not {other}")),
    };
    if let Some(unknown) = fields.keys().next() {
        return Err(format!(
            "a build returns `script`, `deps` and `sources`, not `{unknown}`"
        ));
    }
    Ok(Record {
        script,
        deps,
        sources,
        reads,
    })
}

fn texts(value: Value) -> Result<Vec<String>, String> {
    match value {
        Value::List(items) => items
            .into_iter()
            .map(|item| match item {
                Value::Str(text) => Ok(text),
                other => Err(format!("expected text, found {other}")),
            })
            .collect(),
        other => Err(format!("expected a list, found {other}")),
    }
}

fn source(value: Value) -> Result<Source, String> {
    let Value::Table(mut fields) = value else {
        return Err(format!("a source is a dictionary, not {value}"));
    };
    let mut text = |key: &str| match fields.remove(key) {
        None => Ok(None),
        Some(Value::Str(text)) => Ok(Some(text)),
        Some(other) => Err(format!("a source's `{key}` is text, not {other}")),
    };
    let url = text("url")?.ok_or("a source has a `url`")?;
    let into = text("into")?;
    let sha256 = text("sha256")?;
    let file = match fields.remove("file") {
        None => false,
        Some(Value::Bool(file)) => file,
        Some(other) => return Err(format!("a source's `file` is true or false, not {other}")),
    };
    if let Some(unknown) = fields.keys().next() {
        return Err(format!(
            "a source has `url`, `into`, `sha256` and `file`, not `{unknown}`"
        ));
    }
    Ok(Source {
        url,
        into,
        sha256,
        file,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCKED: &str = r#"
def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    lock = decode.toml(recipe.read("Cargo.lock"))
    sources = []
    for package in lock["package"]:
        if "checksum" in package:
            sources.append({
                "url": "https://static.crates.io/crates/%s/%s-%s.crate" % (
                    package["name"], package["name"], package["version"]),
                "into": "vendor/%s-%s" % (package["name"], package["version"]),
                "sha256": package["checksum"],
            })
    features = " ".join(options["features"])
    return {
        "script": "cargo build # %s %s %s\n" % (recipe.id, recipe.version, features),
        "deps": ["rust-bin"],
        "sources": sources,
    }

style(
    name = "cargo",
    locks = ["Cargo.lock"],
    options = {"features": option(types.list(types.str), doc = "Features to build with.")},
    build = build,
)
"#;

    const LOCK: &str = "[[package]]\nname = \"app\"\nversion = \"1.0.0\"\n\n\
                        [[package]]\nname = \"itoa\"\nversion = \"1.0.15\"\nchecksum = \"abc\"\n";

    fn read(path: &str) -> Result<String, String> {
        match path {
            "Cargo.lock" => Ok(LOCK.to_owned()),
            other => Err(format!("{other} isn't in the source")),
        }
    }

    fn features(names: &[&str]) -> BTreeMap<String, Value> {
        let names = names.iter().map(|n| Value::Str((*n).to_owned())).collect();
        BTreeMap::from([("features".to_owned(), Value::List(names))])
    }

    #[test]
    fn a_style_makes_a_record_from_the_recipe_its_options_and_its_lock() {
        let style = load("cargo.star", LOCKED).unwrap();
        assert_eq!(
            (style.name(), style.locks()),
            ("cargo", &["Cargo.lock".to_owned()][..])
        );
        let record = style
            .run("app", "1.0.0", &features(&["pcre2"]), &read)
            .unwrap();
        assert_eq!(
            record,
            Record {
                script: "cargo build # app 1.0.0 pcre2\n".to_owned(),
                deps: vec!["rust-bin".to_owned()],
                sources: vec![Source {
                    url: "https://static.crates.io/crates/itoa/itoa-1.0.15.crate".to_owned(),
                    into: Some("vendor/itoa-1.0.15".to_owned()),
                    sha256: Some("abc".to_owned()),
                    file: false,
                }],
                reads: vec!["Cargo.lock".to_owned()],
            }
        );
    }

    #[test]
    fn an_option_left_out_takes_its_default() {
        let style = load("cargo.star", LOCKED).unwrap();
        let record = style.run("app", "1.0.0", &BTreeMap::new(), &read).unwrap();
        assert_eq!(record.script, "cargo build # app 1.0.0 \n");
    }

    #[test]
    fn an_option_the_style_doesnt_declare_is_refused_with_those_it_does() {
        let style = load("cargo.star", LOCKED).unwrap();
        let set = BTreeMap::from([("feature".to_owned(), Value::Bool(true))]);
        let error = style.run("app", "1.0.0", &set, &read).unwrap_err();
        assert_eq!(
            error,
            "`app`: the cargo style has no option `feature`; it has `features`"
        );
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused() {
        let style = load("cargo.star", LOCKED).unwrap();
        let set = BTreeMap::from([("features".to_owned(), Value::Str("pcre2".to_owned()))]);
        let error = style.run("app", "1.0.0", &set, &read).unwrap_err();
        assert!(
            error.starts_with("`app`'s cargo option `features`: "),
            "{error}"
        );
    }

    #[test]
    fn reading_what_the_source_doesnt_have_fails_the_recipe() {
        let style = load("s.star", &LOCKED.replace("Cargo.lock\"))", "go.sum\"))")).unwrap();
        let error = style
            .run("app", "1.0.0", &BTreeMap::new(), &read)
            .unwrap_err();
        assert!(error.contains("go.sum isn't in the source"), "{error}");
    }

    #[test]
    fn a_record_with_a_field_no_build_returns_is_refused() {
        let text = "def build(recipe: Recipe, options: dict[str, typing.Any]) -> \
                    dict[str, typing.Any]:\n    return {\"script\": \"\", \"env\": {}}\n\
                    style(name = \"x\", build = build)\n";
        let error = load("x.star", text)
            .unwrap()
            .run("app", "", &BTreeMap::new(), &read)
            .unwrap_err();
        assert_eq!(
            error,
            "`app`, in the x style: a build returns `script`, `deps` and `sources`, not `env`"
        );
    }

    #[test]
    fn a_style_file_that_never_declares_itself_is_an_error() {
        let error = load("x.star", "x = 1\n").err().unwrap();
        assert_eq!(error, "x.star: the style never calls `style(...)`");
    }

    #[test]
    fn decoders_read_toml_and_json() {
        let text = "def build(recipe: Recipe, options: dict[str, typing.Any]) -> \
                    dict[str, typing.Any]:\n    \
                    a = decode.toml('x = 1')['x']\n    \
                    b = decode.json('{\"y\": [2, null], \"z\": null}')\n    \
                    return {\"script\": '%s %s' % (a, b)}\n\
                    style(name = \"x\", build = build)\n";
        let record = load("x.star", text)
            .and_then(|style| style.run("app", "", &BTreeMap::new(), &read))
            .err();
        // `null` inside a list has no value to stand for.
        assert!(record.unwrap().contains("`null` has no value here"));
    }
}
