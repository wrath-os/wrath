//! Converting between Starlark values and configuration values.

use std::fmt;

use allocative::Allocative;
use distro_eval::{RuntimePart, Value};
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

/// A value only known at service start, passed through a module unchanged.
#[derive(Debug, ProvidesStaticType, NoSerialize, StarlarkPagablePanic, Allocative)]
pub(crate) struct Runtime(#[allocative(skip)] pub Vec<RuntimePart>);

starlark_simple_value!(Runtime);

impl fmt::Display for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<runtime value>")
    }
}

#[starlark_value(type = "runtime_value")]
impl<'v> StarlarkValue<'v> for Runtime {}

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
