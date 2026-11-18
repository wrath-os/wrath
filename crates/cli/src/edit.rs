//! `wrath add` and `wrath remove`: editing the packages in a configuration's TOML, keeping its
//! comments and layout, then locking what changed. The TOML stays the only place intent lives.

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table};
use wrath_eval::{Evaluation, OptionPath, Site, Value};
use wrath_model::{Model, package_source};

use crate::{evaluate, run};

/// Adds `packages` to the system's packages, or to `home`'s, and locks them. Nothing is kept
/// when they can't be locked, such as a package that doesn't exist.
pub fn add(
    config: &Path,
    modules: &Path,
    home: Option<&str>,
    packages: &[String],
) -> Result<ExitCode, String> {
    let Some(evaluation) = evaluate(config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    let option = packages_option(&evaluation, home)?;
    let current = strings(&evaluation.options()[&option].value);
    let mut edits = Edits::default();
    let mut names = Vec::new();
    for reference in packages {
        let name = package_name(&evaluation, config, &mut edits, reference)?;
        if current.contains(&name) || names.contains(&name) {
            println!("{name} is already in {option}");
        } else {
            names.push(name);
        }
    }
    if names.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    // Refused before the configuration is touched, so a typo never ends up in it.
    let known = crate::known::names(&Model::from_evaluation(&evaluation))?;
    for name in &names {
        if let Some((package, near)) = crate::known::missing(name, &known) {
            let near = near
                .map(|near| format!("; did you mean `{near}`?"))
                .unwrap_or_default();
            return Err(format!("there's no package `{package}`{near}"));
        }
    }
    let (file, change) = match config_sites(&evaluation, &option).find(|listed| !listed.shared) {
        Some(listed) => (listed.file, Change::Extend(listed.span)),
        None => {
            let (file, keys) = starting_file(config, home);
            (file, Change::Insert(keys))
        }
    };
    edits.packages(&file, change, &names);
    let shown = shown_path(config, &file);
    println!("added {} to {option} in {shown}", names.join(", "));
    edits.apply_and_lock(config, modules)
}

/// Removes `packages` from the system's packages, or from `home`'s, wherever the configuration
/// sets them, and locks again.
pub fn remove(
    config: &Path,
    modules: &Path,
    home: Option<&str>,
    packages: &[String],
) -> Result<ExitCode, String> {
    let Some(evaluation) = evaluate(config, modules) else {
        return Ok(ExitCode::FAILURE);
    };
    let option = packages_option(&evaluation, home)?;
    let sites: Vec<_> = config_sites(&evaluation, &option).collect();
    let current = strings(&evaluation.options()[&option].value);
    if let Some(missing) = packages.iter().find(|package| !current.contains(package)) {
        return Err(format!("`{missing}` isn't in {option}"));
    }
    let mut edits = Edits::default();
    let mut excluded = Vec::new();
    for listed in &sites {
        let gone: Vec<String> = packages
            .iter()
            .filter(|package| listed.items.contains(package))
            .cloned()
            .collect();
        if gone.is_empty() {
            continue;
        }
        let shown = shown_path(config, &listed.file);
        if listed.shared {
            println!(
                "{} comes from {shown}, which other configurations can include too",
                gone.join(", ")
            );
            excluded.extend(gone);
        } else {
            edits.packages(&listed.file, Change::Remove(listed.span.clone()), &gone);
            println!("removed {} from {shown}", gone.join(", "));
        }
    }
    for package in packages {
        if !sites.iter().any(|listed| listed.items.contains(package)) {
            println!("{package} comes from a module");
            excluded.push(package.clone());
        }
    }
    if !excluded.is_empty() {
        excluded.sort();
        excluded.dedup();
        let (file, keys) = starting_file(config, home);
        println!(
            "removed {} with `{{ remove = [...] }}` in {}, leaving the shared file alone",
            excluded.join(", "),
            shown_path(config, &file)
        );
        edits.packages(&file, Change::Exclude(keys), &excluded);
    }
    edits.apply_and_lock(config, modules)
}

/// The option that holds the packages: `packages`, or a user's `users.<name>.packages`.
fn packages_option(evaluation: &Evaluation, home: Option<&str>) -> Result<OptionPath, String> {
    let option = match home {
        Some(user) => OptionPath::root()
            .child("users")
            .child(user)
            .child("packages"),
        None => OptionPath::root().child("packages"),
    };
    if evaluation.options().contains_key(&option) {
        return Ok(option);
    }
    Err(match home {
        Some(user) => format!("there's no user `{user}` in the configuration"),
        None => "the configuration has no `packages` option; is the packages module there?".into(),
    })
}

/// A list of packages in one of the configuration's files.
struct Listed {
    file: PathBuf,
    /// Where the list is in the file.
    span: Range<usize>,
    items: Vec<String>,
    /// Whether the file is only reached through an include, so other configurations may share
    /// it. Commands leave such files alone.
    shared: bool,
}

/// Where the configuration sets `option` itself.
fn config_sites<'a>(
    evaluation: &'a Evaluation,
    option: &OptionPath,
) -> impl Iterator<Item = Listed> + 'a {
    evaluation.options()[option]
        .set_by
        .iter()
        .filter_map(|contribution| match &contribution.site {
            Site::Config(span) => Some(Listed {
                file: PathBuf::from(evaluation.sources().name(span.file)),
                span: span.start..span.end,
                items: strings(&contribution.value),
                shared: !evaluation.is_starting_file(span.file),
            }),
            Site::Default | Site::Module { .. } => None,
        })
}

/// The starting file that holds the packages, and the key in it: the user's own file when there
/// is one, or the role's file or `system.toml`. Packages go here when no starting file lists any
/// yet, and so do removals of what shared files add.
fn starting_file(config: &Path, home: Option<&str>) -> (PathBuf, Vec<String>) {
    let start = match crate::role(config) {
        Some(role) => config.join("roles").join(format!("{role}.toml")),
        None if crate::is_project(config) => crate::project::starting_file(config),
        None => config.join("system.toml"),
    };
    match home {
        Some(user) => {
            let own = config.join("users").join(format!("{user}.toml"));
            if own.exists() {
                (own, vec!["packages".into()])
            } else {
                let keys = vec!["users".into(), user.to_owned(), "packages".into()];
                (start, keys)
            }
        }
        None => (start, vec!["packages".into()]),
    }
}

/// The name the configuration uses for a package the command line names. A source given as a
/// URL is declared under a name taken from the URL, or the name it already has.
fn package_name(
    evaluation: &Evaluation,
    config: &Path,
    edits: &mut Edits,
    reference: &str,
) -> Result<String, String> {
    let (source, attribute) = package_source(reference);
    run::refuse_version(reference, attribute)?;
    let declared: BTreeMap<String, String> = evaluation
        .declared_sources()
        .iter()
        .map(|(name, source)| (name.clone(), source.url.clone()))
        .collect();
    if !reference.contains('#') || declared.contains_key(source) {
        return Ok(reference.to_owned());
    }
    if !source.contains(':') {
        let known = declared.keys().map(String::as_str);
        return Err(run::no_such_source(source, attribute, known));
    }
    let name = match declared.iter().find(|(_, url)| url.as_str() == source) {
        Some((name, _)) => name.clone(),
        None => {
            let name = source_name(source, |name| {
                declared.contains_key(name) || edits.declares(name)
            });
            edits.source(&starting_file(config, None).0, &name, source);
            println!("declared source {name} as {source}");
            name
        }
    };
    Ok(format!("{name}#{attribute}"))
}

/// A name for a source from its URL: its last path segment, as `recipes` for
/// `github:acme/recipes`, numbered when `taken` already has it.
fn source_name(url: &str, taken: impl Fn(&str) -> bool) -> String {
    let last = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit(['/', ':'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("source");
    let base: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let base = base.trim_end_matches(".git");
    if !taken(base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| base.to_owned())
}

/// How a file's packages change.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Change {
    /// Add to the list whose value is at this span.
    Extend(Range<usize>),
    /// Remove from the list whose value is at this span.
    Remove(Range<usize>),
    /// Add to the list at this key, which isn't a plain list in this file yet.
    Insert(Vec<String>),
    /// Remove, in this file, items that files it includes add, with `{ remove = [...] }`.
    Exclude(Vec<String>),
}

/// Changes to make to files, applied together: each file's lists from the back, so the spans
/// read from the original text stay right, then new keys, then new sources.
#[derive(Default)]
struct Edits {
    packages: BTreeMap<PathBuf, Vec<(Change, Vec<String>)>>,
    /// Sources to declare: the file, the name and the URL.
    sources: Vec<(PathBuf, String, String)>,
}

impl Edits {
    fn packages(&mut self, file: &Path, change: Change, names: &[String]) {
        let changes = self.packages.entry(file.to_path_buf()).or_default();
        changes.push((change, names.to_vec()));
    }

    fn source(&mut self, file: &Path, name: &str, url: &str) {
        let declared = (file.to_path_buf(), name.to_owned(), url.to_owned());
        self.sources.push(declared);
    }

    fn declares(&self, name: &str) -> bool {
        self.sources.iter().any(|(_, declared, _)| declared == name)
    }

    /// Each changed file's text before and after.
    fn texts(self) -> Result<BTreeMap<PathBuf, (String, String)>, String> {
        let mut texts = BTreeMap::new();
        for (file, mut changes) in self.packages {
            changes.sort_by_key(|(change, _)| match change {
                Change::Extend(span) | Change::Remove(span) => std::cmp::Reverse(span.start),
                Change::Insert(_) | Change::Exclude(_) => std::cmp::Reverse(0),
            });
            let (_, text) = text_of(&mut texts, &file);
            for (change, names) in changes {
                *text = match change {
                    Change::Extend(span) => extend(text, span, &names),
                    Change::Remove(span) => remove_items(text, span, &names),
                    Change::Insert(keys) => edit_list(text, &keys, &names, true),
                    Change::Exclude(keys) => edit_list(text, &keys, &names, false),
                }
                .map_err(|e| format!("{}: {e}", file.display()))?;
            }
        }
        for (file, name, url) in self.sources {
            let (_, text) = text_of(&mut texts, &file);
            *text = declare_source(text, &name, &url)
                .map_err(|e| format!("{}: {e}", file.display()))?;
        }
        Ok(texts)
    }

    /// Writes the edited files, then locks the configuration. If it doesn't evaluate or can't
    /// be locked, the files are put back as they were.
    fn apply_and_lock(self, config: &Path, modules: &Path) -> Result<ExitCode, String> {
        let texts = self.texts()?;
        for (file, (_, edited)) in &texts {
            std::fs::write(file, edited).map_err(|e| format!("{}: {e}", file.display()))?;
        }
        let locked = crate::lock(config, modules, None).and_then(|code| {
            if code == ExitCode::FAILURE {
                Err("the edited configuration has errors".to_owned())
            } else {
                Ok(code)
            }
        });
        locked.map_err(|message| {
            for (file, (original, _)) in &texts {
                let _ = std::fs::write(file, original);
            }
            format!("{message}; the configuration is unchanged")
        })
    }
}

/// The text of `file` being edited, read the first time it's asked for.
fn text_of<'a>(
    texts: &'a mut BTreeMap<PathBuf, (String, String)>,
    file: &Path,
) -> &'a mut (String, String) {
    texts.entry(file.to_path_buf()).or_insert_with(|| {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        (text.clone(), text)
    })
}

/// `text` with the source `name` declared to follow `url`.
fn declare_source(text: &str, name: &str, url: &str) -> Result<String, String> {
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let sources = doc
        .entry("sources")
        .or_insert_with(|| Item::Table(implicit_table()))
        .as_table_mut()
        .ok_or("`sources` isn't a table")?;
    let mut table = Table::new();
    table.insert("url", toml_edit::value(url));
    sources.insert(name, Item::Table(table));
    Ok(doc.to_string())
}

fn implicit_table() -> Table {
    let mut table = Table::new();
    table.set_implicit(true);
    table
}

/// `text` with `names` added to the list at `span`, keeping its layout.
fn extend(text: &str, span: Range<usize>, names: &[String]) -> Result<String, String> {
    edit_array(text, span, |array| {
        for name in names {
            array.push(name.as_str());
        }
    })
}

/// `text` with `names` taken out of the list at `span`.
fn remove_items(text: &str, span: Range<usize>, names: &[String]) -> Result<String, String> {
    edit_array(text, span, |array| {
        array.retain(|item| !item.as_str().is_some_and(|s| names.iter().any(|n| n == s)));
    })
}

fn edit_array(
    text: &str,
    span: Range<usize>,
    edit: impl FnOnce(&mut Array),
) -> Result<String, String> {
    let original = text
        .get(span.clone())
        .ok_or("the list moved since it was read")?;
    let mut value: toml_edit::Value = original.parse().map_err(|e| format!("{e}"))?;
    let array = value
        .as_array_mut()
        .ok_or_else(|| format!("`{original}` isn't a list"))?;
    edit(array);
    if !original.contains('\n') {
        array.fmt();
    }
    let edited = value.to_string();
    Ok(format!(
        "{}{}{}",
        &text[..span.start],
        edited.trim_start(),
        &text[span.end..]
    ))
}

/// `text` with `names` added to, or removed from, the list at `keys`, whatever form it has: no
/// key yet, a plain list, or `{ add = [...], remove = [...] }`. Removing turns a plain list into
/// that table.
fn edit_list(text: &str, keys: &[String], names: &[String], add: bool) -> Result<String, String> {
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let (last, tables) = keys.split_last().ok_or("no key to set")?;
    let mut table = doc.as_table_mut();
    for key in tables {
        table = table
            .entry(key)
            .or_insert_with(|| Item::Table(implicit_table()))
            .as_table_mut()
            .ok_or_else(|| format!("`{key}` isn't a table"))?;
    }
    let names: Array = names.iter().map(String::as_str).collect();
    let list = match table.remove(last) {
        None => None,
        Some(Item::Value(value)) => Some(value),
        Some(_) => return Err(format!("`{last}` isn't a list")),
    };
    let edited = match (list, add) {
        (None, true) => toml_edit::Value::Array(names),
        (None, false) => change_table(InlineTable::new(), REMOVE_KEY, names)?,
        (Some(toml_edit::Value::Array(mut array)), true) => {
            array.extend(names);
            toml_edit::Value::Array(array)
        }
        (Some(toml_edit::Value::Array(array)), false) => {
            let mut change = InlineTable::new();
            if !array.is_empty() {
                change.insert(ADD_KEY, toml_edit::Value::Array(array));
            }
            change_table(change, REMOVE_KEY, names)?
        }
        (Some(toml_edit::Value::InlineTable(change)), add) => {
            change_table(change, if add { ADD_KEY } else { REMOVE_KEY }, names)?
        }
        (Some(_), _) => return Err(format!("`{last}` isn't a list")),
    };
    table.insert(last, Item::Value(edited));
    Ok(doc.to_string())
}

const ADD_KEY: &str = "add";
const REMOVE_KEY: &str = "remove";

/// `change`, an `{ add = [...], remove = [...] }` table, with `names` appended under `key`.
fn change_table(
    mut change: InlineTable,
    key: &str,
    names: Array,
) -> Result<toml_edit::Value, String> {
    match change.get_mut(key) {
        Some(toml_edit::Value::Array(array)) => array.extend(names),
        Some(_) => return Err(format!("`{key}` isn't a list")),
        None => {
            change.insert(key, toml_edit::Value::Array(names));
        }
    }
    change.fmt();
    Ok(toml_edit::Value::InlineTable(change))
}

fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::List(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// `file` relative to the configuration, as people know it.
fn shown_path(config: &Path, file: &Path) -> String {
    file.strip_prefix(config)
        .unwrap_or(file)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span_of(text: &str, value: &str) -> Range<usize> {
        let start = text.find(value).unwrap();
        start..start + value.len()
    }

    #[test]
    fn lists_keep_their_layout_when_packages_are_added_or_removed() {
        let text = "# tools\npackages = [\"git\", \"jq\"] # mine\n\n[system]\nhostname = \"box\"\n";
        let span = span_of(text, "[\"git\", \"jq\"]");
        let added = extend(text, span.clone(), &["ripgrep".into()]).unwrap();
        assert_eq!(
            added,
            "# tools\npackages = [\"git\", \"jq\", \"ripgrep\"] # mine\n\n\
             [system]\nhostname = \"box\"\n"
        );
        let removed = remove_items(text, span, &["git".into()]).unwrap();
        assert_eq!(
            removed,
            "# tools\npackages = [\"jq\"] # mine\n\n[system]\nhostname = \"box\"\n"
        );

        let multiline = "packages = [\n  \"git\", # for work\n  \"jq\",\n]\n";
        let span = span_of(multiline, "[\n  \"git\", # for work\n  \"jq\",\n]");
        let added = extend(multiline, span, &["rg".into()]).unwrap();
        assert!(added.contains("# for work"), "{added}");
        assert!(added.contains("\"rg\""), "{added}");
    }

    #[test]
    fn a_missing_list_is_added_where_toml_allows_it() {
        let text = "[system]\nhostname = \"box\"\n";
        assert_eq!(
            edit_list(text, &["packages".into()], &["jq".into()], true).unwrap(),
            "packages = [\"jq\"]\n[system]\nhostname = \"box\"\n"
        );
        let keys = ["users".into(), "john".into(), "packages".into()];
        let users = edit_list(
            "[users.john]\nshell = \"bash\"\n",
            &keys,
            &["jq".into()],
            true,
        )
        .unwrap();
        assert_eq!(
            users,
            "[users.john]\nshell = \"bash\"\npackages = [\"jq\"]\n"
        );
    }

    #[test]
    fn removing_what_a_shared_file_adds_writes_a_removal() {
        let keys = ["packages".into()];
        let gone = ["git".into()];
        let cases = [
            (
                "[system]\n",
                "packages = { remove = [\"git\"] }\n[system]\n",
            ),
            (
                "packages = [\"htop\"]\n",
                "packages = { add = [\"htop\"], remove = [\"git\"] }\n",
            ),
            (
                "packages = { remove = [\"jq\"] }\n",
                "packages = { remove = [\"jq\", \"git\"] }\n",
            ),
            ("packages = []\n", "packages = { remove = [\"git\"] }\n"),
        ];
        for (text, expected) in cases {
            assert_eq!(
                edit_list(text, &keys, &gone, false).unwrap(),
                expected,
                "{text}"
            );
        }
    }

    #[test]
    fn adding_next_to_a_removal_writes_add() {
        let keys = ["users".into(), "john".into(), "packages".into()];
        let text = "[users.john]\npackages = { remove = [\"git\"] }\n";
        assert_eq!(
            edit_list(text, &keys, &["htop".into()], true).unwrap(),
            "[users.john]\npackages = { remove = [\"git\"], add = [\"htop\"] }\n"
        );
    }

    #[test]
    fn several_edits_to_one_file_apply_together() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("system.toml");
        let text = "packages = [\"git\"]\n[users.john]\npackages = [\"git\", \"jq\"]\n";
        std::fs::write(&file, text).unwrap();
        let first = span_of(text, "[\"git\"]");
        let second = span_of(text, "[\"git\", \"jq\"]");

        let mut edits = Edits::default();
        edits.packages(&file, Change::Remove(first), &["git".into()]);
        edits.packages(&file, Change::Extend(second), &["stable#rg".into()]);
        edits.source(&file, "stable", "github:wrath-os/grapes/rel-25.11");
        let (_, edited) = edits.texts().unwrap().remove(&file).unwrap();

        assert_eq!(
            edited,
            "packages = []\n[users.john]\npackages = [\"git\", \"jq\", \"stable#rg\"]\n\n\
             [sources.stable]\nurl = \"github:wrath-os/grapes/rel-25.11\"\n"
        );
    }

    #[test]
    fn sources_are_named_after_their_url() {
        let free = |_: &str| false;
        assert_eq!(source_name("github:jqlang/jq", free), "jq");
        assert_eq!(
            source_name("github:wrath-os/grapes/rel-25.05", free),
            "rel-25.05"
        );
        assert_eq!(
            source_name("git+https://example.com/Tools.git?ref=main", free),
            "tools"
        );
        assert_eq!(source_name("github:jqlang/jq", |name| name == "jq"), "jq-2");
    }
}
