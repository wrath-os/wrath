//! Resolved option values.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt::{self, Write};

use crate::path::write_key;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Table(BTreeMap<String, Value>),
    /// A string only known at service start because it contains secrets.
    Runtime(Vec<RuntimePart>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimePart {
    Text(String),
    Secret(String),
}

impl Value {
    /// Whether the value or anything in it is only known at service start.
    pub fn is_runtime(&self) -> bool {
        match self {
            Self::Runtime(_) => true,
            Self::List(items) => items.iter().any(Self::is_runtime),
            Self::Table(entries) => entries.values().any(Self::is_runtime),
            Self::Bool(_) | Self::Int(_) | Self::Float(_) | Self::Str(_) => false,
        }
    }

    /// A total order for sorting sets: by kind, then numbers numerically and strings by bytes,
    /// then anything else by its TOML form.
    pub fn canonical_cmp(&self, other: &Self) -> Ordering {
        let rank = |value: &Self| match value {
            Self::Bool(_) => 0,
            Self::Int(_) | Self::Float(_) => 1,
            Self::Str(_) => 2,
            Self::List(_) => 3,
            Self::Table(_) => 4,
            Self::Runtime(_) => 5,
        };
        let number = |value: &Self| match value {
            // Precision beyond 2^53 doesn't matter for ordering a set.
            #[allow(clippy::cast_precision_loss)]
            Self::Int(int) => Some(*int as f64),
            Self::Float(float) => Some(*float),
            _ => None,
        };
        rank(self)
            .cmp(&rank(other))
            .then_with(|| match (self, other) {
                (Self::Str(a), Self::Str(b)) => a.cmp(b),
                _ => match (number(self), number(other)) {
                    (Some(a), Some(b)) => a.total_cmp(&b),
                    _ => Ordering::Equal,
                },
            })
            .then_with(|| self.to_string().cmp(&other.to_string()))
    }

    /// A short description of the kind of value, for error messages.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Bool(_) => "a boolean",
            Self::Int(_) => "an integer",
            Self::Float(_) => "a float",
            Self::Str(_) => "a string",
            Self::List(_) => "a list",
            Self::Table(_) => "a table",
            Self::Runtime(_) => "a runtime value",
        }
    }
}

/// Displays values as TOML.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(x) => write!(f, "{x:?}"),
            Self::Str(s) => write_string(f, s),
            Self::List(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Self::Table(entries) => {
                if entries.is_empty() {
                    return f.write_str("{}");
                }
                f.write_str("{ ")?;
                for (i, (key, value)) in entries.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write_key(f, key)?;
                    write!(f, " = {value}")?;
                }
                f.write_str(" }")
            }
            Self::Runtime(parts) => match parts.as_slice() {
                [RuntimePart::Secret(name)] => {
                    f.write_str("{ secret = ")?;
                    write_string(f, name)?;
                    f.write_str(" }")
                }
                _ => {
                    let mut template = String::new();
                    for part in parts {
                        match part {
                            RuntimePart::Text(text) => {
                                template.push_str(&text.replace("${", "$${"))
                            }
                            RuntimePart::Secret(name) => write!(template, "${{secret:{name}}}")?,
                        }
                    }
                    f.write_str("{ runtime = ")?;
                    write_string(f, &template)?;
                    f.write_str(" }")
                }
            },
        }
    }
}

fn write_string(f: &mut impl fmt::Write, s: &str) -> fmt::Result {
    f.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\t' => f.write_str("\\t")?,
            '\r' => f.write_str("\\r")?,
            c if c.is_control() => write!(f, "\\u{:04X}", u32::from(c))?,
            c => f.write_char(c)?,
        }
    }
    f.write_char('"')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_as_toml() {
        let table = Value::Table(BTreeMap::from([
            ("port".into(), Value::Int(80)),
            ("a b".into(), Value::Float(1.0)),
        ]));
        let list = Value::List(vec![
            Value::Str("say \"hi\"\n".into()),
            Value::Bool(true),
            table,
        ]);

        assert_eq!(
            list.to_string(),
            r#"["say \"hi\"\n", true, { "a b" = 1.0, port = 80 }]"#
        );
    }

    #[test]
    fn sets_sort_numbers_numerically_and_kinds_apart() {
        let mut values = vec![
            Value::Int(80),
            Value::Str("b".into()),
            Value::Int(22000),
            Value::Float(443.5),
            Value::Str("a".into()),
            Value::Bool(true),
        ];
        values.sort_by(Value::canonical_cmp);

        assert_eq!(
            Value::List(values).to_string(),
            r#"[true, 80, 443.5, 22000, "a", "b"]"#
        );
    }

    #[test]
    fn displays_runtime_values() {
        let secret = Value::Runtime(vec![RuntimePart::Secret("grafana-admin".into())]);
        let template = Value::Runtime(vec![
            RuntimePart::Text("user:".into()),
            RuntimePart::Secret("pw".into()),
        ]);

        assert_eq!(secret.to_string(), r#"{ secret = "grafana-admin" }"#);
        assert_eq!(template.to_string(), r#"{ runtime = "user:${secret:pw}" }"#);
        assert!(Value::List(vec![secret]).is_runtime());
        assert!(!Value::Str("x".into()).is_runtime());
    }
}
