//! Explaining where option values came from.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::eval::{Contribution, Evaluation};
use crate::path::OptionPath;
use crate::suggest::closest;
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

/// Everything the modules rendered, as text: packages, then files, then units, each sorted.
pub fn describe_outputs(evaluation: &Evaluation) -> String {
    let mut packages = BTreeSet::new();
    let mut files = BTreeMap::new();
    let mut units = BTreeMap::new();
    let mut base = BTreeMap::new();
    let mut backends = BTreeMap::new();
    for rendered in evaluation.modules() {
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
        packages.extend(rendered.output.packages.iter());
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
    let packages: Vec<&str> = packages.into_iter().map(String::as_str).collect();
    // Writing to a String cannot fail.
    let _ = writeln!(out, "packages: {}", packages.join(" "));
    for (path, (module, file)) in files {
        let _ = writeln!(out, "\nfile {path} (mode {:o}, from {module})", file.mode);
        out.push_str(&file.text);
    }
    for (name, (module, unit)) in units {
        let _ = writeln!(out, "\nunit {name} (from {module})");
        for section in &unit.sections {
            let _ = writeln!(out, "[{}]", section.name);
            for (key, value) in &section.entries {
                let _ = writeln!(out, "{key}={value}");
            }
        }
    }
    for (key, (module, value)) in base {
        let _ = writeln!(out, "\nbase {key} (from {module}): {value}");
    }
    for (name, (module, value)) in backends {
        let _ = writeln!(out, "\nbackend {name} (from {module}): {value}");
    }
    out
}
