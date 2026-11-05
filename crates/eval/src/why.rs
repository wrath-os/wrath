//! Explaining where option values came from.

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
