//! Package names checked against the recipes before anything builds, so a typo shows up at
//! `check` or `plan`, with where it's written, rather than after a long build.

use std::collections::BTreeSet;

use wrath_eval::{Diagnostic, Evaluation, Site, Value, closest};
use wrath_model::{Model, WRATH_SOURCE, package_source};

/// Whether wrath has recipes to build from: grapes, as [`crate::places`] finds it. A checkout or
/// a grapes beside it does; the wrath a machine runs has none yet.
pub fn recipes_here() -> bool {
    crate::places::grapes().is_ok_and(|grapes| grapes.join("recipes").is_dir())
}

/// The names packages can have: every recipe's id and every build the configuration declares.
pub fn names(model: &Model) -> Result<BTreeSet<String>, String> {
    if !recipes_here() {
        return Err(NO_RECIPES.to_owned());
    }
    let mut names = wrath_build::graph::recipe_ids(&crate::places::grapes()?)?;
    names.extend(model.builds.keys().cloned());
    Ok(names)
}

/// An error for each package `model` installs that no recipe or build provides, pointing at where
/// the configuration names it, with the closest name there is. Packages from other sources are
/// checked when they're fetched.
pub fn unknown(
    evaluation: &Evaluation,
    model: &Model,
    names: &BTreeSet<String>,
) -> Vec<Diagnostic> {
    let mut found = Vec::new();
    for reference in model.all_packages() {
        let Some((name, near)) = missing(reference, names) else {
            continue;
        };
        let diagnostic = Diagnostic::error(format!("there's no package `{name}`"));
        let mut diagnostic = match written_at(evaluation, reference) {
            Some(span) => diagnostic.label(span, "named here"),
            None => diagnostic.note("a module adds it"),
        };
        if let Some(near) = near {
            diagnostic = diagnostic.note(format!("help: did you mean `{near}`?"));
        }
        found.push(diagnostic);
    }
    found
}

/// The package `reference` names, when it should come from a recipe or build but none of `names`
/// is it, with the closest name there is.
pub fn missing<'a>(
    reference: &'a str,
    names: &'a BTreeSet<String>,
) -> Option<(&'a str, Option<&'a str>)> {
    let (source, name) = package_source(reference);
    if source != WRATH_SOURCE || names.contains(name) {
        return None;
    }
    Some((name, closest(name, names.iter().map(String::as_str))))
}

/// Where the configuration writes `reference` in a list of packages, the system's or a user's.
fn written_at(evaluation: &Evaluation, reference: &str) -> Option<wrath_eval::Span> {
    evaluation
        .options()
        .iter()
        .filter(|(path, _)| {
            path.segments()
                .last()
                .is_some_and(|last| last == "packages")
        })
        .flat_map(|(_, resolved)| &resolved.set_by)
        .find_map(|contribution| {
            let Site::Config(span) = contribution.site else {
                return None;
            };
            let Value::List(items) = &contribution.value else {
                return None;
            };
            items
                .iter()
                .any(|item| matches!(item, Value::Str(text) if text == reference))
                .then(|| item_in(evaluation.sources().text(span.file), span, reference))
        })
}

/// The span of the string `item` inside the list at `list` in `text`, or the list's when it isn't
/// written there as a plain string.
fn item_in(text: &str, list: wrath_eval::Span, item: &str) -> wrath_eval::Span {
    let written = text.get(list.start..list.end).unwrap_or_default();
    ["\"", "'"]
        .iter()
        .find_map(|quote| written.find(&format!("{quote}{item}{quote}")))
        .map_or(list, |at| {
            let start = list.start + at;
            wrath_eval::Span::new(list.file, start..start + item.len() + 2)
        })
}

/// What a wrath without recipes says, such as the one a machine runs, which was built elsewhere.
pub(crate) const NO_RECIPES: &str = concat!(
    "this wrath has no recipes, so it can't plan or build here; plan and apply from a ",
    "checkout of wrath, such as with `wrath apply --host` from another machine",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_package_is_pointed_at_in_its_list() {
        let text = "packages = [\"git\", \"neovim\", 'jq']\n";
        let file = wrath_eval::Sources::default().add("system.toml", text);
        let list = wrath_eval::Span::new(file, 11..text.len() - 1);
        let at = |item: &str| {
            let span = item_in(text, list, item);
            &text[span.start..span.end]
        };
        assert_eq!(at("neovim"), "\"neovim\"");
        assert_eq!(at("jq"), "'jq'");
        assert_eq!(at("vim"), "[\"git\", \"neovim\", 'jq']");
    }

    #[test]
    fn a_wrath_without_recipes_says_where_to_plan_from() {
        let error = names(&Model::default()).unwrap_err();
        assert!(error.contains("wrath apply --host"), "{error}");
    }
}
