//! `wrath options`: the options the modules declare, with their types, defaults and docs.

use std::fmt::Write;
use std::path::Path;
use std::process::ExitCode;

use wrath_eval::{ConfigKind, EVERY, OptionPath, OptionSpec, Schema, Sources, Type};

use crate::system::{self, ModuleOrder};

/// Prints the options of a `kind` of configuration as a JSON Schema for the files that set them:
/// the whole configuration, or a file whose options start at `prefix`, such as `users.john` for
/// `users/john.toml`.
pub fn schema(modules: &Path, kind: ConfigKind, prefix: Option<&str>) -> Result<ExitCode, String> {
    let mut sources = Sources::default();
    let mut modules = system::load_modules(modules, &mut sources, ModuleOrder::Sorted)?;
    modules.retain(|module| module.configurations().contains(&kind));
    let mut options: Vec<(OptionPath, OptionSpec)> =
        modules.iter().flat_map(|module| module.options()).collect();
    // Options in every entry of a map come last, so the map is declared whatever the order.
    options.sort_by_key(|(path, _)| path.segments().iter().any(|segment| segment == EVERY));
    let mut schema = Schema::default();
    for (path, spec) in options {
        schema.declare_at(&path, spec)?;
    }
    let (root, frame) = match prefix {
        Some(prefix) => {
            let path = OptionPath::parse(prefix)?;
            let spec = schema
                .lookup(&path)
                .ok_or_else(|| format!("there are no options under `{prefix}`"))?;
            (spec, false)
        }
        None => (schema.root(), true),
    };
    let text = serde_json::to_string_pretty(&crate::schema::document(root, frame))
        .map_err(|e| e.to_string())?;
    println!("{text}");
    Ok(ExitCode::SUCCESS)
}

/// Prints every option under `prefix`, such as `services.nginx` or `users.john.programs`.
pub fn options(modules: &Path, kind: ConfigKind, prefix: Option<&str>) -> Result<ExitCode, String> {
    let prefix = prefix.map(OptionPath::parse).transpose()?;
    let mut sources = Sources::default();
    let mut modules = system::load_modules(modules, &mut sources, ModuleOrder::Sorted)?;
    modules.retain(|module| module.configurations().contains(&kind));
    let mut entries = Vec::new();
    for module in &modules {
        for (path, spec) in module.options() {
            leaves(path.segments().to_vec(), &spec, &mut entries);
        }
    }
    entries.retain(|entry| {
        prefix
            .as_ref()
            .is_none_or(|prefix| matches(prefix, &entry.path))
    });
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    if entries.is_empty() {
        let prefix = prefix.map(|prefix| prefix.to_string()).unwrap_or_default();
        return Err(format!("there are no options under `{prefix}`"));
    }
    print!("{}", render(&entries));
    Ok(ExitCode::SUCCESS)
}

/// One option that holds a value, rather than a table of other options.
#[derive(Debug, PartialEq)]
struct Entry {
    path: Vec<String>,
    spec: OptionSpec,
}

impl Entry {
    fn shown_path(&self) -> String {
        let path: Vec<&str> = self
            .path
            .iter()
            .map(|segment| if segment == EVERY { "<name>" } else { segment })
            .collect();
        path.join(".")
    }
}

/// The options under `path`: records and maps of records are tables of further options; the rest
/// hold values. A map's entries are named by the user, shown as `<name>`.
fn leaves(path: Vec<String>, spec: &OptionSpec, out: &mut Vec<Entry>) {
    let child = |segment: &str| {
        let mut path = path.clone();
        path.push(segment.to_owned());
        path
    };
    match &spec.ty {
        Type::Record(record) => {
            for (name, field) in &record.fields {
                leaves(child(name), field, out);
            }
        }
        Type::Map(item) if matches!(item.ty, Type::Record(_) | Type::Map(_)) => {
            leaves(child(EVERY), item, out);
        }
        _ => out.push(Entry {
            path,
            spec: spec.clone(),
        }),
    }
}

/// Whether an option is under `prefix`. A map's `<name>` matches any name, so `users.john`
/// finds every user's options.
fn matches(prefix: &OptionPath, path: &[String]) -> bool {
    prefix.len() <= path.len()
        && prefix
            .segments()
            .iter()
            .zip(path)
            .all(|(wanted, segment)| segment == EVERY || wanted == segment)
}

fn render(entries: &[Entry]) -> String {
    let mut out = String::new();
    for entry in entries {
        let _ = writeln!(out, "{}\n    {}", entry.shown_path(), about(&entry.spec));
        if !entry.spec.doc.is_empty() {
            let _ = writeln!(out, "    {}", entry.spec.doc);
        }
    }
    out
}

/// An option's type, with its default or whether it's optional or required.
fn about(spec: &OptionSpec) -> String {
    let mut about = describe(&spec.ty);
    match &spec.default {
        Some(default) => {
            let _ = write!(about, ", default {default}");
        }
        None if spec.optional => about.push_str(", optional"),
        None => about.push_str(", required"),
    }
    about
}

/// Every option of system and project configurations as Markdown, by module, for the reference.
pub fn markdown(modules: &Path) -> Result<String, String> {
    let mut sources = Sources::default();
    let mut modules = system::load_modules(modules, &mut sources, ModuleOrder::Sorted)?;
    modules.sort_by(|a, b| a.name().cmp(b.name()));
    let mut out = String::new();
    for (kind, heading) in [
        (ConfigKind::System, "A machine's configuration"),
        (ConfigKind::Project, "A project's `wrath.toml`"),
    ] {
        let _ = writeln!(out, "## {heading}\n");
        for module in modules
            .iter()
            .filter(|m| m.configurations().contains(&kind))
        {
            let mut entries = Vec::new();
            for (path, spec) in module.options() {
                leaves(path.segments().to_vec(), &spec, &mut entries);
            }
            if entries.is_empty() {
                continue;
            }
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            let _ = writeln!(out, "### {}\n", module.name());
            for entry in &entries {
                let _ = write!(out, "- `{}`: {}.", entry.shown_path(), about(&entry.spec));
                if !entry.spec.doc.is_empty() {
                    let _ = write!(out, " {}", entry.spec.doc);
                }
                out.push('\n');
            }
            out.push('\n');
        }
    }
    Ok(out)
}

/// A type in words. Tables of values say what each value is.
fn describe(ty: &Type) -> String {
    match ty {
        Type::Map(item) => format!("a table whose values are each {}", describe(&item.ty)),
        ty => ty.describe(),
    }
}

#[cfg(test)]
mod tests {
    use wrath_eval::{ListMerge, Value};

    use super::*;

    fn spec(ty: Type) -> OptionSpec {
        OptionSpec::new(ty)
    }

    #[test]
    fn options_list_the_values_under_tables_with_their_type_and_default() {
        let site = Type::record([
            (
                "port",
                spec(Type::port())
                    .default(Value::Int(80))
                    .doc("Where it listens."),
            ),
            ("root", spec(Type::Path).optional()),
            (
                "headers",
                spec(Type::map(spec(Type::Str))).default(Value::Table(Default::default())),
            ),
        ]);
        let mut entries = Vec::new();
        let path = vec!["services".to_owned(), "web".to_owned()];
        leaves(path, &spec(Type::map(spec(site))), &mut entries);
        let packages = spec(Type::list(Type::Str, ListMerge::Set));
        leaves(vec!["packages".into()], &packages, &mut entries);

        insta::assert_snapshot!(render(&entries), @"
        services.web.<name>.headers
            a table whose values are each a string, default {}
        services.web.<name>.port
            an integer from 1 to 65535, default 80
            Where it listens.
        services.web.<name>.root
            an absolute path or one starting with `~/`, optional
        packages
            a list of items that are each a string, required
        ");

        let under = |prefix: &str| {
            let prefix = OptionPath::parse(prefix).unwrap();
            entries.iter().filter(|e| matches(&prefix, &e.path)).count()
        };
        assert_eq!(
            under("services.web.main"),
            3,
            "a map entry's name matches any name"
        );
        assert_eq!(under("services.web.main.port"), 1);
        assert_eq!(under("services.mail"), 0);
    }
}
