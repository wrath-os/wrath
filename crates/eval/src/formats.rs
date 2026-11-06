//! Rendering values in applications' config formats. Every module language uses these, so the
//! same value renders to the same bytes whichever language a module is written in.

use std::fmt::Write;

use ::toml::Value as TomlValue;

use crate::module::Unit;
use crate::value::Value;

/// Pretty-printed JSON with sorted keys and a trailing newline.
pub fn json(value: &Value) -> Result<String, String> {
    let mut out = String::new();
    write_json(&mut out, value, 0)?;
    out.push('\n');
    Ok(out)
}

/// A systemd-style INI file, such as a unit or a `.network` file, from a table of sections,
/// ordered and with repeated keys like a unit.
pub fn systemd(value: &Value) -> Result<String, String> {
    Ok(Unit::from_value(value)?.text())
}

/// A TOML document from a table, with keys sorted.
pub fn toml(value: &Value) -> Result<String, String> {
    let TomlValue::Table(table) = to_toml(value)? else {
        return Err(format!("a TOML document is a table, found {value}"));
    };
    ::toml::to_string(&table).map_err(|e| e.to_string())
}

fn to_toml(value: &Value) -> Result<TomlValue, String> {
    Ok(match value {
        Value::Bool(b) => TomlValue::Boolean(*b),
        Value::Int(i) => TomlValue::Integer(*i),
        Value::Float(x) => TomlValue::Float(*x),
        Value::Str(s) => TomlValue::String(s.clone()),
        Value::List(items) => {
            TomlValue::Array(items.iter().map(to_toml).collect::<Result<_, _>>()?)
        }
        Value::Table(entries) => TomlValue::Table(
            entries
                .iter()
                .map(|(key, item)| Ok((key.clone(), to_toml(item)?)))
                .collect::<Result<_, String>>()?,
        ),
        Value::Runtime(_) => return Err(RUNTIME.into()),
    })
}

const RUNTIME: &str =
    "a value only known at service start cannot be rendered when the system is built";

fn write_json(out: &mut String, value: &Value, depth: usize) -> Result<(), String> {
    let indent = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    match value {
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::Float(x) if x.is_finite() => out.push_str(&format!("{x:?}")),
        Value::Float(x) => return Err(format!("JSON has no representation for {x}")),
        Value::Str(s) => write_json_string(out, s),
        Value::List(items) if items.is_empty() => out.push_str("[]"),
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                indent(out, depth + 1);
                write_json(out, item, depth + 1)?;
            }
            indent(out, depth);
            out.push(']');
        }
        Value::Table(entries) if entries.is_empty() => out.push_str("{}"),
        Value::Table(entries) => {
            out.push('{');
            for (i, (key, item)) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                indent(out, depth + 1);
                write_json_string(out, key);
                out.push_str(": ");
                write_json(out, item, depth + 1)?;
            }
            indent(out, depth);
            out.push('}');
        }
        Value::Runtime(_) => return Err(RUNTIME.into()),
    }
    Ok(())
}

fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                // Writing to a String cannot fail.
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::value::RuntimePart;

    #[test]
    fn renders_pretty_json_with_sorted_keys() {
        let value = Value::Table(BTreeMap::from([
            (
                "b".to_owned(),
                Value::List(vec![Value::Int(1), Value::Float(0.5)]),
            ),
            (
                "a".to_owned(),
                Value::Str("quote \" and\nnewline\u{1}".into()),
            ),
            ("c".to_owned(), Value::Table(BTreeMap::new())),
            ("d".to_owned(), Value::List(vec![])),
        ]));

        assert_eq!(
            json(&value).unwrap(),
            "{\n  \"a\": \"quote \\\" and\\nnewline\\u0001\",\n  \"b\": [\n    1,\n    0.5\n  ],\n  \"c\": {},\n  \"d\": []\n}\n"
        );
    }

    #[test]
    fn renders_systemd_files_with_repeated_keys_and_match_first() {
        let section = |entries: &[(&str, Value)]| {
            Value::Table(
                entries
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), value.clone()))
                    .collect(),
            )
        };
        let value = section(&[
            (
                "Network",
                section(&[
                    ("DHCP", Value::Str("yes".into())),
                    ("IPv6AcceptRA", Value::Bool(true)),
                ]),
            ),
            (
                "Match",
                section(&[(
                    "Name",
                    Value::List(vec![Value::Str("enp*".into()), Value::Str("wlan*".into())]),
                )]),
            ),
        ]);

        assert_eq!(
            systemd(&value).unwrap(),
            "[Match]\nName=enp*\nName=wlan*\n\n[Network]\nDHCP=yes\nIPv6AcceptRA=yes\n"
        );
    }

    #[test]
    fn renders_toml_with_tables_after_keys() {
        let value = Value::Table(BTreeMap::from([
            (
                "ui".to_owned(),
                Value::Table(BTreeMap::from([
                    ("editor".to_owned(), Value::Str("hx".into())),
                    (
                        "diff-formatter".to_owned(),
                        Value::List(vec![Value::Str("difft".into()), Value::Str("$left".into())]),
                    ),
                ])),
            ),
            ("a".to_owned(), Value::Bool(true)),
            ("n".to_owned(), Value::Int(3)),
        ]));

        assert_eq!(
            toml(&value).unwrap(),
            "a = true\nn = 3\n\n[ui]\ndiff-formatter = [\"difft\", \"$left\"]\neditor = \"hx\"\n"
        );
        assert!(toml(&Value::Str("x".into())).is_err());
    }

    #[test]
    fn refuses_values_it_cannot_represent() {
        let runtime = Value::Runtime(vec![RuntimePart::Secret("pw".into())]);
        assert!(json(&Value::Float(f64::NAN)).is_err());
        assert!(json(&runtime).is_err());
        assert!(toml(&Value::Table(BTreeMap::from([("k".to_owned(), runtime)]))).is_err());
    }
}
