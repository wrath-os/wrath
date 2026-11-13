//! `wrath options --schema`: the options as a JSON Schema, so an editor that reads one, such as
//! taplo, completes, checks and explains what's written in a configuration's TOML files.
//!
//! Everywhere a value goes, the forms the loader accepts in its place go too: a default written
//! `{ default = ... }`, a forced value `{ value = ..., force = "why" }`, a change to a list
//! `{ add = [...], remove = [...] }`, a secret `{ secret = "name" }` where an option takes one, and
//! a reference such as `"${machine.zone}"` where the value isn't a string.

use serde_json::{Map, Value as Json, json};

use wrath_eval::{ListMerge, OptionSpec, Type, Value};

/// The schema of a whole configuration file whose options start at `root`. A file of the
/// configuration itself, as opposed to one about a single user, has `include`, `[sources]` and
/// `[machine]` too.
pub fn document(root: &OptionSpec, frame: bool) -> Json {
    let mut schema = option(root);
    if frame && let Some(properties) = schema["properties"].as_object_mut() {
        properties.insert("include".into(), include());
        properties.insert("sources".into(), sources());
        properties.insert("machine".into(), json!({"type": "object"}));
    }
    schema["$schema"] = json!("https://json-schema.org/draft/2020-12/schema");
    schema
}

/// The schema of one option and what's under it.
fn option(spec: &OptionSpec) -> Json {
    let mut schema = match &spec.ty {
        Type::Record(record) => json!({
            "type": "object",
            "properties": record
                .fields
                .iter()
                .map(|(name, field)| (name.clone(), option(field)))
                .collect::<Map<_, _>>(),
            "additionalProperties": false,
        }),
        Type::Map(item) => json!({"type": "object", "additionalProperties": option(item)}),
        Type::Freeform => json!({}),
        ty => leaf(ty, spec.runtime),
    };
    if !spec.doc.is_empty() {
        schema["description"] = json!(spec.doc);
    }
    if let Some(default) = spec.default.as_ref().and_then(json_value) {
        schema["default"] = default;
    }
    schema
}

/// A value an option holds, in any of the forms written in its place.
fn leaf(ty: &Type, runtime: bool) -> Json {
    let value = plain(ty);
    let mut forms = vec![
        value.clone(),
        json!({
            "type": "object",
            "properties": {"default": value},
            "required": ["default"],
            "additionalProperties": false,
        }),
        json!({
            "type": "object",
            "properties": {"value": value, "force": {"type": "string", "minLength": 1}},
            "required": ["value", "force"],
            "additionalProperties": false,
        }),
    ];
    if let Type::List { merge, .. } = ty
        && ty.merges_items()
    {
        // An ordered list's items are removed by name.
        let removed = match merge {
            ListMerge::Ordered => json!({"type": "array", "items": {"type": "string"}}),
            ListMerge::Set | ListMerge::Atomic => value.clone(),
        };
        forms.push(json!({
            "type": "object",
            "properties": {"add": value, "remove": removed},
            "minProperties": 1,
            "additionalProperties": false,
        }));
    }
    if runtime {
        forms.push(json!({
            "type": "object",
            "properties": {"secret": {"type": "string"}},
            "required": ["secret"],
            "additionalProperties": false,
        }));
    }
    if !matches!(ty, Type::Str | Type::Path) {
        forms.push(json!({"type": "string", "pattern": "^\\$\\{[^}]+\\}$"}));
    }
    json!({"anyOf": forms})
}

/// A value of `ty` as written plainly.
fn plain(ty: &Type) -> Json {
    match ty {
        Type::Bool => json!({"type": "boolean"}),
        Type::Int { min, max } => {
            let mut schema = json!({"type": "integer"});
            if let Some(min) = min {
                schema["minimum"] = json!(min);
            }
            if let Some(max) = max {
                schema["maximum"] = json!(max);
            }
            schema
        }
        Type::Float => json!({"type": "number"}),
        Type::Str => json!({"type": "string"}),
        Type::Path => json!({"type": "string", "pattern": "^(/|~/)"}),
        Type::File => json!({"type": "string"}),
        Type::Enum(names) => json!({"enum": names}),
        Type::List { item, merge } => {
            let mut items = plain(item);
            // Named items, which may say what they go before or after.
            if *merge == ListMerge::Ordered
                && let Some(properties) = items["properties"].as_object_mut()
            {
                let anchors = json!({"type": "array", "items": {"type": "string"}});
                properties.insert("before".into(), anchors.clone());
                properties.insert("after".into(), anchors);
            }
            json!({"type": "array", "items": items})
        }
        Type::Record(_) | Type::Map(_) => option(&OptionSpec::new(ty.clone())),
        Type::Freeform => json!({}),
    }
}

/// `include = [...]`: files of the configuration, or files from a source.
fn include() -> Json {
    let entry = json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "source": {"type": "string"},
            "as": {"enum": ["defaults"]},
        },
        "required": ["path"],
        "additionalProperties": false,
    });
    json!({
        "description": "Other files whose values this one takes.",
        "type": "array",
        "items": {"anyOf": [{"type": "string"}, entry]},
    })
}

/// `[sources.NAME]`, each a URL wrath fetches and the lock pins.
fn sources() -> Json {
    json!({
        "description": "Where files to include come from, by name, pinned in the lock.",
        "type": "object",
        "additionalProperties": {
            "type": "object",
            "properties": {"url": {"type": "string"}},
            "required": ["url"],
            "additionalProperties": false,
        },
    })
}

/// `value` as JSON, when it's one a file can hold.
fn json_value(value: &Value) -> Option<Json> {
    Some(match value {
        Value::Bool(b) => json!(b),
        Value::Int(i) => json!(i),
        Value::Float(x) => json!(x),
        Value::Str(s) => json!(s),
        Value::List(items) => Json::Array(items.iter().map(json_value).collect::<Option<_>>()?),
        Value::Table(entries) => Json::Object(
            entries
                .iter()
                .map(|(key, item)| Some((key.clone(), json_value(item)?)))
                .collect::<Option<_>>()?,
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(ty: Type) -> OptionSpec {
        OptionSpec::new(ty)
    }

    fn ssh() -> OptionSpec {
        let ssh = Type::record([
            ("enable", spec(Type::Bool).default(Value::Bool(false))),
            (
                "port",
                spec(Type::port())
                    .default(Value::Int(22))
                    .doc("Where it listens."),
            ),
            ("keys", spec(Type::list(Type::Str, ListMerge::Set))),
            ("order", spec(Type::list(Type::Str, ListMerge::Atomic))),
            (
                "rules",
                spec(Type::list(
                    Type::record([("name", spec(Type::Str))]),
                    ListMerge::Ordered,
                )),
            ),
            ("token", spec(Type::Str).runtime()),
        ]);
        spec(Type::record([(
            "services",
            spec(Type::record([("ssh", spec(ssh))])),
        )]))
    }

    #[test]
    fn records_name_their_options_and_nothing_else() {
        let schema = document(&ssh(), true);
        let options = &schema["properties"]["services"]["properties"]["ssh"];
        assert_eq!(options["additionalProperties"], json!(false));
        let port = &options["properties"]["port"];
        assert_eq!(port["description"], "Where it listens.");
        assert_eq!(port["default"], 22);
        assert_eq!(
            port["anyOf"][0],
            json!({"type": "integer", "minimum": 1, "maximum": 65535})
        );
        for key in ["include", "sources", "machine"] {
            assert!(schema["properties"].get(key).is_some(), "{key}");
        }
        assert!(
            document(&ssh(), false)["properties"]
                .get("include")
                .is_none()
        );
    }

    #[test]
    fn every_form_a_value_can_be_written_in_is_allowed() {
        let schema = document(&ssh(), false);
        let options = &schema["properties"]["services"]["properties"]["ssh"]["properties"];
        let forms = |name: &str| options[name]["anyOf"].as_array().unwrap().clone();
        let has = |name: &str, key: &str| {
            forms(name)
                .iter()
                .any(|form| form["properties"].get(key).is_some())
        };
        assert!(has("port", "default") && has("port", "force"));
        assert!(has("keys", "add"), "a list can be changed");
        assert!(!has("port", "add"));
        assert!(
            !has("order", "add"),
            "a list that wins as a whole isn't changed"
        );
        let change = forms("rules")
            .into_iter()
            .find(|form| form["properties"].get("remove").is_some())
            .unwrap();
        assert_eq!(
            change["properties"]["remove"]["items"],
            json!({"type": "string"})
        );
        let added = &change["properties"]["add"]["items"]["properties"];
        assert!(
            added.get("before").is_some(),
            "added items can say where they go"
        );
        assert!(has("token", "secret"), "a runtime option can hold a secret");
        assert!(!has("port", "secret"));
        let reference = json!({"type": "string", "pattern": "^\\$\\{[^}]+\\}$"});
        assert!(
            forms("port").contains(&reference),
            "a port can be a reference"
        );
        assert!(
            !forms("token").contains(&reference),
            "a string is one already"
        );
    }
}
