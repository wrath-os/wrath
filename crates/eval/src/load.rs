//! Finding and reading the files of a configuration tree.
//!
//! A configuration is `system.toml`, everything under `services/`, and `users/<name>.toml`,
//! which configures the user `<name>`. Any file can pull in more files with
//! `include = ["profiles/laptop.toml"]`; paths are relative to the configuration root and an
//! included file configures the same scope as the file that includes it.

use std::collections::HashSet;
use std::io;

use toml::de::{DeTable, DeValue};

use crate::diag::Diagnostic;
use crate::fs::ConfigFs;
use crate::path::OptionPath;
use crate::source::{FileId, Sources, Span};

/// The key that includes other files. It's only recognised at the top of a file.
pub(crate) const INCLUDE: &str = "include";

#[derive(Debug, Clone)]
pub(crate) struct LoadedFile {
    pub file: FileId,
    /// The option the file's top-level keys belong to.
    pub root: OptionPath,
}

pub(crate) fn load(fs: &dyn ConfigFs, sources: &mut Sources) -> (Vec<LoadedFile>, Vec<Diagnostic>) {
    let mut loader = Loader {
        fs,
        sources,
        files: Vec::new(),
        seen: HashSet::new(),
        diagnostics: Vec::new(),
    };
    loader.add("system.toml", &OptionPath::root(), None);
    loader.add_listed("services", |_| Some(OptionPath::root()));
    loader.add_listed("users", |path| {
        let name = path.strip_prefix("users/")?.strip_suffix(".toml")?;
        (!name.contains('/')).then(|| OptionPath::root().child("users").child(name))
    });
    (loader.files, loader.diagnostics)
}

struct Loader<'a> {
    fs: &'a dyn ConfigFs,
    sources: &'a mut Sources,
    files: Vec<LoadedFile>,
    seen: HashSet<String>,
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

    /// Reads a file and the files it includes. `included_at` is the include entry naming it.
    fn add(&mut self, path: &str, root: &OptionPath, included_at: Option<Span>) {
        if !self.seen.insert(path.to_owned()) {
            return;
        }
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
        self.files.push(LoadedFile {
            file,
            root: root.clone(),
        });
        let includes = match DeTable::parse(self.sources.text(file)) {
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
        match includes {
            Ok(includes) => {
                for (include, span) in includes {
                    self.add(&include, root, Some(span));
                }
            }
            Err(diagnostic) => self.diagnostics.push(diagnostic),
        }
    }
}

fn includes(table: &DeTable<'_>, file: FileId) -> Result<Vec<(String, Span)>, Diagnostic> {
    let Some(value) = table.get(INCLUDE) else {
        return Ok(Vec::new());
    };
    let invalid = |span| {
        Diagnostic::error("`include` must be a list of file paths")
            .label(Span::new(file, span), "expected a list of strings")
            .note("help: for example `include = [\"profiles/laptop.toml\"]`")
    };
    let DeValue::Array(items) = value.get_ref() else {
        return Err(invalid(value.span()));
    };
    items
        .iter()
        .map(|item| match item.get_ref() {
            DeValue::String(path) if !path.starts_with('/') => {
                Ok((path.to_string(), Span::new(file, item.span())))
            }
            DeValue::String(_) => Err(Diagnostic::error("included paths are relative").label(
                Span::new(file, item.span()),
                "relative to the configuration root",
            )),
            _ => Err(invalid(item.span())),
        })
        .collect()
}
