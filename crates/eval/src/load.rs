//! Finding and reading the files of a configuration tree.
//!
//! A configuration is `system.toml`, everything under `services/`, and `users/<name>.toml`,
//! which configures the user `<name>`. Any file can pull in more files with
//! `include = ["profiles/laptop.toml"]`; paths are relative to the configuration root and an
//! included file configures the same scope as the file that includes it. A file included with
//! `{ path = "...", as = "defaults" }` gives defaults, and so does everything it includes.

use std::collections::HashMap;
use std::io;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::diag::Diagnostic;
use crate::fs::ConfigFs;
use crate::path::OptionPath;
use crate::source::{FileId, Sources, Span};
use crate::suggest::did_you_mean;
use crate::walk::only_keys;

/// The key that includes other files. It's only recognised at the top of a file.
pub(crate) const INCLUDE: &str = "include";

#[derive(Debug, Clone)]
pub(crate) struct LoadedFile {
    pub file: FileId,
    /// The option the file's top-level keys belong to.
    pub root: OptionPath,
    /// Whether the configuration starts from this file, rather than only reaching it through
    /// includes.
    pub start: bool,
    /// Where the file is included from.
    pub inclusions: Vec<Inclusion>,
    /// Whether the file's values are defaults: it's included as defaults, or from a file that is.
    pub defaults: bool,
}

/// An include entry naming a file.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Inclusion {
    /// The entry, in the including file.
    pub at: Span,
    /// Written `as = "defaults"`.
    pub as_defaults: bool,
}

/// Loads the files of a configuration, starting from `role_given`, or else its only role or
/// `system.toml`, and returns the role it started from.
pub(crate) fn load(
    fs: &dyn ConfigFs,
    sources: &mut Sources,
    role_given: Option<&str>,
) -> (Vec<LoadedFile>, Vec<Diagnostic>, Option<String>) {
    let mut loader = Loader {
        fs,
        sources,
        files: Vec::new(),
        seen: HashMap::new(),
        diagnostics: Vec::new(),
    };
    let role = match starting_file(fs, role_given) {
        Ok(path) => {
            loader.add(&path, &OptionPath::root(), None);
            path.strip_prefix("roles/")
                .and_then(|name| name.strip_suffix(".toml"))
                .map(str::to_owned)
        }
        Err(diagnostic) => {
            loader.diagnostics.push(diagnostic);
            None
        }
    };
    loader.add_listed("services", |_| Some(OptionPath::root()));
    loader.add_listed("users", |path| {
        let name = path.strip_prefix("users/")?.strip_suffix(".toml")?;
        (!name.contains('/')).then(|| OptionPath::root().child("users").child(name))
    });
    let mut diagnostics = loader.diagnostics;
    diagnostics.extend(mark_defaults(&mut loader.files, loader.sources));
    (loader.files, diagnostics, role)
}

struct Loader<'a> {
    fs: &'a dyn ConfigFs,
    sources: &'a mut Sources,
    files: Vec<LoadedFile>,
    /// Each path read or being read, with its index in `files` once it has one.
    seen: HashMap<String, Option<usize>>,
    diagnostics: Vec<Diagnostic>,
}

impl Loader<'_> {
    fn add_listed(&mut self, dir: &str, root: impl Fn(&str) -> Option<OptionPath>) {
        match self.fs.toml_files(dir) {
            Ok(paths) => {
                for path in paths {
                    match root(&path) {
                        Some(root) => self.add(&path, &root, None),
                        None => self.diagnostics.push(
                            Diagnostic::error(format!(
                                "`{}` is not loaded",
                                self.fs.display(&path)
                            ))
                            .note(format!(
                                "help: files in `{dir}/` must be directly in it, or included \
                                 from another file"
                            )),
                        ),
                    }
                }
            }
            Err(e) => self.diagnostics.push(Diagnostic::error(format!(
                "cannot list `{}`: {e}",
                self.fs.display(dir)
            ))),
        }
    }

    /// Reads a file and the files it includes. `inclusion` is the include entry naming it, `None`
    /// for a file the configuration starts from.
    fn add(&mut self, path: &str, root: &OptionPath, inclusion: Option<Inclusion>) {
        let included_at = inclusion.map(|inclusion| inclusion.at);
        if let Some(&index) = self.seen.get(path) {
            if let Some(index) = index {
                let loaded = &mut self.files[index];
                match inclusion {
                    Some(inclusion) => loaded.inclusions.push(inclusion),
                    None => loaded.start = true,
                }
            }
            return;
        }
        self.seen.insert(path.to_owned(), None);
        let text = match self.fs.read(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound && included_at.is_none() => {
                self.diagnostics.push(
                    Diagnostic::error(format!("`{}` does not exist", self.fs.display(path)))
                        .note("help: every configuration starts from `system.toml`"),
                );
                return;
            }
            Err(e) => {
                self.diagnostics.push(
                    Diagnostic::error(format!("cannot read `{}`: {e}", self.fs.display(path)))
                        .label_opt(included_at, "included here"),
                );
                return;
            }
        };
        let file = self.sources.add(self.fs.display(path), text);
        self.seen.insert(path.to_owned(), Some(self.files.len()));
        self.files.push(LoadedFile {
            file,
            root: root.clone(),
            start: inclusion.is_none(),
            inclusions: inclusion.into_iter().collect(),
            defaults: false,
        });
        let (includes, diagnostics) = match DeTable::parse(self.sources.text(file)) {
            Ok(table) => includes(table.get_ref(), file),
            Err(e) => {
                let message = e.message().trim().replace('\n', "; ");
                self.diagnostics.push(
                    Diagnostic::error("invalid TOML")
                        .label_opt(e.span().map(|range| Span::new(file, range)), message),
                );
                return;
            }
        };
        self.diagnostics.extend(diagnostics);
        for include in includes {
            self.add(&include.path, root, Some(include.inclusion));
        }
    }
}

/// One entry of a file's `include` list.
struct Include {
    path: String,
    inclusion: Inclusion,
}

fn includes(table: &DeTable<'_>, file: FileId) -> (Vec<Include>, Vec<Diagnostic>) {
    let Some(value) = table.get(INCLUDE) else {
        return (Vec::new(), Vec::new());
    };
    let DeValue::Array(items) = value.get_ref() else {
        return (
            Vec::new(),
            vec![invalid_include(Span::new(file, value.span()))],
        );
    };
    let mut found = Vec::new();
    let mut diagnostics = Vec::new();
    for item in items {
        match include(item, file) {
            Ok(include) => found.push(include),
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }
    (found, diagnostics)
}

/// An include entry: a path, or `{ path = "...", as = "defaults" }`.
fn include(item: &Spanned<DeValue<'_>>, file: FileId) -> Result<Include, Diagnostic> {
    let at = Span::new(file, item.span());
    let (path, as_defaults) = match item.get_ref() {
        DeValue::String(_) => (item, false),
        DeValue::Table(table) => {
            only_keys(table, &["path", "as"], "an include", file)?;
            let as_defaults = match table.get("as") {
                None => false,
                Some(how) => match how.get_ref() {
                    DeValue::String(value) if value == "defaults" => true,
                    _ => {
                        return Err(Diagnostic::error("`as` can only be \"defaults\"")
                            .label(Span::new(file, how.span()), "expected \"defaults\""));
                    }
                },
            };
            let path = table.get("path").ok_or_else(|| {
                Diagnostic::error("an include needs `path`").label(at, "add `path = \"...\"`")
            })?;
            (path, as_defaults)
        }
        _ => return Err(invalid_include(at)),
    };
    let span = Span::new(file, path.span());
    match path.get_ref() {
        DeValue::String(path) if !path.starts_with('/') => Ok(Include {
            path: path.to_string(),
            inclusion: Inclusion {
                at: span,
                as_defaults,
            },
        }),
        DeValue::String(_) => Err(Diagnostic::error("included paths are relative")
            .label(span, "relative to the configuration root")),
        _ => Err(invalid_include(span)),
    }
}

fn invalid_include(span: Span) -> Diagnostic {
    Diagnostic::error("`include` must be a list of file paths")
        .label(
            span,
            "expected a path, or `{ path = \"...\", as = \"defaults\" }`",
        )
        .note("help: for example `include = [\"profiles/laptop.toml\"]`")
}

/// Works out which files give defaults: those included as defaults, and everything they include.
/// A file reached both that way and as values is an error, since its values can't be both.
fn mark_defaults(files: &mut [LoadedFile], sources: &Sources) -> Vec<Diagnostic> {
    let index: HashMap<FileId, usize> = files
        .iter()
        .enumerate()
        .map(|(index, loaded)| (loaded.file, index))
        .collect();
    // For each file, the inclusion that makes it defaults, and how it's reached as values: as a
    // starting file (`Some(None)`) or through an inclusion.
    let mut as_defaults: Vec<Option<Inclusion>> = vec![None; files.len()];
    let mut as_values: Vec<Option<Option<Span>>> = files
        .iter()
        .map(|loaded| loaded.start.then_some(None))
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for (file, loaded) in files.iter().enumerate() {
            for inclusion in &loaded.inclusions {
                let Some(&includer) = index.get(&inclusion.at.file) else {
                    continue;
                };
                if inclusion.as_defaults || as_defaults[includer].is_some() {
                    if as_defaults[file].is_none() {
                        as_defaults[file] = Some(*inclusion);
                        changed = true;
                    }
                } else if as_values[includer].is_some() && as_values[file].is_none() {
                    as_values[file] = Some(Some(inclusion.at));
                    changed = true;
                }
            }
        }
    }
    let mut diagnostics = Vec::new();
    for ((loaded, defaults), values) in files.iter_mut().zip(as_defaults).zip(as_values) {
        let Some(defaults) = defaults else {
            continue;
        };
        loaded.defaults = true;
        let Some(values) = values else {
            continue;
        };
        let how = if defaults.as_defaults {
            "included as defaults here"
        } else {
            "included from a file that's included as defaults"
        };
        let mut diagnostic = Diagnostic::error(format!(
            "`{}` is included both as defaults and as values",
            sources.name(loaded.file)
        ))
        .label(defaults.at, how);
        diagnostic = match values {
            Some(at) => diagnostic.label(at, "included as values here"),
            None => diagnostic.note("it's also part of the configuration on its own"),
        };
        diagnostics.push(
            diagnostic
                .note("help: include it one way; a file's values are defaults or they aren't"),
        );
    }
    diagnostics
}

/// The roles a configuration has: the names of the files directly in `roles/`, sorted. None when
/// it describes one machine in `system.toml`.
pub fn roles(fs: &dyn ConfigFs) -> io::Result<Vec<String>> {
    Ok(fs
        .toml_files(ROLES)?
        .iter()
        .filter_map(|path| {
            let name = path.strip_prefix("roles/")?.strip_suffix(".toml")?;
            (!name.contains('/')).then(|| name.to_owned())
        })
        .collect())
}

const ROLES: &str = "roles";
const SYSTEM: &str = "system.toml";

/// The file the configuration starts from: `system.toml`, or `roles/<role>.toml` when it has
/// roles. A configuration with one role starts from it without being given its name.
fn starting_file(fs: &dyn ConfigFs, role: Option<&str>) -> Result<String, Diagnostic> {
    let roles = roles(fs)
        .map_err(|e| Diagnostic::error(format!("cannot list `{}`: {e}", fs.display(ROLES))))?;
    let role = match (role, roles.as_slice()) {
        (None, [only]) => Some(only.as_str()),
        _ => role,
    };
    if roles.is_empty() {
        return match role {
            None => Ok(SYSTEM.to_owned()),
            Some(role) => Err(Diagnostic::error(format!(
                "there's no role `{role}`: this configuration describes one machine"
            ))
            .note("help: roles are files in `roles/`, used instead of `system.toml`")),
        };
    }
    if fs.read(SYSTEM).is_ok() {
        return Err(Diagnostic::error(
            "this configuration has both `system.toml` and `roles/`",
        )
        .note(
            "help: a configuration describes one machine in `system.toml`, or kinds of machines \
             in `roles/`",
        ));
    }
    let list = roles.join(", ");
    match role {
        None => Err(Diagnostic::error(format!(
            "this configuration has roles, so it needs one: {list}"
        ))
        .note("help: pick one with `--role NAME`")),
        Some(role) if roles.iter().any(|name| name == role) => Ok(format!("roles/{role}.toml")),
        Some(role) => {
            let mut diagnostic = Diagnostic::error(format!("there's no role `{role}`"))
                .note(format!("the roles are {list}"));
            if let Some(note) = did_you_mean(role, &roles) {
                diagnostic = diagnostic.note(note);
            }
            Err(diagnostic)
        }
    }
}
