//! Reading a model back from its canonical JSON, such as the one a running system carries, so
//! a configuration can be planned against what a machine runs.

use std::collections::{BTreeMap, BTreeSet};

use distro_eval::{RuntimePart, Section, Template, Unit, UnitAction, Value, Wrapper};

use crate::{File, Home, Locked, Model, Secret, Source, VERSION};

impl Model {
    /// The model in `json`, written by [`Model::canonical`] by this version of the format.
    pub fn from_json(json: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| format!("not a model: {e}"))?;
        let model = from_json_value(&value)?;
        let fields = table(&model, "the model")?;
        match fields.get("version") {
            Some(Value::Int(VERSION)) => {}
            Some(Value::Int(other)) => {
                return Err(format!(
                    "the model is in format version {other}, and this distro reads version \
                     {VERSION}"
                ));
            }
            _ => return Err("the model has no format version".into()),
        }
        let field = |name: &str| fields.get(name).ok_or(format!("the model has no `{name}`"));
        let unspelled =
            |name: &str| -> Result<_, String> { Ok(unspell_entries(table(field(name)?, name)?)) };
        let homes = table(field("homes")?, "homes")?
            .iter()
            .map(|(user, home)| Ok((user.clone(), read_home(home)?)))
            .collect::<Result<_, String>>()?;
        Ok(Self {
            options: unspelled("options")?,
            packages: strings(field("packages")?)?,
            files: read_files(field("files")?)?,
            units: read_units(field("units")?)?,
            base: unspelled("base")?,
            backends: unspelled("backends")?,
            triggers: read_triggers(field("triggers")?)?,
            machine: strings(field("machine")?)?.into_iter().collect(),
            secrets: read_secrets(field("secrets")?)?,
            homes,
            sources: read_sources(field("sources")?)?,
            locked: read_locked(field("locked")?)?,
            templates: read_templates(field("templates")?)?,
            next_login: strings(field("next_login")?)?,
            service_packages: strings(field("service_packages")?)?,
            enabled: read_enabled(field("enabled")?)?,
            enabled_user: read_enabled(field("enabled_user")?)?,
            wrappers: read_wrappers(field("wrappers")?)?,
        })
    }
}

fn from_json_value(value: &serde_json::Value) -> Result<Value, String> {
    use serde_json::Value as Json;
    Ok(match value {
        Json::Null => return Err("a model has no nulls".into()),
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => match n.as_i64() {
            Some(int) => Value::Int(int),
            None => Value::Float(n.as_f64().ok_or(format!("{n} is out of range"))?),
        },
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(items) => Value::List(
            items
                .iter()
                .map(from_json_value)
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(entries) => Value::Table(
            entries
                .iter()
                .map(|(key, item)| Ok((key.clone(), from_json_value(item)?)))
                .collect::<Result<_, String>>()?,
        ),
    })
}

/// Turns tables naming a runtime value's parts back into the runtime value.
fn unspell(value: &Value) -> Value {
    match value {
        Value::Table(entries) => match runtime_parts(entries) {
            Some(parts) => Value::Runtime(parts),
            None => Value::Table(unspell_entries(entries)),
        },
        Value::List(items) => Value::List(items.iter().map(unspell).collect()),
        Value::Bool(_) | Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Runtime(_) => {
            value.clone()
        }
    }
}

fn unspell_entries(entries: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    entries
        .iter()
        .map(|(key, value)| (key.clone(), unspell(value)))
        .collect()
}

/// The parts of `{ runtime = [{ text = ".." }, { secret = ".." }] }`.
fn runtime_parts(entries: &BTreeMap<String, Value>) -> Option<Vec<RuntimePart>> {
    let (key, Value::List(parts)) = entries.iter().next()? else {
        return None;
    };
    if key != "runtime" || entries.len() != 1 {
        return None;
    }
    parts
        .iter()
        .map(|part| {
            let Value::Table(part) = part else {
                return None;
            };
            if part.len() != 1 {
                return None;
            }
            match part.iter().next()? {
                (kind, Value::Str(text)) if kind == "text" => Some(RuntimePart::Text(text.clone())),
                (kind, Value::Str(name)) if kind == "secret" => {
                    Some(RuntimePart::Secret(name.clone()))
                }
                (kind, Value::Str(name)) if kind == "quoted_secret" => {
                    Some(RuntimePart::QuotedSecret(name.clone()))
                }
                (kind, Value::Str(path)) if kind == "machine" => {
                    Some(RuntimePart::Machine(path.clone()))
                }
                (kind, Value::Str(path)) if kind == "quoted_machine" => {
                    Some(RuntimePart::QuotedMachine(path.clone()))
                }
                _ => None,
            }
        })
        .collect()
}

fn table<'a>(value: &'a Value, what: &str) -> Result<&'a BTreeMap<String, Value>, String> {
    match value {
        Value::Table(entries) => Ok(entries),
        other => Err(format!("{what} should be a table, found {other}")),
    }
}

fn get<'a>(
    entries: &'a BTreeMap<String, Value>,
    key: &str,
    what: &str,
) -> Result<&'a Value, String> {
    entries.get(key).ok_or(format!("{what} has no `{key}`"))
}

fn string_at(entries: &BTreeMap<String, Value>, key: &str, what: &str) -> Result<String, String> {
    string(get(entries, key, what)?, what)
}

fn string(value: &Value, what: &str) -> Result<String, String> {
    match value {
        Value::Str(text) => Ok(text.clone()),
        other => Err(format!("{what} should be a string, found {other}")),
    }
}

fn optional_string(
    entries: &BTreeMap<String, Value>,
    key: &str,
    what: &str,
) -> Result<Option<String>, String> {
    entries
        .get(key)
        .map(|value| string(value, what))
        .transpose()
}

fn mode(value: &Value, what: &str) -> Result<u32, String> {
    match value {
        Value::Int(mode) => {
            u32::try_from(*mode).map_err(|_| format!("{what}: {mode} isn't a mode"))
        }
        other => Err(format!("{what} should be a mode, found {other}")),
    }
}

fn strings(value: &Value) -> Result<BTreeSet<String>, String> {
    match value {
        Value::List(items) => items.iter().map(|item| string(item, "a name")).collect(),
        other => Err(format!("expected a list of strings, found {other}")),
    }
}

/// Reads each entry of the table `value` as a table with `read`, which is given the entry's name.
fn read_tables<T>(
    value: &Value,
    what: &str,
    read: impl Fn(&BTreeMap<String, Value>, &str) -> Result<T, String>,
) -> Result<BTreeMap<String, T>, String> {
    table(value, what)?
        .iter()
        .map(|(name, entry)| Ok((name.clone(), read(table(entry, name)?, name)?)))
        .collect()
}

fn read_files(value: &Value) -> Result<BTreeMap<String, File>, String> {
    read_tables(value, "files", |file, path| {
        Ok(File {
            mode: mode(get(file, "mode", path)?, path)?,
            text: string_at(file, "text", path)?,
        })
    })
}

fn read_units(value: &Value) -> Result<BTreeMap<String, Unit>, String> {
    read_tables(value, "units", |unit, name| {
        let Value::List(sections) = get(unit, "sections", name)? else {
            return Err(format!("unit {name}: `sections` should be a list"));
        };
        Ok(Unit {
            sections: sections
                .iter()
                .map(|section| read_section(section, name))
                .collect::<Result<_, _>>()?,
            restart_on: strings(get(unit, "restart_on", name)?)?,
        })
    })
}

fn read_section(value: &Value, unit: &str) -> Result<Section, String> {
    let section = table(value, unit)?;
    let Value::List(entries) = get(section, "entries", unit)? else {
        return Err(format!(
            "unit {unit}: a section's `entries` should be a list"
        ));
    };
    let entries = entries
        .iter()
        .map(|entry| match entry {
            Value::List(pair) if pair.len() == 2 => {
                Ok((string(&pair[0], unit)?, string(&pair[1], unit)?))
            }
            other => Err(format!(
                "unit {unit}: expected a key and a value, found {other}"
            )),
        })
        .collect::<Result<_, String>>()?;
    Ok(Section {
        name: string_at(section, "name", unit)?,
        entries,
    })
}

fn read_triggers(
    value: &Value,
) -> Result<BTreeMap<(String, UnitAction), BTreeSet<String>>, String> {
    let mut triggers = BTreeMap::new();
    for (unit, actions) in table(value, "triggers")? {
        for (action, paths) in table(actions, unit)? {
            let action = match action.as_str() {
                "reload" => UnitAction::Reload,
                "restart" => UnitAction::Restart,
                other => return Err(format!("{unit}: `{other}` isn't an action")),
            };
            triggers.insert((unit.clone(), action), strings(paths)?);
        }
    }
    Ok(triggers)
}

fn read_secrets(value: &Value) -> Result<BTreeMap<String, Secret>, String> {
    read_tables(value, "secrets", |fields, name| {
        Ok(Secret {
            // A running system's model doesn't say where the encrypted file was.
            source: String::new(),
            hash: string_at(fields, "hash", name)?,
            owner: string_at(fields, "owner", name)?,
            group: optional_string(fields, "group", name)?,
            mode: mode(get(fields, "mode", name)?, name)?,
            link: optional_string(fields, "link", name)?,
        })
    })
}

fn read_home(value: &Value) -> Result<Home, String> {
    let home = table(value, "a home")?;
    Ok(Home {
        files: read_files(get(home, "files", "a home")?)?,
        packages: strings(get(home, "packages", "a home")?)?,
        units: read_units(get(home, "units", "a home")?)?,
        templates: read_templates(get(home, "templates", "a home")?)?,
    })
}

fn read_sources(value: &Value) -> Result<BTreeMap<String, Source>, String> {
    read_tables(value, "sources", |fields, name| {
        Ok(Source {
            url: string_at(fields, "url", name)?,
            rev: optional_string(fields, "rev", name)?,
            hash: string_at(fields, "hash", name)?,
            date: optional_string(fields, "date", name)?,
        })
    })
}

fn read_locked(value: &Value) -> Result<BTreeMap<String, Locked>, String> {
    read_tables(value, "locked", |fields, name| {
        Ok(Locked {
            version: string_at(fields, "version", name)?,
            id: string_at(fields, "id", name)?,
        })
    })
}

fn read_templates(value: &Value) -> Result<BTreeMap<String, Template>, String> {
    read_tables(value, "templates", |fields, path| {
        let spelled = BTreeMap::from([("runtime".to_owned(), get(fields, "parts", path)?.clone())]);
        Ok(Template {
            parts: runtime_parts(&spelled).ok_or(format!("{path}: unreadable parts"))?,
            owner: string_at(fields, "owner", path)?,
            group: optional_string(fields, "group", path)?,
            mode: mode(get(fields, "mode", path)?, path)?,
        })
    })
}

fn read_wrappers(value: &Value) -> Result<BTreeMap<String, Wrapper>, String> {
    read_tables(value, "wrappers", |fields, name| {
        Ok(Wrapper {
            source: string_at(fields, "source", name)?,
            owner: string_at(fields, "owner", name)?,
            group: string_at(fields, "group", name)?,
            mode: mode(get(fields, "mode", name)?, name)?,
        })
    })
}

fn read_enabled(value: &Value) -> Result<BTreeMap<String, BTreeSet<String>>, String> {
    table(value, "enabled units")?
        .iter()
        .map(|(unit, targets)| Ok((unit.clone(), strings(targets)?)))
        .collect()
}
