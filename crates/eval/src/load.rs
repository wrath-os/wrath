//! Finding and reading the files of a configuration tree.
//!
//! A configuration is `system.toml`, everything under `services/`, and `users/<name>.toml`,
//! which configures the user `<name>`. Any file can pull in more files with
//! `include = ["profiles/laptop.toml"]`; paths are relative to the configuration root and an
//! included file configures the same scope as the file that includes it. A file included with
//! `{ path = "...", as = "defaults" }` gives defaults, and so does everything it includes.

use std::collections::{BTreeMap, HashMap};
use std::io;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::diag::Diagnostic;
use crate::fs::ConfigFs;
use crate::path::OptionPath;
use crate::platform::{self, PLATFORM, Platform};
use crate::source::{FileId, Sources, Span};
use crate::suggest::did_you_mean;
use crate::walk::only_keys;

/// The key that includes other files. It's only recognised at the top of a file.
pub(crate) const INCLUDE: &str = "include";
/// The table of sources, read while loading from files at the top of the configuration.
pub(crate) const SOURCES: &str = "sources";

#[derive(Debug, Clone)]
pub(crate) struct LoadedFile {
    pub file: FileId,
    /// The option the file's top-level keys belong to.
    pub root: OptionPath,
    /// Whether the file is one of the configuration's own, where `include`, `[sources]` and
    /// `[machine]` are read, as opposed to a file that configures one user of a system.
    pub frame: bool,
    /// Whether the configuration starts from this file, rather than only reaching it through
    /// includes.
    pub start: bool,
    /// Whether this is the file the configuration starts from, where `platform` is chosen.
    pub starting: bool,
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

/// What loading a configuration found.
pub(crate) struct Loaded {
    pub files: Vec<LoadedFile>,
    pub diagnostics: Vec<Diagnostic>,
    /// The role it started from.
    pub role: Option<String>,
    /// Sources by name, from `[sources]` in its local files.
    pub sources: BTreeMap<String, DeclaredSource>,
    /// The platform the starting file chooses.
    pub platform: Platform,
}

/// Loads the files of a configuration, starting from `role_given`, or else its only role or
/// `system.toml`. Files included from sources are loaded last, once every local file has said
/// which sources there are.
pub(crate) fn load(
    fs: &dyn ConfigFs,
    sources: &mut Sources,
    kind: ConfigKind,
    user: Option<&str>,
    role_given: Option<&str>,
) -> Loaded {
    let root = match (kind, user) {
        (ConfigKind::Home, Some(user)) => OptionPath::root().child("users").child(user),
        _ => OptionPath::root(),
    };
    let mut loader = Loader {
        fs,
        sources,
        root: root.clone(),
        files: Vec::new(),
        seen: HashMap::new(),
        declared: BTreeMap::new(),
        pending: Vec::new(),
        trees: HashMap::new(),
        diagnostics: Vec::new(),
        kind,
        starting: None,
        platform: Platform::Wrath,
    };
    let start = match kind {
        ConfigKind::System => starting_file(fs, role_given),
        ConfigKind::Project => project_file(fs),
        ConfigKind::Home => home_file(fs, user),
    };
    let role = match start {
        Ok(path) => {
            loader.starting = Some(path.clone());
            loader.add(None, &path, &root, None);
            path.strip_prefix("roles/")
                .and_then(|name| name.strip_suffix(".toml"))
                .map(str::to_owned)
        }
        Err(diagnostic) => {
            loader.diagnostics.push(diagnostic);
            None
        }
    };
    // A project's own directories belong to the project, not its configuration.
    if kind == ConfigKind::System {
        loader.add_listed("services", |_| Some(OptionPath::root()));
        loader.add_listed("users", |path| {
            let name = path.strip_prefix("users/")?.strip_suffix(".toml")?;
            (!name.contains('/')).then(|| OptionPath::root().child("users").child(name))
        });
    }
    for (include, root) in std::mem::take(&mut loader.pending) {
        loader.add_from_source(include, &root);
    }
    let mut diagnostics = loader.diagnostics;
    diagnostics.extend(mark_defaults(&mut loader.files, loader.sources));
    Loaded {
        files: loader.files,
        diagnostics,
        role,
        sources: loader.declared,
        platform: loader.platform,
    }
}

struct Loader<'a> {
    fs: &'a dyn ConfigFs,
    sources: &'a mut Sources,
    /// What the configuration's own files configure: everything, or a home's `users.<name>`.
    root: OptionPath,
    files: Vec<LoadedFile>,
    /// Each file read or being read, by its source and path, with its index in `files` once it
    /// has one.
    seen: HashMap<(Option<String>, String), Option<usize>>,
    declared: BTreeMap<String, DeclaredSource>,
    /// Includes from sources, loaded once every local file is, with the root they configure.
    pending: Vec<(Include, OptionPath)>,
    /// The files of each source fetched so far, or `None` when it couldn't be.
    trees: HashMap<String, Option<Box<dyn ConfigFs>>>,
    diagnostics: Vec<Diagnostic>,
    kind: ConfigKind,
    /// The file the configuration starts from.
    starting: Option<String>,
    platform: Platform,
}

impl Loader<'_> {
    fn add_listed(&mut self, dir: &str, root: impl Fn(&str) -> Option<OptionPath>) {
        match self.fs.toml_files(dir) {
            Ok(paths) => {
                for path in paths {
                    match root(&path) {
                        Some(root) => self.add(None, &path, &root, None),
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

    /// Loads a file included from a source, fetching the source the first time.
    fn add_from_source(&mut self, include: Include, root: &OptionPath) {
        let Some((name, name_at)) = include.source else {
            return;
        };
        let Some(url) = self
            .declared
            .get(&name)
            .map(|declared| declared.url.clone())
        else {
            let note = did_you_mean(&name, self.declared.keys()).unwrap_or_else(|| {
                format!("help: declare it with `[sources.{name}]` and the `url` it follows")
            });
            return self.diagnostics.push(
                Diagnostic::error(format!("there's no source `{name}`"))
                    .label(name_at, "included from here")
                    .note(note),
            );
        };
        let tree =
            self.trees
                .entry(name.clone())
                .or_insert_with(|| match self.fs.source(&name, &url) {
                    Ok(tree) => Some(tree),
                    Err(message) => {
                        self.diagnostics.push(
                            Diagnostic::error(format!("cannot fetch source `{name}`: {message}"))
                                .label(name_at, "included from here"),
                        );
                        None
                    }
                });
        if tree.is_some() {
            self.add(Some(&name), &include.path, root, Some(include.inclusion));
        }
    }

    /// Reads a file and the files it includes, from the source `origin` or else the
    /// configuration. `inclusion` is the include entry naming it, `None` for a file the
    /// configuration starts from.
    fn add(
        &mut self,
        origin: Option<&str>,
        path: &str,
        root: &OptionPath,
        inclusion: Option<Inclusion>,
    ) {
        let included_at = inclusion.map(|inclusion| inclusion.at);
        let key = (origin.map(str::to_owned), path.to_owned());
        if let Some(&index) = self.seen.get(&key) {
            if let Some(index) = index {
                let loaded = &mut self.files[index];
                match inclusion {
                    Some(inclusion) => loaded.inclusions.push(inclusion),
                    None => loaded.start = true,
                }
            }
            return;
        }
        self.seen.insert(key.clone(), None);
        let (read, display) = match origin {
            None => (self.fs.read(path), self.fs.display(path)),
            Some(name) => {
                let tree = self.trees[name]
                    .as_ref()
                    .expect("only fetched sources are read");
                (tree.read(path), format!("{name}:{path}"))
            }
        };
        let text = match read {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound && included_at.is_none() => {
                self.diagnostics.push(
                    Diagnostic::error(format!("`{display}` does not exist"))
                        .note("help: every configuration starts from `system.toml`"),
                );
                return;
            }
            Err(e) => {
                self.diagnostics.push(
                    Diagnostic::error(format!("cannot read `{display}`: {e}"))
                        .label_opt(included_at, "included here"),
                );
                return;
            }
        };
        let file = self.sources.add(display, text);
        self.seen.insert(key, Some(self.files.len()));
        let frame = *root == self.root;
        let starting = origin.is_none() && self.starting.as_deref() == Some(path);
        self.files.push(LoadedFile {
            file,
            root: root.clone(),
            frame,
            start: inclusion.is_none(),
            starting,
            inclusions: inclusion.into_iter().collect(),
            defaults: false,
        });
        // Owned, so the loader can change while the table is read.
        let text = self.sources.text(file).to_owned();
        let table = match DeTable::parse(&text) {
            Ok(table) => table,
            Err(e) => {
                let message = e.message().trim().replace('\n', "; ");
                self.diagnostics.push(
                    Diagnostic::error("invalid TOML")
                        .label_opt(e.span().map(|range| Span::new(file, range)), message),
                );
                return;
            }
        };
        if let Some(sources) = table.get_ref().get(SOURCES).filter(|_| frame) {
            match origin {
                None => self.declare(sources, file),
                Some(name) => self.diagnostics.push(
                    Diagnostic::error(format!("a file from source `{name}` can't declare sources"))
                        .label(Span::new(file, sources.span()), "declared here")
                        .note("help: declare it in the configuration that includes this file"),
                ),
            }
        }
        if let Some(value) = table.get_ref().get(PLATFORM).filter(|_| starting) {
            if self.kind == ConfigKind::System {
                match platform::parse(value, file, &self.declared) {
                    Ok(chosen) => self.platform = chosen,
                    Err(diagnostic) => self.diagnostics.push(diagnostic),
                }
            } else {
                self.diagnostics.push(
                    Diagnostic::error("only a system chooses its platform")
                        .label(Span::new(file, value.span()), "chosen here"),
                );
            }
        }
        let (includes, diagnostics) = includes(table.get_ref(), file);
        self.diagnostics.extend(diagnostics);
        for include in includes {
            match (&include.source, origin) {
                (Some(_), None) => self.pending.push((include, root.clone())),
                (Some((name, at)), Some(own)) if name != own => self.diagnostics.push(
                    Diagnostic::error(format!(
                        "a file from source `{own}` can't include files from `{name}`"
                    ))
                    .label(*at, "included here")
                    .note(
                        "help: include it from the configuration, which says which sources it \
                         uses",
                    ),
                ),
                (None, _) | (Some(_), Some(_)) => {
                    self.add(origin, &include.path, root, Some(include.inclusion));
                }
            }
        }
    }

    /// Reads `[sources]` from a local file.
    fn declare(&mut self, value: &Spanned<DeValue<'_>>, file: FileId) {
        let DeValue::Table(sources) = value.get_ref() else {
            return self.diagnostics.push(
                Diagnostic::error("`sources` is a table of sources by name")
                    .label(Span::new(file, value.span()), "expected `[sources.NAME]`")
                    .note("help: for example `[sources.stable]` with `url = \"github:...\"`"),
            );
        };
        for (name, source) in sources.iter() {
            let name = name.get_ref();
            let (url, at) = match source_url(name, source, file) {
                Ok(found) => found,
                Err(diagnostic) => {
                    self.diagnostics.push(diagnostic);
                    continue;
                }
            };
            match self.declared.get(name.as_ref()) {
                None => {
                    self.declared
                        .insert(name.to_string(), DeclaredSource { url, at });
                }
                Some(existing) if existing.url == url => {}
                Some(existing) => self.diagnostics.push(
                    Diagnostic::error(format!("source `{name}` is defined twice, differently"))
                        .label(existing.at, format!("follows {}", existing.url))
                        .label(at, format!("follows {url}")),
                ),
            }
        }
    }
}

/// One entry of a file's `include` list.
struct Include {
    path: String,
    /// The source it's from, written `source = "..."`, and where.
    source: Option<(String, Span)>,
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

/// An include entry: a path, or `{ path = "...", source = "...", as = "defaults" }`.
fn include(item: &Spanned<DeValue<'_>>, file: FileId) -> Result<Include, Diagnostic> {
    let at = Span::new(file, item.span());
    let (path, source, as_defaults) = match item.get_ref() {
        DeValue::String(_) => (item, None, false),
        DeValue::Table(table) => {
            only_keys(table, &["path", "source", "as"], "an include", file)?;
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
            let source = match table.get("source") {
                None => None,
                Some(source) => match source.get_ref() {
                    DeValue::String(name) => {
                        Some((name.to_string(), Span::new(file, source.span())))
                    }
                    _ => {
                        return Err(Diagnostic::error("`source` names a source")
                            .label(Span::new(file, source.span()), "expected a string"));
                    }
                },
            };
            let path = table.get("path").ok_or_else(|| {
                Diagnostic::error("an include needs `path`").label(at, "add `path = \"...\"`")
            })?;
            (path, source, as_defaults)
        }
        _ => return Err(invalid_include(at)),
    };
    let span = Span::new(file, path.span());
    match path.get_ref() {
        DeValue::String(path) if !path.starts_with('/') => Ok(Include {
            path: path.to_string(),
            source,
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

/// A source as a configuration declares it: the URL it follows, and where that's written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredSource {
    pub url: String,
    pub at: Span,
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

/// The size of configuration being evaluated, which decides the file it starts from and the
/// modules it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    /// A machine: `system.toml` or a role, with `services/` and `users/`.
    System,
    /// A project's development environment: `wrath.toml`, or `wrath.local.toml` over it.
    Project,
    /// One user's own home: `home.toml`, configuring their entry in `users`.
    Home,
}

const PROJECT: &str = "wrath.toml";
const HOME: &str = "home.toml";
const PROJECT_LOCAL: &str = "wrath.local.toml";

/// The file a project starts from: the local file when there is one, so its owner can change
/// anything in the project.
fn project_file(fs: &dyn ConfigFs) -> Result<String, Diagnostic> {
    if fs.read(PROJECT_LOCAL).is_ok() {
        return Ok(PROJECT_LOCAL.to_owned());
    }
    if fs.read(PROJECT).is_ok() {
        return Ok(PROJECT.to_owned());
    }
    Err(
        Diagnostic::error(format!("`{}` does not exist", fs.display(PROJECT)))
            .note("help: a project starts from `wrath.toml`"),
    )
}

/// The file a user's own configuration starts from.
fn home_file(fs: &dyn ConfigFs, user: Option<&str>) -> Result<String, Diagnostic> {
    if user.is_none() {
        return Err(Diagnostic::error(
            "a home configuration needs the user it belongs to",
        ));
    }
    match fs.read(HOME) {
        Ok(_) => Ok(HOME.to_owned()),
        Err(_) => Err(
            Diagnostic::error(format!("`{}` does not exist", fs.display(HOME)))
                .note("help: a user's own configuration starts from `home.toml`"),
        ),
    }
}

/// The kind of configuration in `fs`: a system when it has `system.toml` or roles, a project when
/// it has `wrath.toml` or `wrath.local.toml`, a home when it has `home.toml`, otherwise a system.
pub fn kind_of(fs: &dyn ConfigFs) -> ConfigKind {
    let project = [PROJECT, PROJECT_LOCAL]
        .iter()
        .any(|file| fs.read(file).is_ok());
    let system = fs.read(SYSTEM).is_ok() || roles(fs).is_ok_and(|roles| !roles.is_empty());
    let home = fs.read(HOME).is_ok();
    match (system, project, home) {
        (false, true, _) => ConfigKind::Project,
        (false, false, true) => ConfigKind::Home,
        _ => ConfigKind::System,
    }
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
pub(crate) fn starting_file(fs: &dyn ConfigFs, role: Option<&str>) -> Result<String, Diagnostic> {
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
        .note("help: pick one with `--role NAME`, or name this machine in `machines.toml`")),
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

/// The URL of one source, `[sources.NAME]` with `url = "..."`, and where it's written.
pub(crate) fn source_url(
    name: &str,
    source: &Spanned<DeValue<'_>>,
    file: FileId,
) -> Result<(String, Span), Diagnostic> {
    let span = Span::new(file, source.span());
    if name.is_empty() || name.contains('#') {
        return Err(
            Diagnostic::error(format!("source name `{name}` can't have `#` in it"))
                .label(span, "packages name a source as `NAME#package`"),
        );
    }
    let DeValue::Table(table) = source.get_ref() else {
        return Err(
            Diagnostic::error(format!("source `{name}` is a table with its `url`"))
                .label(span, "expected `{ url = \"...\" }`"),
        );
    };
    only_keys(table, &["url"], "a source", file)?;
    match table.get("url") {
        Some(url) => match url.get_ref() {
            DeValue::String(text) => Ok((text.to_string(), Span::new(file, url.span()))),
            other => Err(Diagnostic::error(format!(
                "a source's `url` is a string, found {}",
                other.type_str()
            ))
            .label(
                Span::new(file, url.span()),
                "expected a URL such as `github:owner/repo`",
            )),
        },
        None => Err(Diagnostic::error(format!("source `{name}` has no `url`"))
            .label(span, "expected `url = \"...\"`")),
    }
}
