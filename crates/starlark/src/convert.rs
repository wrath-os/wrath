//! Converting between Starlark values and configuration values.

use std::fmt;

use allocative::Allocative;
use starlark::any::ProvidesStaticType;
use starlark::starlark_simple_value;
use starlark::values::dict::{AllocDict, DictRef};
use starlark::values::float::StarlarkFloat;
use starlark::values::list::{AllocList, ListRef};
use starlark::values::tuple::TupleRef;
use starlark::values::{
    Heap, NoSerialize, StarlarkPagablePanic, StarlarkValue, UnpackValue, Value as StarValue,
    ValueLike,
};
use starlark_derive::starlark_value;
use wrath_eval::{RuntimePart, Value};

/// A value only known at service start, passed through a module unchanged.
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
pub(crate) struct Runtime(#[allocative(skip)] pub Vec<RuntimePart>);

starlark_simple_value!(Runtime);

impl fmt::Display for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<runtime value>")
    }
}

/// Joining text and runtime values with `+` makes a runtime value.
#[starlark_value(type = "runtime_value")]
impl<'v> StarlarkValue<'v> for Runtime {
    fn add(&self, rhs: StarValue<'v>, heap: Heap<'v>) -> Option<starlark::Result<StarValue<'v>>> {
        let joined = join(&self.0, &runtime_parts(rhs)?);
        Some(Ok(heap.alloc(Runtime(joined))))
    }

    fn radd(&self, lhs: StarValue<'v>, heap: Heap<'v>) -> Option<starlark::Result<StarValue<'v>>> {
        let joined = join(&runtime_parts(lhs)?, &self.0);
        Some(Ok(heap.alloc(Runtime(joined))))
    }
}

/// A string or runtime value as runtime parts.
pub(crate) fn runtime_parts(value: StarValue<'_>) -> Option<Vec<RuntimePart>> {
    match value.unpack_str() {
        Some(text) => Some(vec![RuntimePart::Text(text.to_owned())]),
        None => value
            .downcast_ref::<Runtime>()
            .map(|runtime| runtime.0.clone()),
    }
}

/// The parts of `a` then `b`, with neighbouring text merged.
pub(crate) fn join(a: &[RuntimePart], b: &[RuntimePart]) -> Vec<RuntimePart> {
    let mut out: Vec<RuntimePart> = Vec::with_capacity(a.len() + b.len());
    for part in a.iter().chain(b) {
        match (out.last_mut(), part) {
            (Some(RuntimePart::Text(last)), RuntimePart::Text(text)) => last.push_str(text),
            _ => out.push(part.clone()),
        }
    }
    out
}

pub(crate) fn to_starlark<'v>(heap: Heap<'v>, value: &Value) -> StarValue<'v> {
    match value {
        Value::Bool(b) => StarValue::new_bool(*b),
        Value::Int(int) => heap.alloc(*int),
        Value::Float(float) => heap.alloc(*float),
        Value::Str(text) => heap.alloc(text.as_str()),
        Value::List(items) => {
            let items: Vec<StarValue<'v>> =
                items.iter().map(|item| to_starlark(heap, item)).collect();
            heap.alloc(AllocList(items))
        }
        Value::Table(entries) => heap.alloc(AllocDict(
            entries
                .iter()
                .map(|(key, item)| (key.as_str(), to_starlark(heap, item))),
        )),
        Value::Runtime(parts) => heap.alloc(Runtime(parts.clone())),
    }
}

pub(crate) fn from_starlark(value: StarValue<'_>) -> Result<Value, String> {
    if let Some(b) = value.unpack_bool() {
        return Ok(Value::Bool(b));
    }
    if let Ok(Some(int)) = i64::unpack_value(value) {
        return Ok(Value::Int(int));
    }
    if let Some(float) = value.downcast_ref::<StarlarkFloat>() {
        return Ok(Value::Float(float.0));
    }
    if let Some(text) = value.unpack_str() {
        return Ok(Value::Str(text.to_owned()));
    }
    let items = ListRef::from_value(value)
        .map(|list| list.content())
        .or_else(|| TupleRef::from_value(value).map(|tuple| tuple.content()));
    if let Some(items) = items {
        return items
            .iter()
            .copied()
            .map(from_starlark)
            .collect::<Result<_, _>>()
            .map(Value::List);
    }
    if let Some(dict) = DictRef::from_value(value) {
        return dict
            .iter()
            .map(|(key, item)| {
                let key = key
                    .unpack_str()
                    .ok_or_else(|| format!("keys must be strings, found a {}", key.get_type()))?;
                Ok((key.to_owned(), from_starlark(item)?))
            })
            .collect::<Result<_, String>>()
            .map(Value::Table);
    }
    if let Some(runtime) = value.downcast_ref::<Runtime>() {
        return Ok(Value::Runtime(runtime.0.clone()));
    }
    Err(format!("a {} is not a value", value.get_type()))
}
