//! Converting between JavaScript values and configuration values, and reading the option
//! declarations the prelude builds.

use std::collections::BTreeMap;

use distro_eval::{ListMerge, OptionSpec, RuntimePart, Type, Value};
use rquickjs::{Array, Ctx, Function, Object, Value as JsValue};

/// The key of the object a runtime value becomes, as declared in `distro.d.ts`.
const RUNTIME: &str = "$runtime";

/// Integers up to 2^53 are exact in a JavaScript number.
const MAX_EXACT: f64 = 9_007_199_254_740_992.0;

/// Converts a value for a module. Objects and arrays are frozen, so a module can't change what
/// it reads.
pub(crate) fn to_js<'js>(ctx: &Ctx<'js>, value: &Value) -> rquickjs::Result<JsValue<'js>> {
    let freeze: Function = ctx.globals().get::<_, Object>("Object")?.get("freeze")?;
    to_js_frozen(ctx, &freeze, value)
}

fn to_js_frozen<'js>(
    ctx: &Ctx<'js>,
    freeze: &Function<'js>,
    value: &Value,
) -> rquickjs::Result<JsValue<'js>> {
    let frozen = |value: JsValue<'js>| freeze.call::<_, JsValue<'js>>((value,));
    Ok(match value {
        Value::Bool(b) => JsValue::new_bool(ctx.clone(), *b),
        // Values beyond 2^53 lose precision in a JavaScript number.
        #[allow(clippy::cast_precision_loss)]
        Value::Int(int) => JsValue::new_number(ctx.clone(), *int as f64),
        Value::Float(float) => JsValue::new_number(ctx.clone(), *float),
        Value::Str(text) => rquickjs::String::from_str(ctx.clone(), text)?.into_value(),
        Value::List(items) => {
            let array = Array::new(ctx.clone())?;
            for (i, item) in items.iter().enumerate() {
                array.set(i, to_js_frozen(ctx, freeze, item)?)?;
            }
            frozen(array.into_value())?
        }
        Value::Table(entries) => {
            let object = Object::new(ctx.clone())?;
            for (key, item) in entries {
                object.set(key.as_str(), to_js_frozen(ctx, freeze, item)?)?;
            }
            frozen(object.into_value())?
        }
        Value::Runtime(parts) => {
            let list = parts
                .iter()
                .map(|part| {
                    let (kind, text) = match part {
                        RuntimePart::Text(text) => ("text", text),
                        RuntimePart::Secret(name) => ("secret", name),
                    };
                    Value::Table(BTreeMap::from([(
                        kind.to_owned(),
                        Value::Str(text.clone()),
                    )]))
                })
                .collect();
            let object = Object::new(ctx.clone())?;
            object.set(RUNTIME, to_js_frozen(ctx, freeze, &Value::List(list))?)?;
            frozen(object.into_value())?
        }
    })
}

pub(crate) fn from_js(value: &JsValue<'_>) -> Result<Value, String> {
    if let Some(b) = value.as_bool() {
        return Ok(Value::Bool(b));
    }
    if let Some(int) = value.as_int() {
        return Ok(Value::Int(i64::from(int)));
    }
    if let Some(number) = value.as_float() {
        return Ok(if number.fract() == 0.0 && number.abs() <= MAX_EXACT {
            // Checked above: the number is whole and within the exact range.
            #[allow(clippy::cast_possible_truncation)]
            Value::Int(number as i64)
        } else {
            Value::Float(number)
        });
    }
    if let Some(text) = value.as_string() {
        return text.to_string().map(Value::Str).map_err(|e| e.to_string());
    }
    if let Some(array) = value.as_array() {
        return array
            .iter::<JsValue<'_>>()
            .map(|item| from_js(&item.map_err(|e| e.to_string())?))
            .collect::<Result<_, _>>()
            .map(Value::List);
    }
    if value.is_function() {
        return Err("a function is not a value".into());
    }
    if let Some(object) = value.as_object() {
        if object.contains_key(RUNTIME).unwrap_or(false) {
            return runtime_from_js(object);
        }
        return object
            .props::<String, JsValue<'_>>()
            .map(|pair| {
                let (key, item) = pair.map_err(|e| e.to_string())?;
                Ok((key, from_js(&item)?))
            })
            .collect::<Result<_, String>>()
            .map(Value::Table);
    }
    Err(format!("{} is not a value", value.type_name()))
}

fn runtime_from_js(object: &Object<'_>) -> Result<Value, String> {
    let parts: JsValue<'_> = object.get(RUNTIME).map_err(|e| e.to_string())?;
    let Value::List(parts) = from_js(&parts)? else {
        return Err("a runtime value holds a list of parts".into());
    };
    parts
        .into_iter()
        .map(|part| {
            let first = match part {
                Value::Table(entries) => entries.into_iter().next(),
                _ => None,
            };
            match first {
                Some((kind, Value::Str(text))) if kind == "text" => Ok(RuntimePart::Text(text)),
                Some((kind, Value::Str(name))) if kind == "secret" => Ok(RuntimePart::Secret(name)),
                _ => Err("a runtime part is `{ text }` or `{ secret }`".to_owned()),
            }
        })
        .collect::<Result<_, _>>()
        .map(Value::Runtime)
}

/// A list of strings, such as `writes`.
pub(crate) fn strings(value: &JsValue<'_>) -> Result<Vec<String>, String> {
    match from_js(value)? {
        Value::List(items) => items
            .into_iter()
            .map(|item| match item {
                Value::Str(text) => Ok(text),
                other => Err(format!("expected a string, found {other}")),
            })
            .collect(),
        other => Err(format!("expected a list of strings, found {other}")),
    }
}

/// An option declaration: `option(type, settings)` or a bare type.
pub(crate) fn spec(value: &JsValue<'_>) -> Result<OptionSpec, String> {
    let object = value
        .as_object()
        .ok_or("expected a type such as `types.str` or an `option(...)`")?;
    if kind(object)? != "option" {
        return ty(value).map(OptionSpec::new);
    }
    let mut spec = OptionSpec::new(ty(&get(object, "type")?)?);
    let settings: Object<'_> = object.get("settings").map_err(|e| e.to_string())?;
    for pair in settings.props::<String, JsValue<'_>>() {
        let (key, setting) = pair.map_err(|e| e.to_string())?;
        match key.as_str() {
            "default" => spec.default = Some(from_js(&setting)?),
            "optional" => {
                spec.optional = setting.as_bool().ok_or("`optional` must be a boolean")?
            }
            "runtime" => spec.runtime = setting.as_bool().ok_or("`runtime` must be a boolean")?,
            "doc" => {
                spec.doc = setting
                    .as_string()
                    .and_then(|doc| doc.to_string().ok())
                    .ok_or("`doc` must be a string")?;
            }
            other => {
                return Err(format!(
                    "unknown option setting `{other}`; expected default, optional, runtime or doc"
                ));
            }
        }
    }
    Ok(spec)
}

fn ty(value: &JsValue<'_>) -> Result<Type, String> {
    let object = value
        .as_object()
        .ok_or("expected a type such as `types.str`")?;
    Ok(match kind(object)?.as_str() {
        "bool" => Type::Bool,
        "int" => Type::int(),
        "float" => Type::Float,
        "str" => Type::Str,
        "path" => Type::Path,
        "port" => Type::port(),
        "freeform" => Type::Freeform,
        "list" => {
            let merge = match from_js(&get(object, "merge")?)? {
                Value::Str(merge) if merge == "set" => ListMerge::Set,
                Value::Str(merge) if merge == "ordered" => ListMerge::Ordered,
                _ => ListMerge::Atomic,
            };
            Type::list(ty(&get(object, "item")?)?, merge)
        }
        "map" => Type::map(spec(&get(object, "item")?)?),
        "record" => {
            let fields: Object<'_> = object.get("fields").map_err(|e| e.to_string())?;
            let fields = fields
                .props::<String, JsValue<'_>>()
                .map(|pair| {
                    let (name, field) = pair.map_err(|e| e.to_string())?;
                    Ok((name, spec(&field)?))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Type::record(
                fields
                    .iter()
                    .map(|(name, spec)| (name.as_str(), spec.clone())),
            )
        }
        "one_of" => Type::Enum(strings(&get(object, "choices")?)?),
        "range" => {
            let bound = |key| match from_js(&get(object, key)?)? {
                Value::Int(int) => Ok(int),
                other => Err(format!("`{key}` must be an integer, found {other}")),
            };
            Type::Int {
                min: Some(bound("min")?),
                max: Some(bound("max")?),
            }
        }
        "option" => {
            return Err("expected a type; options only go in records, maps and `options`".into());
        }
        other => return Err(format!("unknown type kind `{other}`")),
    })
}

pub(crate) fn get<'js>(object: &Object<'js>, key: &str) -> Result<JsValue<'js>, String> {
    object.get(key).map_err(|e| format!("`{key}`: {e}"))
}

fn kind(object: &Object<'_>) -> Result<String, String> {
    match from_js(&get(object, "kind")?)? {
        Value::Str(kind) => Ok(kind),
        _ => Err("expected a type such as `types.str` or an `option(...)`".into()),
    }
}
