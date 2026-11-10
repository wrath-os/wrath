//! Splits built packages into outputs by file class, on a copy of the store or as they're built.
//!
//! Each package's run-time libraries, development files and documentation go to store paths of
//! their own, of the same length, so references can be rewritten in place. Whatever needs only a
//! package's libraries then stops pulling in the rest of it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use memchr::memmem;

use crate::pilot::{files_under, path_byte};
use crate::runpath;
use crate::slots::to_slot;
use crate::store::{Lab, StorePath, with_path};

const HASH_LENGTH: usize = 40;
const STORE: &[u8] = b"/store/";
const ELF_MAGIC: &[u8] = b"\x7fELF";
const DOC_DIRS: [&str; 5] = [
    "share/doc",
    "share/man",
    "share/info",
    "share/gtk-doc",
    "doc",
];
const DEV_DIRS: [&str; 5] = [
    "include",
    "lib/pkgconfig",
    "share/pkgconfig",
    "share/aclocal",
    "lib/cmake",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Out,
    Lib,
    Dev,
    Doc,
}

const CLASSES: [Class; 4] = [Class::Out, Class::Lib, Class::Dev, Class::Doc];

impl Class {
    /// The name used for a class in store paths and a package's record.
    pub fn name(self) -> &'static str {
        match self {
            Self::Out => "out",
            Self::Lib => "lib",
            Self::Dev => "dev",
            Self::Doc => "doc",
        }
    }
}

/// Which output a path inside a package belongs to.
pub fn classify(relative: &str) -> Class {
    let relative = relative.trim_matches('/');
    let under = |dir: &str| {
        relative == dir
            || relative
                .strip_prefix(dir)
                .is_some_and(|rest| rest.starts_with('/'))
    };
    // The compiler finds these relative to itself.
    if under("include/c++") || under("lib/gcc") || under("lib/clang") {
        return Class::Out;
    }
    // Headers a library keeps beside it, such as glib's `lib/glib-2.0/include/glibconfig.h`,
    // which its pkg-config file names through `libdir` with the other headers.
    if relative
        .strip_prefix("lib/")
        .and_then(|rest| rest.split_once('/'))
        .is_some_and(|(_, rest)| rest.starts_with("include/"))
    {
        return Class::Dev;
    }
    if DOC_DIRS.iter().any(|dir| under(dir)) {
        return Class::Doc;
    }
    if DEV_DIRS.iter().any(|dir| under(dir)) {
        return Class::Dev;
    }
    if let Some(name) = relative.strip_prefix("lib/")
        && !name.contains('/')
    {
        if name.contains(".so") {
            return Class::Lib;
        }
        if name.ends_with(".a") {
            return Class::Dev;
        }
    }
    Class::Out
}

/// The output a reference to `rest` of a package means. A bare `lib` directory is its libraries.
fn reference_class(rest: &str) -> Class {
    if is_lib_dir(rest) {
        Class::Lib
    } else {
        classify(rest)
    }
}

/// A file's output, which for a few files depends on what's in them: `libc.so` and friends are
/// linker scripts, needed only to link and naming static archives.
fn class_of(relative: &str, file: &Path) -> Result<Class, String> {
    let class = classify(relative);
    if class != Class::Lib
        || fs::symlink_metadata(file)
            .map_err(with_path(file))?
            .is_symlink()
    {
        return Ok(class);
    }
    let bytes = fs::read(file).map_err(with_path(file))?;
    let script = !bytes.starts_with(ELF_MAGIC)
        && [&b"GNU ld script"[..], b"GROUP (", b"INPUT ("]
            .iter()
            .any(|mark| memmem::find(&bytes, mark).is_some());
    Ok(if script { Class::Dev } else { class })
}

/// Where each package's outputs go, and which of them have anything in them.
#[derive(Default)]
struct Plan {
    outputs: BTreeMap<String, BTreeMap<Class, (StorePath, bool)>>,
}

impl Plan {
    fn add(&mut self, package: &StorePath, files: impl Iterator<Item = Class>) {
        let mut outputs: BTreeMap<Class, (StorePath, bool)> = CLASSES
            .into_iter()
            .map(|class| {
                let path = match class {
                    Class::Out => package.clone(),
                    _ => output_path(package, class),
                };
                (class, (path, false))
            })
            .collect();
        for class in files {
            if let Some(output) = outputs.get_mut(&class) {
                output.1 = true;
            }
        }
        self.outputs.insert(package.hash().to_owned(), outputs);
    }

    /// The output of `package` that `class` files live in, falling back on the main one.
    fn output(&self, package: &str, class: Class) -> Option<&StorePath> {
        let outputs = self.outputs.get(package)?;
        match outputs.get(&class) {
            Some((path, true)) => Some(path),
            _ => outputs.get(&Class::Out).map(|(path, _)| path),
        }
    }
}

/// Points every reference at the output holding what it names.
fn rewrite(bytes: &mut [u8], plan: &Plan) {
    rewrite_with(bytes, plan, reference_class);
}

/// [`rewrite`], with `wanted` saying which output the rest of a reference after the package means.
fn rewrite_with(bytes: &mut [u8], plan: &Plan, wanted: impl Fn(&str) -> Class) {
    let found: Vec<usize> = memmem::find_iter(bytes, STORE).collect();
    for at in found {
        let start = at + STORE.len();
        // The entry: its hash, and the name after it when it has one.
        let length = bytes[start..]
            .iter()
            .position(|b| *b == b'/' || !path_byte(*b))
            .unwrap_or(bytes.len() - start);
        if length < HASH_LENGTH {
            continue;
        }
        let Ok(hash) = std::str::from_utf8(&bytes[start..start + length]) else {
            continue;
        };
        let rest_at = start + length;
        let rest_length = bytes[rest_at..]
            .iter()
            .position(|b| !path_byte(*b))
            .unwrap_or(bytes.len() - rest_at);
        let Ok(rest) = std::str::from_utf8(&bytes[rest_at..rest_at + rest_length]) else {
            continue;
        };
        if let Some(output) = plan.output(hash, wanted(rest))
            && output.hash().len() == length
        {
            bytes[start..start + length].copy_from_slice(output.hash().as_bytes());
        }
    }
}

/// A run path entry naming a package's `lib` directory means its libraries.
fn rewrite_runpath(bytes: &mut [u8], plan: &Plan) {
    let Some((mut offset, runpath)) = runpath::location(bytes) else {
        return;
    };
    for entry in runpath.split(':') {
        if let Some(hash) = entry
            .strip_prefix("/store/")
            .and_then(|rest| rest.strip_suffix("/lib"))
            && let Some(output) = plan.output(hash, Class::Lib)
            && output.hash().len() == hash.len()
        {
            let start = offset + STORE.len();
            bytes[start..start + hash.len()].copy_from_slice(output.hash().as_bytes());
        }
        offset += entry.len() + 1;
    }
}

/// `target` of a relative link at `relative`, resolved within the package, or `None` if it leaves.
fn resolve(relative: &str, target: &Path) -> Option<String> {
    let mut parts: Vec<String> = Path::new(relative)
        .parent()?
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    for part in target.components() {
        match part {
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

/// The target a link needs once its file and what it points at may be in different outputs.
fn retarget(plan: &Plan, package: &str, relative: &str, target: &Path) -> PathBuf {
    if target.is_absolute() {
        let mut bytes = target.to_string_lossy().into_owned().into_bytes();
        rewrite(&mut bytes, plan);
        return PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
    }
    let Some(resolved) = resolve(relative, target) else {
        return target.to_path_buf();
    };
    let to = classify(&resolved);
    if classify(relative) == to {
        return target.to_path_buf();
    }
    match plan.output(package, to) {
        Some(output) => PathBuf::from(format!("{}/{resolved}", output.inside())),
        None => target.to_path_buf(),
    }
}

fn relative_of(root: &Path, file: &Path) -> Result<String, String> {
    file.strip_prefix(root)
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|e| format!("{}: {e}", file.display()))
}

fn place(destination: &Path) -> Result<(), String> {
    let parent = destination.parent().ok_or("a path with no parent")?;
    fs::create_dir_all(parent).map_err(with_path(parent))
}

/// Bytes moved into each class, across everything split.
pub type Sizes = BTreeMap<&'static str, u64>;

/// Copies `packages` from `from` into `to`, split into outputs.
pub fn split(from: &Lab, to: &Lab, packages: &[StorePath]) -> Result<Sizes, String> {
    let mut plan = Plan::default();
    let mut listings = Vec::new();
    for package in packages {
        let root = from.on_host(package);
        let mut entries = Vec::new();
        for file in files_under(&root)? {
            let relative = relative_of(&root, &file)?;
            let class = class_of(&relative, &file)?;
            entries.push((file, relative, class));
        }
        plan.add(package, entries.iter().map(|(_, _, class)| *class));
        listings.push((package, entries));
    }
    let mut sizes = Sizes::new();
    for (package, entries) in &listings {
        let hash = package.hash();
        for (file, relative, class) in entries {
            let output = plan.output(hash, *class).ok_or("a package with no plan")?;
            let destination = to.on_host(output).join(relative);
            place(&destination)?;
            let meta = fs::symlink_metadata(file).map_err(with_path(file))?;
            if meta.is_symlink() {
                let target = fs::read_link(file).map_err(with_path(file))?;
                symlink(retarget(&plan, hash, relative, &target), &destination)
                    .map_err(with_path(&destination))?;
                continue;
            }
            let mut bytes = fs::read(file).map_err(with_path(file))?;
            rewrite(&mut bytes, &plan);
            rewrite_runpath(&mut bytes, &plan);
            *sizes.entry(class.name()).or_default() += bytes.len() as u64;
            fs::write(&destination, &bytes).map_err(with_path(&destination))?;
            fs::set_permissions(&destination, meta.permissions())
                .map_err(with_path(&destination))?;
        }
        // Whatever looks in the package's own lib directory by name still finds its libraries.
        let lib = plan
            .output(hash, Class::Lib)
            .ok_or("a package with no plan")?;
        if lib != *package {
            for (_, relative, _) in entries.iter().filter(|(_, _, class)| *class == Class::Lib) {
                let link = to.on_host(package).join(relative);
                place(&link)?;
                symlink(format!("{}/{relative}", lib.inside()), &link).map_err(with_path(&link))?;
            }
        }
    }
    Ok(sizes)
}

/// Removes debug information from every ELF file in `packages`, which otherwise records the
/// compiler's include directories and keeps the whole compiler referenced.
pub fn strip(lab: &Lab, packages: &BTreeSet<StorePath>) -> Result<usize, String> {
    let mut stripped = 0;
    for package in packages {
        for file in files_under(&lab.on_host(package))? {
            let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
            if !meta.is_file()
                || !fs::read(&file)
                    .map_err(with_path(&file))?
                    .starts_with(ELF_MAGIC)
            {
                continue;
            }
            let mode = meta.permissions().mode();
            fs::set_permissions(&file, fs::Permissions::from_mode(mode | 0o200))
                .map_err(with_path(&file))?;
            let status = Command::new("strip")
                .arg("--strip-debug")
                .arg(&file)
                .stderr(Stdio::null())
                .status()
                .map_err(with_path(&file))?;
            fs::set_permissions(&file, fs::Permissions::from_mode(mode))
                .map_err(with_path(&file))?;
            if status.success() {
                stripped += 1;
            }
        }
    }
    Ok(stripped)
}

/// Where a package's run-time libraries go when it has any.
pub fn lib_output(package: &StorePath) -> StorePath {
    output_path(package, Class::Lib)
}

/// Where `package`'s `class` of files goes, with the package's name, so a reference to the
/// package becomes one to the output by rewriting its hash alone.
fn output_path(package: &StorePath, class: Class) -> StorePath {
    let path = StorePath::derive(
        "output",
        &[package.hash().as_bytes(), class.name().as_bytes()],
    );
    match package.name() {
        Some(name) => path.named(name),
        None => path,
    }
}

/// Points every program in `paths` at library slots: `/system/libs/<id>/...` for what's in
/// `packages`, by hash.
pub fn use_slots(
    lab: &Lab,
    paths: &BTreeSet<StorePath>,
    packages: &BTreeMap<String, String>,
    write: impl Fn(&Path, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    for path in paths {
        for file in files_under(&lab.on_host(path))? {
            let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
            if !meta.is_file() {
                continue;
            }
            let mut bytes = fs::read(&file).map_err(with_path(&file))?;
            let old = bytes.clone();
            runpath::rename_libraries(&mut bytes, &|entry| to_slot(entry, packages));
            if bytes != old {
                write(&file, &bytes)?;
            }
        }
    }
    Ok(())
}

/// Needed libraries that no run path entry provides, across every ELF file in `packages`.
pub fn unresolved(lab: &Lab, packages: &BTreeSet<StorePath>) -> Result<Vec<String>, String> {
    let mut missing = Vec::new();
    for package in packages {
        for file in files_under(&lab.on_host(package))? {
            let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
            if !meta.is_file() {
                continue;
            }
            let bytes = fs::read(&file).map_err(with_path(&file))?;
            for name in runpath::unresolved(lab, &bytes) {
                missing.push(format!("{}: {name}", relative_of(&lab.work, &file)?));
            }
        }
    }
    Ok(missing)
}

/// The outputs of a package split as it's built.
pub type Outputs = BTreeMap<Class, StorePath>;

/// The order outputs are hashed in. A file may only name what's at or before its own place, so
/// that each output's hash can be known before anything that refers to it.
fn rank(class: Class) -> u8 {
    match class {
        Class::Lib => 0,
        Class::Out => 1,
        Class::Doc => 2,
        Class::Dev => 3,
    }
}

/// The class called `name` in a package's record.
pub fn class_named(name: &str) -> Option<Class> {
    CLASSES.into_iter().find(|class| class.name() == name)
}

fn is_bare(rest: &str) -> bool {
    matches!(rest, "" | "/")
}

fn is_lib_dir(rest: &str) -> bool {
    matches!(rest, "/lib" | "/lib/")
}

/// Adds the paths inside package `own` that `bytes` name, each as the rest after the package.
fn own_references(bytes: &[u8], own: &str, names: &mut BTreeSet<String>) {
    let needle = format!("/store/{own}");
    for at in memmem::find_iter(bytes, needle.as_bytes()) {
        let rest_at = at + needle.len();
        let length = bytes[rest_at..]
            .iter()
            .position(|b| !path_byte(*b))
            .unwrap_or(bytes.len() - rest_at);
        names.insert(String::from_utf8_lossy(&bytes[rest_at..rest_at + length]).into_owned());
    }
}

/// Which output each of a package's files goes to.
struct Placement {
    class: BTreeMap<String, Class>,
    /// Every file and directory, with the files it stands for.
    files: BTreeMap<String, Vec<String>>,
}

impl Placement {
    fn new(class: BTreeMap<String, Class>) -> Self {
        let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in class.keys() {
            let mut name = file.as_str();
            loop {
                files.entry(name.to_owned()).or_default().push(file.clone());
                match name.rsplit_once('/') {
                    Some((parent, _)) => name = parent,
                    None => break,
                }
            }
        }
        Self { class, files }
    }

    /// The files `rest` stands for. A name that isn't there is often the start of one that is,
    /// such as `share/misc/magic` for `magic.mgc`, so it stands for its directory's files unless
    /// that's a top-level one like `lib`.
    fn named(&self, rest: &str) -> &[String] {
        let name = rest.trim_matches('/');
        self.files
            .get(name)
            .or_else(|| {
                let (parent, _) = name.rsplit_once('/')?;
                if parent.contains('/') {
                    self.files.get(parent)
                } else {
                    None
                }
            })
            .map_or(&[], Vec::as_slice)
    }

    /// A program moved in with the libraries means there's nothing left to split off.
    fn program_in_lib(&self) -> Option<&str> {
        self.class.iter().find_map(|(file, class)| {
            let program = file.starts_with("bin/") || file.starts_with("sbin/");
            (program && *class == Class::Lib).then_some(file.as_str())
        })
    }

    /// Moves files until nothing names anything hashed after it. What a file names goes with it,
    /// so a library keeps its data. Returns why that's impossible, if it is.
    fn settle(&mut self, references: &BTreeMap<String, BTreeSet<String>>) -> Option<String> {
        loop {
            let mut moved = false;
            for (from, names) in references {
                for rest in names {
                    let class = self.class[from];
                    if class == Class::Lib && is_bare(rest) {
                        return Some(format!("lib names the whole package ({from})"));
                    }
                    if is_lib_dir(rest) {
                        continue;
                    }
                    let later: Vec<String> = self
                        .named(rest)
                        .iter()
                        .filter(|file| rank(self.class[*file]) > rank(class))
                        .cloned()
                        .collect();
                    if later.is_empty() {
                        continue;
                    }
                    moved = true;
                    if class == Class::Doc {
                        // Moving a header into `doc` would hide it from builds.
                        self.class.insert(from.clone(), Class::Dev);
                        continue;
                    }
                    for file in later {
                        self.class.insert(file, class);
                    }
                }
            }
            if !moved {
                return self
                    .program_in_lib()
                    .map(|file| format!("lib names a program ({file})"));
            }
        }
    }

    /// The output that a reference to `rest` in a file of class `from` means. In `dev`, a bare
    /// package path is a `.pc` file's `prefix`, and its `${prefix}/include` is in `dev` too.
    fn wanted(&self, from: Class, rest: &str) -> Class {
        if from == Class::Dev && is_bare(rest) {
            return Class::Dev;
        }
        if is_lib_dir(rest) {
            return Class::Lib;
        }
        self.holding(from, rest).unwrap_or_else(|| {
            let class = reference_class(rest);
            if rank(class) > rank(from) {
                from
            } else {
                class
            }
        })
    }

    /// [`Self::wanted`] for a relative link to `resolved`.
    fn linked(&self, from: Class, resolved: &str) -> Class {
        self.holding(from, resolved)
            .unwrap_or_else(|| classify(resolved))
    }

    /// The output holding the files `rest` stands for, if any: `from` when it has some of them,
    /// or else the earliest hashed.
    fn holding(&self, from: Class, rest: &str) -> Option<Class> {
        let classes = || self.named(rest).iter().map(|file| self.class[file]);
        let earliest = classes().min_by_key(|class| rank(*class))?;
        if classes().any(|class| class == from) {
            Some(from)
        } else {
            Some(earliest)
        }
    }

    /// The target a link at `relative` needs now that its file and target may be in different
    /// outputs.
    fn retarget(&self, plan: &Plan, package: &str, relative: &str, target: &Path) -> PathBuf {
        let from = self.class[relative];
        if target.is_absolute() {
            let mut bytes = target.to_string_lossy().into_owned().into_bytes();
            rewrite_with(&mut bytes, plan, |rest| self.wanted(from, rest));
            return PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
        }
        let Some(resolved) = resolve(relative, target) else {
            return target.to_path_buf();
        };
        let to = self.linked(from, &resolved);
        if to == from {
            return target.to_path_buf();
        }
        match plan.output(package, to) {
            Some(output) => PathBuf::from(format!("{}/{resolved}", output.inside())),
            None => target.to_path_buf(),
        }
    }
}

/// Keeps a built package as one content-addressed output, for `reason`.
pub fn keep_whole(
    lab: &Lab,
    package: &StorePath,
    reason: String,
) -> Result<(Outputs, Option<String>), String> {
    let address = crate::content::address(lab, package)?;
    Ok((BTreeMap::from([(Class::Out, address)]), Some(reason)))
}

/// Splits a built package in place, into content-addressed outputs. Returns them and, if the
/// package had to stay whole, why.
pub fn split_package(lab: &Lab, package: &StorePath) -> Result<(Outputs, Option<String>), String> {
    let root = lab.on_host(package);
    let own = package.hash();
    let mut entries = Vec::new();
    for file in files_under(&root)? {
        let relative = relative_of(&root, &file)?;
        let class = class_of(&relative, &file)?;
        entries.push((file, relative, class));
    }
    let mut references: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (file, relative, _) in &entries {
        let mut names = BTreeSet::new();
        let meta = fs::symlink_metadata(file).map_err(with_path(file))?;
        if meta.is_symlink() {
            let target = fs::read_link(file).map_err(with_path(file))?;
            own_references(target.to_string_lossy().as_bytes(), own, &mut names);
            if !target.is_absolute()
                && let Some(resolved) = resolve(relative, &target)
            {
                names.insert(format!("/{resolved}"));
            }
        } else if meta.is_file() {
            own_references(&fs::read(file).map_err(with_path(file))?, own, &mut names);
        }
        if !names.is_empty() {
            references.insert(relative.clone(), names);
        }
    }

    let mut placement = Placement::new(
        entries
            .iter()
            .map(|(_, relative, class)| (relative.clone(), *class))
            .collect(),
    );
    if let Some(reason) = placement.settle(&references) {
        return keep_whole(lab, package, reason);
    }

    let mut plan = Plan::default();
    plan.add(package, placement.class.values().copied());

    for (file, relative, _) in &entries {
        let class = placement.class[relative];
        let output = plan.output(own, class).ok_or("a package with no plan")?;
        let meta = fs::symlink_metadata(file).map_err(with_path(file))?;
        let destination = lab.on_host(output).join(relative);
        if meta.is_symlink() {
            let target = fs::read_link(file).map_err(with_path(file))?;
            let new = placement.retarget(&plan, own, relative, &target);
            fs::remove_file(file).map_err(with_path(file))?;
            place(&destination)?;
            symlink(new, &destination).map_err(with_path(&destination))?;
            continue;
        }
        let mut bytes = fs::read(file).map_err(with_path(file))?;
        rewrite_with(&mut bytes, &plan, |rest| placement.wanted(class, rest));
        if relative.ends_with(".pc") {
            bytes = programs_in(&bytes, &package.inside());
        }
        if output == package {
            fs::set_permissions(
                file,
                fs::Permissions::from_mode(meta.permissions().mode() | 0o200),
            )
            .map_err(with_path(file))?;
            fs::write(file, &bytes).map_err(with_path(file))?;
            fs::set_permissions(file, meta.permissions()).map_err(with_path(file))?;
        } else {
            fs::remove_file(file).map_err(with_path(file))?;
            place(&destination)?;
            fs::write(&destination, &bytes).map_err(with_path(&destination))?;
            fs::set_permissions(&destination, meta.permissions())
                .map_err(with_path(&destination))?;
        }
    }

    remove_empty_dirs(&root)?;

    let mut order: Vec<Class> = CLASSES.to_vec();
    order.sort_by_key(|class| rank(*class));
    let temporary: Vec<(Class, StorePath)> = order
        .into_iter()
        .filter_map(|class| {
            let (path, used) = plan.outputs[own].get(&class)?;
            (*used || class == Class::Out).then(|| (class, path.clone()))
        })
        .collect();
    let mut outputs = Outputs::new();
    for (index, (class, path)) in temporary.iter().enumerate() {
        let dir = lab.on_host(path);
        fs::create_dir_all(&dir).map_err(with_path(&dir))?;
        let address = crate::content::address(lab, path)?;
        for (_, later) in &temporary[index + 1..] {
            let dir = lab.on_host(later);
            if dir.exists() {
                crate::content::rewrite_contents(&dir, path.hash(), address.hash())?;
            }
        }
        outputs.insert(*class, address);
    }
    Ok((outputs, None))
}

/// A pkg-config file whose program directories, `bindir`, `sbindir` and `libexecdir`, name
/// `programs`: its `prefix` is the output with headers, and the programs aren't there.
fn programs_in(bytes: &[u8], programs: &str) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return bytes.to_vec();
    };
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let fixed = ["bindir=", "sbindir=", "libexecdir="]
            .iter()
            .find_map(|key| {
                let rest = line.strip_prefix(key)?;
                let rest = rest
                    .strip_prefix("${exec_prefix}")
                    .or_else(|| rest.strip_prefix("${prefix}"))?;
                Some(format!("{key}{programs}{rest}"))
            });
        out.push_str(fixed.as_deref().unwrap_or(line));
    }
    out.into_bytes()
}

/// Removes the directories below `root` that moving files out of left empty.
fn remove_empty_dirs(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root).map_err(with_path(root))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if fs::symlink_metadata(&path)
            .map_err(with_path(&path))?
            .is_dir()
        {
            remove_empty_dirs(&path)?;
            if fs::read_dir(&path)
                .map_err(with_path(&path))?
                .next()
                .is_none()
            {
                fs::remove_dir(&path).map_err(with_path(&path))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_where_files_live() {
        for (path, class) in [
            ("lib/libz.so.1", Class::Lib),
            ("lib/libz.so", Class::Lib),
            ("lib/libz.a", Class::Dev),
            ("lib/pkgconfig/zlib.pc", Class::Dev),
            ("include/zlib.h", Class::Dev),
            ("include/c++/15/vector", Class::Out),
            ("share/man/man1/x.1", Class::Doc),
            ("bin/ls", Class::Out),
            ("lib/systemd/libsystemd-shared.so", Class::Out),
            ("lib/gcc/x/13/cc1", Class::Out),
            ("lib/gcc/x/13/include/stddef.h", Class::Out),
            ("lib/glib-2.0/include/glibconfig.h", Class::Dev),
            ("lib/dbus-1.0/include/dbus/dbus-arch-deps.h", Class::Dev),
            ("lib", Class::Out),
        ] {
            assert_eq!(classify(path), class, "{path}");
        }
    }

    fn plan() -> (Plan, String, String, String) {
        let package = StorePath::derive("test", &[b"package"]);
        let mut plan = Plan::default();
        plan.add(&package, [Class::Out, Class::Lib, Class::Dev].into_iter());
        let hash = package.hash().to_owned();
        let lib = plan.output(&hash, Class::Lib).unwrap().hash().to_owned();
        let dev = plan.output(&hash, Class::Dev).unwrap().hash().to_owned();
        (plan, hash, lib, dev)
    }

    #[test]
    fn sends_references_to_the_output_holding_the_file() {
        let (plan, hash, lib, dev) = plan();
        let text = format!(
            "/store/{hash}/lib/libz.so.1 /store/{hash}/include/z.h /store/{hash}/lib \
             /store/{hash}/bin/x"
        );
        let mut bytes = text.clone().into_bytes();
        rewrite(&mut bytes, &plan);
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            format!(
                "/store/{lib}/lib/libz.so.1 /store/{dev}/include/z.h /store/{lib}/lib \
                 /store/{hash}/bin/x"
            )
        );
    }

    #[test]
    fn an_empty_output_keeps_references_in_the_main_one() {
        let (plan, hash, ..) = plan();
        let text = format!("/store/{hash}/share/man/man1/x.1");
        let mut bytes = text.clone().into_bytes();
        rewrite(&mut bytes, &plan);
        assert_eq!(String::from_utf8(bytes).unwrap(), text);
    }

    #[test]
    fn leaves_other_store_paths_alone() {
        let (plan, ..) = plan();
        let text = format!("/store/{}/lib/libz.so.1", "a".repeat(HASH_LENGTH));
        let mut bytes = text.clone().into_bytes();
        rewrite(&mut bytes, &plan);
        assert_eq!(String::from_utf8(bytes).unwrap(), text);
    }

    #[test]
    fn a_link_across_outputs_becomes_absolute() {
        let (plan, hash, lib, _) = plan();
        let target = retarget(&plan, &hash, "bin/tool", Path::new("../lib/libz.so.1"));
        assert_eq!(target, PathBuf::from(format!("/store/{lib}/lib/libz.so.1")));
    }

    #[test]
    fn a_link_within_an_output_stays_relative() {
        let (plan, hash, ..) = plan();
        let target = retarget(&plan, &hash, "lib/libz.so", Path::new("libz.so.1"));
        assert_eq!(target, PathBuf::from("libz.so.1"));
    }

    #[test]
    fn a_link_leaving_the_package_is_untouched() {
        let (plan, hash, ..) = plan();
        let target = retarget(&plan, &hash, "bin/tool", Path::new("../../../etc/x"));
        assert_eq!(target, PathBuf::from("../../../etc/x"));
    }
}

#[cfg(test)]
mod in_place {
    use super::*;

    fn lab() -> (tempfile::TempDir, Lab) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("store")).unwrap();
        let lab = Lab::new(dir.path().to_str().unwrap());
        (dir, lab)
    }

    fn built(lab: &Lab, files: &[(&str, String)]) -> StorePath {
        let all: String = files
            .iter()
            .map(|(name, text)| format!("{name}{text}"))
            .collect();
        let package = StorePath::derive("test", &[all.as_bytes()]);
        for (name, text) in files {
            let path = lab.on_host(&package).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        package
    }

    fn package_with(lab: &Lab, library_text: impl Fn(&str) -> String) -> StorePath {
        let seed = StorePath::derive("seed", &[library_text("x").as_bytes()]);
        let own = seed.inside();
        let package = seed;
        for (name, text) in [
            ("lib/libz.so.1".to_owned(), library_text(&own)),
            (
                "bin/z".to_owned(),
                format!("#!{own}/lib/libz.so.1 {own}/share/x\n"),
            ),
            ("share/x".to_owned(), "data\n".to_owned()),
            ("include/z.h".to_owned(), format!("// {own}/bin/z\n")),
            ("share/man/man1/z.1".to_owned(), "docs\n".to_owned()),
        ] {
            let path = lab.on_host(&package).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        package
    }

    #[test]
    fn files_go_to_their_outputs_and_references_point_at_them() {
        let (_dir, lab) = lab();
        let package = package_with(&lab, |_| "library\n".to_owned());
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(whole, None);
        assert!(
            lab.on_host(&outputs[&Class::Lib])
                .join("lib/libz.so.1")
                .exists()
        );
        assert!(
            lab.on_host(&outputs[&Class::Dev])
                .join("include/z.h")
                .exists()
        );
        assert!(
            lab.on_host(&outputs[&Class::Doc])
                .join("share/man/man1/z.1")
                .exists()
        );
        let program = fs::read_to_string(lab.on_host(&outputs[&Class::Out]).join("bin/z")).unwrap();
        assert!(
            program.contains(&outputs[&Class::Lib].inside()),
            "{program}"
        );
        assert!(
            program.contains(&format!("{}/share/x", outputs[&Class::Out].inside())),
            "{program}"
        );
        let header =
            fs::read_to_string(lab.on_host(&outputs[&Class::Dev]).join("include/z.h")).unwrap();
        assert!(header.contains(&outputs[&Class::Out].inside()), "{header}");
    }

    #[test]
    fn a_named_package_splits_into_outputs_with_its_name() {
        let (_dir, lab) = lab();
        let package = StorePath::derive("seed", &[b"z"]).named("zlib-1.3.1");
        let own = package.inside();
        for (name, text) in [
            ("lib/libz.so.1".to_owned(), "library\n".to_owned()),
            (
                "bin/z".to_owned(),
                format!("#!{own}/lib/libz.so.1 {own}/share/x\n"),
            ),
            ("share/x".to_owned(), "data\n".to_owned()),
        ] {
            let path = lab.on_host(&package).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let (outputs, _) = split_package(&lab, &package).unwrap();
        for output in outputs.values() {
            assert_eq!(output.name(), Some("zlib-1.3.1"), "{output:?}");
        }
        let program = fs::read_to_string(lab.on_host(&outputs[&Class::Out]).join("bin/z")).unwrap();
        let lib = &outputs[&Class::Lib];
        assert!(
            program.contains(&format!("{}/lib/libz.so.1", lib.inside())),
            "{program}"
        );
        assert!(!program.contains(&own), "{program}");
    }

    #[test]
    fn a_library_keeps_the_data_it_names() {
        let (_dir, lab) = lab();
        let package = package_with(&lab, |own| format!("uses {own}/share/x\n"));
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(whole, None);
        let lib = lab.on_host(&outputs[&Class::Lib]);
        assert!(lib.join("share/x").exists());
        let wanted = format!("{}/share/x", outputs[&Class::Lib].inside());
        let library = fs::read_to_string(lib.join("lib/libz.so.1")).unwrap();
        assert!(library.contains(&wanted), "{library}");
        let program = fs::read_to_string(lab.on_host(&outputs[&Class::Out]).join("bin/z")).unwrap();
        assert!(program.contains(&wanted), "{program}");
    }

    #[test]
    fn what_a_moved_file_names_moves_too() {
        let (_dir, lab) = lab();
        let package = StorePath::derive("test", &[b"chain"]);
        let own = package.inside();
        for (name, text) in [
            ("lib/libz.so.1", format!("{own}/share/a\n")),
            ("share/a", format!("{own}/share/b\n")),
            ("share/b", "end\n".to_owned()),
            ("bin/z", "program\n".to_owned()),
        ] {
            let path = lab.on_host(&package).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(whole, None);
        let lib = lab.on_host(&outputs[&Class::Lib]);
        assert!(lib.join("share/a").exists());
        assert!(lib.join("share/b").exists());
        assert!(!lib.join("bin/z").exists());
    }

    #[test]
    fn naming_a_directory_brings_its_files() {
        let (_dir, lab) = lab();
        let package = StorePath::derive("test", &[b"dir"]);
        let own = package.inside();
        for (name, text) in [
            ("lib/libz.so.1", format!("{own}/share/d\n")),
            ("share/d/one", "1\n".to_owned()),
            ("share/d/two/three", "3\n".to_owned()),
            ("share/other", "elsewhere\n".to_owned()),
        ] {
            let path = lab.on_host(&package).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let (outputs, _) = split_package(&lab, &package).unwrap();
        let lib = lab.on_host(&outputs[&Class::Lib]);
        assert!(lib.join("share/d/one").exists());
        assert!(lib.join("share/d/two/three").exists());
        assert!(!lib.join("share/other").exists());
    }

    #[test]
    fn a_library_naming_something_missing_points_at_itself() {
        let (_dir, lab) = lab();
        let package = package_with(&lab, |own| format!("reads {own}/etc/none.conf\n"));
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(whole, None);
        let library = lab.on_host(&outputs[&Class::Lib]).join("lib/libz.so.1");
        let text = fs::read_to_string(library).unwrap();
        let wanted = format!("{}/etc/none.conf", outputs[&Class::Lib].inside());
        assert!(text.contains(&wanted), "{text}");
    }

    #[test]
    fn a_library_naming_the_bare_package_keeps_it_whole() {
        let (_dir, lab) = lab();
        let package = package_with(&lab, |own| format!("prefix {own}\n"));
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(whole.unwrap().contains("whole package"));
        assert!(
            lab.on_host(&outputs[&Class::Out])
                .join("lib/libz.so.1")
                .exists()
        );
    }

    #[test]
    fn a_library_naming_a_program_keeps_the_package_whole() {
        let (_dir, lab) = lab();
        let package = StorePath::derive("test", &[b"program"]);
        let own = package.inside();
        for (name, text) in [
            ("lib/libz.so.1", format!("runs {own}/bin/helper\n")),
            ("bin/helper", "program\n".to_owned()),
        ] {
            let path = lab.on_host(&package).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(outputs.len(), 1);
        assert!(whole.unwrap().contains("names a program"));
        let out = lab.on_host(&outputs[&Class::Out]);
        assert!(out.join("bin/helper").exists());
        assert!(out.join("lib/libz.so.1").exists());
    }

    #[test]
    fn a_name_that_is_the_start_of_a_file_brings_its_directory() {
        let (_dir, lab) = lab();
        let package = StorePath::derive("test", &[b"prefix"]);
        let own = package.inside();
        for (name, text) in [
            ("lib/libz.so.1", format!("opens {own}/share/misc/magic\n")),
            ("share/misc/magic.mgc", "data\n".to_owned()),
            ("share/other", "elsewhere\n".to_owned()),
        ] {
            let path = lab.on_host(&package).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(whole, None);
        let lib = lab.on_host(&outputs[&Class::Lib]);
        assert!(lib.join("share/misc/magic.mgc").exists());
        assert!(!lib.join("share/other").exists());
        let text = fs::read_to_string(lib.join("lib/libz.so.1")).unwrap();
        let wanted = format!("{}/share/misc/magic", outputs[&Class::Lib].inside());
        assert!(text.contains(&wanted), "{text}");
    }

    #[test]
    fn a_document_naming_a_header_goes_with_the_headers() {
        let (_dir, lab) = lab();
        let package = StorePath::derive("test", &[b"doc"]);
        let own = package.inside();
        for (name, text) in [
            ("share/doc/z/api.txt", format!("see {own}/include/z.h\n")),
            ("share/doc/z/other.txt", "plain\n".to_owned()),
            ("include/z.h", "// z\n".to_owned()),
        ] {
            let path = lab.on_host(&package).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(whole, None);
        let dev = lab.on_host(&outputs[&Class::Dev]);
        assert!(dev.join("include/z.h").exists());
        assert!(dev.join("share/doc/z/api.txt").exists());
        assert!(
            lab.on_host(&outputs[&Class::Doc])
                .join("share/doc/z/other.txt")
                .exists()
        );
    }

    #[test]
    fn a_link_from_a_library_brings_its_target() {
        let (_dir, lab) = lab();
        let package = StorePath::derive("test", &[b"link"]);
        let target = lab.on_host(&package).join("share/real");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "data\n").unwrap();
        let link = lab.on_host(&package).join("lib/libz.so.1");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink("../share/real", &link).unwrap();
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(whole, None);
        let lib = lab.on_host(&outputs[&Class::Lib]);
        assert!(lib.join("share/real").exists());
        assert_eq!(
            fs::read_link(lib.join("lib/libz.so.1")).unwrap(),
            PathBuf::from("../share/real")
        );
    }

    #[test]
    fn a_pkgconfig_prefix_means_the_dev_output() {
        let (_dir, lab) = lab();
        let package = StorePath::derive("test", &[b"pc"]);
        let own = package.inside();
        let pc = lab.on_host(&package).join("lib/pkgconfig/z.pc");
        fs::create_dir_all(pc.parent().unwrap()).unwrap();
        fs::write(
            &pc,
            format!("prefix={own}\nincludedir=${{prefix}}/include\n"),
        )
        .unwrap();
        let header = lab.on_host(&package).join("include/z.h");
        fs::create_dir_all(header.parent().unwrap()).unwrap();
        fs::write(header, "// z\n").unwrap();
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(whole, None);
        let dev = lab.on_host(&outputs[&Class::Dev]);
        let text = fs::read_to_string(dev.join("lib/pkgconfig/z.pc")).unwrap();
        assert!(
            text.contains(&format!("prefix={}\n", outputs[&Class::Dev].inside())),
            "{text}"
        );
    }

    #[test]
    fn a_pkgconfig_file_finds_the_programs_in_the_main_output() {
        let (_dir, lab) = lab();
        let package = StorePath::derive("test", &[b"tools"]).named("glib-2.84.4");
        let own = package.inside();
        let files = [
            (
                "lib/pkgconfig/glib.pc",
                format!(
                    "prefix={own}\nexec_prefix=${{prefix}}\nbindir=${{exec_prefix}}/bin\n\
                     libexecdir=${{prefix}}/libexec\nincludedir=${{prefix}}/include\n\
                     glib_mkenums=${{bindir}}/glib-mkenums\n"
                ),
            ),
            ("include/glib.h", "// glib\n".to_owned()),
            ("bin/glib-mkenums", "#!/bin/sh\n".to_owned()),
        ];
        for (name, text) in files {
            let path = lab.on_host(&package).join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let (outputs, _) = split_package(&lab, &package).unwrap();
        let dev = lab.on_host(&outputs[&Class::Dev]);
        let text = fs::read_to_string(dev.join("lib/pkgconfig/glib.pc")).unwrap();
        let out = outputs[&Class::Out].inside();
        assert!(text.contains(&format!("bindir={out}/bin\n")), "{text}");
        assert!(
            text.contains(&format!("libexecdir={out}/libexec\n")),
            "{text}"
        );
        let dev_path = outputs[&Class::Dev].inside();
        assert!(text.contains(&format!("prefix={dev_path}\n")), "{text}");
    }

    #[test]
    fn a_library_only_package_has_an_empty_out_and_a_lib() {
        let (_dir, lab) = lab();
        let package = built(&lab, &[("lib/libq.so.1", "library\n".to_owned())]);
        let (outputs, whole) = split_package(&lab, &package).unwrap();
        assert_eq!(whole, None);
        assert_eq!(
            fs::read_dir(lab.on_host(&outputs[&Class::Out]))
                .unwrap()
                .count(),
            0
        );
        assert!(
            lab.on_host(&outputs[&Class::Lib])
                .join("lib/libq.so.1")
                .exists()
        );
    }

    #[test]
    fn the_same_library_in_two_packages_gets_the_same_output() {
        let (_dir, lab) = lab();
        let one = built(
            &lab,
            &[
                ("lib/libq.so.1", "same\n".to_owned()),
                ("a", "1".to_owned()),
            ],
        );
        let two = built(
            &lab,
            &[
                ("lib/libq.so.1", "same\n".to_owned()),
                ("b", "2".to_owned()),
            ],
        );
        assert_ne!(one, two);
        let (first, _) = split_package(&lab, &one).unwrap();
        let (second, _) = split_package(&lab, &two).unwrap();
        assert_eq!(first[&Class::Lib], second[&Class::Lib]);
        assert_ne!(first[&Class::Out], second[&Class::Out]);
    }
}
