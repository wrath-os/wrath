//! Rendering values in applications' config formats. Every module language uses these, so the
//! same value renders to the same bytes whichever language a module is written in.

use std::fmt::Write;

use ::toml::Value as TomlValue;

use crate::module::Unit;
use crate::value::{RuntimePart, Value};

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

/// A KDL document, as niri reads, from a table of nodes. Each key is written as it is, so it can
/// carry arguments and properties too, such as `output "eDP-1"`. A string, number or list of them
/// is the node's arguments, `true` makes it a flag and `false` leaves it out, a table is its
/// children, and a list of tables or lists repeats the node, once for each.
pub fn kdl(value: &Value) -> Result<String, String> {
    let Value::Table(nodes) = value else {
        return Err(format!("a KDL document is a table, found {value}"));
    };
    let mut out = String::new();
    for (head, value) in nodes {
        write_kdl(&mut out, head, value, 0)?;
    }
    Ok(out)
}

fn write_kdl(out: &mut String, head: &str, value: &Value, depth: usize) -> Result<(), String> {
    let indent = "    ".repeat(depth);
    match value {
        Value::Bool(false) => {}
        Value::Bool(true) => out.push_str(&format!("{indent}{head}\n")),
        Value::Table(children) => {
            out.push_str(&format!("{indent}{head} {{\n"));
            for (child, value) in children {
                write_kdl(out, child, value, depth + 1)?;
            }
            out.push_str(&format!("{indent}}}\n"));
        }
        Value::List(items)
            if items
                .iter()
                .any(|i| matches!(i, Value::Table(_) | Value::List(_))) =>
        {
            for item in items {
                match item {
                    Value::Table(_) => write_kdl(out, head, item, depth)?,
                    Value::List(arguments) => write_kdl_arguments(out, &indent, head, arguments)?,
                    _ => {
                        return Err(format!(
                            "`{head}`: a list either repeats the node, with a table or list for \
                             each, or gives its arguments, not both"
                        ));
                    }
                }
            }
        }
        Value::List(arguments) => write_kdl_arguments(out, &indent, head, arguments)?,
        _ => write_kdl_arguments(out, &indent, head, std::slice::from_ref(value))?,
    }
    Ok(())
}

fn write_kdl_arguments(
    out: &mut String,
    indent: &str,
    head: &str,
    arguments: &[Value],
) -> Result<(), String> {
    out.push_str(indent);
    out.push_str(head);
    for argument in arguments {
        out.push(' ');
        match argument {
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => out.push_str(&i.to_string()),
            Value::Float(x) if x.is_finite() => out.push_str(&format!("{x:?}")),
            Value::Str(s) => write_kdl_string(out, s),
            Value::Runtime(_) => return Err(RUNTIME.into()),
            _ => {
                return Err(format!(
                    "`{head}`: an argument is a string, number or boolean, found {argument}"
                ));
            }
        }
    }
    out.push('\n');
    Ok(())
}

/// A KDL string, whose escapes are JSON's but for `\u{..}`.
fn write_kdl_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                // Writing to a String cannot fail.
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
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

/// [`json`] for a value that may contain secrets or machine values: the text around them, and each
/// as a [`RuntimePart::QuotedSecret`] or [`RuntimePart::QuotedMachine`] to fill in on the machine.
pub fn json_parts(value: &Value) -> Result<Vec<RuntimePart>, String> {
    with_secrets(value, json)
}

/// [`toml`] for a value that may contain secrets, like [`json_parts`].
pub fn toml_parts(value: &Value) -> Result<Vec<RuntimePart>, String> {
    with_secrets(value, toml)
}

/// [`systemd`] for a value that may contain secrets, like [`json_parts`].
pub fn systemd_parts(value: &Value) -> Result<Vec<RuntimePart>, String> {
    with_secrets(value, systemd)
}

/// [`kdl`] for a value that may contain secrets, like [`json_parts`].
pub fn kdl_parts(value: &Value) -> Result<Vec<RuntimePart>, String> {
    with_secrets(value, kdl)
}

/// Marks a secret in rendered text: the secret's index between two private-use characters,
/// which formats write as they are.
const MARKER_START: char = '\u{E000}';
const MARKER_END: char = '\u{E001}';

/// Renders `value` with each secret replaced by a marker, then splits the text at the markers.
fn with_secrets(
    value: &Value,
    render: impl Fn(&Value) -> Result<String, String>,
) -> Result<Vec<RuntimePart>, String> {
    let mut secrets = Vec::new();
    let marked = mark(value, &mut secrets)?;
    let text = render(&marked)?;
    let mut parts = Vec::new();
    let mut rest = text.as_str();
    while let Some(start) = rest.find(MARKER_START) {
        let end = rest[start..]
            .find(MARKER_END)
            .map(|end| start + end)
            .ok_or("a secret's marker was cut apart")?;
        let index: usize = rest[start + MARKER_START.len_utf8()..end]
            .parse()
            .map_err(|_| "a secret's marker was changed")?;
        if start > 0 {
            parts.push(RuntimePart::Text(rest[..start].to_owned()));
        }
        parts.push(secrets[index].clone());
        rest = &rest[end + MARKER_END.len_utf8()..];
    }
    if !rest.is_empty() {
        parts.push(RuntimePart::Text(rest.to_owned()));
    }
    Ok(parts)
}

/// `value` with runtime values turned into strings that mark each secret and machine value,
/// recorded in `secrets` by index as the quoted part that fills the marker in.
fn mark(value: &Value, secrets: &mut Vec<RuntimePart>) -> Result<Value, String> {
    Ok(match value {
        Value::Str(text) if text.contains(MARKER_START) => {
            return Err(format!(
                "text containing U+E000 cannot be rendered with secrets: {text:?}"
            ));
        }
        Value::Runtime(parts) => {
            let mut text = String::new();
            for part in parts {
                match part {
                    RuntimePart::Text(part) => {
                        text.push_str(part);
                        continue;
                    }
                    RuntimePart::Secret(name) | RuntimePart::QuotedSecret(name) => {
                        secrets.push(RuntimePart::QuotedSecret(name.clone()));
                    }
                    RuntimePart::Machine(path) | RuntimePart::QuotedMachine(path) => {
                        secrets.push(RuntimePart::QuotedMachine(path.clone()));
                    }
                }
                text.push(MARKER_START);
                text.push_str(&(secrets.len() - 1).to_string());
                text.push(MARKER_END);
            }
            Value::Str(text)
        }
        Value::List(items) => Value::List(
            items
                .iter()
                .map(|item| mark(item, secrets))
                .collect::<Result<_, _>>()?,
        ),
        Value::Table(entries) => Value::Table(
            entries
                .iter()
                .map(|(key, item)| Ok((key.clone(), mark(item, secrets)?)))
                .collect::<Result<_, String>>()?,
        ),
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_) => value.clone(),
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

    fn table(entries: &[(&str, Value)]) -> Value {
        Value::Table(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect(),
        )
    }

    #[test]
    fn kdl_nodes_take_their_arguments_children_and_flags_from_the_values() {
        let settings = table(&[
            ("prefer-no-csd", Value::Bool(true)),
            (
                "hotkey-overlay",
                table(&[("skip-at-startup", Value::Bool(false))]),
            ),
            (
                "layout",
                table(&[
                    ("gaps", Value::Int(16)),
                    ("center-focused-column", Value::Str("never".into())),
                ]),
            ),
            ("output \"eDP-1\"", table(&[("scale", Value::Float(2.0))])),
            (
                "binds",
                table(&[(
                    "Mod+T hotkey-overlay-title=\"Terminal\"",
                    table(&[("spawn", Value::List(vec![Value::Str("alacritty".into())]))]),
                )]),
            ),
            (
                "spawn-at-startup",
                Value::List(vec![
                    Value::List(vec![Value::Str("mako".into())]),
                    Value::List(vec![Value::Str("sh".into()), Value::Str("-c".into())]),
                ]),
            ),
            (
                "window-rule",
                Value::List(vec![
                    table(&[
                        ("match app-id=\"firefox\"", Value::Bool(true)),
                        ("open-maximized", Value::List(vec![Value::Bool(true)])),
                    ]),
                    table(&[("geometry-corner-radius", Value::Int(8))]),
                ]),
            ),
        ]);
        assert_eq!(
            kdl(&settings).unwrap(),
            "binds {\n    Mod+T hotkey-overlay-title=\"Terminal\" {\n        spawn \"alacritty\"\n    \
             }\n}\nhotkey-overlay {\n}\nlayout {\n    center-focused-column \"never\"\n    \
             gaps 16\n}\noutput \"eDP-1\" {\n    scale 2.0\n}\nprefer-no-csd\n\
             spawn-at-startup \"mako\"\nspawn-at-startup \"sh\" \"-c\"\nwindow-rule {\n    \
             match app-id=\"firefox\"\n    open-maximized true\n}\nwindow-rule {\n    \
             geometry-corner-radius 8\n}\n"
        );
    }

    #[test]
    fn kdl_strings_are_quoted_and_mixed_lists_refused() {
        let quoted = table(&[("title", Value::Str("say \"hi\"\n\u{1}".into()))]);
        assert_eq!(kdl(&quoted).unwrap(), "title \"say \\\"hi\\\"\\n\\u{1}\"\n");
        let mixed = table(&[(
            "spawn-at-startup",
            Value::List(vec![Value::Str("a".into()), Value::List(vec![])]),
        )]);
        assert!(kdl(&mixed).unwrap_err().contains("spawn-at-startup"));
        assert!(kdl(&Value::Int(1)).is_err());
    }

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
    fn values_with_secrets_render_to_text_around_the_secrets() {
        let url = Value::Runtime(vec![
            RuntimePart::Text("postgres://app:".into()),
            RuntimePart::Secret("db".into()),
            RuntimePart::Text("@db".into()),
        ]);
        let value = Value::Table(BTreeMap::from([
            ("url".to_owned(), url),
            ("port".to_owned(), Value::Int(5432)),
        ]));
        let shown = |parts: Vec<RuntimePart>| -> String {
            parts
                .iter()
                .map(|part| match part {
                    RuntimePart::Text(text) => text.clone(),
                    RuntimePart::Secret(name) => format!("<{name}>"),
                    RuntimePart::QuotedSecret(name) => format!("<quoted {name}>"),
                    RuntimePart::Machine(path) => format!("<machine {path}>"),
                    RuntimePart::QuotedMachine(path) => format!("<quoted machine {path}>"),
                })
                .collect()
        };

        insta::assert_snapshot!(shown(json_parts(&value).unwrap()), @r#"
        {
          "port": 5432,
          "url": "postgres://app:<quoted db>@db"
        }
        "#);
        insta::assert_snapshot!(shown(toml_parts(&value).unwrap()), @r#"
        port = 5432
        url = "postgres://app:<quoted db>@db"
        "#);
        let unit = Value::Table(BTreeMap::from([(
            "Service".to_owned(),
            Value::Table(BTreeMap::from([(
                "Environment".to_owned(),
                Value::Runtime(vec![
                    RuntimePart::Text("\"TOKEN=".into()),
                    RuntimePart::Secret("token".into()),
                    RuntimePart::Text("\"".into()),
                ]),
            )])),
        )]));
        insta::assert_snapshot!(shown(systemd_parts(&unit).unwrap()), @r#"
        [Service]
        Environment="TOKEN=<quoted token>"
        "#);
        assert_eq!(
            json_parts(&Value::Int(1)).unwrap(),
            [RuntimePart::Text("1\n".into())]
        );
        let marker = Value::Str(format!("{MARKER_START}0{MARKER_END}"));
        assert!(json_parts(&marker).is_err());
    }

    #[test]
    fn refuses_values_it_cannot_represent() {
        let runtime = Value::Runtime(vec![RuntimePart::Secret("pw".into())]);
        assert!(json(&Value::Float(f64::NAN)).is_err());
        assert!(json(&runtime).is_err());
        assert!(toml(&Value::Table(BTreeMap::from([("k".to_owned(), runtime)]))).is_err());
    }
}
