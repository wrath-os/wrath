//! Building a chain of packages from recipes, under one arm of the experiment.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use memchr::memmem;
use serde::Deserialize;

use crate::sandbox::{self, Identity, Limits, Mount, Status, Step, View};
use crate::seed::{self, run as run_host, sha256_of};
use crate::split;
use crate::store::{Lab, StorePath, with_path};
use crate::view;

/// One way of building: an arm of the experiments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    A,
    B,
    C,
    /// B, with outputs moved to paths named by their contents, and `DT_RUNPATH`.
    D,
    /// D with the toolchain's own `DT_RPATH`.
    E,
    /// E, with each package split into outputs as it's built.
    F,
}

impl Arm {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "a" => Ok(Self::A),
            "b" => Ok(Self::B),
            "c" => Ok(Self::C),
            "d" => Ok(Self::D),
            "e" => Ok(Self::E),
            "f" => Ok(Self::F),
            other => Err(format!(
                "there's no arm `{other}`, only a, b, c, d, e and f"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::A => "a",
            Self::B => "b",
            Self::C => "c",
            Self::D => "d",
            Self::E => "e",
            Self::F => "f",
        }
    }

    /// Whether this arm moves packages to paths named by their contents, and strips them.
    pub fn moves_packages(self) -> bool {
        matches!(self, Self::D | Self::E | Self::F)
    }

    /// Whether packages become several outputs.
    pub fn splits_outputs(self) -> bool {
        self == Self::F
    }
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
    },
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
        let (id, view, rpath, skip) = match self {
            Self::Id(id) => (id, true, true, &None),
            Self::Full {
                id,
                view,
                rpath,
                skip,
            } => (id, *view, *rpath, skip),
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
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    url: String,
    into: Option<String>,
    /// The file's name, for a URL that doesn't end in it.
    name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Package {
    pub(crate) id: String,
    scope: Scope,
    /// Build on the final toolchain, in addition to `deps` and `extra_deps`.
    #[serde(default)]
    toolset: bool,
    /// One output, for a package that finds its own files relative to a library at run time,
    /// which no scan of its files can see.
    #[serde(default)]
    whole: bool,
    /// Build on the core tools as well, instead of the seed's.
    #[serde(default)]
    core: bool,
    #[serde(default)]
    deps: Vec<DepSpec>,
    #[serde(default)]
    extra_deps: Vec<DepSpec>,
    #[serde(default)]
    sources: Vec<Source>,
    #[serde(default)]
    files: Vec<String>,
    script: String,
    #[serde(default = "yes")]
    wrap: bool,
    #[serde(default = "yes")]
    fixup: bool,
    #[serde(default)]
    toolchain_limits: bool,
    /// Commands to run from the final location, with paths relative to the package.
    #[serde(default)]
    pub(crate) smoke: Vec<String>,
    /// Libraries to link a small program against and run.
    #[serde(default)]
    pub(crate) probes: Vec<String>,
    /// Builds the tests, in the view, for tests that need a compiler.
    #[serde(default)]
    pub(crate) check_build: Option<String>,
    #[serde(default)]
    pub(crate) check: Option<String>,
    /// The package's tests compile as they run, so they need a whole view in every arm.
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
    /// Whether this package is built once for every arm from the seed.
    pub(crate) fn is_boot(&self) -> bool {
        self.scope == Scope::Boot
    }

    /// Whether fixups apply to it after it's installed.
    pub(crate) fn fixed_up(&self) -> bool {
        self.toolset || self.core
    }

    /// The ids of what it depends on.
    pub(crate) fn dep_ids(&self) -> Result<Vec<String>, String> {
        Ok(self.all_deps()?.into_iter().map(|dep| dep.id).collect())
    }

    /// The package with `shell` run first, which changes everything that depends on it.
    pub(crate) fn perturbed(&self, shell: &str) -> Self {
        let mut changed = self.clone();
        changed.script = format!("{shell}\n{}", self.script);
        changed
    }

    /// The package built from another release of its first source.
    pub(crate) fn with_source(&self, url: &str) -> Self {
        let mut changed = self.clone();
        changed.sources[0].url = url.to_owned();
        changed
    }

    /// All the dependencies, the toolchain's first when the package builds on it.
    fn all_deps(&self) -> Result<Vec<Dep>, String> {
        let toolset = [
            DepSpec::Id("wrapper".to_owned()),
            DepSpec::Full {
                id: "seed".to_owned(),
                view: true,
                rpath: true,
                skip: Some("compilers".to_owned()),
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
        base.iter()
            .chain(&core)
            .chain(&self.deps)
            .chain(&self.extra_deps)
            .map(DepSpec::resolve)
            .collect()
    }

    /// A package linking a small program against `library` from this package.
    pub(crate) fn probe(&self, library: &str) -> Self {
        let mut extra_deps = vec![DepSpec::Id(self.id.clone())];
        extra_deps.extend(self.extra_deps.iter().cloned());
        let script = format!(
            "mkdir -p $DEST$PREFIX/bin\n\
             printf 'int main(void) {{ return 0; }}\\n' > probe.c\n\
             gcc probe.c -o $DEST$PREFIX/bin/probe -Wl,--no-as-needed -l{library}\n"
        );
        Self {
            id: format!("probe-{}-{library}", self.id),
            scope: Scope::Runtime,
            toolset: true,
            whole: false,
            core: self.core,
            deps: Vec::new(),
            extra_deps,
            sources: Vec::new(),
            files: Vec::new(),
            script,
            wrap: true,
            fixup: true,
            toolchain_limits: false,
            smoke: vec!["bin/probe".to_owned()],
            probes: Vec::new(),
            check_build: None,
            check: None,
            check_in_view: false,
            check_deps: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipes {
    package: Vec<Package>,
}

const LOADER: &str = "lib/ld-linux-x86-64.so.2";
const SEED_GCC_LIBS: &str = "lib/gcc/x86_64-unknown-linux-gnu/13.2.0";

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
    pub views: Views,
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

/// Which of its dependencies a package sees in its view.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Views {
    /// What its recipe lists.
    #[default]
    Declared,
    /// Everything its dependencies pulled in too.
    Closure,
    /// What it lists, less anything another dependency already pulled in.
    Reduced,
}

impl Views {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "declared" => Ok(Self::Declared),
            "closure" => Ok(Self::Closure),
            "reduced" => Ok(Self::Reduced),
            other => Err(format!("nothing is called a `{other}` view")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Closure => "closure",
            Self::Reduced => "reduced",
        }
    }
}

/// Packages that exist only to bootstrap, and would clash with their final versions in a view.
pub(crate) const BOOTSTRAP_ONLY: [&str; 5] = ["seed", "wrapper", "m4", "bison", "python"];

/// `deps` as `built.views` narrows or widens them. Boot packages keep what they list.
fn in_views(mut deps: Vec<Dep>, package: &Package, built: &Built) -> Result<Vec<Dep>, String> {
    if package.is_boot() {
        return Ok(deps);
    }
    let reaches = |from: &Dep, to: &StorePath| {
        built
            .closures
            .get(&from.id)
            .is_some_and(|closure| closure.contains(to))
    };
    match built.views {
        Views::Declared => Ok(deps),
        Views::Reduced => {
            let mut listed = BTreeSet::new();
            for spec in package.deps.iter().chain(&package.extra_deps) {
                listed.insert(spec.resolve()?.id);
            }
            let covered: BTreeSet<String> = deps
                .iter()
                .filter(|dep| listed.contains(&dep.id))
                .filter(|dep| {
                    built.paths.get(&dep.id).is_some_and(|path| {
                        deps.iter()
                            .any(|other| other.id != dep.id && reaches(other, path))
                    })
                })
                .map(|dep| dep.id.clone())
                .collect();
            for dep in &mut deps {
                dep.view &= !covered.contains(&dep.id);
            }
            Ok(deps)
        }
        Views::Closure => {
            let have: BTreeSet<String> = deps.iter().map(|dep| dep.id.clone()).collect();
            let extra: Vec<Dep> = built
                .paths
                .iter()
                .filter(|(id, _)| !have.contains(*id))
                .filter(|(id, _)| {
                    !BOOTSTRAP_ONLY.contains(&id.as_str())
                        && !id.ends_with("-stage1")
                        && !id.starts_with("probe-")
                })
                .filter(|(_, path)| deps.iter().any(|dep| reaches(dep, path)))
                .map(|(id, _)| Dep {
                    id: id.clone(),
                    view: true,
                    rpath: false,
                    skip: Vec::new(),
                })
                .collect();
            deps.extend(extra);
            Ok(deps)
        }
    }
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

fn fresh(path: &Path) -> Result<(), String> {
    remove(path)?;
    fs::create_dir_all(path).map_err(with_path(path))
}

/// The pinned hashes of sources, kept in `sources.lock` as `<sha256>  <url>` lines. A URL not in
/// it yet is pinned the first time it's fetched.
pub(crate) struct Sources {
    file: PathBuf,
    pins: BTreeMap<String, String>,
}

impl Sources {
    pub(crate) fn load(repository: &Path) -> Result<Self, String> {
        let file = repository.join("sources.lock");
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(with_path(&file)(e)),
        };
        let pins = text
            .lines()
            .filter_map(|line| line.split_once("  "))
            .map(|(hash, url)| (url.to_owned(), hash.to_owned()))
            .collect();
        Ok(Self { file, pins })
    }

    fn save(&self) -> Result<(), String> {
        let text: String = self
            .pins
            .iter()
            .map(|(url, hash)| format!("{hash}  {url}\n"))
            .collect();
        fs::write(&self.file, text).map_err(with_path(&self.file))
    }

    /// Where the source at `url` is on the host, with its hash.
    fn fetch(
        &mut self,
        lab: &Lab,
        url: &str,
        name: Option<&str>,
    ) -> Result<(PathBuf, String), String> {
        let dir = lab.work.join("sources");
        fs::create_dir_all(&dir).map_err(with_path(&dir))?;
        let name = name.unwrap_or_else(|| url.rsplit('/').next().unwrap_or(url));
        let path = dir.join(name);
        if !path.exists() {
            let partial = dir.join(format!("{name}.partial"));
            run_host(
                Command::new("curl")
                    .args(["--fail", "--location", "--silent", "--show-error"])
                    .args(["--max-time", "1800", "--retry", "3", "--output"])
                    .arg(&partial)
                    .arg(url),
                &format!("downloading {url}"),
            )?;
            fs::rename(&partial, &path).map_err(with_path(&path))?;
        }
        let hash = sha256_of(&path)?;
        match self.pins.get(url) {
            Some(pinned) if *pinned != hash => {
                Err(format!("{url} has hash {hash}, and {pinned} is pinned"))
            }
            Some(_) => Ok((path, hash)),
            None => {
                self.pins.insert(url.to_owned(), hash.clone());
                self.save()?;
                Ok((path, hash))
            }
        }
    }
}

pub(crate) fn load(repository: &Path, recipes: &str) -> Result<Vec<Package>, String> {
    let path = repository.join("recipes").join(recipes);
    let text = fs::read_to_string(&path).map_err(with_path(&path))?;
    let parsed: Recipes = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(parsed.package)
}

/// Whether a package's arm matters: those built once for every arm share a path.
fn arm_key(package: &Package, arm: Arm) -> &'static str {
    if package.scope == Scope::Boot {
        "boot"
    } else {
        arm.name()
    }
}

/// What the arm does after a package is installed: only arm A patches binaries afterwards.
fn fixup_script(package: &Package, arm: Arm, rpath: &str, interpreter: &str) -> String {
    if arm != Arm::A || package.scope == Scope::Boot || !package.fixup {
        return String::new();
    }
    format!(
        r#"
find "$OUT" -type f | while read -r file; do
    case "$file" in *.o|*.a) continue ;; esac
    [ "$(head -c 4 "$file" | od -An -c | tr -d ' ')" = '177ELF' ] || continue
    chmod u+w "$file"
    $LAB_SEED/bin/patchelf --set-rpath '{rpath}' "$file"
    if $LAB_SEED/bin/readelf -l "$file" | grep -q 'program interpreter'; then
        $LAB_SEED/bin/patchelf --set-interpreter '{interpreter}' "$file"
    fi
done
"#
    )
}

pub(crate) fn built_from_seed(seed: &StorePath) -> Built {
    let mut built = Built::default();
    built.paths.insert("seed".to_owned(), seed.clone());
    built.closures.insert("seed".to_owned(), BTreeSet::new());
    built
}

/// Whether `package` is moved to a path named by its contents once it's built.
pub(crate) fn content_addressed(package: &Package, arm: Arm) -> bool {
    arm.moves_packages() && package.scope != Scope::Boot
}

/// Moves a finished package to its content address, if the arm does that, so what's built next
/// is keyed on what this one really produced.
pub(crate) fn finish(
    lab: &Lab,
    package: &Package,
    arm: Arm,
    out: &StorePath,
    built: &mut Built,
) -> Result<(), String> {
    if !content_addressed(package, arm) {
        return Ok(());
    }
    let (outputs, whole) = place_outputs(lab, package, arm, out)?;
    if let Some(reason) = whole {
        note_whole(lab, arm, package, &reason)?;
    }
    let record = if arm.splits_outputs() {
        outputs
            .iter()
            .map(|(class, path)| format!("{}={}", class.name(), path.hash()))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        outputs[&split::Class::Out].hash().to_owned()
    };
    let done = lab.work.join("done").join(out.hash());
    fs::write(&done, &record).map_err(with_path(&done))?;
    remember_outputs(built, package, arm, &record);
    Ok(())
}

/// Moves a finished package to its content addresses, and says why if it stayed whole.
pub(crate) fn place_outputs(
    lab: &Lab,
    package: &Package,
    arm: Arm,
    out: &StorePath,
) -> Result<(split::Outputs, Option<String>), String> {
    if !arm.splits_outputs() {
        let address = crate::content::address(lab, out)?;
        return Ok((split::Outputs::from([(split::Class::Out, address)]), None));
    }
    if package.whole {
        split::keep_whole(lab, out, "the recipe asks".to_owned())
    } else {
        split::split_package(lab, out)
    }
}

/// Sets where a package is from what its record says.
fn remember_outputs(built: &mut Built, package: &Package, arm: Arm, record: &str) {
    if !arm.splits_outputs() {
        built
            .paths
            .insert(package.id.clone(), StorePath::from_hash(record));
        return;
    }
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

/// Records that a package stayed whole, and why.
fn note_whole(lab: &Lab, arm: Arm, package: &Package, reason: &str) -> Result<(), String> {
    let path = lab.work.join("logs").join(arm.name()).join("whole.txt");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(with_path(&path))?;
    writeln!(file, "{} {reason}", package.id).map_err(with_path(&path))
}

/// Builds every package of `recipes` for `arm`, reusing what's already built.
pub fn build(lab: &Lab, repository: &Path, recipes: &str, arm: Arm) -> Result<Built, String> {
    let packages = load(repository, recipes)?;
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
    let mut built = built_from_seed(&seed);
    for package in &packages {
        if let Some(prepared) = build_one(
            lab,
            repository,
            package,
            arm,
            &seed,
            &mut sources,
            &mut built,
        )? {
            remove(&prepared.build_dir)?;
            finish(lab, package, arm, &prepared.out, &mut built)?;
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
    pub(crate) links: Vec<(String, String)>,
    pub(crate) limits: Limits,
    pub(crate) build_dir: PathBuf,
    pub(crate) dep_paths: Vec<StorePath>,
}

/// What goes into a package's key from its recipe, before any dependency.
fn recipe_parts(
    repository: &Path,
    package: &Package,
    arm: Arm,
    source_hashes: &[&str],
) -> Result<Vec<Vec<u8>>, String> {
    let mut parts: Vec<Vec<u8>> = vec![
        arm_key(package, arm).as_bytes().to_vec(),
        package.id.as_bytes().to_vec(),
        package.script.as_bytes().to_vec(),
        vec![
            u8::from(package.wrap),
            u8::from(package.fixup),
            u8::from(package.whole),
        ],
    ];
    for name in &package.files {
        let path = repository.join("recipes").join(name);
        parts.push(fs::read(&path).map_err(with_path(&path))?);
    }
    parts.extend(source_hashes.iter().map(|hash| hash.as_bytes().to_vec()));
    Ok(parts)
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
            format!(
                "{} {} {} {} {:?}",
                dep.id,
                reference(dep),
                dep.view,
                dep.rpath,
                dep.skip
            )
        })
        .collect();
    let parts: Vec<&[u8]> = recipe
        .iter()
        .map(Vec::as_slice)
        .chain(dep_texts.iter().map(String::as_bytes))
        .collect();
    StorePath::derive(kind, &parts)
}

/// The unresolved key of every package in `recipes`, with nothing built.
pub fn unresolved_keys(
    repository: &Path,
    recipes: &[&str],
    arm: Arm,
) -> Result<BTreeMap<String, String>, String> {
    let lab = Lab::new("work");
    let seed = seed::prepare(&lab)?;
    let mut sources = Sources::load(repository)?;
    let mut keys = BTreeMap::from([("seed".to_owned(), seed.hash().to_owned())]);
    for file in recipes {
        for package in load(repository, file)? {
            let mut hashes = Vec::new();
            for source in &package.sources {
                hashes.push(sources.fetch(&lab, &source.url, source.name.as_deref())?.1);
            }
            let hashes: Vec<&str> = hashes.iter().map(String::as_str).collect();
            let recipe = recipe_parts(repository, &package, arm, &hashes)?;
            let deps = package.all_deps()?;
            if let Some(dep) = deps.iter().rfind(|dep| !keys.contains_key(&dep.id)) {
                return Err(format!(
                    "`{}` needs `{}`, listed after it",
                    package.id, dep.id
                ));
            }
            let key = package_key("unresolved", &recipe, &deps, |dep| keys[&dep.id].clone());
            keys.insert(package.id.clone(), key.hash().to_owned());
        }
    }
    keys.remove("seed");
    Ok(keys)
}

/// Builds one package. `None` means it was built before.
pub(crate) fn build_one(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    arm: Arm,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
) -> Result<Option<Prepared>, String> {
    let deps = in_views(package.all_deps()?, package, built)?;
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

    let mut fetched = Vec::new();
    for source in &package.sources {
        let (path, hash) = sources.fetch(lab, &source.url, source.name.as_deref())?;
        fetched.push((source, path, hash));
    }
    let source_hashes: Vec<&str> = fetched.iter().map(|(_, _, hash)| hash.as_str()).collect();
    let recipe = recipe_parts(repository, package, arm, &source_hashes)?;
    let out = package_key("build", &recipe, &deps, |dep| built.reference(&dep.id));
    let unresolved = package_key("unresolved", &recipe, &deps, |dep| {
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
            "arm": arm_key(package, arm),
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

    let done_dir = lab.work.join("done");
    let done = done_dir.join(out.hash());
    if done.exists() {
        if !content_addressed(package, arm) {
            println!("  have   {}", package.id);
            return Ok(None);
        }
        let address = fs::read_to_string(&done).map_err(with_path(&done))?;
        // An empty record means the build stopped before the package was moved.
        if !address.is_empty() {
            println!("  have   {}", package.id);
            remember_outputs(built, package, arm, &address);
            return Ok(None);
        }
    }
    println!("  build  {}", package.id);

    let build_dir = lab.work.join("build").join(out.hash());
    fresh(&build_dir)?;
    fresh(&lab.on_host(&out))?;
    let src = build_dir.join("src");
    fs::create_dir_all(&src).map_err(with_path(&src))?;
    for (source, path, _) in &fetched {
        let target = match &source.into {
            Some(into) => src.join(into),
            None => src.clone(),
        };
        fs::create_dir_all(&target).map_err(with_path(&target))?;
        run_host(
            Command::new("tar")
                .arg("-xf")
                .arg(path)
                .arg("--strip-components=1")
                .arg("-C")
                .arg(&target),
            &format!("unpacking {}", path.display()),
        )?;
    }
    for name in &package.files {
        let from = repository.join("recipes").join(name);
        fs::copy(&from, src.join(name)).map_err(with_path(&from))?;
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

    let usr_prefix = package.scope != Scope::Runtime || arm == Arm::A;
    let inside = out.inside();
    let (prefix, dest) = if usr_prefix {
        ("/usr".to_owned(), inside.clone())
    } else {
        (inside.clone(), String::new())
    };

    let mount = if arm == Arm::C && package.scope != Scope::Boot {
        Mount::Sentinel(StorePath::derive("sentinel", &[out.hash().as_bytes()]))
    } else {
        Mount::Usr
    };
    let real_bin = match &mount {
        Mount::Usr => "/usr/bin".to_owned(),
        Mount::Sentinel(path) => format!("{}/bin", path.inside()),
    };

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
    let rpath = rpath_dirs.join(":");

    let wrapped = package.wrap && (package.scope == Scope::Boot || arm != Arm::A);
    let mut env = BTreeMap::from([
        ("OUT".to_owned(), inside),
        ("PREFIX".to_owned(), prefix),
        ("DEST".to_owned(), dest),
        ("NPROC".to_owned(), Identity::current().jobs.to_string()),
        ("LAB_ARM".to_owned(), arm.name().to_owned()),
        ("LAB_SEED".to_owned(), seed.inside()),
        ("LAB_REAL_BIN".to_owned(), real_bin.clone()),
    ]);
    for dep in &deps {
        let name = dep.id.replace('-', "_");
        env.insert(format!("LAB_DEP_{name}"), built.paths[&dep.id].inside());
        env.insert(
            format!("LAB_LIB_{name}"),
            built.library_path(&dep.id).inside(),
        );
    }
    let path_var = if wrapped {
        let wrapper = built
            .paths
            .get("wrapper")
            .ok_or("nothing built the wrapper")?;
        let mut flags = vec![format!("-Wl,--dynamic-linker={interpreter}")];
        if arm == Arm::D {
            flags.push("-Wl,--enable-new-dtags".to_owned());
        }
        flags.extend(rpath_dirs.iter().map(|dir| format!("-Wl,-rpath,{dir}")));
        env.insert("LAB_LINK_FLAGS".to_owned(), flags.join(" "));
        format!("{}/bin:{real_bin}", wrapper.inside())
    } else {
        real_bin
    };
    env.insert("PATH".to_owned(), path_var);

    let script = format!(
        "cd /build/src\n{}\ncd /build\n\
         if [ -d \"$OUT/usr\" ]; then cp -a \"$OUT/usr/.\" \"$OUT/\"; rm -rf \"$OUT/usr\"; fi\n{}",
        package.script,
        fixup_script(package, arm, &rpath, &interpreter),
    );

    let links = if arm == Arm::A && package.scope != Scope::Boot {
        vec![("/usr/lib".to_owned(), "/lib64".to_owned())]
    } else {
        Vec::new()
    };
    let limits = if package.toolchain_limits {
        Limits::TOOLCHAIN
    } else {
        Limits::DEFAULT
    };
    let log = lab
        .work
        .join("logs")
        .join(arm.name())
        .join(format!("{}.log", package.id));
    let view = View {
        dir: view_dir,
        mount,
    };
    let outcome = sandbox::run(
        lab,
        &Step {
            name: package.id.clone(),
            argv: vec!["/bin/sh".to_owned(), "-euc".to_owned(), script],
            inputs: closure.iter().cloned().collect(),
            output: Some(out.clone()),
            view: Some(view.clone()),
            build_dir: build_dir.clone(),
            log: log.clone(),
            env: env.clone(),
            limits,
            links: links.clone(),
        },
    )?;
    if outcome.status != Status::Passed {
        return Err(format!(
            "`{}` {:?} after {} s, see {}",
            package.id,
            outcome.status,
            outcome.milliseconds / 1000,
            log.display()
        ));
    }
    fs::create_dir_all(&done_dir).map_err(with_path(&done_dir))?;
    fs::write(&done, "").map_err(with_path(&done))?;
    Ok(Some(Prepared {
        out,
        closure,
        view,
        env,
        links,
        limits,
        build_dir,
        dep_paths: deps
            .iter()
            .map(|dep| built.paths[&dep.id].clone())
            .collect(),
    }))
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
        let Some(tail) = bytes.get(at + 7..at + 47) else {
            continue;
        };
        if tail.iter().all(u8::is_ascii_hexdigit) {
            let hash = String::from_utf8_lossy(tail).into_owned();
            found.entry(hash).or_insert_with(|| file.to_path_buf());
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
        for hash in references(&lab.on_host(&path))?.into_keys() {
            let next = StorePath::from_hash(&hash);
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
        },
    )?;
    let text = fs::read_to_string(&log).map_err(with_path(&log))?;
    Ok((outcome.status, text))
}

#[cfg(test)]
mod view_tests {
    use super::*;

    fn store(tag: u8) -> StorePath {
        StorePath::derive("test", &[&[tag]])
    }

    fn dep(id: &str) -> Dep {
        Dep {
            id: id.to_owned(),
            view: true,
            rpath: true,
            skip: Vec::new(),
        }
    }

    fn package(listed: &[&str]) -> Package {
        toml::from_str::<Recipes>(&format!(
            "[[package]]\nid = \"app\"\nscope = \"runtime\"\nscript = \"\"\nextra_deps = [{}]\n",
            listed
                .iter()
                .map(|id| format!("\"{id}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .unwrap()
        .package
        .remove(0)
    }

    /// `lib` depends on `base`, and `tool` on nothing.
    fn built(views: Views) -> Built {
        let mut built = Built {
            views,
            ..Built::default()
        };
        for (id, tag) in [("base", 1), ("lib", 2), ("tool", 3), ("m4", 4)] {
            built.paths.insert(id.to_owned(), store(tag));
            built.closures.insert(id.to_owned(), BTreeSet::new());
        }
        built
            .closures
            .insert("lib".to_owned(), BTreeSet::from([store(1), store(4)]));
        built
    }

    fn shown(deps: &[Dep]) -> Vec<(String, bool)> {
        deps.iter().map(|dep| (dep.id.clone(), dep.view)).collect()
    }

    #[test]
    fn declared_views_are_left_alone() {
        let deps = vec![dep("lib")];
        let result = in_views(deps, &package(&["lib"]), &built(Views::Declared)).unwrap();
        assert_eq!(shown(&result), [("lib".to_owned(), true)]);
    }

    #[test]
    fn a_reduced_view_drops_what_another_dependency_pulled_in() {
        let deps = vec![dep("lib"), dep("base"), dep("tool")];
        let result = in_views(
            deps,
            &package(&["lib", "base", "tool"]),
            &built(Views::Reduced),
        )
        .unwrap();
        assert_eq!(
            shown(&result),
            [
                ("lib".to_owned(), true),
                ("base".to_owned(), false),
                ("tool".to_owned(), true)
            ]
        );
    }

    #[test]
    fn a_closure_view_adds_what_dependencies_pulled_in_but_no_bootstrap_tools() {
        let result =
            in_views(vec![dep("lib")], &package(&["lib"]), &built(Views::Closure)).unwrap();
        assert_eq!(
            shown(&result),
            [("lib".to_owned(), true), ("base".to_owned(), true)]
        );
        assert!(!result[1].rpath);
    }
}
