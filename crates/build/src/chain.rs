//! Building a chain of packages from recipes.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use wrath_starlark::style::{self, Style};

use memchr::memmem;
use serde::Deserialize;

use crate::fixup::{finish, linked_by_path, settle_modes, write_runtime_deps};
use crate::requires;
use crate::sandbox::{self, Identity, Limits, Status, Step, View};
use crate::seed::{self, run as run_host, sha256_of};
use crate::split;
use crate::store::{self, Lab, StorePath, with_path};
use crate::view;

/// The name of the experiment arm every package used to be built under. It's in non-boot
/// packages' keys and records, the sandbox's `WRATH_ARM` and the logs' directory, and is kept
/// so existing builds stay valid and their logs stay where they are.
pub(crate) const ARM: &str = "f";

/// What boot packages have in their keys in place of an arm, kept for the same reason.
const BOOT_ARM: &str = "boot";

/// Where build logs go.
pub(crate) fn logs_dir(lab: &Lab) -> PathBuf {
    lab.work.join("logs").join(ARM)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Scope {
    Boot,
    Tool,
    Runtime,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum DepSpec {
    Id(String),
    Full {
        id: String,
        #[serde(default = "yes")]
        view: bool,
        #[serde(default = "yes")]
        rpath: bool,
        skip: Option<String>,
        #[serde(default)]
        propagate: Propagate,
        #[serde(default)]
        exclude: Vec<String>,
    },
}

/// What of a dependency's requirements a dependent's view takes (decision 0019).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Propagate {
    #[default]
    All,
    Public,
    None,
}

fn yes() -> bool {
    true
}

/// A dependency as a package asks for it.
#[derive(Debug, Clone)]
struct Dep {
    id: String,
    view: bool,
    rpath: bool,
    skip: Vec<String>,
    propagate: Propagate,
    exclude: Vec<String>,
}

/// What of the seed is left in a view once a new compiler and libc take over.
const SEED_COMPILERS: &[&str] = &[
    "bin/gcc",
    "bin/g++",
    "bin/cpp",
    "bin/ld",
    "bin/as",
    "bin/ar",
    "bin/nm",
    "bin/ranlib",
    "bin/strip",
    "bin/objdump",
    "bin/readelf",
    "lib",
    "include",
    "libexec",
];

/// The seed's binutils, for a package that needs a tool the seed doesn't have.
const SEED_BINUTILS: &[&str] = &[
    "bin/ld",
    "bin/as",
    "bin/ar",
    "bin/nm",
    "bin/ranlib",
    "bin/strip",
    "bin/objdump",
    "bin/readelf",
    "lib/bfd-plugins",
    "lib/libbfd.la",
    "lib/libctf.la",
    "lib/libctf-nobfd.la",
    "lib/libopcodes.la",
    "lib/libsframe.la",
];

impl DepSpec {
    fn resolve(&self) -> Result<Dep, String> {
        let (id, view, rpath, skip, propagate, exclude) = match self {
            Self::Id(id) => (id, true, true, &None, Propagate::All, &[][..]),
            Self::Full {
                id,
                view,
                rpath,
                skip,
                propagate,
                exclude,
            } => (id, *view, *rpath, skip, *propagate, exclude.as_slice()),
        };
        let skip = match skip.as_deref() {
            None => Vec::new(),
            Some("compilers") => SEED_COMPILERS.iter().map(|p| (*p).to_owned()).collect(),
            Some("binutils") => SEED_BINUTILS.iter().map(|p| (*p).to_owned()).collect(),
            Some(other) => return Err(format!("`{id}`: nothing is called skip = `{other}`")),
        };
        Ok(Dep {
            id: id.clone(),
            view: view && id != "wrapper",
            rpath: rpath && id != "wrapper",
            skip,
            propagate,
            exclude: exclude.to_vec(),
        })
    }
}

/// An option a recipe's script reads, with what a build that doesn't set it gets.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecipeOption {
    #[serde(rename = "type")]
    kind: OptionKind,
    default: serde_json::Value,
    /// What it does, for whoever sets it.
    #[allow(dead_code)]
    doc: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OptionKind {
    Bool,
    Str,
    Int,
    List,
}

impl OptionKind {
    /// Why `value` isn't one of these, if it isn't.
    fn refuse(self, value: &serde_json::Value) -> Option<&'static str> {
        use serde_json::Value;
        match (self, value) {
            (Self::Bool, Value::Bool(_)) | (Self::Str, Value::String(_)) => None,
            (Self::Int, Value::Number(n)) if n.is_i64() => None,
            (Self::List, Value::Array(items)) if items.iter().all(Value::is_string) => None,
            (Self::List, Value::Array(_)) => Some("a list of text"),
            (Self::Bool, _) => Some("true or false"),
            (Self::Str, _) => Some("text"),
            (Self::Int, _) => Some("a whole number"),
            (Self::List, _) => Some("a list"),
        }
    }
}

/// A version a recipe stays below, and why, such as
/// `hold = { below = "1.58", reason = "needs glib 2.88" }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Hold {
    pub(crate) below: String,
    pub(crate) reason: String,
}

/// A file a package's tool makes from what every package of a generation has under `from`, by
/// running `run`, a program in the package and its arguments (decision 0020).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub from: String,
    pub run: String,
}

/// A variable a package reads to find other packages' files, such as Python's `PYTHONPATH` with
/// `files = ["site"]`: in a build that has the package among its dependencies, it lists each
/// dependency's directories of `files` that are there, in order, between `separator`s.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchPath {
    pub variable: String,
    pub files: Vec<String>,
    #[serde(default = "colon")]
    pub separator: String,
}

fn colon() -> String {
    ":".to_owned()
}

/// The search paths `deps` in a build's view declare, for a key worked out before they're built.
fn declared_parts(deps: &[Dep], declared: impl Fn(&str) -> Vec<SearchPath>) -> Vec<Vec<u8>> {
    deps.iter()
        .filter(|dep| dep.view)
        .flat_map(|dep| {
            declared(&dep.id).into_iter().map(|search| {
                let SearchPath {
                    variable,
                    files,
                    separator,
                } = search;
                format!("declares {} {variable} {files:?} {separator}", dep.id).into_bytes()
            })
        })
        .collect()
}

/// The search path variables a build with `deps` gets: those every dependency in its view
/// declares, each listing the directories of its `files` among the dependencies' outputs, by
/// where they are inside the sandbox. `declared` gives a dependency's declarations, and `has`
/// whether a directory is in an output. A variable nothing lists is left out.
fn search_path_values(
    deps: &[(&str, Vec<StorePath>)],
    declared: impl Fn(&str) -> Vec<SearchPath>,
    has: impl Fn(&StorePath, &str) -> bool,
) -> BTreeMap<String, String> {
    let mut values: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    for (id, _) in deps {
        for search in declared(id) {
            let (_, listed) = values
                .entry(search.variable.clone())
                .or_insert_with(|| (search.separator.clone(), Vec::new()));
            for (_, outputs) in deps {
                for output in outputs {
                    for file in &search.files {
                        let dir = format!("{}/{file}", output.inside());
                        if has(output, file) && !listed.contains(&dir) {
                            listed.push(dir);
                        }
                    }
                }
            }
        }
    }
    values
        .into_iter()
        .filter(|(_, (_, listed))| !listed.is_empty())
        .map(|(variable, (separator, listed))| (variable, listed.join(&separator)))
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Source {
    pub(crate) url: String,
    into: Option<String>,
    /// For a `tree:` source, a directory relative to the repository: what in it to take.
    #[serde(default)]
    include: Vec<String>,
    /// The hash the file must have, when the recipe knows it, such as a crate's from a
    /// `Cargo.lock`. Otherwise `sources.lock` pins it.
    sha256: Option<String>,
    /// Put the file in place at `into` as it is, instead of unpacking it.
    #[serde(default)]
    file: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct Package {
    pub(crate) id: String,
    /// The version it builds, which store paths carry in their names and source URLs can use as
    /// `{version}`.
    #[serde(default)]
    pub(crate) version: Option<String>,
    scope: Scope,
    /// Build on the final toolchain, in addition to `deps` and `extra_deps`.
    #[serde(default)]
    toolset: bool,
    /// One output, for a package that finds its own files relative to a library at run time,
    /// which no scan of its files can see.
    #[serde(default)]
    pub(crate) whole: bool,
    /// Build on the core tools as well, instead of the seed's.
    #[serde(default)]
    core: bool,
    #[serde(default)]
    deps: Vec<DepSpec>,
    #[serde(default)]
    extra_deps: Vec<DepSpec>,
    /// Packages it brings into its dependents' views that no `.pc` file of its names.
    #[serde(default)]
    propagates: Vec<String>,
    /// Whether what its `.pc` files require privately comes into its dependents' views too.
    #[serde(default = "yes")]
    propagate_private: bool,
    /// What its tools make from what a generation's packages share.
    #[serde(default)]
    indexes: Vec<Index>,
    /// The variables it reads to find files of other packages, which every build that has it
    /// among its dependencies gets, listing those directories of its dependencies.
    #[serde(default)]
    search_paths: Vec<SearchPath>,
    /// How its project names each target, such as `{ x86_64 = "amd64" }` for Go, which sources
    /// say as `{arch_name}` and scripts read as `$WRATH_ARCH_NAME`. Without it, our own name.
    #[serde(default)]
    arch_names: BTreeMap<String, String>,
    /// The release series it stays on, such as `3.` for GTK 3, which update checks keep to.
    #[serde(default)]
    pub(crate) track: Option<String>,
    /// A version it can't move to yet, and why, which update checks stop below.
    #[serde(default)]
    pub(crate) hold: Option<Hold>,
    /// Packages it needs at run time without referring to them, such as data it reads by name,
    /// written into it so the reference scan finds them (decision 0021).
    #[serde(default)]
    pub(crate) runtime_deps: Vec<String>,
    #[serde(default)]
    pub(crate) sources: Vec<Source>,
    #[serde(default)]
    pub(crate) files: Vec<String>,
    /// What a build can set, which its script reads as `OPTION_<name>`, by name.
    #[serde(default)]
    options: BTreeMap<String, RecipeOption>,
    /// The build style that makes its script, when it has none, and the options it gives it: a
    /// table named after the style, such as `[package.cargo]`, empty when it gives none. Without
    /// one, the files at the top of its first source pick the style (decision 0025). Any other
    /// field is an error when the recipe loads.
    #[serde(flatten)]
    style_options: BTreeMap<String, toml::Value>,
    /// Whether a style makes its script, decided when it loads.
    #[serde(skip)]
    styled: bool,
    /// The style that made its script, and the recipe as it was before, which a build that sets
    /// the style's options is styled from again.
    #[serde(skip)]
    style: Option<String>,
    #[serde(skip)]
    unstyled: Option<Box<Package>>,
    /// What its lock records its style made, read with it.
    #[serde(skip)]
    made: Option<Made>,
    /// The digest of what its style made, once it's styled or planned from its lock.
    #[serde(skip)]
    record: Option<String>,
    /// Planned from what its lock records, without its style's script: styled when it builds.
    #[serde(skip)]
    planned: bool,
    /// Where it was read from.
    #[serde(skip)]
    pub(crate) origin: Origin,
    #[serde(default)]
    script: String,
    #[serde(default = "yes")]
    wrap: bool,
    /// Asked a past experiment not to patch binaries after install. It does nothing now, but
    /// it's still in the key so packages that set it aren't rebuilt.
    #[serde(default = "yes")]
    fixup: bool,
    /// How long the build may go without printing anything, such as `3h`, for one that works
    /// quietly for longer than an hour.
    #[serde(default)]
    silent_for: Option<String>,
    /// Commands to run from the final location, with paths relative to the package.
    #[serde(default)]
    pub(crate) smoke: Vec<String>,
    /// Libraries to link a small program against. Recipes still list them, but nothing links
    /// the probes any more.
    #[allow(dead_code)]
    #[serde(default)]
    pub(crate) probes: Vec<String>,
    /// Builds the tests, in the view, for tests that need a compiler.
    #[serde(default)]
    pub(crate) check_build: Option<String>,
    #[serde(default)]
    pub(crate) check: Option<String>,
    /// The package's tests compile as they run, so they need a whole view.
    #[serde(default)]
    pub(crate) check_in_view: bool,
    /// Tools only the tests use, which don't count towards what the package is built from.
    #[serde(default)]
    pub(crate) check_deps: Vec<String>,
}

/// The tools that replace the seed's, for the packages built after them.
const CORE_TOOLS: [&str; 11] = [
    "coreutils",
    "sed",
    "grep",
    "gawk",
    "findutils",
    "diffutils",
    "patch",
    "make",
    "tar",
    "gzip",
    "bash",
];

impl Package {
    /// Whether its style ran as it loaded, its lock beside it not recording what the style makes.
    pub(crate) fn restyled_now(&self) -> bool {
        self.style.is_some()
            && !self.planned
            && !self.built_from_a_tree()
            && matches!(self.origin.lock, Lock::Beside { .. })
    }

    /// Whether a source is a tree of a checkout, such as wrath's, which changes with every edit
    /// to it, so nothing a style makes from it is recorded.
    fn built_from_a_tree(&self) -> bool {
        self.sources
            .iter()
            .any(|source| source.url.starts_with("tree:"))
    }

    /// Whether this package is built from the seed, before the toolchain.
    pub(crate) fn is_boot(&self) -> bool {
        self.scope == Scope::Boot
    }

    /// Whether fixups apply to it after it's installed.
    pub(crate) fn fixed_up(&self) -> bool {
        self.toolset || self.core
    }

    /// Where its first source is downloaded from, with `{version}` in it. A source from the
    /// repository has none.
    pub(crate) fn source_url(&self) -> Option<String> {
        let url = &self.sources.first()?.url;
        (!url.starts_with("tree:")).then(|| url.clone())
    }

    /// The target as its project names it: from `arch_names`, or ours when it gives none.
    pub(crate) fn arch_name(&self) -> Result<&str, String> {
        if self.arch_names.is_empty() {
            return Ok(ARCH);
        }
        self.arch_names
            .get(ARCH)
            .map(String::as_str)
            .ok_or_else(|| {
                format!(
                    "`{}` says how its project names targets in `arch_names`, but not `{ARCH}`",
                    self.id
                )
            })
    }

    /// The name its store paths carry after their hash: its id and version, as `jq-1.8.1`.
    pub(crate) fn store_name(&self) -> String {
        match &self.version {
            Some(version) => format!("{}-{version}", self.id),
            None => self.id.clone(),
        }
    }

    /// Refuses a source put somewhere other than inside the source tree: an absolute `into`, or one
    /// with `..` in it.
    fn check_sources(&self) -> Result<(), String> {
        for into in self
            .sources
            .iter()
            .filter_map(|source| source.into.as_deref())
        {
            let inside = Path::new(into)
                .components()
                .all(|part| matches!(part, Component::Normal(_)));
            if !inside {
                return Err(format!(
                    "`{}` puts a source at `{into}`, outside its source tree: name a directory \
                     inside it",
                    self.id
                ));
            }
        }
        Ok(())
    }

    /// Refuses an id or version that can't go in a store path's name.
    fn check_name(&self) -> Result<(), String> {
        let fits = |text: &str| !text.is_empty() && text.bytes().all(store::name_byte);
        if !fits(&self.id) {
            return Err(format!(
                "`{}` can't be an id: use letters, digits and ._+-",
                self.id
            ));
        }
        match &self.version {
            Some(version) if !fits(version) => Err(format!(
                "`{}` has version `{version}`, which can't go in a store path: use letters, \
                 digits and ._+-",
                self.id
            )),
            _ => Ok(()),
        }
    }

    /// The ids of what it depends on.
    pub(crate) fn dep_ids(&self) -> Result<Vec<String>, String> {
        Ok(self.all_deps()?.into_iter().map(|dep| dep.id).collect())
    }

    /// The ids of the dependencies it has that `recipe`, the recipe its style styled, doesn't.
    fn added_deps(&self, recipe: &Package) -> Result<Vec<String>, String> {
        let listed = recipe.dep_ids()?;
        Ok(self
            .dep_ids()?
            .into_iter()
            .filter(|dep| !listed.contains(dep))
            .collect())
    }

    /// All the dependencies, the toolchain's first when the package builds on it, and last what
    /// it needs only at run time, which isn't in its view.
    fn all_deps(&self) -> Result<Vec<Dep>, String> {
        let toolset = [
            DepSpec::Id("wrapper".to_owned()),
            DepSpec::Full {
                id: "seed".to_owned(),
                view: true,
                rpath: true,
                skip: Some("compilers".to_owned()),
                propagate: Propagate::All,
                exclude: Vec::new(),
            },
            DepSpec::Id("gcc".to_owned()),
            DepSpec::Id("binutils".to_owned()),
            DepSpec::Id("glibc".to_owned()),
            DepSpec::Id("linux-headers".to_owned()),
        ];
        let base: &[DepSpec] = if self.toolset || self.core {
            &toolset
        } else {
            &[]
        };
        let core: Vec<DepSpec> = if self.core {
            CORE_TOOLS
                .iter()
                .map(|id| DepSpec::Id((*id).to_owned()))
                .collect()
        } else {
            Vec::new()
        };
        let runtime = self.runtime_deps.iter().map(|id| Dep {
            id: id.clone(),
            view: false,
            rpath: false,
            skip: Vec::new(),
            propagate: Propagate::None,
            exclude: Vec::new(),
        });
        base.iter()
            .chain(&core)
            .chain(&self.deps)
            .chain(&self.extra_deps)
            .map(DepSpec::resolve)
            .chain(runtime.map(Ok))
            .collect()
    }

    /// The `OPTION_<name>` variables for each option the recipe declares: the value in `set`, or
    /// its default. A list is its items separated by spaces. An option it doesn't declare, or a
    /// value of the wrong type, is refused.
    fn option_exports(
        &self,
        set: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<String, String> {
        let id = &self.id;
        if let Some(name) = set.keys().find(|name| !self.options.contains_key(*name)) {
            if self.options.is_empty() {
                return Err(format!("`{id}` takes no options"));
            }
            let names: Vec<String> = self.options.keys().map(|n| format!("`{n}`")).collect();
            let (last, rest) = names.split_last().expect("the recipe has options");
            let listed = match rest {
                [] => last.clone(),
                _ => format!("{} and {last}", rest.join(", ")),
            };
            return Err(format!("`{id}` has no option `{name}`; it has {listed}"));
        }
        let text_of = |value: &serde_json::Value| match value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        let mut exports = String::new();
        for (name, option) in &self.options {
            let value = set.get(name).unwrap_or(&option.default);
            if let Some(kind) = option.kind.refuse(value) {
                return Err(format!("`{id}`'s option `{name}` is {kind}, not {value}"));
            }
            let text = match value {
                serde_json::Value::Array(items) => {
                    items.iter().map(text_of).collect::<Vec<_>>().join(" ")
                }
                other => text_of(other),
            };
            let quoted = text.replace('\'', "'\\''");
            exports.push_str(&format!("export OPTION_{name}='{quoted}'\n"));
        }
        Ok(exports)
    }

    /// The recipe with its options' defaults set at the top of its script, so the script reads
    /// them without defaults of its own.
    fn with_option_defaults(mut self) -> Result<Self, String> {
        if let Some(name) = self
            .options
            .keys()
            .find(|name| !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        {
            return Err(format!("`{}`'s option `{name}` isn't a name", self.id));
        }
        self.script = self.option_exports(&serde_json::Map::new())? + &self.script;
        Ok(self)
    }

    /// This recipe built as `id` with `options`, which its script reads as `OPTION_<name>`
    /// variables set at its top, so the script says what they are and they're part of its key.
    /// `patches` apply after that, in order, from where the script starts.
    pub(crate) fn variant(
        &self,
        id: &str,
        options: &serde_json::Map<String, serde_json::Value>,
        patches: &[String],
    ) -> Result<Self, String> {
        // A styled recipe's build starts from the recipe as it was, with what isn't the recipe's
        // own option given to its style, and is styled again.
        if let (Some(recipe), Some(style)) = (&self.unstyled, &self.style) {
            let (own, given): (serde_json::Map<_, _>, serde_json::Map<_, _>) = options
                .clone()
                .into_iter()
                .partition(|(name, _)| recipe.options.contains_key(name));
            let mut variant = recipe.variant(id, &own, patches)?;
            let table = variant
                .style_options
                .entry(style.clone())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()));
            if let toml::Value::Table(table) = table {
                for (name, value) in given {
                    let value = serde_json::from_value(value).map_err(|e| e.to_string())?;
                    table.insert(name, value);
                }
            }
            return Ok(variant);
        }
        let defaults = self.option_exports(&serde_json::Map::new())?;
        let script = self.script.strip_prefix(&defaults).unwrap_or(&self.script);
        let mut prelude = self.option_exports(options)?;
        for (index, patch) in patches.iter().enumerate() {
            let end = if patch.ends_with('\n') { "" } else { "\n" };
            prelude.push_str(&format!(
                "patch -p1 <<'WRATH_PATCH_{index}'\n{patch}{end}WRATH_PATCH_{index}\n"
            ));
        }
        let mut extra_deps = self.extra_deps.clone();
        if !patches.is_empty() && !self.dep_ids()?.iter().any(|id| id == "patch") {
            extra_deps.push(DepSpec::Id("patch".to_owned()));
        }
        Ok(Self {
            id: id.to_owned(),
            script: format!("{prelude}{script}"),
            extra_deps,
            ..self.clone()
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipes {
    package: Vec<Package>,
}

const LOADER: &str = "lib/ld-linux-x86-64.so.2";
const SEED_GCC_LIBS: &str = "lib/gcc/x86_64-unknown-linux-gnu/13.2.0";

/// The loader's file name on this target, for programs that name it themselves.
fn loader_name() -> &'static str {
    LOADER.rsplit_once('/').map_or(LOADER, |(_, name)| name)
}

/// A package's action, as a record, under both of its keys.
#[derive(Debug)]
pub struct PackageRecord {
    pub unresolved: String,
    pub resolved: String,
    pub text: serde_json::Value,
}

/// A built chain: where each package ended up.
#[derive(Debug, Default)]
pub struct Built {
    pub paths: BTreeMap<String, StorePath>,
    /// The key each package has before anything is built.
    pub unresolved: BTreeMap<String, String>,
    /// The outputs of each package that was split.
    pub outputs: BTreeMap<String, split::Outputs>,
    /// The action record of each package seen.
    pub records: BTreeMap<String, PackageRecord>,
    /// Where each package was built, which is where its record is kept.
    pub temporary: BTreeMap<String, StorePath>,
    pub closures: BTreeMap<String, BTreeSet<StorePath>>,
    /// What each package built here was built with and brings along, for its dependents.
    pub providers: BTreeMap<String, requires::Provider>,
    /// What each package brings into a dependent's view, once worked out.
    pub requirements: BTreeMap<String, requires::Requirements>,
    /// What each package's tools make from what a generation's packages share.
    pub indexes: BTreeMap<String, Vec<Index>>,
    /// The variables each package that declares any reads, for the builds that have it.
    pub search_paths: BTreeMap<String, Vec<SearchPath>>,
}

impl Built {
    /// What a package's dependents see of it. Documentation isn't part of building against it.
    pub fn output_paths(&self, id: &str) -> Vec<StorePath> {
        match self.outputs.get(id) {
            Some(outputs) => outputs
                .iter()
                .filter(|(class, _)| **class != split::Class::Doc)
                .map(|(_, path)| path.clone())
                .collect(),
            None => vec![self.paths[id].clone()],
        }
    }

    /// The path holding a package's run-time libraries.
    pub fn library_path(&self, id: &str) -> &StorePath {
        self.outputs
            .get(id)
            .and_then(|outputs| outputs.get(&split::Class::Lib))
            .unwrap_or(&self.paths[id])
    }

    /// What a dependency contributes to a key: every output a dependent sees.
    fn reference(&self, id: &str) -> String {
        self.output_paths(id)
            .iter()
            .map(StorePath::hash)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// What names package `id` in unresolved keys. The seed has no key, so its path stands in.
    pub fn unresolved_or_path(&self, id: &str) -> String {
        self.unresolved
            .get(id)
            .cloned()
            .unwrap_or_else(|| self.paths[id].hash().to_owned())
    }
}

/// The wrath checkout running the build, which `tree:@wrath` sources read: grapes carries the
/// recipe for wrath, built from the tool's own files (0036). Set once by the command.
static WRATH: OnceLock<PathBuf> = OnceLock::new();

/// What a `tree:` source starts with to name the wrath checkout instead of grapes.
const WRATH_TREE: &str = "@wrath";

/// Says where the wrath checkout running this command is, for `tree:@wrath` sources.
pub fn set_wrath_tree(dir: &Path) {
    let _ = WRATH.set(dir.to_path_buf());
}
/// A listed dependency another one brings, and the one that brings it.
pub(crate) type Redundant = (String, String);

/// For each of `packages`, the dependencies it lists by name that another dependency it lists
/// already brings, each with one that brings it: what a recipe can leave out since 0019.
pub(crate) fn redundant_deps(
    lab: &Lab,
    built: &mut Built,
    packages: &[Package],
) -> Result<Vec<(String, Vec<Redundant>)>, String> {
    let mut found = Vec::new();
    for package in packages.iter().filter(|package| !package.is_boot()) {
        let specs: Vec<&DepSpec> = package.deps.iter().chain(&package.extra_deps).collect();
        let listed: Vec<Dep> = specs
            .iter()
            .map(|spec| spec.resolve())
            .collect::<Result<_, _>>()?;
        let mut redundant = Vec::new();
        for spec in specs {
            // A dependency written as a table says something about itself; it stays.
            let DepSpec::Id(id) = spec else {
                continue;
            };
            let bringers = listed.iter().filter(|other| {
                other.id != *id
                    && other.view
                    && other.propagate != Propagate::None
                    && !other.exclude.contains(id)
            });
            for other in bringers {
                let brought = requires::requirements(lab, built, &other.id)?;
                let public_only = other.propagate == Propagate::Public;
                if brought
                    .get(id)
                    .is_some_and(|private| !(public_only && *private))
                {
                    redundant.push((id.clone(), other.id.clone()));
                    break;
                }
            }
        }
        if !redundant.is_empty() {
            found.push((package.id.clone(), redundant));
        }
    }
    Ok(found)
}

/// `listed` and what each of them brings along (decision 0019). Boot packages keep what they list.
fn with_requirements(
    lab: &Lab,
    package: &Package,
    listed: &[Dep],
    built: &mut Built,
) -> Result<Vec<Dep>, String> {
    let mut deps = listed.to_vec();
    if package.is_boot() {
        return Ok(deps);
    }
    for dep in listed
        .iter()
        .filter(|dep| dep.view && dep.propagate != Propagate::None)
    {
        let brought = requires::requirements(lab, built, &dep.id)?;
        if let Some(excluded) = dep.exclude.iter().find(|id| !brought.contains_key(*id)) {
            return Err(format!(
                "`{}` excludes `{excluded}` from `{}`, which doesn't bring it",
                package.id, dep.id
            ));
        }
        for (id, private) in brought {
            let wanted =
                !(private && dep.propagate == Propagate::Public) && !dep.exclude.contains(&id);
            if wanted && !deps.iter().any(|known| known.id == id) {
                deps.push(Dep {
                    id,
                    view: true,
                    rpath: true,
                    skip: Vec::new(),
                    propagate: Propagate::All,
                    exclude: Vec::new(),
                });
            }
        }
    }
    Ok(deps)
}

pub(crate) fn remove(path: &Path) -> Result<(), String> {
    if path.symlink_metadata().is_err() {
        return Ok(());
    }
    run_host(
        Command::new("chmod").args(["-R", "u+w"]).arg(path),
        "making writable",
    )?;
    fs::remove_dir_all(path).map_err(with_path(path))
}

pub(crate) fn fresh(path: &Path) -> Result<(), String> {
    remove(path)?;
    fs::create_dir_all(path).map_err(with_path(path))
}

/// `names` in `from`, packed into `to` the same way whenever their files are the same: sorted, with
/// no times, owners or version control, under one top directory as a downloaded source is. `skip`
/// names directories to leave out wherever they are.
fn pack(from: &Path, names: &[String], skip: &[&str], to: &Path) -> Result<(), String> {
    run_host(
        Command::new("tar")
            .args(["--create", "--file"])
            .arg(to)
            .arg("--directory")
            .arg(from)
            .args([
                "--sort=name",
                "--mtime=@0",
                "--owner=0",
                "--group=0",
                "--numeric-owner",
                "--mode=go-w",
                "--format=gnu",
                "--exclude-vcs",
            ])
            .args(skip.iter().map(|name| format!("--exclude={name}")))
            .arg("--transform=flags=r;s,^\\./,,;s,^,tree/,")
            .arg("--")
            .args(names),
        &format!("packing {}", from.display()),
    )
}

/// Where recipes are, in a repository.
const RECIPES: &str = "recipes";

/// Whether the set's locks are only read, set once for a pinned set: anything not pinned or
/// recorded in them is then an error, never written.
static LOCKS_READ_ONLY: OnceLock<bool> = OnceLock::new();

/// Makes the set's locks read only, for a set that's pinned rather than a checkout.
pub fn read_only_locks() {
    let _ = LOCKS_READ_ONLY.set(true);
}

fn locks_read_only() -> bool {
    LOCKS_READ_ONLY.get().copied().unwrap_or(false)
}

/// The lock of the recipes kept in files of a theme.
const SHARED_LOCK: &str = "sources.lock";

/// Every recipe file under `recipes/`, as a path from there: files of a theme at the top, such
/// as `desktop.toml`, and files of one package or family a directory down, such as
/// `gh/ghostty.toml`. What's further down is a recipe's own files.
pub(crate) fn recipe_files(repository: &Path) -> Result<Vec<String>, String> {
    let dir = repository.join(RECIPES);
    let tomls = |dir: &Path| -> Result<Vec<String>, String> {
        let names = fs::read_dir(dir)
            .map_err(with_path(dir))?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".toml"));
        Ok(names.collect())
    };
    let mut files = tomls(&dir)?;
    for entry in fs::read_dir(&dir).map_err(with_path(&dir))? {
        let entry = entry.map_err(with_path(&dir))?;
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let shard = entry.file_name().to_string_lossy().into_owned();
            files.extend(
                tomls(&entry.path())?
                    .into_iter()
                    .map(|name| format!("{shard}/{name}")),
            );
        }
    }
    files.sort();
    Ok(files)
}

/// `names` as the borrowed names the builder takes.
pub(crate) fn names_of(names: &[String]) -> Vec<&str> {
    names.iter().map(String::as_str).collect()
}

/// The lock beside each recipe file of one package or family, as a path from the repository.
fn beside_locks(repository: &Path) -> Result<Vec<PathBuf>, String> {
    if !repository.join(RECIPES).is_dir() {
        return Ok(Vec::new());
    }
    Ok(recipe_files(repository)?
        .iter()
        .filter_map(|file| match Origin::of(file, "").lock {
            Lock::Beside { file, .. } => Some(file),
            Lock::Shared => None,
        })
        .collect())
}

/// The lock a recipe's new pins are written to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Lock {
    /// `sources.lock`, which pins the sources of recipes kept in files of a theme.
    #[default]
    Shared,
    /// The lock beside the recipe's own file, under the recipe's id.
    Beside { file: PathBuf, id: String },
}

/// Where a recipe was read from: the directory its own `files` are in, relative to the
/// repository, and the lock its sources are pinned in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Origin {
    pub(crate) dir: PathBuf,
    pub(crate) lock: Lock,
}

impl Default for Origin {
    fn default() -> Self {
        Self {
            dir: PathBuf::from(RECIPES),
            lock: Lock::Shared,
        }
    }
}

impl Origin {
    /// Where the recipes in `file`, relative to `recipes/`, come from. A file of a theme, at the
    /// top, has its files beside it and its pins in `sources.lock`. A file of one package or
    /// family, `gh/ghostty.toml`, has its files in `gh/ghostty/` and its pins in `gh/ghostty.lock`.
    pub(crate) fn of(file: &str, id: &str) -> Self {
        let Some(stem) = file.strip_suffix(".toml").filter(|_| file.contains('/')) else {
            return Self::default();
        };
        let dir = Path::new(RECIPES).join(stem);
        Self {
            lock: Lock::Beside {
                file: dir.with_extension("lock"),
                id: id.to_owned(),
            },
            dir,
        }
    }
}

/// What a lock beside a recipe file holds for one of its packages: what its style made, when it
/// has one, and its pins.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageLock {
    style: Option<String>,
    record: Option<String>,
    from: Option<String>,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    sources: Vec<Pinned>,
}

impl PackageLock {
    fn made(&self) -> Option<Made> {
        Some(Made {
            style: self.style.clone()?,
            record: self.record.clone()?,
            from: self.from.clone()?,
            deps: self.deps.clone(),
        })
    }
}

/// What a recipe's style made, as its lock records it, so a plan needn't read its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Made {
    /// The style that made it.
    style: String,
    /// The digest of what it made: the script, and the sources and dependencies it added.
    record: String,
    /// The digest of what it was made from: the style's code, and the recipe's options, sources
    /// and own files.
    from: String,
    /// The dependencies it added.
    deps: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pinned {
    url: String,
    hash: String,
}

/// The lock at `file`, by package id. A lock that isn't there yet pins nothing.
fn read_lock(file: &Path) -> Result<BTreeMap<String, PackageLock>, String> {
    match fs::read_to_string(file) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(with_path(file)(e)),
    }
}

/// `locks` as a lock's text: a table per package, its sources in the order of their URLs.
fn lock_text(locks: &BTreeMap<String, PackageLock>) -> String {
    let quoted = |text: &str| toml::Value::String(text.to_owned()).to_string();
    let key = |id: &str| {
        let bare = id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if bare { id.to_owned() } else { quoted(id) }
    };
    let mut text = String::new();
    for (id, lock) in locks {
        if !text.is_empty() {
            text.push('\n');
        }
        let key = key(id);
        text.push_str(&format!("[{key}]\n"));
        if let Some(Made {
            style,
            record,
            from,
            deps,
        }) = lock.made()
        {
            let deps: Vec<String> = deps.iter().map(|dep| quoted(dep)).collect();
            text.push_str(&format!(
                "style = {}\nrecord = {}\nfrom = {}\ndeps = [{}]\n",
                quoted(&style),
                quoted(&record),
                quoted(&from),
                deps.join(", ")
            ));
        }
        let mut sources = lock.sources.clone();
        sources.sort_by(|a, b| a.url.cmp(&b.url));
        for Pinned { url, hash } in &sources {
            text.push_str(&format!(
                "\n[[{key}.sources]]\nurl = {}\nhash = {}\n",
                quoted(url),
                quoted(hash)
            ));
        }
    }
    text
}

/// The pinned hashes of sources: in `sources.lock`, as `<sha256>  <url>` lines, for recipes kept
/// in files of a theme, and in the lock beside each recipe file of one package or family. A URL
/// not pinned yet is pinned in its recipe's lock the first time it's fetched.
pub(crate) struct Sources {
    repository: PathBuf,
    /// Every pin, by URL.
    pins: BTreeMap<String, String>,
    /// What `sources.lock` pins.
    shared: BTreeMap<String, String>,
    /// What the locks beside recipe files pin: each lock, package id and URL.
    beside: BTreeSet<(PathBuf, String, String)>,
    /// Whether the locks are only read, as a pinned set's are.
    read_only: bool,
}

impl Sources {
    pub(crate) fn load(repository: &Path) -> Result<Self, String> {
        let file = repository.join(SHARED_LOCK);
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(with_path(&file)(e)),
        };
        let shared: BTreeMap<String, String> = text
            .lines()
            .filter_map(|line| line.split_once("  "))
            .map(|(hash, url)| (url.to_owned(), hash.to_owned()))
            .collect();
        let mut pins = shared.clone();
        let mut pinned_by: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut beside = BTreeSet::new();
        for lock in beside_locks(repository)? {
            for (id, package) in read_lock(&repository.join(&lock))? {
                for Pinned { url, hash } in package.sources {
                    if let Some(other) = pins.get(&url)
                        && *other != hash
                    {
                        let by = pinned_by
                            .get(&url)
                            .map_or(SHARED_LOCK.into(), |by| by.display().to_string());
                        return Err(format!(
                            "{url} is pinned as {hash} in {} and as {other} in {by}",
                            lock.display()
                        ));
                    }
                    pinned_by.insert(url.clone(), lock.clone());
                    beside.insert((lock.clone(), id.clone(), url.clone()));
                    pins.insert(url, hash);
                }
            }
        }
        Ok(Self {
            repository: repository.to_owned(),
            pins,
            shared,
            beside,
            read_only: locks_read_only(),
        })
    }

    /// Records in `lock` what a style made, in place of what it recorded before.
    fn remember(&self, lock: &Lock, made: &Made) -> Result<(), String> {
        let Lock::Beside { file, id } = lock else {
            return Ok(());
        };
        let file = self.repository.join(file);
        let mut locks = read_lock(&file)?;
        let entry = locks.entry(id.clone()).or_default();
        entry.style = Some(made.style.clone());
        entry.record = Some(made.record.clone());
        entry.from = Some(made.from.clone());
        entry.deps.clone_from(&made.deps);
        fs::write(&file, lock_text(&locks)).map_err(with_path(&file))
    }

    /// Pins `url` as `hash` in `lock`.
    fn pin(&mut self, url: &str, hash: &str, lock: &Lock) -> Result<(), String> {
        self.pins.insert(url.to_owned(), hash.to_owned());
        match lock {
            Lock::Shared => {
                self.shared.insert(url.to_owned(), hash.to_owned());
                let text: String = self
                    .shared
                    .iter()
                    .map(|(url, hash)| format!("{hash}  {url}\n"))
                    .collect();
                let file = self.repository.join(SHARED_LOCK);
                fs::write(&file, text).map_err(with_path(&file))
            }
            Lock::Beside { file, id } => {
                self.beside
                    .insert((file.clone(), id.clone(), url.to_owned()));
                let file = self.repository.join(file);
                let mut locks = read_lock(&file)?;
                let sources = &mut locks.entry(id.clone()).or_default().sources;
                sources.retain(|pinned| pinned.url != url);
                sources.push(Pinned {
                    url: url.to_owned(),
                    hash: hash.to_owned(),
                });
                fs::write(&file, lock_text(&locks)).map_err(with_path(&file))
            }
        }
    }

    /// The hash of `source`, from the recipe or a lock when either knows it, so working out
    /// keys fetches only what was never pinned.
    fn hash_of(&mut self, lab: &Lab, source: &Source, lock: &Lock) -> Result<String, String> {
        let known = if source.url.starts_with("tree:") {
            None
        } else {
            source
                .sha256
                .clone()
                .or_else(|| self.pins.get(&source.url).cloned())
        };
        match known {
            Some(hash) => Ok(hash),
            None => self.fetch_source(lab, source, lock).map(|(_, hash)| hash),
        }
    }

    /// Where `source` is on the host, with its hash, pinned in `lock` unless the recipe carries
    /// its hash. A source another lock pins is pinned in `lock` too, so each lock pins all its
    /// recipes fetch, and removing a recipe removes only its own pins.
    pub(crate) fn fetch_source(
        &mut self,
        lab: &Lab,
        source: &Source,
        lock: &Lock,
    ) -> Result<(PathBuf, String), String> {
        if let Some(tree) = source.url.strip_prefix("tree:") {
            return self.fetch_tree(lab, tree, &source.include);
        }
        let known = source.sha256.is_some() || self.pins.contains_key(&source.url);
        if !known && self.read_only {
            return Err(format!(
                "{} isn't pinned, and a pinned grapes never pins anything: that version of \
                 grapes is broken, and a checkout of it pins the source when it builds",
                source.url
            ));
        }
        let (path, hash) = if source.url.starts_with("git+") {
            self.fetch_git(lab, &source.url)?
        } else {
            self.fetch(lab, &source.url, source.sha256.as_deref())?
        };
        if source.sha256.is_none() && !self.holds(lock, &source.url) && !self.read_only {
            self.pin(&source.url, &hash, lock)?;
        }
        Ok((path, hash))
    }

    /// Whether `lock` pins `url`.
    fn holds(&self, lock: &Lock, url: &str) -> bool {
        match lock {
            Lock::Shared => self.shared.contains_key(url),
            Lock::Beside { file, id } => {
                self.beside
                    .contains(&(file.clone(), id.clone(), url.to_owned()))
            }
        }
    }

    /// The files `include` names in `tree`, a directory relative to the repository, or under the
    /// wrath checkout for `@wrath/...`, packed by [`pack`]. Named by its hash, which isn't pinned:
    /// the tree is the repository's own and changes with it.
    fn fetch_tree(
        &mut self,
        lab: &Lab,
        tree: &str,
        include: &[String],
    ) -> Result<(PathBuf, String), String> {
        let repository = &self.repository;
        let from = match tree.strip_prefix(WRATH_TREE) {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => WRATH
                .get()
                .ok_or("only a checkout of wrath can build wrath from `tree:@wrath`")?
                .join(rest.trim_start_matches('/')),
            Some(_) | None => repository.join(tree),
        };
        let mut include = include.to_vec();
        include.sort();
        if let Some(missing) = include.iter().find(|name| !from.join(name).exists()) {
            return Err(format!("{} has no `{missing}`", from.display()));
        }
        let dir = lab.work.join("sources");
        fs::create_dir_all(&dir).map_err(with_path(&dir))?;
        let partial = dir.join(format!("tree.partial{}", std::process::id()));
        // What was built in the repository, `target`, is left out.
        pack(&from, &include, &["target"], &partial)?;
        let hash = sha256_of(&partial)?;
        let path = dir.join(&hash);
        fs::rename(&partial, &path).map_err(with_path(&path))?;
        Ok((path, hash))
    }

    /// Where the commit `url` names, `git+REPOSITORY#COMMIT`, is on the host, with its hash: the
    /// commit cloned with its submodules from any host and packed by [`pack`], so the hash is of
    /// the files, not of how a host compresses an archive (decision 0028). It's pinned in
    /// a lock the first time.
    fn fetch_git(&mut self, lab: &Lab, url: &str) -> Result<(PathBuf, String), String> {
        let dir = lab.work.join("sources");
        fs::create_dir_all(&dir).map_err(with_path(&dir))?;
        let known = self.pins.get(url).cloned();
        if let Some(hash) = &known
            && dir.join(hash).is_file()
        {
            return Ok((dir.join(hash), hash.clone()));
        }
        let (repository, commit) = url
            .strip_prefix("git+")
            .and_then(|rest| rest.split_once('#'))
            .ok_or_else(|| format!("{url} names no commit after `#`"))?;
        // git would read either as an option, such as `--upload-pack=COMMAND`.
        if repository.starts_with('-') {
            return Err(format!("{url}: a repository can't start with `-`"));
        }
        let is_hash = matches!(commit.len(), 40 | 64)
            && commit
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if !is_hash {
            return Err(format!(
                "{url} names no commit after `#`, only its full hash is one"
            ));
        }
        eprintln!(
            "  fetch  {repository} at {}",
            &commit[..commit.len().min(12)]
        );
        let checkout = dir.join(format!("git.partial{}", std::process::id()));
        fresh(&checkout)?;
        let git = |args: &[&str]| {
            run_host(
                Command::new("git")
                    .arg("-C")
                    .arg(&checkout)
                    .args(["-c", "advice.detachedHead=false"])
                    .args(args)
                    // The host's own settings, such as `insteadOf` or filters, change nothing.
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_TERMINAL_PROMPT", "0"),
                &format!("fetching {commit} of {repository}"),
            )
        };
        git(&["init", "--quiet"])?;
        // Most hosts hand out one commit by its hash; for those that don't, everything.
        git(&["fetch", "--quiet", "--depth", "1", "--", repository, commit])
            .or_else(|_| git(&["fetch", "--quiet", "--tags", "--", repository]))
            .and_then(|()| git(&["checkout", "--quiet", commit]))
            .and_then(|()| {
                git(&[
                    "submodule",
                    "update",
                    "--quiet",
                    "--init",
                    "--recursive",
                    "--depth",
                    "1",
                ])
            })
            .map_err(|e| format!("{url}: {e}"))?;
        let partial = dir.join(format!("git.partial{}.tar", std::process::id()));
        pack(&checkout, &[".".to_owned()], &[], &partial)?;
        remove(&checkout)?;
        let hash = sha256_of(&partial)?;
        if let Some(known) = &known
            && hash != *known
        {
            fs::remove_file(&partial).map_err(with_path(&partial))?;
            return Err(format!("{url} has hash {hash}, and {known} is pinned"));
        }
        let path = dir.join(&hash);
        fs::rename(&partial, &path).map_err(with_path(&path))?;
        Ok((path, hash))
    }

    /// Where the source at `url` is on the host, with its hash. Downloads are kept by their hash,
    /// so a source pinned in a lock or by `expected` that's kept already isn't fetched
    /// again, and two URLs ending alike never share a file.
    fn fetch(
        &mut self,
        lab: &Lab,
        url: &str,
        expected: Option<&str>,
    ) -> Result<(PathBuf, String), String> {
        let dir = lab.work.join("sources");
        fs::create_dir_all(&dir).map_err(with_path(&dir))?;
        let known = expected.or_else(|| self.pins.get(url).map(String::as_str));
        if let Some(hash) = known
            && dir.join(hash).is_file()
        {
            return Ok((dir.join(hash), hash.to_owned()));
        }
        eprintln!("  fetch  {}", url.rsplit('/').next().unwrap_or(url));
        let partial = dir.join(format!("partial.{}", std::process::id()));
        run_host(
            Command::new("curl")
                .args(["-q", "--fail", "--location", "--silent", "--show-error"])
                .args(["--max-time", "1800", "--retry", "3", "--output"])
                .arg(&partial)
                .arg(url),
            &format!("downloading {url}"),
        )?;
        if is_web_page(&partial)? {
            fs::remove_file(&partial).map_err(with_path(&partial))?;
            return Err(format!(
                "{url} gave a web page, such as a sign-in or missing-release page"
            ));
        }
        let hash = sha256_of(&partial)?;
        if let Some(known) = known
            && hash != known
        {
            fs::remove_file(&partial).map_err(with_path(&partial))?;
            let what = if expected.is_some() {
                "was expected"
            } else {
                "is pinned"
            };
            return Err(format!("{url} has hash {hash}, and {known} {what}"));
        }
        let path = dir.join(&hash);
        fs::rename(&partial, &path).map_err(with_path(&path))?;
        Ok((path, hash))
    }
}

/// Whether the file at `path` starts as an HTML page does, which a forge hands out with a
/// success status for a release it doesn't have or one that needs signing in.
fn is_web_page(path: &Path) -> Result<bool, String> {
    let mut start = [0; 512];
    let read = fs::File::open(path)
        .and_then(|mut file| file.read(&mut start))
        .map_err(with_path(path))?;
    let text = String::from_utf8_lossy(&start[..read]);
    let text = text.trim_start().to_ascii_lowercase();
    Ok(text.starts_with("<!doctype html") || text.starts_with("<html"))
}

pub(crate) fn load(repository: &Path, recipes: &str) -> Result<Vec<Package>, String> {
    let path = repository.join(RECIPES).join(recipes);
    let text = fs::read_to_string(&path).map_err(with_path(&path))?;
    let parsed: Recipes = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let styles = styles(repository)?;
    let locked = match Origin::of(recipes, "").lock {
        Lock::Beside { file, .. } => read_lock(&repository.join(file))?,
        Lock::Shared => BTreeMap::new(),
    };
    parsed
        .package
        .into_iter()
        .map(|mut package| {
            package.origin = Origin::of(recipes, &package.id);
            package.made = locked.get(&package.id).and_then(PackageLock::made);
            package
                .check_name()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            package
                .check_style(&styles)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            package
                .check_sources()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            let arch_name = package
                .arch_name()
                .map_err(|e| format!("{}: {e}", path.display()))?
                .to_owned();
            for source in &mut package.sources {
                source.url = expand(&source.url, package.version.as_deref(), &arch_name);
            }
            package
                .with_option_defaults()
                .map_err(|e| format!("{}: {e}", path.display()))
        })
        .collect()
}

/// What a failed build said it was missing, from the end of its log.
#[derive(Debug, PartialEq, Eq)]
enum Missing {
    Program(String),
    /// A pkg-config module.
    Module(String),
    Header(String),
    /// An autoconf macro left unexpanded in `configure`, which the shell then runs as a command.
    Macro(String),
    /// A place outside the package's prefix that its install wrote to, which is read-only.
    Outside(String),
}

/// What the last lines of `log` say a build was missing, when they say it the way Meson,
/// configure, a compiler or the shell do.
fn missing_in(log: &str) -> Option<Missing> {
    let between = |line: &str, start: &str, end: &str| {
        let rest = &line[line.find(start)? + start.len()..];
        Some(rest[..rest.find(end)?].to_owned())
    };
    log.lines().rev().take(40).find_map(|line| {
        if let Some(name) = between(line, "ERROR: Program '", "' not found") {
            return Some(Missing::Program(name));
        }
        if let Some(name) = between(line, "Dependency \"", "\" not found") {
            return Some(Missing::Module(name));
        }
        if let Some(name) =
            line.strip_prefix("Looking for a fallback subproject for the dependency ")
        {
            return Some(Missing::Module(name.trim().to_owned()));
        }
        if line.contains("no suitable Python interpreter") {
            return Some(Missing::Program("python3".to_owned()));
        }
        if let Some(name) = between(line, "fatal error: ", ": No such file or directory") {
            return Some(Missing::Header(name));
        }
        if let Some(place) = line
            .strip_prefix("Installing ")
            .and_then(|l| l.rsplit(" to ").next())
            && place.starts_with('/')
            && !place.starts_with("/store/")
        {
            return Some(Missing::Outside(place.to_owned()));
        }
        if line.contains("Read-only file system")
            && let Some(place) = between(line, "'/", "'")
        {
            return Some(Missing::Outside(format!("/{place}")));
        }
        let program = line
            .strip_suffix(": command not found")?
            .rsplit(": ")
            .next()?;
        let macro_like = program.contains('_')
            && program
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
        Some(match macro_like {
            true => Missing::Macro(program.to_owned()),
            false => Missing::Program(program.to_owned()),
        })
    })
}

/// Which built recipe has what a build was missing, said as what to do about it.
fn failure_hint(lab: &Lab, built: &Built, missing: &Missing) -> String {
    let (what, name, file, places): (&str, &str, String, &[&str]) = match missing {
        Missing::Macro(name) => {
            return format!(
                "`{name}` is an autoconf macro configure was made without: when it's made in \
                 the build, aclocal finds the macros of every dependency once automake is one"
            );
        }
        Missing::Outside(place) => {
            return format!(
                "it installs into `{place}`, outside its prefix: set the option for that \
                 directory to one under the prefix"
            );
        }
        Missing::Program(name) => ("the program", name, name.clone(), &["bin"]),
        Missing::Module(name) => (
            "the pkg-config module",
            name,
            format!("{name}.pc"),
            &["lib/pkgconfig", "share/pkgconfig"],
        ),
        Missing::Header(name) => ("the header", name, name.clone(), &["include"]),
    };
    let has = built.paths.keys().find(|id| {
        built.output_paths(id).iter().any(|path| {
            places
                .iter()
                .any(|place| lab.on_host(path).join(place).join(&file).exists())
        })
    });
    match (has, missing) {
        (None, _) => format!("no recipe built so far has {what} `{name}`; one may need writing"),
        (Some(id), Missing::Header(_)) => format!(
            "`{id}` has {what} `{name}`: add it to `extra_deps`, or to `propagates` of the \
             library whose headers include it"
        ),
        (Some(id), _) => format!("`{id}` has {what} `{name}`: add it to `extra_deps`"),
    }
}

/// Where build styles are, beside the recipes.
const STYLES: &str = "styles";

/// The build styles in `repository`, loaded once for each repository.
fn styles(repository: &Path) -> Result<Arc<Vec<Style>>, String> {
    static LOADED: Mutex<BTreeMap<PathBuf, Arc<Vec<Style>>>> = Mutex::new(BTreeMap::new());
    let mut loaded = LOADED
        .lock()
        .map_err(|_| "the build styles failed to load before")?;
    if let Some(styles) = loaded.get(repository) {
        return Ok(Arc::clone(styles));
    }
    let dir = repository.join(STYLES);
    let mut files: Vec<PathBuf> = match fs::read_dir(&dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "star"))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    let styles = files
        .iter()
        .map(|file| {
            let text = fs::read_to_string(file).map_err(with_path(file))?;
            style::load(&file.display().to_string(), &text)
        })
        .collect::<Result<Vec<_>, String>>()?;
    let styles = Arc::new(styles);
    loaded.insert(repository.to_owned(), Arc::clone(&styles));
    Ok(styles)
}

/// The recipes in `recipes`, with the script of each that has a build style made by it. A styled
/// recipe's first source is fetched, if it hasn't been, for the style to read.
pub(crate) fn load_styled(
    lab: &Lab,
    repository: &Path,
    recipes: &str,
) -> Result<Vec<Package>, String> {
    let mut packages = load(repository, recipes)?;
    if !packages.iter().any(|package| package.styled) {
        return Ok(packages);
    }
    let styles = styles(repository)?;
    let mut sources = Sources::load(repository)?;
    for package in packages.iter_mut().filter(|package| package.styled) {
        *package = style_one(lab, repository, &styles, &mut sources, package.clone())?;
    }
    Ok(packages)
}

/// The recipes in `files` that `wanted` needs, each styled as it's reached. Recipes nothing
/// wanted needs aren't styled, so their sources aren't fetched: a configuration costs what it
/// uses, however many recipes there are.
pub(crate) fn load_reached(
    lab: &Lab,
    repository: &Path,
    files: &[&str],
    wanted: &[&str],
) -> Result<Vec<Package>, String> {
    let mut packages = Vec::new();
    for file in files {
        packages.extend(load(repository, file)?);
    }
    if !packages.iter().any(|package| package.styled) {
        return reached(packages, wanted, Ok);
    }
    let styles = styles(repository)?;
    let mut sources = Sources::load(repository)?;
    reached(packages, wanted, |package| {
        style_one(lab, repository, &styles, &mut sources, package)
    })
}

/// The packages `wanted` needs, and what those need in turn, with `style` run on each styled one
/// before its dependencies are followed, since a style adds some. Ids no package has are left
/// for whoever builds to report.
fn reached(
    packages: Vec<Package>,
    wanted: &[&str],
    mut style: impl FnMut(Package) -> Result<Package, String>,
) -> Result<Vec<Package>, String> {
    let mut waiting: BTreeMap<String, Package> = packages
        .into_iter()
        .map(|package| (package.id.clone(), package))
        .collect();
    let mut queue: Vec<String> = wanted.iter().rev().map(|id| (*id).to_owned()).collect();
    let mut found = Vec::new();
    while let Some(id) = queue.pop() {
        let Some(package) = waiting.remove(&id) else {
            continue;
        };
        let package = if package.styled {
            style(package)?
        } else {
            package
        };
        queue.extend(package.dep_ids()?.into_iter().rev());
        found.push(package);
    }
    Ok(found)
}

/// `package` styled: planned from what its lock records, when that was made from what it would be
/// made from now, or else with its style's script. What a style makes for a checkout of the set
/// is recorded in the lock; a pinned set's lock has to have it already. A recipe built from a tree
/// of a checkout is always styled, and never recorded.
fn style_one(
    lab: &Lab,
    repository: &Path,
    styles: &[Style],
    sources: &mut Sources,
    package: Package,
) -> Result<Package, String> {
    if package.built_from_a_tree() {
        return style_now(lab, repository, styles, sources, package);
    }
    if let Some(made) = package.made.clone()
        && let Some(style) = styles.iter().find(|style| style.name() == made.style)
        && made_from(lab, repository, sources, &package, style)? == made.from
    {
        return package.planned_from(made);
    }
    let lock = package.origin.lock.clone();
    let recorded = package.made.clone();
    let styled = style_now(lab, repository, styles, sources, package)?;
    let (Some(style), Some(record), Lock::Beside { id, .. }) =
        (&styled.style, &styled.record, &lock)
    else {
        return Ok(styled);
    };
    // A build made from the recipe under another name isn't what the lock records.
    if *id != styled.id {
        return Ok(styled);
    }
    let unstyled = styled
        .unstyled
        .as_deref()
        .expect("a styled package keeps its recipe");
    let style = styles
        .iter()
        .find(|found| found.name() == style)
        .expect("its style loaded");
    let made = Made {
        style: style.name().to_owned(),
        record: record.clone(),
        from: made_from(lab, repository, sources, unstyled, style)?,
        deps: styled.added_deps(unstyled)?,
    };
    if recorded.as_ref() != Some(&made) {
        if sources.read_only {
            return Err(format!(
                "`{}`'s lock doesn't record what its style makes now, and a pinned grapes never \
                 records anything: that version of grapes is broken",
                styled.id
            ));
        }
        sources.remember(&lock, &made)?;
    }
    Ok(styled)
}

/// The digest of what `package`'s style makes its script from: the style's code, and the
/// recipe's name, version, options for the style, sources and own files.
fn made_from(
    lab: &Lab,
    repository: &Path,
    sources: &mut Sources,
    package: &Package,
    style: &Style,
) -> Result<String, String> {
    let options = serde_json::to_value(package.style_options.get(style.name()))
        .map_err(|e| e.to_string())?
        .to_string();
    let mut parts = vec![
        "from".to_owned(),
        style.name().to_owned(),
        style.digest().to_owned(),
        package.id.clone(),
        package.version.clone().unwrap_or_default(),
        options,
    ];
    for source in &package.sources {
        parts.push(sources.hash_of(lab, source, &package.origin.lock)?);
    }
    for name in &package.files {
        let file = repository.join(&package.origin.dir).join(name);
        parts.push(sha256_of(&file)?);
    }
    Ok(digest(&parts))
}

/// The SHA-256 of `parts`, each preceded by its length so no two lists of parts run together.
fn digest(parts: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    crate::store::hex(&hasher.finalize())
}

/// `package` with its style's script, its first source fetched, if it hasn't been, for the style to
/// read, and the digest of what the style made.
fn style_now(
    lab: &Lab,
    repository: &Path,
    styles: &[Style],
    sources: &mut Sources,
    package: Package,
) -> Result<Package, String> {
    let first = package
        .sources
        .first()
        .ok_or_else(|| format!("`{}` has a build style but no source", package.id))?;
    let (archive, _) = sources.fetch_source(lab, first, &package.origin.lock)?;
    let top = || top_files(&archive);
    // A lock among the recipe's own files, or under `recipes/`, is one kept beside the recipe, for
    // a project that ships none.
    let read = |path: &str| {
        let file = if package.files.iter().any(|name| name == path) {
            repository.join(&package.origin.dir).join(path)
        } else if path.starts_with("recipes/") {
            repository.join(path)
        } else {
            return read_in(&archive, path);
        };
        fs::read_to_string(&file).map_err(with_path(&file))
    };
    let mut styled = package.clone().styled_by(styles, &top, &read)?;
    // What the style made: the script, the dependencies and the sources it added, each source by
    // its hash, so a record differs whenever what's built from it would.
    let mut parts = vec![styled.script.clone()];
    parts.extend(styled.added_deps(&package)?);
    for source in &styled.sources[package.sources.len()..] {
        let hash = match &source.sha256 {
            Some(hash) => hash.clone(),
            None => sources.hash_of(lab, source, &package.origin.lock)?,
        };
        parts.push(format!(
            "{} {:?} {} {hash}",
            source.url, source.into, source.file
        ));
    }
    styled.record = Some(digest(&parts));
    styled.unstyled = Some(Box::new(package));
    Ok(styled)
}

/// A build of a styled recipe, styled again with the options it sets.
pub(crate) fn restyled(lab: &Lab, repository: &Path, package: Package) -> Result<Package, String> {
    if !package.styled {
        return Ok(package);
    }
    let styles = styles(repository)?;
    let mut sources = Sources::load(repository)?;
    style_one(lab, repository, &styles, &mut sources, package)
}

impl Package {
    /// Refuses fields that are neither the recipe's nor its style's options, and a style beside
    /// a script. A recipe with no script, or one that names a style, is built by a style.
    fn check_style(&mut self, styles: &[Style]) -> Result<(), String> {
        for name in self.style_options.keys() {
            if !styles.iter().any(|style| style.name() == name) {
                return Err(format!(
                    "`{}` has `{name}`, which is neither a recipe field nor a build style",
                    self.id
                ));
            }
        }
        if self.style_options.len() > 1 {
            return Err(format!(
                "`{}` gives options to more than one style",
                self.id
            ));
        }
        let names_style = !self.style_options.is_empty();
        if names_style && !self.script.is_empty() {
            return Err(format!(
                "`{}` has a script and a build style; a style makes the script",
                self.id
            ));
        }
        self.styled = names_style || self.script.is_empty();
        Ok(())
    }

    /// The recipe planned from what its lock records its style made: the dependencies the style
    /// added and the digest of what it made, but not the script, which a build styles it for.
    fn planned_from(self, made: Made) -> Result<Self, String> {
        let mut planned = self.clone();
        let listed = self.dep_ids()?;
        planned.extra_deps.extend(
            made.deps
                .into_iter()
                .filter(|dep| !listed.contains(dep))
                .map(DepSpec::Id),
        );
        planned.style = Some(made.style);
        planned.record = Some(made.record);
        planned.planned = true;
        planned.unstyled = Some(Box::new(self));
        Ok(planned)
    }

    /// The recipe with its style's script, after its options, and the style's dependencies and
    /// sources added. `top` lists the files at the top of its first source, and `read` reads one.
    fn styled_by(
        mut self,
        styles: &[Style],
        top: &dyn Fn() -> Result<Vec<String>, String>,
        read: &dyn Fn(&str) -> Result<String, String>,
    ) -> Result<Self, String> {
        let style = match self.style_options.keys().next() {
            Some(name) => styles
                .iter()
                .find(|style| style.name() == name)
                .ok_or_else(|| {
                    let names: Vec<&str> = styles.iter().map(Style::name).collect();
                    format!(
                        "`{}` builds with the {name} style, and the styles are {}",
                        self.id,
                        names.join(", ")
                    )
                })?,
            None => found_style(&self.id, styles, &top()?)?,
        };
        let mut set = BTreeMap::new();
        if let Some(toml::Value::Table(table)) = self.style_options.get(style.name()) {
            for (name, value) in table {
                let json = serde_json::to_value(value).map_err(|e| e.to_string())?;
                let value = style::from_json(json).map_err(|e| e.to_string())?;
                set.insert(name.clone(), value);
            }
        }
        let version = self.version.clone().unwrap_or_default();
        let record = style.run(&self.id, &version, &set, read)?;
        self.script.push_str(&record.script);
        let listed = self.dep_ids()?;
        for dep in record.deps {
            if !listed.contains(&dep) {
                self.extra_deps.push(DepSpec::Id(dep));
            }
        }
        self.sources
            .extend(record.sources.into_iter().map(|source| Source {
                url: source.url,
                into: source.into,
                include: Vec::new(),
                sha256: source.sha256,
                file: source.file,
            }));
        self.style = Some(style.name().to_owned());
        Ok(self)
    }
}

/// The style the files at the top of a source pick: the one whose lock is there, or with no lock,
/// the one whose build file is there.
fn found_style<'a>(id: &str, styles: &'a [Style], top: &[String]) -> Result<&'a Style, String> {
    let has = |names: &[String]| names.iter().any(|name| top.contains(name));
    for (kind, picks) in [
        ("lock", Style::locks as fn(&Style) -> &[String]),
        ("build file", Style::files),
    ] {
        let found: Vec<&Style> = styles.iter().filter(|style| has(picks(style))).collect();
        match found[..] {
            [] => {}
            [style] => return Ok(style),
            _ => {
                let names: Vec<&str> = found.iter().map(|style| style.name()).collect();
                return Err(format!(
                    "`{id}`'s source has a {kind} for each of {}: name one with its table, \
                     such as `[package.{}]`",
                    names.join(" and "),
                    names[0]
                ));
            }
        }
    }
    Err(format!(
        "`{id}` has no script, and nothing at the top of its source picks a build style: give it \
         a style's table, such as `[package.meson]`, or a `script`"
    ))
}

/// The names at the top of the source archive at `archive`, under its top directory, kept beside
/// it like what [`read_in`] reads.
fn top_files(archive: &Path) -> Result<Vec<String>, String> {
    let mut name = archive.as_os_str().to_owned();
    name.push(".files");
    let kept = PathBuf::from(name).join("%%top");
    if let Ok(text) = fs::read_to_string(&kept) {
        return Ok(text.lines().map(str::to_owned).collect());
    }
    let listing = listing(archive)?;
    let top = top_directory(&listing);
    let mut names = BTreeSet::new();
    for line in listing.lines() {
        let inside = match top {
            Some(top) => line
                .strip_prefix(top)
                .and_then(|rest| rest.strip_prefix('/')),
            None => Some(line),
        };
        if let Some(name) = inside.and_then(|rest| rest.split('/').next())
            && !name.is_empty()
        {
            names.insert(name.to_owned());
        }
    }
    let text: String = names.iter().map(|name| format!("{name}\n")).collect();
    let partial = kept.with_extension(format!("partial{}", std::process::id()));
    if let Some(dir) = kept.parent() {
        fs::create_dir_all(dir).map_err(with_path(dir))?;
    }
    fs::write(&partial, &text)
        .and_then(|()| fs::rename(&partial, &kept))
        .map_err(with_path(&kept))?;
    Ok(names.into_iter().collect())
}

/// `url` with `{version}`, `{series}` and `{underscored}`, the version with underscores for dots
/// as some projects tag releases (`NSS_3_133_RTM`), when the recipe has a version, and the
/// target's names, `arch_name` being how its project names the target, filled in.
fn expand(url: &str, version: Option<&str>, arch_name: &str) -> String {
    let url = match version {
        Some(version) => url
            .replace("{version}", version)
            .replace("{series}", &series(version))
            .replace("{underscored}", &version.replace('.', "_")),
        None => url.to_owned(),
    };
    url.replace("{arch_name}", arch_name)
        .replace("{arch}", ARCH)
        .replace("{os}", OS)
        .replace("{target}", &format!("{ARCH}-{OS}"))
        .replace("{triple}", TRIPLE)
}

/// The release series a version belongs to, as GNOME names its download directories: `2.84` for
/// `2.84.4`, and the major version alone from 40 on, so `51` for `51.1`.
fn series(version: &str) -> String {
    let mut parts = version.split('.');
    let major = parts.next().unwrap_or_default();
    match (major.parse::<u64>(), parts.next()) {
        (Ok(number), Some(minor)) if number < 40 => format!("{major}.{minor}"),
        _ => major.to_owned(),
    }
}

/// A recipe from its TOML, for tests elsewhere in the crate.
#[cfg(test)]
pub(crate) fn recipe_from_str(text: &str) -> Package {
    toml::from_str::<Recipes>(text)
        .expect("the test recipe parses")
        .package
        .remove(0)
        .with_option_defaults()
        .expect("the test recipe's options are right")
}

#[cfg(test)]
mod series_tests {
    #[test]
    fn a_series_is_major_and_minor_or_from_40_the_major_alone() {
        assert_eq!(super::series("2.84.4"), "2.84");
        assert_eq!(super::series("1.10.8"), "1.10");
        assert_eq!(super::series("51.1"), "51");
        assert_eq!(super::series("7"), "7");
    }

    #[test]
    fn a_url_names_the_version_its_series_or_the_version_with_underscores() {
        let url = "https://example.org/{series}/NSS_{underscored}_RTM/nss-{version}.tar.gz";
        assert_eq!(
            super::expand(url, Some("3.133"), "x86_64"),
            "https://example.org/3.133/NSS_3_133_RTM/nss-3.133.tar.gz"
        );
    }

    #[test]
    fn a_recipe_says_how_its_project_names_the_target() {
        let go = super::recipe_from_str(
            "[[package]]\nid = \"go\"\nscope = \"runtime\"\nscript = \"\"\n\
             arch_names = { x86_64 = \"amd64\", aarch64 = \"arm64\" }\n",
        );
        assert_eq!(go.arch_name().unwrap(), "amd64");
        assert_eq!(
            super::expand(
                "go-{version}.linux-{arch_name}.tar.gz",
                Some("1.27"),
                "amd64"
            ),
            "go-1.27.linux-amd64.tar.gz"
        );
        let plain = super::recipe_from_str(
            "[[package]]\nid = \"jq\"\nscope = \"runtime\"\nscript = \"\"\n",
        );
        assert_eq!(plain.arch_name().unwrap(), super::ARCH);
        let other = super::recipe_from_str(
            "[[package]]\nid = \"x\"\nscope = \"runtime\"\nscript = \"\"\n\
             arch_names = { aarch64 = \"arm64\" }\n",
        );
        assert!(other.arch_name().unwrap_err().contains("but not `x86_64`"));
        assert_eq!(super::loader_name(), "ld-linux-x86-64.so.2");
    }

    #[test]
    fn a_url_names_the_target_as_projects_spell_it() {
        let url = "https://example.org/{os}/{arch}/x-{version}-{triple}.tar.xz?for={target}";
        assert_eq!(
            super::expand(url, Some("1.0"), "x86_64"),
            "https://example.org/linux/x86_64/x-1.0-x86_64-unknown-linux-gnu.tar.xz?for=x86_64-linux"
        );
    }
}

/// Fetches `package`'s sources, those a style added included.
fn fetch_all(
    sources: &mut Sources,
    lab: &Lab,
    package: &Package,
) -> Result<Vec<(Source, PathBuf, String)>, String> {
    let mut fetched = Vec::new();
    for source in &package.sources {
        let (path, hash) = sources.fetch_source(lab, source, &package.origin.lock)?;
        fetched.push((source.clone(), path, hash));
    }
    Ok(fetched)
}

/// The text of `relative`, a path inside the source archive at `archive`, under its top directory.
/// Archives are kept by their hash, so what was read from one, or found missing from it, is kept
/// beside it and known from there next time, which spares a plan unpacking every lock.
fn read_in(archive: &Path, relative: &str) -> Result<String, String> {
    let mut name = archive.as_os_str().to_owned();
    name.push(".files");
    let dir = PathBuf::from(name);
    let flat = relative.replace('/', "%");
    let kept = dir.join(&flat);
    let missing = dir.join(format!("{flat}%%missing"));
    let lacks = || format!("the source has no `{relative}`");
    if let Ok(text) = fs::read_to_string(&kept) {
        return Ok(text);
    }
    if missing.exists() {
        return Err(lacks());
    }
    let found = unpack_one(archive, relative)?;
    let (path, text) = match &found {
        Some(text) => (&kept, text.as_str()),
        None => (&missing, ""),
    };
    let partial = path.with_extension(format!("partial{}", std::process::id()));
    fs::create_dir_all(&dir)
        .and_then(|()| fs::write(&partial, text))
        .and_then(|()| fs::rename(&partial, path))
        .map_err(with_path(path))?;
    found.ok_or_else(lacks)
}

/// The names in an archive, one per line.
fn listing(archive: &Path) -> Result<String, String> {
    let output = Command::new("tar")
        .arg("-tf")
        .arg(archive)
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "listing {}: {}",
            archive.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The directory everything in an archive is under, as a release's `project-1.0/`, when there
/// is one. Some archives, such as font releases, hold their files with nothing around them.
fn top_directory(listing: &str) -> Option<&str> {
    let top = listing.lines().next()?.split('/').next()?;
    let mut has_files = false;
    for line in listing.lines() {
        // Some archives list the directory itself without its slash, as `openssh-10.0p1`.
        match line.strip_prefix(top) {
            Some("" | "/") => {}
            Some(rest) if rest.starts_with('/') => has_files = true,
            _ => return None,
        }
    }
    has_files.then_some(top)
}

/// Unpacks a source archive into `target`, without the directory around its files.
fn unpack(archive: &Path, target: &Path) -> Result<(), String> {
    let names = listing(archive)?;
    fs::create_dir_all(target).map_err(with_path(target))?;
    store::usual_umask();
    let mut command = Command::new("tar");
    // Owners and modes from the archive stay out, also when building as root.
    command.args(["--no-same-owner", "--no-same-permissions", "-xf"]);
    command.arg(archive).arg("-C").arg(target);
    if top_directory(&names).is_some() {
        command.arg("--strip-components=1");
    }
    run_host(&mut command, &format!("unpacking {}", archive.display()))
}

/// The text of `relative` in `archive`, or `None` when the archive doesn't have it.
fn unpack_one(archive: &Path, relative: &str) -> Result<Option<String>, String> {
    let names = listing(archive)?;
    if names.is_empty() {
        return Err(format!("{} is empty", archive.display()));
    }
    let path = match top_directory(&names) {
        Some(top) => format!("{top}/{relative}"),
        None => relative.to_owned(),
    };
    // An archive appended to can hold a file twice, and unpacking it leaves the last one.
    let copies = names.lines().filter(|name| *name == path).count();
    if copies == 0 {
        return Ok(None);
    }
    let output = Command::new("tar")
        .arg("-xOf")
        .arg(archive)
        .arg(format!("--occurrence={copies}"))
        .arg(path)
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "unpacking `{relative}`: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    String::from_utf8(output.stdout)
        .map(Some)
        .map_err(|_| format!("`{relative}` isn't text"))
}

/// What stands for the arm in a package's key and record: boot packages had one of their own.
fn arm_key(package: &Package) -> &'static str {
    if package.is_boot() { BOOT_ARM } else { ARM }
}

pub(crate) fn built_from_seed(seed: &StorePath) -> Built {
    let mut built = Built::default();
    built.paths.insert("seed".to_owned(), seed.clone());
    built.closures.insert("seed".to_owned(), BTreeSet::new());
    built
}

/// Whether `package` is moved to a path named by its contents once it's built.
pub(crate) fn content_addressed(package: &Package) -> bool {
    !package.is_boot()
}

/// Sets where a package is from what its record says.
pub(crate) fn remember_outputs(built: &mut Built, package: &Package, record: &str) {
    let outputs: split::Outputs = record
        .split(' ')
        .filter_map(|part| part.split_once('='))
        .filter_map(|(name, hash)| Some((split::class_named(name)?, StorePath::from_hash(hash))))
        .collect();
    built
        .paths
        .insert(package.id.clone(), outputs[&split::Class::Out].clone());
    built.outputs.insert(package.id.clone(), outputs);
}

/// What everything else builds on, and a program built with it to show it works. The toolchain
/// is these and what they need, wherever their recipes are.
pub(crate) const TOOLCHAIN: [&str; 6] = [
    "wrapper",
    "linux-headers",
    "binutils",
    "glibc",
    "gcc",
    "hello",
];

/// Builds the toolchain, reusing what's already built.
pub fn build_toolchain(lab: &Lab, repository: &Path) -> Result<Built, String> {
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
    let mut built = built_from_seed(&seed);
    let files = recipe_files(repository)?;
    let reached = load_reached(lab, repository, &names_of(&files), &TOOLCHAIN)?;
    let packages = in_order(reached, |id| built.paths.contains_key(id))?;
    for package in &packages {
        if let Some(prepared) =
            build_one(lab, repository, package, &seed, &mut sources, &mut built)?
        {
            remove(&prepared.build_dir)?;
            finish(lab, package, &prepared.out, &mut built)?;
        }
    }
    Ok(built)
}

/// What a build ran with, for the steps that come after it.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub(crate) out: StorePath,
    pub(crate) closure: BTreeSet<StorePath>,
    pub(crate) view: View,
    pub(crate) env: BTreeMap<String, String>,
    pub(crate) limits: Limits,
    pub(crate) build_dir: PathBuf,
    pub(crate) dep_paths: Vec<StorePath>,
    /// Held until the package is where it ends up, so another build of the same key waits for it.
    pub(crate) _lock: fs::File,
}

/// The version of what the builder does to every package beside its recipe, such as fixing up
/// run paths. Every package's key has it, so changing it rebuilds everything.
const BUILD_RULES: &str = "12";

/// The machines packages are built for, which every key has, so another one's never match.
const TARGET: &str = "x86_64-linux-gnu";

/// The target as recipes name it (0032): in sources as `{arch}`, `{os}`, `{target}` and
/// `{triple}`, and in scripts as `$WRATH_ARCH`, `$WRATH_OS` and `$WRATH_TRIPLE`. One target for
/// now; these become the toolset's once there are more.
const ARCH: &str = "x86_64";
const OS: &str = "linux";
const TRIPLE: &str = "x86_64-unknown-linux-gnu";
/// What goes into a package's key from its recipe, before any dependency. `source_hashes` are
/// its sources', in order. A styled package's are its recipe's, as written, and the digest of what
/// its style made, so a plan knows its key from its lock without reading its source.
///
/// Every field is named here, so a new one doesn't compile until it's either in the key or said
/// not to change what's built: a key that leaves a field out reuses a build that's wrong for it.
fn recipe_parts(
    repository: &Path,
    package: &Package,
    source_hashes: &[&str],
) -> Result<Vec<Vec<u8>>, String> {
    if let (Some(recipe), Some(record)) = (&package.unstyled, &package.record) {
        let own = source_hashes
            .get(..recipe.sources.len())
            .ok_or_else(|| format!("`{}` has fewer source hashes than sources", package.id))?;
        let mut parts = recipe_parts(repository, recipe, own)?;
        parts.push(format!("record {record}").into_bytes());
        return Ok(parts);
    }
    let Package {
        id,
        version,
        scope,
        arch_names,
        whole,
        propagates,
        propagate_private,
        sources,
        files,
        script,
        wrap,
        fixup,
        // In the key as the dependencies they add, which `package_key` names.
        toolset: _,
        core: _,
        deps: _,
        extra_deps: _,
        runtime_deps: _,
        // Their values reach the build only through `script`, which they're written into.
        options: _,
        style_options: _,
        styled: _,
        style: _,
        // A styled package's key is its recipe's and the digest of what its style made, above.
        unstyled: _,
        made: _,
        record: _,
        planned: _,
        // Where its files are, which are in the key by what they hold.
        origin,
        // What a generation makes from the packages, not this package's build.
        indexes: _,
        // What its dependents' builds see, in their keys as what it expands to there.
        search_paths: _,
        // For update checks, the tests and how long a build may be quiet: none changes what's
        // built.
        track: _,
        hold: _,
        silent_for: _,
        smoke: _,
        probes: _,
        check_build: _,
        check: _,
        check_in_view: _,
        check_deps: _,
    } = package;
    let mut parts: Vec<Vec<u8>> = vec![
        BUILD_RULES.as_bytes().to_vec(),
        TARGET.as_bytes().to_vec(),
        arm_key(package).as_bytes().to_vec(),
        // Whether it builds under /usr or its own prefix.
        format!("{scope:?}").into_bytes(),
        id.as_bytes().to_vec(),
        version.as_deref().unwrap_or_default().as_bytes().to_vec(),
        script.as_bytes().to_vec(),
        vec![u8::from(*wrap), u8::from(*fixup), u8::from(*whole)],
    ];
    // Scripts read the name it gives this target, which no source's hash covers.
    if !arch_names.is_empty() {
        parts.push(format!("arch_names {arch_names:?}").into_bytes());
    }
    if !propagates.is_empty() || !propagate_private {
        parts.push(format!("propagates {propagates:?} {propagate_private}").into_bytes());
    }
    for name in files {
        let path = repository.join(&origin.dir).join(name);
        parts.push(fs::read(&path).map_err(with_path(&path))?);
    }
    parts.extend(source_hashes.iter().map(|hash| hash.as_bytes().to_vec()));
    parts.extend(sources.iter().map(source_part));
    Ok(parts)
}

/// Where a source goes in the tree. What it holds is in the key as its hash.
fn source_part(source: &Source) -> Vec<u8> {
    let Source {
        into,
        file,
        // The hash covers what these fetch and take.
        url: _,
        include: _,
        sha256: _,
    } = source;
    format!("source {into:?} {file}").into_bytes()
}

/// A package's key of `kind`: its recipe, and its dependencies as named by `reference`.
fn package_key(
    kind: &str,
    recipe: &[Vec<u8>],
    deps: &[Dep],
    reference: impl Fn(&Dep) -> String,
) -> StorePath {
    let dep_texts: Vec<String> = deps
        .iter()
        .map(|dep| {
            // Every field is named, so a new one doesn't compile until it's keyed.
            let Dep {
                id,
                view,
                rpath,
                skip,
                propagate,
                exclude,
            } = dep;
            let mut text = format!("{id} {} {view} {rpath} {skip:?}", reference(dep));
            // Only a choice that differs from the default changes a key.
            if *propagate != Propagate::All || !exclude.is_empty() {
                text.push_str(&format!(" {propagate:?} {exclude:?}"));
            }
            text
        })
        .collect();
    let parts: Vec<&[u8]> = recipe
        .iter()
        .map(Vec::as_slice)
        .chain(dep_texts.iter().map(String::as_bytes))
        .collect();
    StorePath::derive(kind, &parts)
}

/// The key each of `packages` has before anything is built.
pub(crate) fn keys_of(
    lab: &Lab,
    repository: &Path,
    packages: Vec<Package>,
) -> Result<BTreeMap<String, String>, String> {
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
    let mut keys = BTreeMap::from([("seed".to_owned(), seed.hash().to_owned())]);
    let packages = in_order(packages, |id| keys.contains_key(id))?;
    for package in &packages {
        let hashes = package
            .sources
            .iter()
            .map(|source| sources.hash_of(lab, source, &package.origin.lock))
            .collect::<Result<Vec<String>, String>>()?;
        let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
        let mut recipe = recipe_parts(repository, package, &hashes)?;
        let deps = package.all_deps()?;
        recipe.extend(declared_parts(&deps, |id| {
            let found = packages.iter().find(|package| package.id == id);
            found
                .map(|package| package.search_paths.clone())
                .unwrap_or_default()
        }));
        let key = package_key("unresolved", &recipe, &deps, |dep| keys[&dep.id].clone());
        keys.insert(package.id.clone(), key.hash().to_owned());
    }
    keys.remove("seed");
    Ok(keys)
}

/// Builds one package. `None` means it was built before.
pub(crate) fn build_one(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
) -> Result<Option<Prepared>, String> {
    build_package(lab, repository, package, seed, sources, built, false)
}

/// Builds `package` again even when it's built, at its temporary path, to run what only its build
/// tree has, such as its tests. What was built before stays as it was, record and all, so the
/// caller removes the build when done with it. A package that isn't moved to its content
/// address is never built again, since its temporary path is where it lives.
pub(crate) fn rebuild_one(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
) -> Result<Option<Prepared>, String> {
    if !content_addressed(package) {
        return Ok(None);
    }
    build_package(lab, repository, package, seed, sources, built, true)
}

/// Works out how to build `package`: its key, sources and view, recorded in `built`. Says when
/// it's built already, by this process or another.
pub(crate) fn begin(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
    again: bool,
) -> Result<Begun, String> {
    let styled;
    let package = match (&package.unstyled, package.planned) {
        (Some(recipe), true) => {
            styled = style_now(
                lab,
                repository,
                &styles(repository)?,
                sources,
                (**recipe).clone(),
            )?;
            if styled.record != package.record {
                return Err(format!(
                    "`{}`'s style made something other than what its lock records from the same \
                     style and source, so it doesn't make the same thing twice",
                    package.id
                ));
            }
            &styled
        }
        _ => package,
    };
    let listed = package.all_deps()?;
    let deps = with_requirements(lab, package, &listed, built)?;
    let mut closure = BTreeSet::from([seed.clone()]);
    for dep in &deps {
        let path = built
            .paths
            .get(&dep.id)
            .ok_or_else(|| format!("`{}` needs `{}`, not built before it", package.id, dep.id))?;
        closure.insert(path.clone());
        closure.extend(built.output_paths(&dep.id));
        closure.extend(built.closures[&dep.id].iter().cloned());
    }

    let fetched = fetch_all(sources, lab, package)?;
    let source_hashes: Vec<&str> = fetched.iter().map(|(_, _, hash)| hash.as_str()).collect();
    let recipe = recipe_parts(repository, package, &source_hashes)?;
    let declared = |id: &str| built.search_paths.get(id).cloned().unwrap_or_default();
    let in_view: Vec<(&str, Vec<StorePath>)> = deps
        .iter()
        .filter(|dep| dep.view)
        .map(|dep| (dep.id.as_str(), built.output_paths(&dep.id)))
        .collect();
    let searched = search_path_values(&in_view, declared, |output, file| {
        lab.on_host(output).join(file).is_dir()
    });
    // What the build sees through them changes it, though no dependency's output changed.
    let mut built_from = recipe.clone();
    built_from.extend(
        searched
            .iter()
            .map(|(variable, value)| format!("search {variable}={value}").into_bytes()),
    );
    let out = package_key("build", &built_from, &deps, |dep| built.reference(&dep.id))
        .named(&package.store_name());
    let mut planned_from = recipe;
    planned_from.extend(declared_parts(&listed, declared));
    let unresolved = package_key("unresolved", &planned_from, &listed, |dep| {
        built.unresolved_or_path(&dep.id)
    })
    .hash()
    .to_owned();
    let record = PackageRecord {
        unresolved: unresolved.clone(),
        resolved: out.hash().to_owned(),
        text: serde_json::json!({
            "kind": "package",
            "id": package.id,
            "arm": arm_key(package),
            "script": package.script,
            "sources": source_hashes,
            "deps": deps
                .iter()
                .map(|dep| (dep.id.clone(), built.paths[&dep.id].hash().to_owned()))
                .collect::<BTreeMap<_, _>>(),
        }),
    };
    built.unresolved.insert(package.id.clone(), unresolved);
    built.records.insert(package.id.clone(), record);
    built.paths.insert(package.id.clone(), out.clone());
    built.temporary.insert(package.id.clone(), out.clone());
    built.closures.insert(package.id.clone(), closure.clone());
    built.requirements.remove(&package.id);
    if !package.indexes.is_empty() {
        built
            .indexes
            .insert(package.id.clone(), package.indexes.clone());
    }
    if !package.search_paths.is_empty() {
        built
            .search_paths
            .insert(package.id.clone(), package.search_paths.clone());
    }
    built.providers.insert(
        package.id.clone(),
        requires::Provider {
            deps: deps
                .iter()
                .filter(|dep| dep.view)
                .map(|dep| dep.id.clone())
                .collect(),
            propagates: package.propagates.clone(),
            private: package.propagate_private,
        },
    );

    let lock = build_lock(lab, &out, &package.id)?;
    let done_dir = lab.work.join("done");
    let done = done_dir.join(out.hash());
    // Read once the key is ours: a build that waited finds what the other one made.
    if done.exists() && !again {
        if !content_addressed(package) {
            return Ok(Begun::Built);
        }
        let address = fs::read_to_string(&done).map_err(with_path(&done))?;
        // An empty record means the build stopped before the package was moved.
        if !address.is_empty() {
            remember_outputs(built, package, &address);
            return Ok(Begun::Built);
        }
    }
    let build_dir = lab.work.join("build").join(out.hash());
    fresh(&build_dir)?;
    fresh(&lab.on_host(&out))?;
    // A build under the same path may make different files, which refer to other paths.
    forget_references(lab, &out)?;
    let src = build_dir.join("src");
    fs::create_dir_all(&src).map_err(with_path(&src))?;
    for (source, path, _) in &fetched {
        let target = match &source.into {
            Some(into) => src.join(into),
            None => src.clone(),
        };
        if source.file {
            let parent = target.parent().unwrap_or(&src);
            fs::create_dir_all(parent).map_err(with_path(parent))?;
            fs::copy(path, &target).map_err(with_path(path))?;
            continue;
        }
        unpack(path, &target)?;
    }
    for name in &package.files {
        let from = repository.join(&package.origin.dir).join(name);
        let to = src.join(name);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(with_path(parent))?;
        }
        fs::copy(&from, &to).map_err(with_path(&from))?;
    }

    let view_dir = lab.work.join("views").join(out.hash());
    remove(&view_dir)?;
    let shown: Vec<(&Dep, Vec<StorePath>)> = deps
        .iter()
        .filter(|dep| dep.view)
        .map(|dep| (dep, built.output_paths(&dep.id)))
        .collect();
    let in_view: Vec<(&str, &StorePath, &[String])> = shown
        .iter()
        .flat_map(|(dep, paths)| {
            paths
                .iter()
                .map(|path| (dep.id.as_str(), path, dep.skip.as_slice()))
        })
        .collect();
    view::compose_skipping(lab, &view_dir, &in_view)?;

    let usr_prefix = package.scope != Scope::Runtime;
    let inside = out.temporary().inside();
    let (prefix, dest) = if usr_prefix {
        ("/usr".to_owned(), inside.clone())
    } else {
        (inside.clone(), String::new())
    };
    let real_bin = "/usr/bin";

    let mut rpath_dirs = vec![format!("{inside}/lib")];
    let mut interpreter = format!("{}/{LOADER}", seed.inside());
    for dep in deps.iter().filter(|dep| dep.rpath) {
        let path = built.library_path(&dep.id);
        let host = lab.on_host(path);
        if dep.skip.iter().any(|skipped| skipped == "lib") {
            continue;
        }
        if host.join("lib").is_dir() {
            rpath_dirs.push(format!("{}/lib", path.inside()));
        }
        if dep.id == "seed" {
            rpath_dirs.push(format!("{}/{SEED_GCC_LIBS}", path.inside()));
        }
        if host.join(LOADER).exists() {
            interpreter = format!("{}/{LOADER}", path.inside());
        }
    }

    let mut env = BTreeMap::from([
        ("OUT".to_owned(), inside),
        ("PREFIX".to_owned(), prefix),
        ("DEST".to_owned(), dest),
        ("NPROC".to_owned(), Identity::current().jobs.to_string()),
        ("WRATH_ARM".to_owned(), ARM.to_owned()),
        ("WRATH_SEED".to_owned(), seed.inside()),
        ("WRATH_REAL_BIN".to_owned(), real_bin.to_owned()),
        ("WRATH_ARCH".to_owned(), ARCH.to_owned()),
        ("WRATH_OS".to_owned(), OS.to_owned()),
        ("WRATH_TRIPLE".to_owned(), TRIPLE.to_owned()),
        (
            "WRATH_ARCH_NAME".to_owned(),
            package.arch_name()?.to_owned(),
        ),
        ("WRATH_LOADER_NAME".to_owned(), loader_name().to_owned()),
    ]);
    for dep in &deps {
        let name = dep.id.replace('-', "_");
        env.insert(format!("WRATH_DEP_{name}"), built.paths[&dep.id].inside());
        env.insert(
            format!("WRATH_LIB_{name}"),
            built.library_path(&dep.id).inside(),
        );
    }
    env.extend(searched);
    // The loader programs built here run with, for recipes that set one themselves, such as for
    // programs a project built elsewhere.
    env.insert("WRATH_LOADER".to_owned(), interpreter.clone());
    let path_var = if package.wrap {
        let wrapper = built
            .paths
            .get("wrapper")
            .ok_or("nothing built the wrapper")?;
        env.insert(
            "WRATH_LINK_FLAGS".to_owned(),
            link_flags(&interpreter, &rpath_dirs),
        );
        if let Some(gcc) = deps.iter().find(|dep| dep.id == "gcc") {
            env.insert(
                "WRATH_COMPILE_FLAGS".to_owned(),
                compile_flags(&built.paths[&gcc.id]),
            );
        }
        format!("{}/bin:{real_bin}", wrapper.inside())
    } else {
        real_bin.to_owned()
    };
    env.insert("PATH".to_owned(), path_var);

    let script = format!(
        "cd /build/src\n{}\ncd /build\n\
         if [ -d \"$OUT/usr\" ]; then cp -a \"$OUT/usr/.\" \"$OUT/\"; rm -rf \"$OUT/usr\"; fi\n",
        package.script,
    );

    let limits = match &package.silent_for {
        Some(text) => Limits {
            silence: duration(text).map_err(|e| format!("`{}`: silent_for: {e}", package.id))?,
            ..Limits::DEFAULT
        },
        None => Limits::DEFAULT,
    };
    let log = logs_dir(lab).join(format!("{}.log", package.id));
    let view = View { dir: view_dir };
    let step = Step {
        name: package.id.clone(),
        argv: vec!["/bin/sh".to_owned(), "-euc".to_owned(), script],
        inputs: closure.iter().cloned().collect(),
        output: Some(out.clone()),
        view: Some(view.clone()),
        build_dir: build_dir.clone(),
        log,
        env,
        limits,
        links: Vec::new(),
        at: Some((out.clone(), out.temporary())),
    };
    Ok(Begun::Run(Box::new(Pending {
        step,
        out,
        closure,
        view,
        build_dir,
        done: (!again).then_some(done),
        dep_paths: deps
            .iter()
            .map(|dep| built.paths[&dep.id].clone())
            .collect(),
        lock,
    })))
}

/// A build worked out and ready for its sandbox, which needs nothing shared to run.
pub(crate) struct Pending {
    pub(crate) step: Step,
    out: StorePath,
    closure: BTreeSet<StorePath>,
    view: View,
    build_dir: PathBuf,
    /// Its record, written once it's built, unless it's built again only to be looked at.
    done: Option<PathBuf>,
    dep_paths: Vec<StorePath>,
    lock: fs::File,
}

/// What working out a build found: that it's built already, or what to run.
pub(crate) enum Begun {
    Built,
    Run(Box<Pending>),
}

/// Builds `package`: works it out, runs its sandbox, and checks what it made.
fn build_package(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
    again: bool,
) -> Result<Option<Prepared>, String> {
    match begin(lab, repository, package, seed, sources, built, again)? {
        Begun::Built => Ok(None),
        Begun::Run(pending) => {
            let outcome = sandbox::run_shown(lab, &pending.step, Some(("build", "built")))?;
            end(lab, package, *pending, &outcome, built).map(Some)
        }
    }
}

/// Checks what the sandbox of `pending` made, and readies it to be placed.
pub(crate) fn end(
    lab: &Lab,
    package: &Package,
    pending: Pending,
    outcome: &sandbox::Outcome,
    built: &Built,
) -> Result<Prepared, String> {
    let Pending {
        step,
        out,
        closure,
        view,
        build_dir,
        done,
        dep_paths,
        lock,
    } = pending;
    if outcome.status != Status::Passed {
        let hint = fs::read_to_string(&step.log)
            .ok()
            .and_then(|text| missing_in(&text))
            .map(|missing| format!("\n  hint: {}", failure_hint(lab, built, &missing)))
            .unwrap_or_default();
        return Err(format!(
            "`{}` {:?} after {} s, see {}{hint}",
            package.id,
            outcome.status,
            outcome.milliseconds / 1000,
            step.log.display()
        ));
    }
    settle_modes(&lab.on_host(&out)).map_err(|e| format!("`{}` installed {e}", package.id))?;
    if let Some((file, library)) = linked_by_path(&lab.on_host(&out))? {
        return Err(format!(
            "`{}`'s {} needs `{library}` by its path, which only the build has: the library has \
             no SONAME, so its recipe should give it one",
            package.id,
            file.display()
        ));
    }
    write_runtime_deps(lab, package, &out, built)?;
    if let Some(done) = done {
        if let Some(dir) = done.parent() {
            fs::create_dir_all(dir).map_err(with_path(dir))?;
        }
        fs::write(&done, "").map_err(with_path(&done))?;
    }
    Ok(Prepared {
        out,
        closure,
        view,
        env: step.env,
        limits: step.limits,
        build_dir,
        dep_paths,
        _lock: lock,
    })
}

/// The lock on building `out`, waiting for another build of it to finish first. Two builds of one
/// key would share a build directory and a temporary path, and wreck each other.
fn build_lock(lab: &Lab, out: &StorePath, id: &str) -> Result<fs::File, String> {
    let dir = lab.work.join("locks");
    fs::create_dir_all(&dir).map_err(with_path(&dir))?;
    let path = dir.join(out.hash());
    let lock = fs::File::create(&path).map_err(with_path(&path))?;
    if lock.try_lock().is_err() {
        eprintln!("  wait   {id}, which another build is building");
        lock.lock().map_err(with_path(&path))?;
    }
    Ok(lock)
}

/// What the compiler wrapper adds to every compile with the store's `gcc`: its own headers' paths
/// in `__FILE__`, such as libstdc++'s in its assertion messages, read as `/usr/include`, so a
/// program doesn't refer to the whole compiler.
fn compile_flags(gcc: &StorePath) -> String {
    format!("-fmacro-prefix-map={}/include=/usr/include", gcc.inside())
}

/// What the compiler wrapper adds when it links: the interpreter and run paths a program needs to
/// run from the store, and relocations made read-only once they're resolved at start (`relro`,
/// `now`), so they can't be overwritten later.
fn link_flags(interpreter: &str, rpath_dirs: &[String]) -> String {
    let mut flags = vec![
        format!("-Wl,--dynamic-linker={interpreter}"),
        "-Wl,-z,relro".to_owned(),
        "-Wl,-z,now".to_owned(),
    ];
    flags.extend(rpath_dirs.iter().map(|dir| format!("-Wl,-rpath,{dir}")));
    flags.join(" ")
}

/// Where the store paths a store path refers to are recorded, beside the store (decision 0021).
fn references_record(lab: &Lab, path: &StorePath) -> PathBuf {
    lab.work.join("refs").join(path.hash())
}

/// Forgets what `path` refers to, for a path about to be built again.
fn forget_references(lab: &Lab, path: &StorePath) -> Result<(), String> {
    let record = references_record(lab, path);
    match fs::remove_file(&record) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(with_path(&record)(error))
        }
        _ => Ok(()),
    }
}

/// The store paths `path` refers to, other than itself. A store path never changes, so they're
/// recorded the first time they're found and read from the record afterwards.
pub fn references_of(lab: &Lab, path: &StorePath) -> Result<Vec<StorePath>, String> {
    let record = references_record(lab, path);
    if let Ok(text) = fs::read_to_string(&record) {
        return Ok(text.lines().map(StorePath::from_hash).collect());
    }
    let found: Vec<String> = references(&lab.on_host(path))?
        .into_keys()
        .filter(|hash| hash != path.hash())
        .collect();
    let text: String = found.iter().map(|hash| format!("{hash}\n")).collect();
    // A record that can't be written is only a scan to do again next time.
    let temporary = record.with_extension(format!("tmp{}", std::process::id()));
    if let Some(parent) = record.parent()
        && fs::create_dir_all(parent).is_ok()
        && fs::write(&temporary, text).is_ok()
    {
        let _ = fs::rename(&temporary, &record);
    }
    Ok(found
        .iter()
        .map(|hash| StorePath::from_hash(hash))
        .collect())
}

/// The store paths `dir` refers to, with one file referring to each.
pub fn references(dir: &Path) -> Result<BTreeMap<String, PathBuf>, String> {
    let mut found = BTreeMap::new();
    let finder = memmem::Finder::new(b"/store/");
    scan(dir, dir, &finder, &mut found)?;
    Ok(found)
}

fn note(
    bytes: &[u8],
    finder: &memmem::Finder<'_>,
    file: &Path,
    found: &mut BTreeMap<String, PathBuf>,
) {
    for at in finder.find_iter(bytes) {
        let start = at + "/store/".len();
        if let Some(length) = store::entry_length(&bytes[start..]) {
            let entry = String::from_utf8_lossy(&bytes[start..start + length]).into_owned();
            found.entry(entry).or_insert_with(|| file.to_path_buf());
        }
    }
}

fn scan(
    root: &Path,
    dir: &Path,
    finder: &memmem::Finder<'_>,
    found: &mut BTreeMap<String, PathBuf>,
) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(with_path(dir))? {
        let entry = entry.map_err(with_path(dir))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(with_path(&path))?;
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if kind.is_dir() {
            scan(root, &path, finder, found)?;
        } else if kind.is_symlink() {
            let target = fs::read_link(&path).map_err(with_path(&path))?;
            note(
                target.as_os_str().as_encoded_bytes(),
                finder,
                relative,
                found,
            );
        } else {
            let bytes = fs::read(&path).map_err(with_path(&path))?;
            note(&bytes, finder, relative, found);
        }
    }
    Ok(())
}

/// Everything `start` needs at run time, going by the paths its files mention.
pub fn runtime_closure(lab: &Lab, start: &StorePath) -> Result<BTreeSet<StorePath>, String> {
    let mut seen = BTreeSet::new();
    let mut queue = vec![start.clone()];
    while let Some(path) = queue.pop() {
        if !seen.insert(path.clone()) {
            continue;
        }
        for next in references_of(lab, &path)? {
            if lab.on_host(&next).exists() && !seen.contains(&next) {
                queue.push(next);
            }
        }
    }
    Ok(seen)
}

/// Runs `program` of `package` with only what it refers to in the sandbox: no view and no `/usr`.
pub fn run_alone(
    lab: &Lab,
    package: &StorePath,
    program: &str,
    name: &str,
) -> Result<(Status, String), String> {
    let dir = lab.work.join("alone").join(name);
    fresh(&dir)?;
    let log = dir.join("log");
    let outcome = sandbox::run(
        lab,
        &Step {
            name: name.to_owned(),
            argv: vec![format!("{}/{program}", package.inside())],
            inputs: runtime_closure(lab, package)?.into_iter().collect(),
            output: None,
            view: None,
            build_dir: dir.join("build"),
            log: log.clone(),
            env: BTreeMap::new(),
            limits: Limits::DEFAULT,
            links: Vec::new(),
            at: None,
        },
    )?;
    let text = fs::read_to_string(&log).map_err(with_path(&log))?;
    Ok((outcome.status, text))
}

/// A duration written as a number and a unit: `90s`, `45m` or `3h`.
fn duration(text: &str) -> Result<std::time::Duration, String> {
    let invalid = || format!("`{text}` isn't a duration such as `45m` or `3h`");
    let (number, seconds) = [("s", 1), ("m", 60), ("h", 60 * 60)]
        .into_iter()
        .find_map(|(unit, seconds)| Some((text.strip_suffix(unit)?, seconds)))
        .ok_or_else(invalid)?;
    let count: u64 = number.parse().map_err(|_| invalid())?;
    Ok(std::time::Duration::from_secs(count * seconds))
}

#[cfg(test)]
mod requirement_tests {
    use super::*;

    fn consumer(specs: &str, scope: &str) -> Package {
        let text = format!(
            "[[package]]\nid = \"app\"\nscope = \"{scope}\"\nscript = \"\"\n\
             extra_deps = [{specs}]\n"
        );
        toml::from_str::<Recipes>(&text).unwrap().package.remove(0)
    }

    /// gtk brings pango publicly and zlib privately. Nothing is read from disk: what each
    /// brings is already worked out.
    fn built() -> Built {
        let mut built = Built::default();
        for (tag, id) in ["gtk", "pango", "zlib", "cairo"].into_iter().enumerate() {
            let tag = u8::try_from(tag).unwrap();
            built
                .paths
                .insert(id.to_owned(), StorePath::derive("test", &[&[tag]]));
        }
        built.requirements.insert(
            "gtk".to_owned(),
            BTreeMap::from([("pango".to_owned(), false), ("zlib".to_owned(), true)]),
        );
        built
    }

    fn seen(specs: &str) -> Result<Vec<(String, bool)>, String> {
        seen_as(specs, "runtime")
    }

    fn seen_as(specs: &str, scope: &str) -> Result<Vec<(String, bool)>, String> {
        let package = consumer(specs, scope);
        let lab = Lab::new("/nonexistent");
        let deps = with_requirements(&lab, &package, &package.all_deps()?, &mut built())?;
        Ok(deps.into_iter().map(|dep| (dep.id, dep.view)).collect())
    }

    fn ids(seen: &[(String, bool)]) -> Vec<&str> {
        seen.iter().map(|(id, _)| id.as_str()).collect()
    }

    #[test]
    fn a_dependency_brings_all_it_requires_by_default() {
        assert_eq!(ids(&seen("\"gtk\"").unwrap()), ["gtk", "pango", "zlib"]);
    }

    #[test]
    fn public_leaves_private_requirements_out_and_none_brings_nothing() {
        let public = seen(r#"{ id = "gtk", propagate = "public" }"#).unwrap();
        assert_eq!(ids(&public), ["gtk", "pango"]);
        let none = seen(r#"{ id = "gtk", propagate = "none" }"#).unwrap();
        assert_eq!(ids(&none), ["gtk"]);
    }

    #[test]
    fn an_exclusion_leaves_one_out_and_must_name_something_brought() {
        let excluded = seen(r#"{ id = "gtk", exclude = ["zlib"] }"#).unwrap();
        assert_eq!(ids(&excluded), ["gtk", "pango"]);
        assert_eq!(
            seen(r#"{ id = "gtk", exclude = ["cairo"] }"#).unwrap_err(),
            "`app` excludes `cairo` from `gtk`, which doesn't bring it"
        );
    }

    #[test]
    fn what_a_recipe_lists_itself_keeps_how_it_lists_it() {
        let listed = seen(r#"{ id = "zlib", view = false }, "gtk""#).unwrap();
        assert_eq!(
            listed,
            [
                ("zlib".to_owned(), false),
                ("gtk".to_owned(), true),
                ("pango".to_owned(), true)
            ]
        );
    }

    #[test]
    fn what_a_package_needs_only_at_run_time_is_a_dependency_outside_its_view() {
        let text = "[[package]]\nid = \"app\"\nscope = \"runtime\"\nscript = \"\"\n\
                    extra_deps = [\"gtk\"]\nruntime_deps = [\"cairo\"]\n";
        let package = toml::from_str::<Recipes>(text).unwrap().package.remove(0);
        let deps = package.all_deps().unwrap();
        let cairo = deps.iter().find(|dep| dep.id == "cairo").unwrap();
        assert!(!cairo.view && !cairo.rpath);
        let lab = Lab::new("/nonexistent");
        let seen = with_requirements(&lab, &package, &deps, &mut built()).unwrap();
        let ids: Vec<&str> = seen.iter().map(|dep| dep.id.as_str()).collect();
        assert_eq!(ids, ["gtk", "cairo", "pango", "zlib"]);
    }

    #[test]
    fn a_boot_package_sees_only_what_it_lists() {
        assert_eq!(ids(&seen_as("\"gtk\"", "boot").unwrap()), ["gtk"]);
    }

    #[test]
    fn a_listed_dependency_another_one_brings_is_redundant_unless_written_as_a_table() {
        let lab = Lab::new("/nonexistent");
        let packages = [
            consumer(r#""gtk", "pango", "zlib", "cairo""#, "runtime"),
            consumer(
                r#"{ id = "gtk", propagate = "public" }, "pango", "zlib""#,
                "runtime",
            ),
            consumer(r#""gtk", { id = "pango", rpath = false }"#, "runtime"),
        ];
        let found = redundant_deps(&lab, &mut built(), &packages).unwrap();
        let pairs = |deps: &[Redundant]| -> Vec<String> {
            deps.iter().map(|(dep, by)| format!("{dep}<{by}")).collect()
        };
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(pairs(&found[0].1), ["pango<gtk", "zlib<gtk"]);
        assert_eq!(pairs(&found[1].1), ["pango<gtk"], "zlib is private to gtk");
    }

    #[test]
    fn compiles_name_the_compilers_headers_without_its_store_path() {
        let gcc = StorePath::from_hash("abc").named("gcc-15.2.0");
        assert_eq!(
            compile_flags(&gcc),
            "-fmacro-prefix-map=/store/abc-gcc-15.2.0/include=/usr/include"
        );
    }

    #[test]
    fn links_are_hardened_and_run_from_the_store() {
        let flags = link_flags("/store/s/ld.so", &["/store/a/lib".to_owned()]);
        assert_eq!(
            flags,
            "-Wl,--dynamic-linker=/store/s/ld.so -Wl,-z,relro -Wl,-z,now -Wl,-rpath,/store/a/lib"
        );
    }

    #[test]
    fn a_packages_scope_and_the_target_are_in_its_key() {
        let parts = |scope: &str| {
            recipe_parts(Path::new("/nonexistent"), &consumer("", scope), &[]).unwrap()
        };
        assert_ne!(parts("runtime"), parts("tool"));
        assert!(parts("runtime").contains(&TARGET.as_bytes().to_vec()));
    }

    #[test]
    fn a_package_naming_itself_with_its_name_cut_apart_stays_whole_and_is_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let package = consumer("", "runtime");
        let out = StorePath::derive("key", &[b"cut"]).named("app");
        let seen = out.temporary();
        let path = format!("{}/lib", seen.inside());
        let (head, tail) = path.split_at(path.len() - 8);
        let cut = format!("{head}{}{tail}", &head[head.len() - 1..]);
        let root = lab.on_host(&out);
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::create_dir_all(root.join("include")).unwrap();
        fs::write(root.join("lib/libapp.so"), &cut).unwrap();
        fs::write(root.join("include/app.h"), "header\n").unwrap();

        let (outputs, whole) = crate::fixup::place_outputs(&lab, &package, &out).unwrap();

        assert_eq!(outputs.len(), 1, "{outputs:?}");
        assert!(whole.unwrap().starts_with("lib/libapp.so names its path"));
        let placed = fs::read(
            lab.on_host(&outputs[&split::Class::Out])
                .join("lib/libapp.so"),
        );
        let digest = |path: &StorePath| path.hash()[..crate::store::HASH_LENGTH].to_owned();
        let placed = String::from_utf8(placed.unwrap()).unwrap();
        assert!(!placed.contains(&digest(&seen)), "{placed}");
        assert!(
            placed.contains(&digest(&outputs[&split::Class::Out])),
            "{placed}"
        );
    }

    /// A recipe from `extra` added to a plain one with a source, as its key's recipe parts.
    fn parts_with(extra: &str) -> Vec<Vec<u8>> {
        let text = format!(
            "[[package]]\nid = \"app\"\nscope = \"runtime\"\nscript = \"make\"\n\
             sources = [{{ url = \"https://example.org/app.tar.xz\" }}]\n{extra}"
        );
        let package = toml::from_str::<Recipes>(&text).unwrap().package.remove(0);
        recipe_parts(Path::new("/nonexistent"), &package, &["abc123"]).unwrap()
    }

    #[test]
    fn what_changes_a_build_changes_its_key() {
        let plain = parts_with("");
        let changed = |extra: &str| parts_with(extra) != plain;
        assert!(changed("version = \"2\"\n"));
        assert!(changed("whole = true\n"));
        assert!(changed("wrap = false\n"));
        assert!(changed("propagates = [\"zlib\"]\n"));
        // Where the source goes, and whether it's unpacked.
        let text = |source: &str| {
            format!(
                "[[package]]\nid = \"app\"\nscope = \"runtime\"\nscript = \"make\"\n\
                 sources = [{source}]\n"
            )
        };
        let source_parts = |source: &str| {
            let package = toml::from_str::<Recipes>(&text(source))
                .unwrap()
                .package
                .remove(0);
            recipe_parts(Path::new("/nonexistent"), &package, &["abc123"]).unwrap()
        };
        let unpacked = source_parts("{ url = \"https://example.org/a.tar.xz\" }");
        let into = source_parts("{ url = \"https://example.org/a.tar.xz\", into = \"sub\" }");
        let file = source_parts("{ url = \"https://example.org/a.tar.xz\", file = true }");
        assert_ne!(unpacked, into);
        assert_ne!(unpacked, file);
        assert_ne!(into, file);
    }

    #[test]
    fn what_doesnt_change_a_build_leaves_its_key() {
        let plain = parts_with("");
        for extra in [
            "smoke = [\"bin/app --version\"]\n",
            "track = \"2.\"\n",
            "silent_for = \"3h\"\n",
            "check = \"make check\"\n",
        ] {
            assert_eq!(parts_with(extra), plain, "{extra}");
        }
    }

    /// Pinned so a change to what goes into a key can't slip through and rebuild everything. A
    /// bump of `BUILD_RULES` changes them on purpose, and they're updated with it.
    #[test]
    fn keys_stay_what_they_were() {
        let key = |scope: &str| {
            let text = format!(
                "[[package]]\nid = \"app\"\nversion = \"1.2.3\"\nscope = \"{scope}\"\n\
                 script = \"make install\\n\"\n\
                 extra_deps = [\"gtk\", {{ id = \"zlib\", rpath = false }}]\n\
                 sources = [{{ url = \"https://example.org/app.tar.xz\", into = \"sub\" }}]\n"
            );
            let package = toml::from_str::<Recipes>(&text).unwrap().package.remove(0);
            let recipe = recipe_parts(Path::new("/nonexistent"), &package, &["abc123"]).unwrap();
            let deps = package.all_deps().unwrap();
            package_key("build", &recipe, &deps, |dep| format!("ref-{}", dep.id))
                .hash()
                .to_owned()
        };
        assert_eq!(key("runtime"), "1f23acc873d6c00cb410e3d9c50587fe5e186413");
        assert_eq!(key("boot"), "0fa83efc38227e8fb500e51fbde2d98e282ff297");
    }

    #[test]
    fn only_a_choice_other_than_the_default_changes_a_key() {
        let key = |specs: &str| {
            let deps = consumer(specs, "runtime").all_deps().unwrap();
            package_key("test", &[], &deps, |dep| dep.id.clone())
        };
        let plain = key("\"gtk\"");
        assert_eq!(
            key(r#"{ id = "gtk", propagate = "all", exclude = [] }"#),
            plain
        );
        assert_ne!(key(r#"{ id = "gtk", propagate = "public" }"#), plain);
        assert_ne!(key(r#"{ id = "gtk", exclude = ["zlib"] }"#), plain);
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;

    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let tree = dir.path().join("project");
        fs::create_dir_all(tree.join("src")).unwrap();
        fs::create_dir_all(tree.join("target")).unwrap();
        fs::write(tree.join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(tree.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(tree.join("target/junk"), "built\n").unwrap();
        fs::create_dir_all(dir.path().join("build")).unwrap();
        dir
    }

    fn fetch(dir: &Path, include: &[&str]) -> (PathBuf, String) {
        let repository = dir.join("build");
        let lab = Lab::new(repository.join("work"));
        let mut sources = Sources::load(&repository).unwrap();
        let include: Vec<String> = include.iter().map(|s| (*s).to_owned()).collect();
        sources.fetch_tree(&lab, "../project", &include).unwrap()
    }

    fn listing(tar: &Path) -> String {
        let output = Command::new("tar").arg("-tf").arg(tar).output().unwrap();
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn a_tree_packs_the_same_twice() {
        let dir = repository();
        let (first, hash) = fetch(dir.path(), &["Cargo.toml", "src"]);
        fs::remove_file(&first).unwrap();
        // Times change between checkouts and mustn't change the source.
        let main = dir.path().join("project/src/main.rs");
        Command::new("touch")
            .args(["-d", "2001-01-01"])
            .arg(&main)
            .status()
            .unwrap();
        assert_eq!(fetch(dir.path(), &["Cargo.toml", "src"]).1, hash);
    }

    #[test]
    fn a_changed_file_changes_the_tree() {
        let dir = repository();
        let (_, before) = fetch(dir.path(), &["Cargo.toml", "src"]);
        fs::write(dir.path().join("project/src/main.rs"), "fn main() { }\n").unwrap();
        assert_ne!(fetch(dir.path(), &["Cargo.toml", "src"]).1, before);
    }

    #[test]
    fn a_tree_has_only_what_it_includes_under_one_top_directory() {
        let dir = repository();
        let (tar, _) = fetch(dir.path(), &["Cargo.toml", "src"]);
        assert_eq!(
            listing(&tar),
            "tree/Cargo.toml\ntree/src/\ntree/src/main.rs\n"
        );
    }

    #[test]
    fn a_tree_never_takes_what_was_built_in_it() {
        let dir = repository();
        fs::create_dir_all(dir.path().join("project/src/target")).unwrap();
        fs::write(dir.path().join("project/src/target/x"), "built\n").unwrap();
        let (tar, _) = fetch(dir.path(), &["src"]);
        assert!(!listing(&tar).contains("target"), "{}", listing(&tar));
    }

    #[test]
    fn a_missing_include_is_an_error() {
        let dir = repository();
        let repository = dir.path().join("build");
        let lab = Lab::new(repository.join("work"));
        let mut sources = Sources::load(&repository).unwrap();
        let error = sources
            .fetch_tree(&lab, "../project", &["nothing".to_owned()])
            .unwrap_err();
        assert!(error.contains("nothing"), "{error}");
    }
}

#[cfg(test)]
mod crate_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn two_builds_of_one_key_take_turns() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let out = StorePath::derive("build", &[b"app"]);
        let held = build_lock(&lab, &out, "app").unwrap();
        let other = fs::File::open(dir.path().join("locks").join(out.hash())).unwrap();
        assert!(other.try_lock().is_err());
        drop(held);
        // A command another test starts can hold the lock's file for a moment, between forking
        // and running its program, so the lock frees soon after, not at once.
        let freed = (0..200).any(|_| {
            other.try_lock().is_ok() || {
                std::thread::sleep(std::time::Duration::from_millis(5));
                false
            }
        });
        assert!(freed);
    }

    #[test]
    fn a_source_goes_inside_the_source_tree() {
        let with_into = |into: &str| {
            let text = format!(
                "[[package]]\nid = \"app\"\nscope = \"runtime\"\nscript = \"\"\n\
                 sources = [{{ url = \"x\", into = \"{into}\" }}]\n"
            );
            toml::from_str::<Recipes>(&text).unwrap().package.remove(0)
        };
        assert!(with_into("pdf-poppler").check_sources().is_ok());
        assert!(with_into("3rdparty/glad").check_sources().is_ok());
        let error = with_into("../elsewhere").check_sources().unwrap_err();
        assert!(error.contains("outside its source tree"), "{error}");
        assert!(with_into("/etc").check_sources().is_err());
    }

    #[test]
    fn every_recipe_in_the_repository_loads() {
        let repository = grapes_checkout();
        let files = crate::chain::recipe_files(&repository).unwrap();
        assert!(files.len() > 10, "{files:?}");
        for file in files {
            if let Err(e) = load(&repository, &file) {
                panic!("{file}: {e}");
            }
        }
    }

    #[test]
    fn a_library_needed_by_a_path_outside_the_store_is_one_a_machine_lacks() {
        assert!(crate::fixup::outside_store("/usr/lib/libsqlite3.so"));
        assert!(!crate::fixup::outside_store("libsqlite3.so.0"));
        assert!(!crate::fixup::outside_store(
            "/store/abc-sqlite/lib/libsqlite3.so.0"
        ));
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("script"), "#!/bin/sh\n").unwrap();
        assert_eq!(linked_by_path(dir.path()).unwrap(), None);
    }

    #[test]
    fn a_built_package_loses_setuid_and_setgid_and_cant_hold_special_files() {
        let dir = tempfile::tempdir().unwrap();
        let (bin, shared) = (dir.path().join("bin"), dir.path().join("share/games"));
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&shared).unwrap();
        fs::write(bin.join("sudo"), "").unwrap();
        fs::set_permissions(bin.join("sudo"), fs::Permissions::from_mode(0o4755)).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o2775)).unwrap();
        std::os::unix::fs::symlink("sudo", bin.join("su")).unwrap();
        settle_modes(dir.path()).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode(&bin.join("sudo")), 0o755);
        assert_eq!(mode(&shared), 0o775);

        let status = Command::new("mkfifo")
            .arg(bin.join("pipe"))
            .status()
            .unwrap();
        assert!(status.success());
        let error = settle_modes(dir.path()).unwrap_err();
        assert!(
            error.contains("bin/pipe") && error.contains("only files"),
            "{error}"
        );
    }

    /// A source archive with a lock in its top directory, as a release tarball has.
    fn archive(dir: &Path, files: &[(&str, &str)]) -> PathBuf {
        let top = dir.join("project-1.0");
        fs::create_dir_all(&top).unwrap();
        for (name, text) in files {
            fs::write(top.join(name), text).unwrap();
        }
        let archive = dir.join("project-1.0.tar");
        let status = Command::new("tar")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(dir)
            .arg("project-1.0")
            .status()
            .unwrap();
        assert!(status.success());
        archive
    }

    #[test]
    fn the_names_at_the_top_of_a_source_are_listed_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let archive = archive(dir.path(), &[("meson.build", ""), ("Cargo.lock", "")]);
        assert_eq!(top_files(&archive).unwrap(), ["Cargo.lock", "meson.build"]);
        fs::write(&archive, "not an archive any more").unwrap();
        assert_eq!(top_files(&archive).unwrap(), ["Cargo.lock", "meson.build"]);
    }

    #[test]
    fn a_lock_read_from_a_source_once_is_read_again_without_unpacking_it() {
        let dir = tempfile::tempdir().unwrap();
        let go_sum = "filippo.io/edwards25519 v1.1.0/go.mod h1:BxyFTGdWcka3PhytdK4V28tE5=\n";
        let archive = archive(dir.path(), &[("go.sum", go_sum)]);
        assert_eq!(read_in(&archive, "go.sum").unwrap(), go_sum);
        // Sources are kept by their hash, so what was read from one stays true of it.
        fs::write(&archive, "not an archive any more").unwrap();
        assert_eq!(read_in(&archive, "go.sum").unwrap(), go_sum);
        assert!(read_in(&archive, "go.mod").is_err());
    }

    #[test]
    fn a_file_a_source_lacks_is_known_missing_without_unpacking_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let archive = archive(dir.path(), &[("meson_options.txt", "")]);
        let error = read_in(&archive, "meson.options").unwrap_err();
        assert!(error.contains("has no `meson.options`"), "{error}");
        fs::write(&archive, "not an archive any more").unwrap();
        assert_eq!(read_in(&archive, "meson.options").unwrap_err(), error);
    }

    /// An archive of files with no directory around them, as font releases often are.
    fn flat_archive(dir: &Path, names: &[&str]) -> PathBuf {
        let files = dir.join("flat");
        fs::create_dir_all(&files).unwrap();
        for name in names {
            fs::write(files.join(name), name).unwrap();
        }
        let archive = dir.join("flat.tar");
        let status = Command::new("tar")
            .arg("-cf")
            .arg(&archive)
            .arg("-C")
            .arg(&files)
            .args(names)
            .status()
            .unwrap();
        assert!(status.success());
        archive
    }

    #[test]
    fn a_source_loses_its_top_directory_when_unpacked() {
        let dir = tempfile::tempdir().unwrap();
        let archive = archive(dir.path(), &[("meson.build", "project")]);
        let target = dir.path().join("src");
        unpack(&archive, &target).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("meson.build")).unwrap(),
            "project"
        );
    }

    #[test]
    fn a_source_without_a_top_directory_is_unpacked_whole() {
        let dir = tempfile::tempdir().unwrap();
        let archive = flat_archive(dir.path(), &["A.ttf", "B.ttf"]);
        let target = dir.path().join("src");
        unpack(&archive, &target).unwrap();
        assert_eq!(fs::read_to_string(target.join("A.ttf")).unwrap(), "A.ttf");
        assert_eq!(fs::read_to_string(target.join("B.ttf")).unwrap(), "B.ttf");
    }

    #[test]
    fn the_names_at_the_top_of_a_source_without_a_top_directory_are_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let archive = flat_archive(dir.path(), &["README.md", "A.ttf"]);
        assert_eq!(top_files(&archive).unwrap(), ["A.ttf", "README.md"]);
        assert_eq!(read_in(&archive, "README.md").unwrap(), "README.md");
    }

    #[test]
    fn a_file_an_archive_holds_twice_is_read_as_unpacking_leaves_it() {
        let dir = tempfile::tempdir().unwrap();
        let archive = archive(dir.path(), &[("Cargo.lock", "version = 3\n")]);
        // As fish's release has it: a newer lock appended to the archive.
        fs::write(dir.path().join("project-1.0/Cargo.lock"), "version = 4\n").unwrap();
        let status = Command::new("tar")
            .arg("-rf")
            .arg(&archive)
            .arg("-C")
            .arg(dir.path())
            .arg("project-1.0/Cargo.lock")
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            unpack_one(&archive, "Cargo.lock").unwrap().unwrap(),
            "version = 4\n"
        );
        let target = dir.path().join("src");
        unpack(&archive, &target).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("Cargo.lock")).unwrap(),
            "version = 4\n"
        );
    }

    #[test]
    fn the_top_directory_is_found_however_the_archive_lists_it() {
        assert_eq!(top_directory("x-1/\nx-1/a\nx-1/b/c\n"), Some("x-1"));
        // As OpenSSH's release lists it: the directory without its slash.
        assert_eq!(top_directory("x-1\nx-1/a\n"), Some("x-1"));
        assert_eq!(top_directory("x-1/a\nx-1/b\n"), Some("x-1"));
        assert_eq!(top_directory("A.ttf\nB.ttf\n"), None);
        assert_eq!(top_directory("only.ttf\n"), None);
        assert_eq!(top_directory("x-1/a\nx-10/b\n"), None);
        assert_eq!(top_directory("x-1/\nREADME\n"), None);
        assert_eq!(top_directory(""), None);
    }

    #[test]
    fn a_source_of_one_file_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let archive = flat_archive(dir.path(), &["only.ttf"]);
        let target = dir.path().join("src");
        unpack(&archive, &target).unwrap();
        assert_eq!(
            fs::read_to_string(target.join("only.ttf")).unwrap(),
            "only.ttf"
        );
    }

    #[test]
    fn a_source_with_a_hash_must_have_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.crate");
        fs::write(&file, "not the crate\n").unwrap();
        let lab = Lab::new(dir.path().join("work"));
        let mut sources = Sources::load(dir.path()).unwrap();
        let source = Source {
            url: format!("file://{}", file.display()),
            into: None,
            include: Vec::new(),
            sha256: Some("00".repeat(32)),
            file: false,
        };
        let error = sources
            .fetch_source(&lab, &source, &Lock::Shared)
            .unwrap_err();
        assert!(error.contains(&"00".repeat(32)), "{error}");
        let right = sha256_of(&file).unwrap();
        let source = Source {
            sha256: Some(right.clone()),
            ..source
        };
        assert_eq!(
            sources
                .fetch_source(&lab, &source, &Lock::Shared)
                .unwrap()
                .1,
            right
        );
        // Hashes the recipe carries stay out of `sources.lock`.
        assert!(!dir.path().join("sources.lock").exists());
    }

    #[test]
    fn a_known_hash_is_taken_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path().join("work"));
        let mut sources = Sources::load(dir.path()).unwrap();
        let gone = |name: &str| Source {
            url: format!("file://{}/{name}", dir.path().display()),
            into: None,
            include: Vec::new(),
            sha256: None,
            file: false,
        };
        let carried = Source {
            sha256: Some("ab".repeat(32)),
            ..gone("a.crate")
        };
        assert_eq!(
            sources.hash_of(&lab, &carried, &Lock::Shared).unwrap(),
            "ab".repeat(32)
        );
        let pinned = gone("b.tar.gz");
        sources.pins.insert(pinned.url.clone(), "cd".repeat(32));
        assert_eq!(
            sources.hash_of(&lab, &pinned, &Lock::Shared).unwrap(),
            "cd".repeat(32)
        );
        // Neither known nor there to fetch.
        assert!(
            sources
                .hash_of(&lab, &gone("c.tar.gz"), &Lock::Shared)
                .is_err()
        );
    }

    #[test]
    fn a_git_commit_is_fetched_and_pinned_by_its_tree_but_not_with_local_submodules() {
        let dir = tempfile::tempdir().unwrap();
        let git = |repo: &Path, args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(repo)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "protocol.file.allow=always",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_owned()
        };
        let (inner, repo) = (dir.path().join("inner"), dir.path().join("repo"));
        for path in [&inner, &repo] {
            fs::create_dir_all(path).unwrap();
            git(path, &["init", "-q"]);
        }
        fs::write(inner.join("lib.rs"), "inner\n").unwrap();
        git(&inner, &["add", "."]);
        git(&inner, &["commit", "-qm", "inner"]);
        fs::write(repo.join("Cargo.toml"), "[package]\n").unwrap();
        git(
            &repo,
            &["submodule", "add", "-q", inner.to_str().unwrap(), "inner"],
        );
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "one"]);
        let lab = Lab::new(dir.path().join("work"));
        let mut sources = Sources::load(dir.path()).unwrap();
        // A submodule on this host could pack the host's own repositories into a source.
        let commit = git(&repo, &["rev-parse", "HEAD"]);
        let local = format!("git+file://{}#{commit}", repo.display());
        assert!(
            sources
                .fetch_source(&lab, &source(local), &Lock::Shared)
                .is_err()
        );
        let commit = git(&inner, &["rev-parse", "HEAD"]);
        let url = format!("git+file://{}#{commit}", inner.display());
        let (path, hash) = sources
            .fetch_source(&lab, &source(url.clone()), &Lock::Shared)
            .unwrap();
        let listing = Command::new("tar").arg("-tf").arg(&path).output().unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout);
        assert!(listing.contains("tree/lib.rs"), "{listing}");
        assert!(!listing.contains(".git/"), "{listing}");
        let lock = fs::read_to_string(dir.path().join("sources.lock")).unwrap();
        assert!(lock.contains(&format!("{hash}  {url}")), "{lock}");
        // Pinned and kept, it needs the repository no more.
        fs::remove_dir_all(&inner).unwrap();
        assert_eq!(
            sources
                .fetch_source(&lab, &source(url), &Lock::Shared)
                .unwrap()
                .1,
            hash
        );
        let missing = format!("git+file://{}#{}", repo.display(), "0".repeat(40));
        let error = sources
            .fetch_source(&lab, &source(missing), &Lock::Shared)
            .unwrap_err();
        assert!(error.contains(&"0".repeat(40)), "{error}");
    }

    #[test]
    fn a_git_url_read_as_an_option_or_without_a_commit_hash_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path().join("work"));
        let mut sources = Sources::load(dir.path()).unwrap();
        let ran = dir.path().join("ran");
        let option = format!(
            "git+--upload-pack=touch {}#{}",
            ran.display(),
            "a".repeat(40)
        );
        let error = sources
            .fetch_source(&lab, &source(option), &Lock::Shared)
            .unwrap_err();
        assert!(error.contains("can't start with `-`"), "{error}");
        assert!(!ran.exists());
        for commit in ["main", "--tags", "abc123"] {
            let url = format!("git+https://example.org/repo#{commit}");
            let error = sources
                .fetch_source(&lab, &source(url), &Lock::Shared)
                .unwrap_err();
            assert!(error.contains("names no commit"), "{error}");
        }
    }

    fn source(url: String) -> Source {
        Source {
            url,
            into: None,
            include: Vec::new(),
            sha256: None,
            file: false,
        }
    }

    #[test]
    fn downloads_are_kept_by_their_hash_so_like_named_urls_dont_collide() {
        let dir = tempfile::tempdir().unwrap();
        for (sub, text) in [("one", "first\n"), ("two", "second\n")] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
            fs::write(dir.path().join(sub).join("v1.0.tar.gz"), text).unwrap();
        }
        let lab = Lab::new(dir.path().join("work"));
        let mut sources = Sources::load(dir.path()).unwrap();
        let url = |sub: &str| format!("file://{}/{sub}/v1.0.tar.gz", dir.path().display());
        let (first, first_hash) = sources
            .fetch_source(&lab, &source(url("one")), &Lock::Shared)
            .unwrap();
        let (second, second_hash) = sources
            .fetch_source(&lab, &source(url("two")), &Lock::Shared)
            .unwrap();
        assert_ne!(first_hash, second_hash);
        assert_eq!(fs::read_to_string(&first).unwrap(), "first\n");
        assert_eq!(fs::read_to_string(&second).unwrap(), "second\n");
        assert_eq!(first.file_name().unwrap().to_string_lossy(), first_hash);
    }

    #[test]
    fn a_pinned_download_already_kept_isnt_fetched_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.tar.gz");
        fs::write(&file, "x\n").unwrap();
        let lab = Lab::new(dir.path().join("work"));
        let mut sources = Sources::load(dir.path()).unwrap();
        let url = format!("file://{}", file.display());
        let (_, hash) = sources
            .fetch_source(&lab, &source(url.clone()), &Lock::Shared)
            .unwrap();
        fs::remove_file(&file).unwrap();
        let (kept, again) = sources
            .fetch_source(&lab, &source(url), &Lock::Shared)
            .unwrap();
        assert_eq!(
            (again, fs::read_to_string(kept).unwrap()),
            (hash, "x\n".to_owned())
        );
    }

    #[test]
    fn a_web_page_where_a_source_should_be_is_refused_and_not_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("v1.0.tar.gz");
        fs::write(
            &file,
            "\n<!DOCTYPE html>\n<html><title>Sign in</title></html>\n",
        )
        .unwrap();
        let lab = Lab::new(dir.path().join("work"));
        let mut sources = Sources::load(dir.path()).unwrap();
        let url = format!("file://{}", file.display());
        let error = sources
            .fetch_source(&lab, &source(url.clone()), &Lock::Shared)
            .unwrap_err();
        assert_eq!(
            error,
            format!("{url} gave a web page, such as a sign-in or missing-release page")
        );
        assert!(!sources.pins.contains_key(&url));
        let kept = fs::read_dir(lab.work.join("sources")).unwrap().count();
        assert_eq!(kept, 0);
    }
}

#[cfg(test)]
mod variant_tests {
    use super::*;

    fn recipe() -> Package {
        let text = "[[package]]\nid = \"locales\"\nscope = \"runtime\"\nscript = \"make\\n\"\n";
        toml::from_str::<Recipes>(text).unwrap().package.remove(0)
    }

    /// A recipe that declares the options its script reads.
    fn with_options() -> Package {
        recipe_from_str(
            "[[package]]\nid = \"locales\"\nscope = \"runtime\"\nscript = \"make\\n\"\n\
             [package.options.locales]\ntype = \"list\"\ndefault = [\"C.UTF-8/UTF-8\"]\n\
             doc = \"Locales to build.\"\n\
             [package.options.small]\ntype = \"bool\"\ndefault = false\ndoc = \"Smaller.\"\n",
        )
    }

    #[test]
    fn a_recipe_starts_its_script_with_the_defaults_of_its_options() {
        assert_eq!(
            with_options().script,
            "export OPTION_locales='C.UTF-8/UTF-8'\nexport OPTION_small='false'\nmake\n"
        );
        assert_eq!(
            recipe().script,
            "make\n",
            "a recipe without options is as written"
        );
    }

    #[test]
    fn a_variant_sets_the_options_it_gives_and_keeps_the_defaults_of_the_rest() {
        let options = serde_json::json!({"locales": ["en_US.UTF-8/UTF-8", "sv_SE.UTF-8/UTF-8"]});
        let variant = with_options()
            .variant("system-locales", options.as_object().unwrap(), &[])
            .unwrap();
        assert_eq!(variant.id, "system-locales");
        assert_eq!(
            variant.script,
            "export OPTION_locales='en_US.UTF-8/UTF-8 sv_SE.UTF-8/UTF-8'\n\
             export OPTION_small='false'\n\
             make\n"
        );
    }

    #[test]
    fn a_value_is_quoted_for_the_shell() {
        let recipe = recipe_from_str(
            "[[package]]\nid = \"x\"\nscope = \"runtime\"\nscript = \"make\\n\"\n\
             [package.options.note]\ntype = \"str\"\ndefault = \"\"\ndoc = \"A note.\"\n",
        );
        let options = serde_json::json!({"note": "it's"});
        let variant = recipe
            .variant("y", options.as_object().unwrap(), &[])
            .unwrap();
        assert_eq!(variant.script, "export OPTION_note='it'\\''s'\nmake\n");
    }

    #[test]
    fn an_option_the_recipe_doesnt_declare_is_refused_with_those_it_does() {
        let options = serde_json::json!({"smal": true});
        let error = with_options()
            .variant("x", options.as_object().unwrap(), &[])
            .unwrap_err();
        assert_eq!(
            error,
            "`locales` has no option `smal`; it has `locales` and `small`"
        );
        let error = recipe()
            .variant("x", options.as_object().unwrap(), &[])
            .unwrap_err();
        assert_eq!(error, "`locales` takes no options");
    }

    #[test]
    fn a_value_of_the_wrong_type_is_refused() {
        for (value, what) in [
            (
                serde_json::json!({"small": "yes"}),
                "`small` is true or false, not \"yes\"",
            ),
            (
                serde_json::json!({"locales": "en_US"}),
                "`locales` is a list, not \"en_US\"",
            ),
            (
                serde_json::json!({"locales": [1]}),
                "`locales` is a list of text, not [1]",
            ),
        ] {
            let error = with_options()
                .variant("x", value.as_object().unwrap(), &[])
                .unwrap_err();
            assert_eq!(error, format!("`locales`'s option {what}"));
        }
    }

    #[test]
    fn a_default_of_the_wrong_type_is_an_error_in_the_recipe() {
        let text = "[[package]]\nid = \"x\"\nscope = \"runtime\"\nscript = \"make\\n\"\n\
                    [package.options.small]\ntype = \"bool\"\ndefault = \"no\"\n\
                    doc = \"Smaller.\"\n";
        let error = toml::from_str::<Recipes>(text)
            .unwrap()
            .package
            .remove(0)
            .with_option_defaults()
            .unwrap_err();
        assert_eq!(error, "`x`'s option `small` is true or false, not \"no\"");
    }

    #[test]
    fn patches_apply_in_order_after_the_options_and_need_patch_to_build() {
        let patches = ["--- a/x\n+++ b/x\n".to_owned(), "second\n".to_owned()];
        let variant = recipe()
            .variant("patched", &serde_json::Map::new(), &patches)
            .unwrap();
        assert_eq!(
            variant.script,
            "patch -p1 <<'WRATH_PATCH_0'\n--- a/x\n+++ b/x\nWRATH_PATCH_0\n\
             patch -p1 <<'WRATH_PATCH_1'\nsecond\nWRATH_PATCH_1\n\
             make\n"
        );
        assert!(variant.dep_ids().unwrap().contains(&"patch".to_owned()));
        let plain = recipe()
            .variant("plain", &serde_json::Map::new(), &[])
            .unwrap();
        assert!(!plain.dep_ids().unwrap().contains(&"patch".to_owned()));
        let ids = variant
            .variant("twice", &serde_json::Map::new(), &patches)
            .unwrap();
        let count = ids
            .dep_ids()
            .unwrap()
            .iter()
            .filter(|id| *id == "patch")
            .count();
        assert_eq!(count, 1, "patch is a dependency once");
    }
}

#[cfg(test)]
mod reference_tests {
    use super::*;

    #[test]
    fn references_are_recorded_the_first_time_and_read_from_the_record_after() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let (path, other) = (
            StorePath::derive("test", &[b"a"]),
            StorePath::derive("test", &[b"b"]),
        );
        let root = lab.on_host(&path);
        fs::create_dir_all(root.join("bin")).unwrap();
        let text = format!("#!{0}/bin/sh\n{1}/lib\n", other.inside(), path.inside());
        fs::write(root.join("bin/run"), text).unwrap();
        assert_eq!(
            references_of(&lab, &path).unwrap(),
            std::slice::from_ref(&other)
        );
        fs::remove_file(root.join("bin/run")).unwrap();
        assert_eq!(references_of(&lab, &path).unwrap(), [other]);
        forget_references(&lab, &path).unwrap();
        assert!(references_of(&lab, &path).unwrap().is_empty());
    }
}

#[cfg(test)]
mod duration_tests {
    use super::*;

    #[test]
    fn durations_are_a_number_and_a_unit() {
        assert_eq!(duration("90s").unwrap().as_secs(), 90);
        assert_eq!(duration("45m").unwrap().as_secs(), 45 * 60);
        assert_eq!(duration("3h").unwrap().as_secs(), 3 * 60 * 60);
        for bad in ["3", "h", "3d", "", "-1h", "3é"] {
            assert!(duration(bad).is_err(), "{bad}");
        }
    }
}

/// `packages` in an order where each comes after what it depends on, otherwise in the order
/// given. A dependency is either one of them or something `have` says is there already, such as
/// the toolchain.
pub(crate) fn in_order(
    packages: Vec<Package>,
    have: impl Fn(&str) -> bool,
) -> Result<Vec<Package>, String> {
    let ids: BTreeMap<String, usize> = packages
        .iter()
        .enumerate()
        .map(|(index, package)| (package.id.clone(), index))
        .collect();
    let mut needs: Vec<BTreeSet<usize>> = Vec::with_capacity(packages.len());
    for package in &packages {
        let mut deps = BTreeSet::new();
        for dep in package.dep_ids()? {
            match ids.get(&dep) {
                Some(index) => {
                    deps.insert(*index);
                }
                None if have(&dep) => {}
                None => {
                    return Err(format!(
                        "`{}` needs `{dep}`, which no recipe builds",
                        package.id
                    ));
                }
            }
        }
        needs.push(deps);
    }
    let mut placed = vec![false; packages.len()];
    let mut order = Vec::with_capacity(packages.len());
    while order.len() < packages.len() {
        let ready = (0..packages.len())
            .find(|index| !placed[*index] && needs[*index].iter().all(|dep| placed[*dep]))
            .ok_or_else(|| {
                let stuck: Vec<&str> = (0..packages.len())
                    .filter(|index| !placed[*index])
                    .map(|index| packages[index].id.as_str())
                    .collect();
                format!("these packages depend on each other: {}", stuck.join(", "))
            })?;
        placed[ready] = true;
        order.push(ready);
    }
    let mut packages: Vec<Option<Package>> = packages.into_iter().map(Some).collect();
    Ok(order
        .into_iter()
        .filter_map(|index| packages[index].take())
        .collect())
}

#[cfg(test)]
mod order_tests {
    use super::*;

    fn packages(text: &str) -> Vec<Package> {
        toml::from_str::<Recipes>(text).unwrap().package
    }

    fn recipe(id: &str, deps: &str) -> String {
        format!("[[package]]\nid = \"{id}\"\nscope = \"runtime\"\nscript = \"\"\ndeps = [{deps}]\n")
    }

    #[test]
    fn a_package_comes_after_what_it_needs_wherever_it_was_written() {
        let text = [
            recipe("git", "\"curl\", \"glibc\""),
            recipe("jq", ""),
            recipe("curl", "\"zlib\""),
            recipe("zlib", ""),
        ]
        .concat();
        let ordered = in_order(packages(&text), |id| id == "glibc").unwrap();
        let ids: Vec<&str> = ordered.iter().map(|package| package.id.as_str()).collect();
        assert_eq!(ids, ["jq", "zlib", "curl", "git"]);
    }

    #[test]
    fn a_missing_dependency_or_a_cycle_says_which_packages() {
        let missing = recipe("git", "\"curl\"");
        let error = in_order(packages(&missing), |_| false).unwrap_err();
        assert_eq!(error, "`git` needs `curl`, which no recipe builds");

        let cycle = [recipe("a", "\"b\""), recipe("b", "\"a\""), recipe("c", "")].concat();
        let error = in_order(packages(&cycle), |_| false).unwrap_err();
        assert_eq!(error, "these packages depend on each other: a, b");
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;

    const CARGO: &str = r#"
def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    lock = decode.toml(recipe.read("Cargo.lock"))
    sources = [
        {"url": "https://static.crates.io/crates/%s/%s-%s.crate" % (
            p["name"], p["name"], p["version"]),
         "into": "vendor/%s-%s" % (p["name"], p["version"]), "sha256": p["checksum"]}
        for p in lock["package"] if "checksum" in p
    ]
    return {
        "script": "cargo build %s\n" % " ".join(options["features"]),
        "deps": ["rust-bin"],
        "sources": sources,
    }

style(name = "cargo", locks = ["Cargo.lock"], files = ["Cargo.toml"],
      options = {"features": option(types.list(types.str))}, build = build)
"#;

    const MESON: &str = r#"
def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    return {"script": "meson setup\n", "deps": ["meson"]}

style(name = "meson", files = ["meson.build"], build = build)
"#;

    const LOCK: &str = "[[package]]\nname = \"itoa\"\nversion = \"1.0.15\"\nchecksum = \"abc\"\n";

    fn styles() -> Vec<Style> {
        vec![
            style::load("cargo.star", CARGO).unwrap(),
            style::load("meson.star", MESON).unwrap(),
        ]
    }

    fn recipe(fields: &str) -> Result<Package, String> {
        let text =
            format!("[[package]]\nid = \"app\"\nversion = \"1.0\"\nscope = \"runtime\"\n{fields}");
        let mut package = toml::from_str::<Recipes>(&text)
            .map_err(|e| e.to_string())?
            .package
            .remove(0);
        package.check_style(&styles())?;
        Ok(package)
    }

    fn style(package: Package, top: &[&str]) -> Result<Package, String> {
        let top: Vec<String> = top.iter().map(|name| (*name).to_owned()).collect();
        package.styled_by(&styles(), &|| Ok(top.clone()), &|path| match path {
            "Cargo.lock" => Ok(LOCK.to_owned()),
            other => Err(format!("no {other}")),
        })
    }

    fn named(id: &str, fields: &str) -> Package {
        recipe(fields)
            .map(|package| Package {
                id: id.to_owned(),
                ..package
            })
            .unwrap()
    }

    #[test]
    fn only_the_recipes_a_configuration_reaches_are_styled() {
        let packages = vec![
            named("unused", "[package.cargo]\n"),
            named("tool", "extra_deps = [\"zlib\"]\n[package.cargo]\n"),
            named("zlib", "script = \"make\"\n"),
            named("rust-bin", "script = \"cp\"\n"),
        ];
        let mut styled = Vec::new();
        let found = reached(packages, &["tool"], |package| {
            styled.push(package.id.clone());
            style(package, &["Cargo.lock"])
        })
        .unwrap();
        assert_eq!(styled, ["tool"]);
        let ids: Vec<&str> = found.iter().map(|package| package.id.as_str()).collect();
        // `rust-bin` is reached through what the style adds.
        assert_eq!(ids, ["tool", "zlib", "rust-bin"]);
    }

    #[test]
    fn an_id_no_recipe_has_reaches_nothing() {
        let packages = vec![named("tool", "[package.cargo]\n")];
        let found = reached(packages, &["missing"], |_| panic!("nothing is styled")).unwrap();
        assert!(found.is_empty());
    }

    #[test]
    fn a_recipe_without_a_script_gets_the_style_its_lock_picks() {
        let package =
            recipe("extra_deps = [\"pam\"]\n[package.cargo]\nfeatures = [\"x\"]\n").unwrap();
        assert!(package.styled);
        let package = style(package, &["Cargo.lock", "Cargo.toml", "meson.build"]).unwrap();
        assert_eq!(package.script, "cargo build x\n");
        assert_eq!(
            package.dep_ids().unwrap()[..],
            ["pam".to_owned(), "rust-bin".to_owned()]
        );
        assert_eq!(
            package.sources[0].url,
            "https://static.crates.io/crates/itoa/itoa-1.0.15.crate"
        );
        assert_eq!(package.sources[0].sha256.as_deref(), Some("abc"));
    }

    #[test]
    fn a_lock_wins_over_a_build_file_and_without_one_the_build_file_picks() {
        let found = |top: &[&str]| style(recipe("").unwrap(), top).map(|p| p.script);
        assert_eq!(
            found(&["Cargo.lock", "meson.build"]).unwrap(),
            "cargo build \n"
        );
        assert_eq!(found(&["meson.build"]).unwrap(), "meson setup\n");
    }

    #[test]
    fn two_build_files_or_none_ask_for_a_named_style() {
        let found = |top: &[&str]| style(recipe("").unwrap(), top).map(|p| p.script);
        assert_eq!(
            found(&["Cargo.toml", "meson.build"]).unwrap_err(),
            "`app`'s source has a build file for each of cargo and meson: name one with its \
             table, such as `[package.cargo]`"
        );
        assert!(found(&["Makefile"]).unwrap_err().contains("or a `script`"));
        let named = style(
            recipe("[package.meson]\n").unwrap(),
            &["Cargo.toml", "meson.build"],
        );
        assert_eq!(named.unwrap().script, "meson setup\n");
    }

    #[test]
    fn a_recipe_with_a_script_keeps_it() {
        let package = recipe("script = \"make\\n\"\n").unwrap();
        assert!(!package.styled);
    }

    #[test]
    fn style_options_beside_a_script_or_for_two_styles_are_refused() {
        assert_eq!(
            recipe("script = \"make\\n\"\n[package.cargo]\nfeatures = []\n").unwrap_err(),
            "`app` has a script and a build style; a style makes the script"
        );
        assert_eq!(
            recipe("[package.meson]\n[package.cargo]\nfeatures = []\n").unwrap_err(),
            "`app` gives options to more than one style"
        );
        assert_eq!(
            recipe("build = \"meson\"\n").unwrap_err(),
            "`app` has `build`, which is neither a recipe field nor a build style"
        );
        assert_eq!(
            recipe("[package.cmake]\nx = 1\n").unwrap_err(),
            "`app` has `cmake`, which is neither a recipe field nor a build style"
        );
        assert_eq!(
            recipe("scirpt = \"make\"\n").unwrap_err(),
            "`app` has `scirpt`, which is neither a recipe field nor a build style"
        );
    }

    #[test]
    fn a_style_option_of_the_wrong_type_is_an_error_in_the_recipe() {
        let package = recipe("[package.cargo]\nfeatures = \"x\"\n").unwrap();
        let error = style(package, &["Cargo.lock"]).unwrap_err();
        assert!(
            error.starts_with("`app`'s cargo option `features`: "),
            "{error}"
        );
    }
}

#[cfg(test)]
mod style_goldens {
    use super::*;

    #[test]
    fn a_crate_the_project_patches_from_git_is_patched_for_crates_io() {
        let styles = styles(&repository()).unwrap();
        let text = "[[package]]\nid = \"yazi\"\nscope = \"runtime\"\n[package.cargo]\n";
        let mut package = toml::from_str::<Recipes>(text).unwrap().package.remove(0);
        package.check_style(&styles).unwrap();
        let url = "https://github.com/yazi-rs/ratatui.git";
        let lock = format!(
            "[[package]]\nname = \"ratatui-core\"\nversion = \"0.1.2\"\n\
             source = \"git+{url}?branch=fix#dde5\"\n"
        );
        let manifest = format!("[patch.crates-io]\nratatui-core = {{ git = \"{url}\" }}\n");
        let read = |path: &str| match path {
            "Cargo.lock" => Ok(lock.clone()),
            "Cargo.toml" => Ok(manifest.clone()),
            other => Err(format!("no {other}")),
        };
        let package = package
            .styled_by(&styles, &|| Ok(Vec::new()), &read)
            .unwrap();
        assert!(package.script.contains("echo '[patch.crates-io]'"));
        assert!(!package.script.contains(&format!("[patch.\"{url}\"]")));
    }

    #[test]
    fn a_crate_that_links_a_system_library_brings_it() {
        let styles = styles(&repository()).unwrap();
        let text = "[[package]]\nid = \"atuin\"\nscope = \"runtime\"\nextra_deps = [\"openssl\"]\n\
                    [package.cargo]\n";
        let mut package = toml::from_str::<Recipes>(text).unwrap().package.remove(0);
        package.check_style(&styles).unwrap();
        let lock = "[[package]]\nname = \"openssl-sys\"\nversion = \"0.9.117\"\n\n\
                    [[package]]\nname = \"atuin\"\nversion = \"18.0.0\"\n";
        let read = |_: &str| Ok(lock.to_owned());
        let package = package
            .styled_by(&styles, &|| Ok(Vec::new()), &read)
            .unwrap();
        // Listed once, though the recipe names it too.
        assert_eq!(
            package.dep_ids().unwrap(),
            ["openssl", "rust-bin", "pkgconf"]
        );
        // And builds with it, even where the project asks for its own.
        assert!(package.script.contains("export OPENSSL_NO_VENDOR=1\n"));
    }

    #[test]
    fn the_cargo_style_patches_in_crates_from_git_found_by_name_in_their_checkout() {
        let styles = styles(&repository()).unwrap();
        let text = "[[package]]\nid = \"niri\"\nscope = \"runtime\"\n[package.cargo]\n";
        let mut package = toml::from_str::<Recipes>(text).unwrap().package.remove(0);
        package.check_style(&styles).unwrap();
        let lock = "[[package]]\nname = \"smithay\"\nversion = \"0.7.0\"\n\
                    source = \"git+https://github.com/Smithay/smithay.git?rev=ff5f#ff5f\"\n\n\
                    [[package]]\nname = \"drm-extras\"\nversion = \"0.1.0\"\n\
                    source = \"git+https://github.com/Smithay/smithay.git?rev=ff5f#ff5f\"\n";
        let read = |_: &str| Ok(lock.to_owned());
        let package = package
            .styled_by(&styles, &|| Ok(Vec::new()), &read)
            .unwrap();
        assert_eq!(package.sources.len(), 1);
        assert_eq!(
            package.sources[0].url,
            "git+https://github.com/Smithay/smithay.git#ff5f"
        );
        assert_eq!(package.sources[0].into.as_deref(), Some("../git/smithay"));
        assert!(
            package
                .script
                .contains("cargo build --release --offline -j $NPROC\n")
        );
        // The part that finds the crates, run on a checkout laid out as Smithay's is.
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("git/smithay");
        fs::create_dir_all(checkout.join("drm-extras")).unwrap();
        fs::write(
            checkout.join("Cargo.toml"),
            "[package]\nname = \"smithay\"\n\n[workspace]\nmembers = [\"drm-extras\"]\n",
        )
        .unwrap();
        fs::write(
            checkout.join("drm-extras/Cargo.toml"),
            "[package]\nname = \"drm-extras\"\nversion.workspace = true\n",
        )
        .unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(src.join(".cargo")).unwrap();
        let start = package
            .script
            .find("# A crate the lock takes from git")
            .unwrap();
        let end = package.script.find("cargo build").unwrap();
        let output = Command::new("sh")
            .args(["-eu", "-c", &package.script[start..end]])
            .current_dir(&src)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(src.join(".cargo/config.toml")).unwrap(),
            "[patch.\"https://github.com/Smithay/smithay.git\"]\n\
             smithay = { path = \"../git/smithay\" }\n\
             drm-extras = { path = \"../git/smithay/drm-extras\" }\n"
        );
    }

    #[test]
    fn a_meson_option_the_project_doesnt_declare_is_refused_before_it_builds() {
        let styles = styles(&repository()).unwrap();
        let text = "[[package]]\nid = \"libsecret\"\nscope = \"runtime\"\n[package.meson]\n\
                    args = [\"-D gir=false\", \"-D b_lto=true\", \"--sysconfdir=/etc\"]\n";
        let mut package = toml::from_str::<Recipes>(text).unwrap().package.remove(0);
        package.check_style(&styles).unwrap();
        // An older project keeps its options in meson_options.txt.
        let read = |path: &str| match path {
            "meson_options.txt" => Ok("option('introspection', type: 'boolean')\n\
                                       option('vapi',\n  type: 'boolean')\n"
                .to_owned()),
            _ => Err(format!("no {path}")),
        };
        let error = package
            .styled_by(&styles, &|| Ok(Vec::new()), &read)
            .unwrap_err();
        assert_eq!(
            error,
            "`libsecret`: the project has no option `gir`; it has `introspection`, `vapi`"
        );
    }

    #[test]
    fn the_build_system_styles_make_their_usual_three_steps() {
        let styles = styles(&repository()).unwrap();
        let script = |style: &str, args: &str| {
            let text = format!(
                "[[package]]\nid = \"x\"\nscope = \"runtime\"\n[package.{style}]\nargs = [{args}]\n"
            );
            let mut package = toml::from_str::<Recipes>(&text).unwrap().package.remove(0);
            package.check_style(&styles).unwrap();
            let top = || Ok(Vec::new());
            let read = |path: &str| match path {
                "meson.options" => Ok("option('docs', type: 'feature')\n".to_owned()),
                _ => Err(String::new()),
            };
            package.styled_by(&styles, &top, &read).unwrap().script
        };
        assert_eq!(
            script("meson", "\"-D docs=disabled\""),
            "meson setup /build/b . --prefix=$PREFIX --libdir=lib --buildtype=release \
             -D docs=disabled\nninja -C /build/b -j$NPROC\n\
             DESTDIR=$DEST ninja -C /build/b install\n"
        );
        assert_eq!(
            script("autotools", "\"--disable-nls\""),
            "./configure --prefix=$PREFIX --disable-nls\nmake -j$NPROC\n\
             make install DESTDIR=$DEST\n"
        );
        assert_eq!(
            script("cmake", ""),
            "cmake -S . -B /build/b -G Ninja -DCMAKE_INSTALL_PREFIX=$PREFIX \
             -DCMAKE_INSTALL_LIBDIR=lib -DCMAKE_BUILD_TYPE=Release\nninja -C /build/b -j$NPROC\n\
             DESTDIR=$DEST ninja -C /build/b install\n"
        );
    }

    #[test]
    fn compiler_flags_are_given_as_a_list_and_quoted_by_the_style() {
        let styles = styles(&repository()).unwrap();
        let script = |style: &str| {
            let text = format!(
                "[[package]]\nid = \"x\"\nscope = \"runtime\"\n[package.{style}]\n\
                 cflags = [\"-std=gnu17\", \"-include\", \"stdbool.h\"]\n\
                 cxxflags = [\"-I$WRATH_DEP_foo/include\", \"-DX=\\\"y\\\"\"]\n"
            );
            let mut package = toml::from_str::<Recipes>(&text).unwrap().package.remove(0);
            package.check_style(&styles).unwrap();
            let top = || Ok(Vec::new());
            let read = |_: &str| Err(String::new());
            package.styled_by(&styles, &top, &read).unwrap().script
        };
        let autotools = script("autotools");
        assert!(
            autotools.starts_with(
                "export CFLAGS=\"-O2 -std=gnu17 -include stdbool.h\"\n\
                 export CXXFLAGS=\"-O2 -I$WRATH_DEP_foo/include -DX=\\\"y\\\"\"\n./configure"
            ),
            "{autotools}"
        );
        for style in ["meson", "cmake"] {
            let text = script(style);
            assert!(
                text.starts_with("export CFLAGS=\"-std=gnu17 -include stdbool.h\"\n"),
                "{text}"
            );
        }
    }

    #[test]
    fn flags_set_both_as_a_list_and_in_env_are_refused() {
        let styles = styles(&repository()).unwrap();
        let text = "[[package]]\nid = \"x\"\nscope = \"runtime\"\n[package.autotools]\n\
                    cflags = [\"-std=gnu17\"]\nenv = { CFLAGS = \"-O2\" }\n";
        let mut package = toml::from_str::<Recipes>(text).unwrap().package.remove(0);
        package.check_style(&styles).unwrap();
        let top = || Ok(Vec::new());
        let read = |_: &str| Err(String::new());
        let error = package.styled_by(&styles, &top, &read).unwrap_err();
        assert!(error.contains("say it once"), "{error}");
    }

    #[test]
    fn the_go_style_takes_the_files_the_go_sum_names_as_go_rs_did() {
        let styles = styles(&repository()).unwrap();
        let text = "[[package]]\nid = \"age\"\nscope = \"runtime\"\n[package.go]\n\
                    packages = [\"./cmd/age\"]\nldflags = \"-X main.Version=v1\"\n";
        let mut package = toml::from_str::<Recipes>(text).unwrap().package.remove(0);
        package.check_style(&styles).unwrap();
        let go_sum = "filippo.io/edwards25519 v1.1.0 h1:FNf4=\n\
                      filippo.io/edwards25519 v1.1.0/go.mod h1:BxyF=\n\
                      github.com/BurntSushi/toml v1.2.0/go.mod h1:CxXY=\n";
        let package = package
            .styled_by(&styles, &|| Ok(vec!["go.sum".to_owned()]), &|_| {
                Ok(go_sum.to_owned())
            })
            .unwrap();
        let proxied = |path: &str| {
            (
                format!("https://proxy.golang.org/{path}"),
                Some(format!("goproxy/{path}")),
            )
        };
        let by_rust = vec![
            proxied("filippo.io/edwards25519/@v/v1.1.0.zip"),
            proxied("filippo.io/edwards25519/@v/v1.1.0.mod"),
            proxied("github.com/!burnt!sushi/toml/@v/v1.2.0.mod"),
        ];
        let by_style: Vec<(String, Option<String>)> = package
            .sources
            .iter()
            .map(|s| (s.url.clone(), s.into.clone()))
            .collect();
        assert_eq!(by_style, by_rust);
        assert!(package.sources.iter().all(|s| s.file));
        assert!(package.script.ends_with(
            "go build -trimpath -ldflags \"-X main.Version=v1\" -o $DEST$PREFIX/bin/ ./cmd/age\n"
        ));
    }

    fn repository() -> PathBuf {
        super::grapes_checkout()
    }

    #[test]
    fn the_cargo_style_makes_the_script_greetd_had_by_hand() {
        let styles = styles(&repository()).unwrap();
        let text = "[[package]]\nid = \"greetd\"\nversion = \"0.10.3\"\nscope = \"runtime\"\n\
                    [package.cargo]\nprograms = [\"greetd\", \"agreety\"]\n";
        let mut package = toml::from_str::<Recipes>(text).unwrap().package.remove(0);
        package.check_style(&styles).unwrap();
        let lock = "[[package]]\nname = \"greetd\"\nversion = \"0.10.3\"\n\n\
                    [[package]]\nname = \"itoa\"\nversion = \"1.0.15\"\n\
                    source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                    checksum = \"abc\"\n";
        let package = package
            .styled_by(&styles, &|| Ok(vec!["Cargo.lock".to_owned()]), &|_| {
                Ok(lock.to_owned())
            })
            .unwrap();
        let golden = repository().join("styles/tests/cargo-greetd.sh");
        assert_eq!(package.script, fs::read_to_string(golden).unwrap());
        assert_eq!(package.sources.len(), 1);
        assert_eq!(
            package.sources[0].into.as_deref(),
            Some(".cargo-vendor/itoa-1.0.15")
        );
    }
}

#[cfg(test)]
mod styled_variant_tests {
    use super::*;

    const CARGO: &str = r#"
def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    return {"script": "cargo build %s\n" % " ".join(options["features"])}

style(name = "cargo", locks = ["Cargo.lock"],
      options = {"features": option(types.list(types.str))}, build = build)
"#;

    fn styled(fields: &str) -> Package {
        let styles = vec![style::load("cargo.star", CARGO).unwrap()];
        let text = format!("[[package]]\nid = \"app\"\nscope = \"runtime\"\n{fields}");
        let mut package = recipe_from_str(&text);
        package.check_style(&styles).unwrap();
        let mut styled = restyle(package.clone()).unwrap();
        styled.unstyled = Some(Box::new(package));
        styled
    }

    fn restyle(package: Package) -> Result<Package, String> {
        let styles = vec![style::load("cargo.star", CARGO).unwrap()];
        let top = || Ok(vec!["Cargo.lock".to_owned()]);
        package.styled_by(&styles, &top, &|_| Ok(String::new()))
    }

    fn options(json: &str) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_build_sets_a_style_option_and_the_style_makes_the_script_again() {
        let recipe = styled("[package.cargo]\nfeatures = [\"wayland\"]\n");
        assert_eq!(recipe.script, "cargo build wayland\n");
        let variant = recipe.variant("app-x11", &options(r#"{"features": ["x11"]}"#), &[]);
        let variant = restyle(variant.unwrap()).unwrap();
        assert_eq!(
            (variant.id.as_str(), variant.script.as_str()),
            ("app-x11", "cargo build x11\n")
        );
    }

    #[test]
    fn recipe_options_and_patches_come_before_the_style_script() {
        let recipe = styled(
            "[package.options.small]\ntype = \"bool\"\ndefault = false\ndoc = \"Smaller.\"\n",
        );
        let set = options(r#"{"small": true, "features": ["a"]}"#);
        let variant = restyle(
            recipe
                .variant("app-small", &set, &["--- a\n".to_owned()])
                .unwrap(),
        );
        let script = variant.unwrap().script;
        assert!(
            script.starts_with("export OPTION_small='true'\npatch -p1"),
            "{script}"
        );
        assert!(
            script.ends_with("WRATH_PATCH_0\ncargo build a\n"),
            "{script}"
        );
    }

    #[test]
    fn an_option_neither_the_recipe_nor_its_style_has_is_refused() {
        let recipe = styled("");
        let variant = recipe
            .variant("app-x", &options(r#"{"feature": ["a"]}"#), &[])
            .unwrap();
        let error = restyle(variant).unwrap_err();
        assert_eq!(
            error,
            "`app-x`: the cargo style has no option `feature`; it has `features`"
        );
    }
}

#[cfg(test)]
mod hint_tests {
    use super::*;

    #[test]
    fn a_missing_program_module_or_header_is_found_at_the_end_of_a_log() {
        let cases = [
            (
                "src/meson.build:58:7: ERROR: Program 'cmake' not found or not executable",
                Missing::Program("cmake".into()),
            ),
            (
                "doc/meson.build:2:8: ERROR: Dependency \"scdoc\" not found, tried pkgconfig",
                Missing::Module("scdoc".into()),
            ),
            (
                "Looking for a fallback subproject for the dependency wayland-client",
                Missing::Module("wayland-client".into()),
            ),
            (
                "configure: error: no suitable Python interpreter found",
                Missing::Program("python3".into()),
            ),
            (
                "Number.h:17:10: fatal error: gmp.h: No such file or directory",
                Missing::Header("gmp.h".into()),
            ),
            (
                "/bin/sh: line 3: scdoc: command not found",
                Missing::Program("scdoc".into()),
            ),
            (
                "./configure: line 6130: PKG_PROG_PKG_CONFIG: command not found",
                Missing::Macro("PKG_PROG_PKG_CONFIG".into()),
            ),
            (
                "Installing src/wl-copy.fish to /usr/share/fish/vendor_completions.d",
                Missing::Outside("/usr/share/fish/vendor_completions.d".into()),
            ),
            (
                "install: cannot create regular file '/etc/udev/x.rules': Read-only file system",
                Missing::Outside("/etc/udev/x.rules".into()),
            ),
            (
                "OSError: [Errno 30] Read-only file system: '/store/abc-systemd/lib/x.service'",
                Missing::Outside("/store/abc-systemd/lib/x.service".into()),
            ),
        ];
        for (line, missing) in cases {
            let log = format!("checking things...\n{line}\nA full log can be found at x\n");
            assert_eq!(missing_in(&log), Some(missing), "{line}");
        }
        assert_eq!(missing_in("error: something else entirely\n"), None);
        assert_eq!(
            missing_in("Installing src/x to /store/abc-x/bin\nFAILED\n"),
            None
        );
    }

    #[test]
    fn the_hint_names_the_recipe_that_has_what_was_missing() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let mut built = Built::default();
        for (id, file) in [
            ("cmake", "bin/cmake"),
            ("gmp-lib", "include/gmp.h"),
            ("wayland", "lib/pkgconfig/wayland-client.pc"),
        ] {
            let path = StorePath::from_hash(&format!("{id}hash"));
            let at = lab.on_host(&path).join(file);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, "").unwrap();
            built.paths.insert(id.to_owned(), path);
        }
        let hint = |missing| failure_hint(&lab, &built, &missing);
        assert_eq!(
            hint(Missing::Program("cmake".into())),
            "`cmake` has the program `cmake`: add it to `extra_deps`"
        );
        assert_eq!(
            hint(Missing::Header("gmp.h".into())),
            "`gmp-lib` has the header `gmp.h`: add it to `extra_deps`, or to `propagates` of the \
             library whose headers include it"
        );
        assert_eq!(
            hint(Missing::Module("wayland-client".into())),
            "`wayland` has the pkg-config module `wayland-client`: add it to `extra_deps`"
        );
        assert_eq!(
            hint(Missing::Program("scdoc".into())),
            "no recipe built so far has the program `scdoc`; one may need writing"
        );
        assert_eq!(
            hint(Missing::Outside("/usr/share/fish".into())),
            "it installs into `/usr/share/fish`, outside its prefix: set the option for that \
             directory to one under the prefix"
        );
    }
}

/// A checkout of grapes, whose recipes and styles tests read: `WRATH_GRAPES`, or one beside
/// wrath's.
#[cfg(test)]
fn grapes_checkout() -> PathBuf {
    std::env::var_os("WRATH_GRAPES").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../grapes"),
        PathBuf::from,
    )
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    const GHOSTTY: &str = "[[package]]\nid = \"ghostty\"\nscope = \"runtime\"\nscript = \"true\"\n\
                           files = [\"fix.patch\"]\n\n[[package]]\nid = \"ghostty-docs\"\n\
                           scope = \"runtime\"\nscript = \"true\"\n";

    /// A repository with a file of a theme, `tools.toml`, and a family's file, `gh/ghostty.toml`,
    /// with its own file `gh/ghostty/fix.patch`.
    fn repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let recipes = dir.path().join("recipes");
        fs::create_dir_all(recipes.join("gh/ghostty")).unwrap();
        let jq = "[[package]]\nid = \"jq\"\nscope = \"runtime\"\nscript = \"true\"\n";
        fs::write(recipes.join("tools.toml"), jq).unwrap();
        fs::write(recipes.join("gh/ghostty.toml"), GHOSTTY).unwrap();
        fs::write(recipes.join("gh/ghostty/fix.patch"), "a fix\n").unwrap();
        // A recipe's own files are never recipes, whatever they're called.
        fs::write(recipes.join("gh/ghostty/config.toml"), "x = 1\n").unwrap();
        dir
    }

    fn source(url: String) -> Source {
        Source {
            url,
            into: None,
            include: Vec::new(),
            sha256: None,
            file: false,
        }
    }

    #[test]
    fn recipe_files_are_files_of_a_theme_and_of_a_package_or_family() {
        let dir = repository();
        fs::write(dir.path().join("recipes/gh/ghostty.lock"), "").unwrap();
        assert_eq!(
            recipe_files(dir.path()).unwrap(),
            ["gh/ghostty.toml", "tools.toml"]
        );
    }

    #[test]
    fn a_family_has_its_files_in_its_directory_and_its_pins_beside_it() {
        let dir = repository();
        let family = load(dir.path(), "gh/ghostty.toml").unwrap();
        assert_eq!(family[1].origin.dir, Path::new("recipes/gh/ghostty"));
        assert_eq!(
            family[1].origin.lock,
            Lock::Beside {
                file: "recipes/gh/ghostty.lock".into(),
                id: "ghostty-docs".into()
            }
        );
        let theme = load(dir.path(), "tools.toml").unwrap();
        assert_eq!(theme[0].origin, Origin::default());
    }

    #[test]
    fn a_recipes_own_files_are_in_its_key_from_its_directory() {
        let dir = repository();
        let ghostty = load(dir.path(), "gh/ghostty.toml").unwrap().remove(0);
        let before = recipe_parts(dir.path(), &ghostty, &[]).unwrap();
        assert!(before.contains(&b"a fix\n".to_vec()));
        fs::write(dir.path().join("recipes/gh/ghostty/fix.patch"), "another\n").unwrap();
        assert_ne!(recipe_parts(dir.path(), &ghostty, &[]).unwrap(), before);
    }

    #[test]
    fn a_source_is_pinned_in_the_lock_beside_its_recipe_and_read_back_from_there() {
        let dir = repository();
        let file = dir.path().join("ghostty.tar.gz");
        fs::write(&file, "ghostty\n").unwrap();
        let url = format!("file://{}", file.display());
        let lab = Lab::new(dir.path().join("work"));
        let family = load(dir.path(), "gh/ghostty.toml").unwrap();
        let mut sources = Sources::load(dir.path()).unwrap();
        for package in &family {
            sources
                .fetch_source(&lab, &source(url.clone()), &package.origin.lock)
                .unwrap();
        }
        let hash = sha256_of(&file).unwrap();
        let lock = fs::read_to_string(dir.path().join("recipes/gh/ghostty.lock")).unwrap();
        let entry = format!("[[ghostty.sources]]\nurl = \"{url}\"\nhash = \"{hash}\"\n");
        assert!(lock.contains(&entry), "{lock}");
        assert!(lock.contains("[ghostty-docs]"), "{lock}");
        assert!(!dir.path().join(SHARED_LOCK).exists());
        // Known from the lock, the source isn't fetched again.
        fs::remove_file(&file).unwrap();
        let mut again = Sources::load(dir.path()).unwrap();
        assert_eq!(
            again.hash_of(&lab, &source(url), &Lock::Shared).unwrap(),
            hash
        );
    }

    #[test]
    fn a_url_pinned_two_ways_is_an_error_naming_both_locks() {
        let dir = repository();
        let pin = |hash: &str| {
            format!(
                "[x]\n\n[[x.sources]]\nurl = \"https://example.org/x.tar.gz\"\nhash = \"{hash}\"\n"
            )
        };
        fs::write(dir.path().join("recipes/gh/ghostty.lock"), pin("aa")).unwrap();
        fs::create_dir_all(dir.path().join("recipes/jq")).unwrap();
        fs::write(dir.path().join("recipes/jq/jq.toml"), "package = []\n").unwrap();
        fs::write(dir.path().join("recipes/jq/jq.lock"), pin("bb")).unwrap();
        let error = Sources::load(dir.path()).err().unwrap();
        assert!(error.contains("recipes/gh/ghostty.lock"), "{error}");
        assert!(error.contains("recipes/jq/jq.lock"), "{error}");
    }

    #[test]
    fn the_toolchain_is_what_its_roots_need() {
        let repository = grapes_checkout();
        let mut packages = Vec::new();
        for file in recipe_files(&repository).unwrap() {
            packages.extend(load(&repository, &file).unwrap());
        }
        let found: BTreeSet<String> = reached(packages, &TOOLCHAIN, Ok)
            .unwrap()
            .into_iter()
            .map(|package| package.id)
            .collect();
        for id in [
            "gcc-stage1",
            "binutils-stage1",
            "m4",
            "bison",
            "python",
            "glibc",
            "hello",
        ] {
            assert!(found.contains(id), "{found:?}");
        }
        assert!(!found.contains("bash"), "{found:?}");
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;

    const MESON: &str = r#"
def build(recipe: Recipe, options: dict[str, typing.Any]) -> dict[str, typing.Any]:
    return {"script": "meson setup\n", "deps": ["meson"]}

style(name = "meson", files = ["meson.build"], build = build)
"#;

    struct Set {
        dir: tempfile::TempDir,
        lab: Lab,
        tarball: PathBuf,
    }

    /// A set with one recipe, `app`, built by the meson style from a downloaded source.
    fn set() -> Set {
        let dir = tempfile::tempdir().unwrap();
        let tarball = dir.path().join("app-1.0.tar.gz");
        fs::write(&tarball, "app\n").unwrap();
        let recipe = format!(
            "[[package]]\nid = \"app\"\nversion = \"1.0\"\nscope = \"runtime\"\n\
             sources = [{{ url = \"file://{}\" }}]\n\n[package.meson]\n",
            tarball.display()
        );
        fs::create_dir_all(dir.path().join("recipes/ap")).unwrap();
        fs::write(dir.path().join("recipes/ap/app.toml"), recipe).unwrap();
        fs::create_dir_all(dir.path().join("styles")).unwrap();
        fs::write(dir.path().join("styles/meson.star"), MESON).unwrap();
        let lab = Lab::new(dir.path().join("work"));
        Set { dir, lab, tarball }
    }

    impl Set {
        fn styled(&self, code: &str, read_only: bool) -> Result<Package, String> {
            let styles = [style::load("meson.star", code).unwrap()];
            let mut sources = Sources::load(self.dir.path()).unwrap();
            sources.read_only = read_only;
            let package = load(self.dir.path(), "ap/app.toml").unwrap().remove(0);
            style_one(&self.lab, self.dir.path(), &styles, &mut sources, package)
        }

        fn lock(&self) -> BTreeMap<String, PackageLock> {
            read_lock(&self.dir.path().join("recipes/ap/app.lock")).unwrap()
        }

        fn key(&self, package: &Package) -> Vec<Vec<u8>> {
            let mut sources = Sources::load(self.dir.path()).unwrap();
            let hashes: Vec<String> = package
                .sources
                .iter()
                .map(|source| {
                    sources
                        .hash_of(&self.lab, source, &package.origin.lock)
                        .unwrap()
                })
                .collect();
            let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
            recipe_parts(self.dir.path(), package, &hashes).unwrap()
        }

        /// Takes the source away, downloaded and kept, so only the lock can say what it holds.
        fn lose_source(&self) {
            fs::remove_file(&self.tarball).unwrap();
            fs::remove_dir_all(self.lab.work.join("sources")).unwrap();
        }
    }

    #[test]
    fn styling_records_what_the_style_made_in_the_lock() {
        let set = set();
        let styled = set.styled(MESON, false).unwrap();
        assert!(!styled.planned);
        assert!(styled.script.ends_with("meson setup\n"));
        let made = set.lock()["app"].made().unwrap();
        assert_eq!(made.style, "meson");
        assert_eq!(made.deps, ["meson"]);
        assert_eq!(Some(made.record), styled.record);
    }

    #[test]
    fn a_recorded_recipe_is_planned_without_its_source_with_the_same_key() {
        let set = set();
        let styled = set.styled(MESON, false).unwrap();
        let key = set.key(&styled);
        set.lose_source();
        let planned = set.styled(MESON, false).unwrap();
        assert!(planned.planned);
        assert!(planned.dep_ids().unwrap().contains(&"meson".to_owned()));
        assert_eq!(set.key(&planned), key);
    }

    #[test]
    fn a_style_changed_to_make_the_same_script_is_styled_again_and_keeps_the_key() {
        let set = set();
        let before = set.styled(MESON, false).unwrap();
        let from = set.lock()["app"].from.clone();
        let changed = format!("{MESON}\n# Reworded.\n");
        let after = set.styled(&changed, false).unwrap();
        assert!(!after.planned);
        assert_ne!(set.lock()["app"].from, from);
        assert_eq!(set.key(&after), set.key(&before));
    }

    #[test]
    fn a_style_that_makes_something_else_changes_the_record_and_the_key() {
        let set = set();
        let before = set.styled(MESON, false).unwrap();
        let after = set.styled(&MESON.replace("meson setup", "meson setup -D x=1"), false);
        let after = after.unwrap();
        assert_ne!(after.record, before.record);
        assert_ne!(set.key(&after), set.key(&before));
    }

    #[test]
    fn a_pinned_set_whose_lock_is_behind_its_styles_is_refused_and_left_as_it_was() {
        let set = set();
        set.styled(MESON, false).unwrap();
        let lock = fs::read_to_string(set.dir.path().join("recipes/ap/app.lock")).unwrap();
        let error = set
            .styled(&format!("{MESON}\n# Reworded.\n"), true)
            .unwrap_err();
        assert!(error.contains("doesn't record"), "{error}");
        let after = fs::read_to_string(set.dir.path().join("recipes/ap/app.lock")).unwrap();
        assert_eq!(after, lock);
    }

    #[test]
    fn a_pinned_set_never_pins_a_source() {
        let set = set();
        let error = set.styled(MESON, true).unwrap_err();
        assert!(error.contains("isn't pinned"), "{error}");
        assert!(!set.dir.path().join("recipes/ap/app.lock").exists());
    }
}

#[cfg(test)]
mod tree_record_tests {
    use super::*;

    #[test]
    fn a_recipe_built_from_a_tree_of_a_checkout_is_always_styled_and_never_recorded() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("project")).unwrap();
        fs::write(dir.path().join("project/meson.build"), "project('p')\n").unwrap();
        fs::create_dir_all(dir.path().join("recipes/pr")).unwrap();
        let recipe = "[[package]]\nid = \"project\"\nscope = \"runtime\"\n\
                      sources = [{ url = \"tree:project\", include = [\"meson.build\"] }]\n\n\
                      [package.meson]\n";
        fs::write(dir.path().join("recipes/pr/project.toml"), recipe).unwrap();
        let meson = "def build(recipe: Recipe, options: dict[str, typing.Any]) -> \
                     dict[str, typing.Any]:\n    return {\"script\": \"meson setup\\n\"}\n\n\
                     style(name = \"meson\", files = [\"meson.build\"], build = build)\n";
        fs::create_dir_all(dir.path().join("styles")).unwrap();
        fs::write(dir.path().join("styles/meson.star"), meson).unwrap();
        let styles = [style::load("meson.star", meson).unwrap()];
        let lab = Lab::new(dir.path().join("work"));
        for _ in 0..2 {
            let mut sources = Sources::load(dir.path()).unwrap();
            sources.read_only = true;
            let package = load(dir.path(), "pr/project.toml").unwrap().remove(0);
            let styled = style_one(&lab, dir.path(), &styles, &mut sources, package).unwrap();
            assert!(!styled.planned);
            assert!(!styled.restyled_now());
        }
        assert!(!dir.path().join("recipes/pr/project.lock").exists());
    }
}

#[cfg(test)]
mod search_path_tests {
    use super::*;

    fn python() -> SearchPath {
        SearchPath {
            variable: "PYTHONPATH".into(),
            files: vec!["site".into()],
            separator: colon(),
        }
    }

    fn path(name: &str) -> StorePath {
        StorePath::derive(name, &[name.as_bytes()])
    }

    /// `jinja` and `markupsafe` have `site`; `zlib` doesn't.
    fn values(declaring: &[&str]) -> BTreeMap<String, String> {
        let deps = [
            ("python3", vec![path("python3")]),
            ("jinja", vec![path("jinja")]),
            ("zlib", vec![path("zlib")]),
            (
                "markupsafe",
                vec![path("markupsafe-bin"), path("markupsafe")],
            ),
        ];
        let with_site = [path("jinja"), path("markupsafe")];
        search_path_values(
            &deps,
            |id| {
                if declaring.contains(&id) {
                    vec![python()]
                } else {
                    Vec::new()
                }
            },
            |output, file| file == "site" && with_site.contains(output),
        )
    }

    #[test]
    fn a_build_with_the_declaring_package_lists_its_dependencies_directories_in_order() {
        let found = values(&["python3"]);
        let expected = format!(
            "{}/site:{}/site",
            path("jinja").inside(),
            path("markupsafe").inside()
        );
        assert_eq!(found, BTreeMap::from([("PYTHONPATH".into(), expected)]));
    }

    #[test]
    fn without_the_declaring_package_there_is_no_variable() {
        assert!(values(&[]).is_empty());
    }

    #[test]
    fn two_packages_declaring_one_variable_list_each_directory_once() {
        assert_eq!(values(&["python3", "jinja"]), values(&["python3"]));
    }

    #[test]
    fn a_variable_nothing_lists_is_left_out() {
        let deps = [("python3", vec![path("python3")])];
        let found = search_path_values(&deps, |_| vec![python()], |_, _| false);
        assert!(found.is_empty());
    }

    #[test]
    fn what_dependencies_in_the_view_declare_is_in_a_key_worked_out_before_building() {
        let dep = |id: &str, view: bool| Dep {
            id: id.into(),
            view,
            rpath: view,
            skip: Vec::new(),
            propagate: Propagate::All,
            exclude: Vec::new(),
        };
        let declared = |id: &str| {
            if id == "python3" {
                vec![python()]
            } else {
                Vec::new()
            }
        };
        let parts = declared_parts(&[dep("python3", true), dep("zlib", true)], declared);
        assert_eq!(parts.len(), 1);
        assert!(declared_parts(&[dep("python3", false)], declared).is_empty());
    }
}
