//! Option schemas: what can be configured and how values merge.

use std::collections::BTreeMap;

use crate::ordered;
use crate::path::OptionPath;
use crate::value::{RuntimePart, Value};

/// The path segment that stands for every entry of a map when declaring options.
pub const EVERY: &str = "*";

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    Bool,
    Int {
        min: Option<i64>,
        max: Option<i64>,
    },
    Float,
    Str,
    /// An absolute path, or one starting with `~/` in user scope.
    Path,
    Enum(Vec<String>),
    List {
        item: Box<Type>,
        merge: ListMerge,
    },
    /// Named entries chosen by the user, such as service instances or sites.
    Map(Box<OptionSpec>),
    Record(Record),
    /// Any TOML value, passed through to an application's own config format.
    Freeform,
}

/// How a list option combines values set in several places at the same level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListMerge {
    /// The union of all values, sorted and without duplicates.
    Set,
    /// Named items, placed with `before` and `after` anchors and otherwise ordered by name.
    Ordered,
    /// One value wins as a whole.
    Atomic,
}

/// A step down from an option to the one a path segment names.
pub(crate) enum Child<'s> {
    /// A field of a record.
    Field(&'s OptionSpec),
    /// An entry of a map, which exists only when the configuration names it.
    Entry(&'s OptionSpec),
    /// Anything inside a freeform option.
    Freeform,
    /// The segment names nothing.
    None,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub fields: BTreeMap<String, OptionSpec>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OptionSpec {
    pub ty: Type,
    pub default: Option<Value>,
    /// Leaving the option unset is allowed when it has no default.
    pub optional: bool,
    /// The option may hold values only known at service start, such as secrets.
    pub runtime: bool,
    pub doc: String,
}

impl OptionSpec {
    pub fn new(ty: Type) -> Self {
        Self {
            ty,
            default: None,
            optional: false,
            runtime: false,
            doc: String::new(),
        }
    }

    pub fn default(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    pub fn runtime(mut self) -> Self {
        self.runtime = true;
        self
    }

    pub fn doc(mut self, doc: impl Into<String>) -> Self {
        self.doc = doc.into();
        self
    }

    pub(crate) fn freeform() -> Self {
        Self::new(Type::Freeform).optional().runtime()
    }
}

impl Type {
    pub fn port() -> Self {
        Self::Int {
            min: Some(1),
            max: Some(65535),
        }
    }

    pub fn int() -> Self {
        Self::Int {
            min: None,
            max: None,
        }
    }

    pub fn list(item: Type, merge: ListMerge) -> Self {
        Self::List {
            item: Box::new(item),
            merge,
        }
    }

    pub fn map(item: OptionSpec) -> Self {
        Self::Map(Box::new(item))
    }

    pub fn record<'a>(fields: impl IntoIterator<Item = (&'a str, OptionSpec)>) -> Self {
        Self::Record(Record {
            fields: fields
                .into_iter()
                .map(|(name, spec)| (name.to_owned(), spec))
                .collect(),
        })
    }

    /// Whether values from several places merge item by item, as set and ordered lists do. Only
    /// these take `{ remove = [...] }`.
    pub fn merges_items(&self) -> bool {
        matches!(
            self,
            Self::List {
                merge: ListMerge::Set | ListMerge::Ordered,
                ..
            }
        )
    }

    /// Options of these types are set through their fields or entries, never as a whole.
    pub fn is_table(&self) -> bool {
        matches!(self, Self::Record(_) | Self::Map(_))
    }

    /// What `segment` names under an option of this type.
    pub(crate) fn child(&self, segment: &str) -> Child<'_> {
        match self {
            Self::Record(record) => match record.fields.get(segment) {
                Some(field) => Child::Field(field),
                None => Child::None,
            },
            Self::Map(item) => Child::Entry(item),
            Self::Freeform => Child::Freeform,
            Self::Bool
            | Self::Int { .. }
            | Self::Float
            | Self::Str
            | Self::Path
            | Self::Enum(_)
            | Self::List { .. } => Child::None,
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Self::Bool => "a boolean".into(),
            Self::Int {
                min: Some(min),
                max: Some(max),
            } => format!("an integer from {min} to {max}"),
            Self::Int {
                min: Some(min),
                max: None,
            } => format!("an integer of at least {min}"),
            Self::Int {
                min: None,
                max: Some(max),
            } => format!("an integer of at most {max}"),
            Self::Int {
                min: None,
                max: None,
            } => "an integer".into(),
            Self::Float => "a number".into(),
            Self::Str => "a string".into(),
            Self::Path => "an absolute path or one starting with `~/`".into(),
            Self::Enum(choices) => {
                let choices: Vec<String> = choices.iter().map(|c| format!("\"{c}\"")).collect();
                format!("one of {}", choices.join(", "))
            }
            Self::List { item, .. } => format!("a list of items that are each {}", item.describe()),
            Self::Map(_) | Self::Record(_) => "a table".into(),
            Self::Freeform => "any value".into(),
        }
    }

    /// Fills in record defaults, and reads an empty table as an empty list where a list is
    /// expected, since some module languages can't tell the two apart.
    pub(crate) fn normalize(&self, value: Value) -> Value {
        match (self, value) {
            (Self::List { .. }, Value::Table(entries)) if entries.is_empty() => {
                Value::List(Vec::new())
            }
            (Self::List { item, .. }, Value::List(items)) => Value::List(
                items
                    .into_iter()
                    .map(|value| item.normalize(value))
                    .collect(),
            ),
            (Self::Map(item), Value::Table(entries)) => Value::Table(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, item.ty.normalize(value)))
                    .collect(),
            ),
            (Self::Record(record), Value::Table(mut entries)) => {
                for (name, field) in &record.fields {
                    match entries.remove(name) {
                        Some(value) => {
                            entries.insert(name.clone(), field.ty.normalize(value));
                        }
                        None => {
                            if let Some(default) = &field.default {
                                entries.insert(name.clone(), default.clone());
                            }
                        }
                    }
                }
                Value::Table(entries)
            }
            (_, value) => value,
        }
    }

    /// Checks that a resolved value has this type. Runtime values pass where strings do, and a
    /// secret where a path does.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        let ok = match (self, value) {
            (Self::Freeform, _)
            | (Self::Bool, Value::Bool(_))
            | (Self::Float, Value::Float(_) | Value::Int(_))
            | (Self::Str, Value::Str(_) | Value::Runtime(_)) => true,
            (Self::Int { min, max }, Value::Int(i)) => {
                min.is_none_or(|min| *i >= min) && max.is_none_or(|max| *i <= max)
            }
            (Self::Path, Value::Str(s)) => s.starts_with('/') || s.starts_with("~/"),
            // The file a secret is decrypted to.
            (Self::Path, Value::Runtime(parts)) => {
                matches!(parts.as_slice(), [RuntimePart::Secret(_)])
            }
            (Self::Enum(choices), Value::Str(s)) => choices.contains(s),
            (
                Self::List {
                    item,
                    merge: ListMerge::Ordered,
                },
                Value::List(items),
            ) => {
                return items
                    .iter()
                    .try_for_each(|value| check_ordered_item(item, value));
            }
            (
                Self::List {
                    item,
                    merge: ListMerge::Set | ListMerge::Atomic,
                },
                Value::List(items),
            ) => return items.iter().try_for_each(|value| item.check(value)),
            (Self::Map(item), Value::Table(entries)) => {
                return entries.values().try_for_each(|value| item.ty.check(value));
            }
            (Self::Record(record), Value::Table(entries)) => return record.check(entries),
            (
                Self::Bool
                | Self::Int { .. }
                | Self::Float
                | Self::Str
                | Self::Path
                | Self::Enum(_)
                | Self::List { .. }
                | Self::Map(_)
                | Self::Record(_),
                _,
            ) => false,
        };
        if ok {
            Ok(())
        } else {
            Err(format!("expected {}, found {value}", self.describe()))
        }
    }
}

impl Record {
    /// Checks a table against the record: known fields with valid values, and every required
    /// field present.
    fn check(&self, entries: &BTreeMap<String, Value>) -> Result<(), String> {
        for (key, value) in entries {
            let field = self
                .fields
                .get(key)
                .ok_or_else(|| format!("unknown field `{key}`"))?;
            field.ty.check(value).map_err(|e| format!("`{key}`: {e}"))?;
        }
        match self.fields.iter().find(|(key, field)| {
            field.default.is_none() && !field.optional && !entries.contains_key(*key)
        }) {
            Some((key, _)) => Err(format!("missing field `{key}`")),
            None => Ok(()),
        }
    }
}

/// Checks an ordered list item without its anchors.
fn check_ordered_item(item: &Type, value: &Value) -> Result<(), String> {
    let split = ordered::split(value)?;
    item.check(&split.value)
}

/// All options the loaded modules declare, as one tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Schema {
    root: OptionSpec,
}

impl Default for Schema {
    fn default() -> Self {
        Self {
            root: OptionSpec::new(Type::Record(Record::default())),
        }
    }
}

impl Schema {
    pub fn root(&self) -> &OptionSpec {
        &self.root
    }

    /// Declares an option, creating the records above it.
    pub fn declare(&mut self, path: &str, spec: OptionSpec) -> Result<(), String> {
        self.declare_at(&OptionPath::parse(path)?, spec)
    }

    /// Declares an option, creating the records above it. A `*` segment declares it in every
    /// entry of a map, such as `users."*".programs.git` for every user; the map must exist.
    pub fn declare_at(&mut self, path: &OptionPath, spec: OptionSpec) -> Result<(), String> {
        let (name, parents) = path
            .segments()
            .split_last()
            .ok_or("cannot declare the root")?;
        let mut node = &mut self.root;
        for segment in parents {
            node = match &mut node.ty {
                Type::Map(item) if segment == EVERY => item.as_mut(),
                Type::Record(record) => record
                    .fields
                    .entry(segment.clone())
                    .or_insert_with(|| OptionSpec::new(Type::Record(Record::default()))),
                _ => return Err(format!("`{path}` is inside an option that isn't a record")),
            };
        }
        let Type::Record(record) = &mut node.ty else {
            return Err(format!("`{path}` is inside an option that isn't a record"));
        };
        if record.fields.contains_key(name) {
            return Err(format!("`{path}` is declared twice"));
        }
        record.fields.insert(name.clone(), spec);
        Ok(())
    }

    /// The spec for a path. Map entries match any name.
    pub fn lookup(&self, path: &OptionPath) -> Option<&OptionSpec> {
        path.segments()
            .iter()
            .try_fold(&self.root, |spec, segment| match &spec.ty {
                Type::Record(record) => record.fields.get(segment),
                Type::Map(item) => Some(item.as_ref()),
                Type::Bool
                | Type::Int { .. }
                | Type::Float
                | Type::Str
                | Type::Path
                | Type::Enum(_)
                | Type::List { .. }
                | Type::Freeform => None,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_values_against_types() {
        assert_eq!(Type::port().check(&Value::Int(8080)), Ok(()));
        assert_eq!(
            Type::port().check(&Value::Int(70000)),
            Err("expected an integer from 1 to 65535, found 70000".into())
        );
        assert_eq!(
            Type::Path.check(&Value::Str("srv".into())),
            Err("expected an absolute path or one starting with `~/`, found \"srv\"".into())
        );
        assert_eq!(Type::Path.check(&Value::Str("~/notes".into())), Ok(()));
        assert_eq!(
            Type::list(Type::Str, ListMerge::Set).check(&Value::List(vec![Value::Int(1)])),
            Err("expected a string, found 1".into())
        );
        assert_eq!(
            Type::Enum(vec!["a".into(), "b".into()]).check(&Value::Str("c".into())),
            Err("expected one of \"a\", \"b\", found \"c\"".into())
        );
        assert_eq!(
            Type::list(Type::port(), ListMerge::Set).describe(),
            "a list of items that are each an integer from 1 to 65535"
        );
    }

    #[test]
    fn normalize_fills_record_defaults_in_lists_and_reads_empty_tables_as_lists() {
        let item = Type::record([
            ("name", OptionSpec::new(Type::Str)),
            (
                "args",
                OptionSpec::new(Type::list(Type::Str, ListMerge::Atomic))
                    .default(Value::List(vec![])),
            ),
        ]);
        let list = Type::list(item, ListMerge::Ordered);
        let value = Value::List(vec![Value::Table(BTreeMap::from([
            ("name".to_owned(), Value::Str("a".into())),
            ("before".to_owned(), Value::Str("b".into())),
        ]))]);

        assert_eq!(
            list.normalize(value).to_string(),
            r#"[{ args = [], before = "b", name = "a" }]"#
        );
        assert_eq!(
            list.normalize(Value::Table(BTreeMap::new())),
            Value::List(vec![])
        );
        assert_eq!(Type::Str.normalize(Value::Int(1)), Value::Int(1));
    }

    #[test]
    fn declares_and_looks_up_options() {
        let mut schema = Schema::default();
        schema
            .declare("system.hostname", OptionSpec::new(Type::Str))
            .unwrap();
        schema
            .declare(
                "services.nginx",
                OptionSpec::new(Type::map(OptionSpec::new(Type::record([(
                    "port",
                    OptionSpec::new(Type::port()),
                )])))),
            )
            .unwrap();

        let lookup = |path: &str| schema.lookup(&OptionPath::parse(path).unwrap());
        assert_eq!(lookup("system.hostname").unwrap().ty, Type::Str);
        assert_eq!(lookup("services.nginx.main.port").unwrap().ty, Type::port());
        assert!(lookup("services.nginx.main.prot").is_none());
        assert_eq!(
            schema.declare("system.hostname", OptionSpec::new(Type::Str)),
            Err("`system.hostname` is declared twice".into())
        );
        assert_eq!(
            schema.declare("system.hostname.x", OptionSpec::new(Type::Str)),
            Err("`system.hostname.x` is inside an option that isn't a record".into())
        );
    }
}
