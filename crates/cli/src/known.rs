//! Package names checked against the recipes before anything builds, so a typo shows up at
//! `check` or `plan`, with where it's written, rather than after a long build.

use std::collections::BTreeSet;
use std::path::Path;

use distro_eval::{Diagnostic, Evaluation, Site, Value, closest};
use distro_model::{DISTRO_SOURCE, Model, package_source};

/// The names packages can have: every recipe's id and every build the configuration declares.
pub fn names(root: &Path, model: &Model) -> Result<BTreeSet<String>, String> {
    let repository = root.join("build");
    if !repository.join("recipes").is_dir() {
        return Err(NO_RECIPES.to_owned());
    }
    let mut names = distro_build::graph::recipe_ids(&repository)?;
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
    if source != DISTRO_SOURCE || names.contains(name) {
        return None;
    }
    Some((name, closest(name, names.iter().map(String::as_str))))
}

/// Where the configuration writes `reference` in a list of packages, the system's or a user's.
fn written_at(evaluation: &Evaluation, reference: &str) -> Option<distro_eval::Span> {
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
                .then_some(span)
        })
}

/// What a distro without recipes says, such as the one a machine runs, which was built elsewhere.
const NO_RECIPES: &str = "this distro has no recipes, so it can't plan or build here; plan and \
                          apply from a checkout of distro, such as with `distro apply --host` \
                          from another machine";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_distro_without_recipes_says_where_to_plan_from() {
        let dir = tempfile::tempdir().unwrap();
        let error = names(dir.path(), &Model::default()).unwrap_err();
        assert!(error.contains("distro apply --host"), "{error}");
    }
}
