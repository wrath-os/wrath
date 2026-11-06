//! Explaining where option values came from.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::eval::{Contribution, Evaluation};
use crate::module::home_path;
use crate::path::OptionPath;
use crate::suggest::closest;
use crate::value::Value;
use crate::walk::{Level, Site};

/// Explains every option under `prefix`, or every option when it is `None`.
pub fn explain(evaluation: &Evaluation, prefix: Option<&OptionPath>) -> Result<String, String> {
    let root = OptionPath::root();
    let prefix = prefix.unwrap_or(&root);
    let mut out = String::new();
    for (path, resolved) in evaluation
        .options()
        .range(prefix.clone()..)
        .take_while(|(path, _)| path.starts_with(prefix))
    {
        // Writing to a String cannot fail.
        let _ = writeln!(out, "{path} = {}", resolved.value);
        let several = resolved.set_by.len() > 1;
        for contribution in &resolved.set_by {
            let _ = write!(out, "  {}", origin(evaluation, contribution));
            if several {
                let _ = write!(out, ": {}", contribution.value);
            }
            out.push('\n');
        }
        for contribution in &resolved.overridden {
            let _ = writeln!(
                out,
                "  overrides {}, {}",
                contribution.value,
                origin(evaluation, contribution)
            );
        }
        for contribution in &resolved.removed_by {
            let _ = writeln!(
                out,
                "  removes {}, {}",
                contribution.value,
                origin(evaluation, contribution)
            );
        }
        for used in &resolved.uses {
            let _ = writeln!(
                out,
                "  uses {} at {}",
                used.path,
                evaluation.sources().location(used.span)
            );
        }
    }
    if out.is_empty() {
        let names: Vec<String> = evaluation
            .options()
            .keys()
            .map(ToString::to_string)
            .collect();
        let target = prefix.to_string();
        let mut message = format!("no option `{prefix}` has a value");
        if let Some(name) = closest(&target, names.iter().map(String::as_str)) {
            let _ = write!(message, "; did you mean `{name}`?");
        }
        return Err(message);
    }
    Ok(out)
}

fn origin(evaluation: &Evaluation, contribution: &Contribution) -> String {
    match (contribution.site, contribution.level, &contribution.reason) {
        (Site::Default, _, _) => "default".into(),
        (Site::Config(span), Level::Force, Some(reason)) => format!(
            "forced at {} ({reason})",
            evaluation.sources().location(span)
        ),
        (Site::Config(span), _, _) => format!("set at {}", evaluation.sources().location(span)),
        (Site::Module { module, at }, level, _) => {
            let how = match level {
                Level::Default => "default from",
                Level::Normal | Level::Force => "set by",
            };
            let name = evaluation.module_name(module);
            match at {
                Some(span) => format!(
                    "{how} module `{name}` at {}",
                    evaluation.sources().location(span)
                ),
                None => format!("{how} module `{name}`"),
            }
        }
    }
}

/// Explains where a file or a package comes from: the module that renders it, and the options
/// set in the configuration that the module read. Files are named as plans show them:
/// `/etc/nginx/main.conf`, or `~john/.config/git/config` in a home. `None` when no module
/// renders it.
pub fn explain_output(evaluation: &Evaluation, thing: &str) -> Option<String> {
    let mut out = String::new();
    for rendered in evaluation.modules() {
        let output = &rendered.output;
        let how = if output.files.contains_key(thing) || output.templates.contains_key(thing) {
            "is written".to_owned()
        } else if output.packages.contains(thing) {
            "is installed for everyone".to_owned()
        } else if let Some((user, _)) = output.homes.iter().find(|(user, home)| {
            home.files
                .keys()
                .chain(home.templates.keys())
                .any(|path| home_path(user, path) == thing)
        }) {
            format!("is written in {user}'s home")
        } else if let Some((user, _)) = output
            .homes
            .iter()
            .find(|(_, home)| home.packages.contains(thing))
        {
            format!("is installed for {user}")
        } else {
            continue;
        };
        // Writing to a String cannot fail.
        let _ = writeln!(out, "{thing} {how} by module `{}`", rendered.name);
        let set: Vec<(&OptionPath, &Value, &Contribution)> = evaluation
            .options()
            .iter()
            .filter(|(path, _)| rendered.reads.iter().any(|read| path.starts_with(read)))
            .flat_map(|(path, resolved)| {
                resolved
                    .set_by
                    .iter()
                    .filter(|contribution| matches!(contribution.site, Site::Config(_)))
                    .map(move |contribution| (path, &resolved.value, contribution))
            })
            .collect();
        // Modules read whole tables, so narrow down to what's about this: the values that name
        // a package, or else the options named after the module.
        let naming: Vec<_> = set
            .iter()
            .filter(|(_, _, contribution)| names(&contribution.value, thing))
            .copied()
            .collect();
        let own: Vec<_> = set
            .iter()
            .filter(|(path, _, _)| path.segments().contains(&rendered.name))
            .copied()
            .collect();
        let shown = [naming, own, set].into_iter().find(|set| !set.is_empty());
        match shown {
            None => {
                let _ = writeln!(
                    out,
                    "  from defaults; nothing it reads is set in the configuration"
                );
            }
            Some(shown) => {
                let _ = writeln!(out, "  from what the configuration sets:");
                for (path, value, contribution) in shown {
                    let _ = writeln!(
                        out,
                        "    {path} = {value}, {}",
                        origin(evaluation, contribution)
                    );
                }
            }
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Everything the modules rendered, as text: packages (users' with who they're for), then files,
/// then units, each sorted.
pub fn describe_outputs(evaluation: &Evaluation) -> String {
    let mut packages = BTreeSet::new();
    let mut files = BTreeMap::new();
    let mut units = BTreeMap::new();
    let mut base = BTreeMap::new();
    let mut backends = BTreeMap::new();
    let mut triggers = Vec::new();
    for rendered in evaluation.modules() {
        triggers.extend(
            rendered
                .output
                .triggers
                .iter()
                .map(|((unit, action), paths)| (unit, action.as_str(), &rendered.name, paths)),
        );
        backends.extend(
            rendered
                .output
                .backends
                .iter()
                .map(|(name, value)| (name, (&rendered.name, value))),
        );
        base.extend(
            rendered
                .output
                .base
                .iter()
                .map(|(key, value)| (key, (&rendered.name, value))),
        );
        packages.extend(rendered.output.packages.iter().cloned());
        packages.extend(rendered.output.homes.iter().flat_map(|(user, home)| {
            home.packages
                .iter()
                .map(move |package| format!("{package} (for {user})"))
        }));
        files.extend(
            rendered
                .output
                .files
                .iter()
                .map(|(path, file)| (path, (&rendered.name, file))),
        );
        units.extend(
            rendered
                .output
                .units
                .iter()
                .map(|(name, unit)| (name, (&rendered.name, unit))),
        );
    }
    let mut out = String::new();
    let packages: Vec<String> = packages.into_iter().collect();
    // Writing to a String cannot fail.
    let _ = writeln!(out, "packages: {}", packages.join(" "));
    for (path, (module, file)) in files {
        let _ = writeln!(out, "\nfile {path} (mode {:o}, from {module})", file.mode);
        out.push_str(&file.text);
    }
    for (name, (module, unit)) in units {
        let _ = writeln!(out, "\nunit {name} (from {module})");
        out.push_str(&unit.text());
    }
    triggers.sort();
    for (unit, action, module, paths) in triggers {
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let _ = writeln!(
            out,
            "\n{action} {unit} (from {module}) on {}",
            paths.join(" ")
        );
    }
    for (key, (module, value)) in base {
        let _ = writeln!(out, "\nbase {key} (from {module}): {value}");
    }
    for (name, (module, value)) in backends {
        let _ = writeln!(out, "\nbackend {name} (from {module}): {value}");
    }
    out
}

/// Whether `value` is `name`, or a list with `name` in it.
fn names(value: &Value, name: &str) -> bool {
    match value {
        Value::Str(text) => text == name,
        Value::List(items) => items.iter().any(|item| names(item, name)),
        _ => false,
    }
}
