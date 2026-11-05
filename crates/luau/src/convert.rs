//! Converting between Luau values and configuration values.
//!
//! Luau has one table type for lists and records, so a table with keys `1..n` is a list, a table
//! with string keys is a record, and an empty table is an empty record. The evaluator reads an
//! empty record as an empty list where the schema expects a list.

use std::collections::BTreeMap;

use distro_eval::{RuntimePart, Value};
use mlua::{Lua, Table, UserData, Value as LuaValue};

/// A value only known at service start, passed through a module unchanged.
pub(crate) struct Runtime(pub Vec<RuntimePart>);

impl UserData for Runtime {}

/// Integers up to 2^53 are exact in a Luau number.
const MAX_EXACT: f64 = 9_007_199_254_740_992.0;

/// Converts a value for a module. Tables are read-only, so a module can't change what it reads.
pub(crate) fn to_lua(lua: &Lua, value: &Value) -> mlua::Result<LuaValue> {
    Ok(match value {
        Value::Bool(b) => LuaValue::Boolean(*b),
        // Values beyond 2^53 lose precision in a Luau number.
        #[allow(clippy::cast_precision_loss)]
        Value::Int(int) => LuaValue::Number(*int as f64),
        Value::Float(float) => LuaValue::Number(*float),
        Value::Str(text) => LuaValue::String(lua.create_string(text)?),
        Value::List(items) => {
            let table = lua.create_table_with_capacity(items.len(), 0)?;
            for (i, item) in items.iter().enumerate() {
                table.raw_set(i + 1, to_lua(lua, item)?)?;
            }
            table.set_readonly(true);
            LuaValue::Table(table)
        }
        Value::Table(entries) => {
            let table = lua.create_table_with_capacity(0, entries.len())?;
            for (key, item) in entries {
                table.raw_set(key.as_str(), to_lua(lua, item)?)?;
            }
            table.set_readonly(true);
            LuaValue::Table(table)
        }
        Value::Runtime(parts) => LuaValue::UserData(lua.create_userdata(Runtime(parts.clone()))?),
    })
}

pub(crate) fn from_lua(value: &LuaValue) -> Result<Value, String> {
    match value {
        LuaValue::Boolean(b) => Ok(Value::Bool(*b)),
        LuaValue::Integer(int) => Ok(Value::Int(*int)),
        LuaValue::Number(number) => Ok(number_value(*number)),
        LuaValue::String(text) => text
            .to_str()
            .map(|text| Value::Str(text.to_owned()))
            .map_err(|_| "strings must be valid UTF-8".to_owned()),
        LuaValue::Table(table) => table_value(table),
        LuaValue::UserData(data) => match data.borrow::<Runtime>() {
            Ok(runtime) => Ok(Value::Runtime(runtime.0.clone())),
            Err(_) => Err("this userdata is not a value".into()),
        },
        LuaValue::Nil => Err("nil is not a value".into()),
        other => Err(format!("a {} is not a value", other.type_name())),
    }
}

/// Whole numbers that a Luau number holds exactly become integers.
fn number_value(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() <= MAX_EXACT {
        // Checked above: the number is whole and within the exact range.
        #[allow(clippy::cast_possible_truncation)]
        Value::Int(number as i64)
    } else {
        Value::Float(number)
    }
}

fn table_value(table: &Table) -> Result<Value, String> {
    let len = table.raw_len();
    let mut items = Vec::with_capacity(len);
    let mut entries = BTreeMap::new();
    for pair in table.pairs::<LuaValue, LuaValue>() {
        let (key, item) = pair.map_err(|e| e.to_string())?;
        let item = from_lua(&item)?;
        match key {
            LuaValue::String(key) => {
                let key = key
                    .to_str()
                    .map_err(|_| "keys must be valid UTF-8".to_owned())?
                    .to_owned();
                entries.insert(key, item);
            }
            LuaValue::Integer(_) | LuaValue::Number(_) => items.push(item),
            other => return Err(format!("a {} cannot be a key", other.type_name())),
        }
    }
    match (items.is_empty(), entries.is_empty()) {
        (true, _) => Ok(Value::Table(entries)),
        (false, true) if items.len() == len => {
            // `pairs` order isn't defined, so read the list again in index order.
            (1..=len)
                .map(|i| {
                    let item: LuaValue = table.raw_get(i).map_err(|e| e.to_string())?;
                    from_lua(&item)
                })
                .collect::<Result<_, _>>()
                .map(Value::List)
        }
        (false, true) => Err("a list cannot have gaps".into()),
        (false, false) => Err("a table cannot mix list items and named fields".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(source: &str) -> Result<Value, String> {
        let lua = Lua::new();
        let value: LuaValue = lua.load(source).eval().unwrap();
        from_lua(&value)
    }

    #[test]
    fn tables_become_lists_or_records() {
        assert_eq!(
            round_trip("{ 'a', 2, 2.5, true }").unwrap().to_string(),
            r#"["a", 2, 2.5, true]"#
        );
        assert_eq!(
            round_trip("{ b = 1, a = { 1 } }").unwrap().to_string(),
            "{ a = [1], b = 1 }"
        );
        assert_eq!(round_trip("{}").unwrap(), Value::Table(BTreeMap::new()));
    }

    #[test]
    fn rejects_mixed_and_sparse_tables() {
        assert_eq!(
            round_trip("{ 1, a = 2 }").unwrap_err(),
            "a table cannot mix list items and named fields"
        );
        assert_eq!(
            round_trip("{ [1] = 1, [3] = 3 }").unwrap_err(),
            "a list cannot have gaps"
        );
        assert_eq!(
            round_trip("function() end").unwrap_err(),
            "a function is not a value"
        );
    }

    #[test]
    fn values_round_trip_through_read_only_tables() {
        let lua = Lua::new();
        let value = Value::Table(BTreeMap::from([
            (
                "list".to_owned(),
                Value::List(vec![Value::Int(1), Value::Float(0.5)]),
            ),
            (
                "secret".to_owned(),
                Value::Runtime(vec![RuntimePart::Secret("pw".into())]),
            ),
        ]));
        let converted = to_lua(&lua, &value).unwrap();

        assert_eq!(from_lua(&converted).unwrap(), value);
        let LuaValue::Table(table) = converted else {
            panic!("expected a table");
        };
        assert!(table.is_readonly());
    }
}
