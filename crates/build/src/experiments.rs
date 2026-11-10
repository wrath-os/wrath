//! Steps 5 and 6: how many packages reproduce, and how far a change spreads under each way of
//! computing keys.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use memchr::memmem;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::chain::{self, Arm, Built, Package, Prepared, Sources, Views};
use crate::pilot::{self, Fixups, Leaks};
use crate::sandbox::Identity;
use crate::seed;
use crate::split::{Class, Outputs};
use crate::store::{Lab, StorePath, hex, with_path};

const HASH_LENGTH: usize = 40;

/// Replaces the hash in every store path, so a tree compares equal however its dependencies are
/// addressed.
pub fn mask_references(bytes: &[u8]) -> Vec<u8> {
    let finder = memmem::Finder::new(b"/store/");
    let mut masked = bytes.to_vec();
    for at in finder.find_iter(bytes) {
        let start = at + "/store/".len();
        let Some(tail) = bytes.get(start..start + HASH_LENGTH) else {
            continue;
        };
        if tail.iter().all(u8::is_ascii_hexdigit) {
            masked[start..start + HASH_LENGTH].fill(b'0');
        }
    }
    masked
}

/// Each file of a tree with what identifies its contents, raw and with references masked.
fn tree(dir: &Path) -> Result<BTreeMap<String, (String, String)>, String> {
    let digest = |bytes: &[u8]| hex(&Sha256::digest(bytes));
    let mut found = BTreeMap::new();
    for file in pilot::files_under(dir)? {
        let relative = file
            .strip_prefix(dir)
            .unwrap_or(&file)
            .display()
            .to_string();
        let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
        let (kind, bytes) = if meta.is_symlink() {
            let target = fs::read_link(&file).map_err(with_path(&file))?;
            ("link", target.into_os_string().into_encoded_bytes())
        } else {
            let bytes = fs::read(&file).map_err(with_path(&file))?;
            let executable = meta.permissions().mode() & 0o111 != 0;
            (if executable { "exec" } else { "file" }, bytes)
        };
        let raw = format!("{kind}:{}", digest(&bytes));
        let masked = format!("{kind}:{}", digest(&mask_references(&bytes)));
        // A path can be in a file's name too, as glib's gdb scripts are.
        let name = String::from_utf8_lossy(&mask_references(relative.as_bytes())).into_owned();
        found.insert(name, (raw, masked));
    }
    Ok(found)
}

fn fold(entries: impl Iterator<Item = String>) -> String {
    let mut hasher = Sha256::new();
    for entry in entries {
        hasher.update(entry.as_bytes());
        hasher.update([0]);
    }
    hex(&hasher.finalize())
}

/// One hash for a whole tree, raw and with references masked.
fn tree_hashes(dir: &Path) -> Result<(String, String), String> {
    let found = tree(dir)?;
    let raw = fold(found.iter().map(|(path, (raw, _))| format!("{path} {raw}")));
    let masked = fold(
        found
            .iter()
            .map(|(path, (_, masked))| format!("{path} {masked}")),
    );
    Ok((raw, masked))
}

pub(crate) fn all_packages(repository: &Path) -> Result<Vec<Package>, String> {
    let mut packages = Vec::new();
    for file in ["toolchain.toml", "pilot.toml", "full.toml", "rust.toml"] {
        packages.extend(chain::load(repository, file)?);
    }
    Ok(packages)
}

fn fix_up_if_needed(
    lab: &Lab,
    arm: Arm,
    package: &Package,
    prepared: &Prepared,
) -> Result<(), String> {
    if package.fixed_up() {
        let (mut fixups, mut leaks) = (Fixups::default(), Leaks::default());
        pilot::fix_up(lab, arm, prepared, &mut fixups, &mut leaks)?;
    }
    Ok(())
}

#[derive(Debug)]
struct Reproduction {
    package: String,
    status: String,
    differing_files: usize,
    samples: Vec<String>,
}

impl Reproduction {
    fn new(package: &Package) -> Self {
        Self {
            package: package.id.clone(),
            status: "reproduced".to_owned(),
            differing_files: 0,
            samples: Vec::new(),
        }
    }

    fn note_difference(&mut self, lab: &Lab, name: &str, first: &Path, second: &Path) {
        keep_difference(lab, &self.package, name, first, second);
        self.differing_files += 1;
        if self.samples.len() < 8 {
            self.samples.push(name.to_owned());
        }
    }
}

/// Rebuilds one package as someone else, on a different number of cores, and compares.
fn compare_rebuild(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    arm: Arm,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
) -> Result<Reproduction, String> {
    chain::build_one(lab, repository, package, arm, seed, sources, built)
        .map_err(|e| format!("{}: not built: {e}", package.id))?;
    if chain::content_addressed(package, arm) {
        return compare_moved(lab, repository, package, arm, seed, sources, built);
    }
    let mut result = Reproduction::new(package);
    let out = built.paths[&package.id].clone();
    let dir = lab.on_host(&out);
    let first = tree(&dir)?;
    let done = lab.work.join("done").join(out.hash());
    let keep = PathBuf::from(format!("{}.keep", dir.display()));
    chain::remove(&keep)?;
    fs::rename(&dir, &keep).map_err(with_path(&dir))?;
    fs::remove_file(&done).map_err(with_path(&done))?;

    let rebuilt =
        elsewhere(|| chain::build_one(lab, repository, package, arm, seed, sources, built));
    let outcome = match rebuilt {
        Ok(Some(prepared)) => {
            fix_up_if_needed(lab, arm, package, &prepared)?;
            let second = tree(&dir)?;
            let names: BTreeSet<&String> = first.keys().chain(second.keys()).collect();
            for name in names {
                if first.get(name) != second.get(name) {
                    result.note_difference(lab, name, &keep, &dir);
                }
            }
            if result.differing_files > 0 {
                result.status = "differs".to_owned();
            }
            chain::remove(&prepared.build_dir)
        }
        Ok(None) => Err(format!("{}: unexpectedly already built", package.id)),
        Err(message) => {
            result.status = format!("rebuild failed: {message}");
            Ok(())
        }
    };
    // The first build goes back, so what depends on it sees what it was built with.
    chain::remove(&dir)?;
    fs::rename(&keep, &dir).map_err(with_path(&keep))?;
    fs::write(&done, "").map_err(with_path(&done))?;
    outcome?;
    Ok(result)
}

/// Runs `build` with another uid, hostname and number of jobs.
fn elsewhere<T>(build: impl FnOnce() -> T) -> T {
    Identity::set(Identity {
        uid: 1234,
        hostname: "elsewhere".to_owned(),
        jobs: 5,
    });
    let result = build();
    Identity::set(Identity::standard());
    result
}

/// Builds a package again that was moved to its content addresses. It reproduces if the second
/// build lands on the same addresses. The first copy never moves, so what depends on it is
/// undisturbed.
fn compare_moved(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    arm: Arm,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
) -> Result<Reproduction, String> {
    let mut result = Reproduction::new(package);
    let path = built.paths[&package.id].clone();
    let first = built
        .outputs
        .get(&package.id)
        .cloned()
        .unwrap_or_else(|| Outputs::from([(Class::Out, path.clone())]));
    let done = lab
        .work
        .join("done")
        .join(built.temporary[&package.id].hash());
    let record = fs::read_to_string(&done).map_err(with_path(&done))?;
    fs::remove_file(&done).map_err(with_path(&done))?;
    let rebuilt =
        elsewhere(|| chain::build_one(lab, repository, package, arm, seed, sources, built));
    let outcome = match rebuilt {
        Ok(Some(prepared)) => fix_up_if_needed(lab, arm, package, &prepared)
            .and_then(|()| chain::place_outputs(lab, package, arm, &prepared.out))
            .and_then(|(second, _)| {
                record_differences(lab, &mut result, &first, &second)?;
                chain::remove(&prepared.build_dir)
            }),
        Ok(None) => Err(format!("{}: unexpectedly already built", package.id)),
        Err(message) => {
            result.status = format!("rebuild failed: {message}");
            Ok(())
        }
    };
    built.paths.insert(package.id.clone(), path);
    fs::write(&done, record).map_err(with_path(&done))?;
    outcome?;
    Ok(result)
}

/// Notes which files of two builds' outputs differ, references aside, and removes the second
/// build's copy of each output that isn't the first's.
fn record_differences(
    lab: &Lab,
    result: &mut Reproduction,
    first: &Outputs,
    second: &Outputs,
) -> Result<(), String> {
    for class in first.keys().chain(second.keys()).collect::<BTreeSet<_>>() {
        let (one, two) = (first.get(class), second.get(class));
        if one == two {
            continue;
        }
        result.status = "differs".to_owned();
        if let (Some(one), Some(two)) = (one, two) {
            compare_files(lab, result, &lab.on_host(one), &lab.on_host(two))?;
        } else {
            result.differing_files += 1;
            result
                .samples
                .push(format!("{} in one build only", class.name()));
        }
        if let Some(two) = two {
            chain::remove(&lab.on_host(two))?;
        }
    }
    Ok(())
}

fn compare_files(
    lab: &Lab,
    result: &mut Reproduction,
    first: &Path,
    second: &Path,
) -> Result<(), String> {
    let (one, two) = (tree(first)?, tree(second)?);
    for name in one.keys().chain(two.keys()).collect::<BTreeSet<_>>() {
        if one.get(name).map(|(_, masked)| masked) != two.get(name).map(|(_, masked)| masked) {
            result.note_difference(lab, name, first, second);
        }
    }
    Ok(())
}

/// Keeps both copies of a file that differed, to see why.
fn keep_difference(lab: &Lab, id: &str, name: &str, first: &Path, second: &Path) {
    for (label, tree) in [("first", first), ("second", second)] {
        let to = lab.work.join("diffs").join(id).join(label).join(name);
        if let Some(parent) = to.parent() {
            let _ = fs::create_dir_all(parent);
        }
        // An earlier copy from the store is read-only, and copying over it would fail.
        let _ = fs::remove_file(&to);
        let _ = fs::copy(tree.join(name), to);
    }
}

/// Step 5: rebuilds every package and writes what differs to `results/reproduce-<arm>.md`.
pub fn reproduce(lab: &Lab, repository: &Path, arm: Arm, only: &[&str]) -> Result<(), String> {
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
    let mut built = chain::built_from_seed(&seed);
    let mut results = Vec::new();
    for package in all_packages(repository)? {
        if package.is_boot() || !(only.is_empty() || only.contains(&package.id.as_str())) {
            chain::build_one(
                lab,
                repository,
                &package,
                arm,
                &seed,
                &mut sources,
                &mut built,
            )?;
            continue;
        }
        let result = compare_rebuild(
            lab,
            repository,
            &package,
            arm,
            &seed,
            &mut sources,
            &mut built,
        );
        match result {
            Ok(result) => {
                println!(
                    "{}: {} {}",
                    result.package, result.status, result.differing_files
                );
                results.push(result);
            }
            Err(message) => println!("{}: skipped: {message}", package.id),
        }
    }
    if !only.is_empty() {
        return Ok(());
    }
    let same = results.iter().filter(|r| r.status == "reproduced").count();
    let mut text = format!(
        "# Reproduction, arm {}\n\nBuilt a second time with another uid, hostname and 5 jobs \
         instead of 32.\n{same} of {} reproduced bit for bit.\n\n| Package | Result | Files | \
         Examples |\n|---|---|---|---|\n",
        arm.name(),
        results.len()
    );
    for result in &results {
        text.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            result.package,
            result.status,
            result.differing_files,
            result.samples.join(" ")
        ));
    }
    let path = repository.join(format!("results/reproduce-{}.md", arm.name()));
    fs::write(&path, text).map_err(with_path(&path))
}

/// A change to make, and what it's meant to show.
pub(crate) const CHANGES: [(&str, &str, Edit); 4] = [
    (
        "comment in a low-level library",
        "zlib",
        Edit::Shell("printf '\\n/* comment */\\n' >> zutil.c"),
    ),
    (
        "the previous release of a library",
        "expat",
        Edit::Source(
            "https://github.com/libexpat/libexpat/releases/download/R_2_7_0/expat-2.7.0.tar.xz",
        ),
    ),
    (
        "patched build tool",
        "m4-final",
        Edit::Shell("sed -i 's/Usage: %s/Usage:  %s/' src/m4.c && touch doc/m4.1"),
    ),
    (
        "a change inside glibc, no change to its interface",
        "glibc",
        Edit::Shell("sed -i 's/Copyright/Copyright  /' csu/version.c"),
    ),
];

/// What a change does to its package.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Edit {
    Shell(&'static str),
    Source(&'static str),
}

impl Edit {
    fn apply(self, package: &Package) -> Package {
        match self {
            Self::Shell(shell) => package.perturbed(shell),
            Self::Source(url) => package.with_source(url),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Node {
    deps: Vec<String>,
    raw: String,
    masked: String,
    refs: BTreeSet<String>,
}

/// One build of the whole set, and what wouldn't build.
struct Survey {
    nodes: BTreeMap<String, Node>,
    failed: Vec<(String, String)>,
    /// What really got built, because nothing already matched it.
    rebuilt: Vec<String>,
    /// The packages of a system, leaving out what only exists to bootstrap the toolchain.
    paths: BTreeMap<String, StorePath>,
}

/// The id, dependencies and hashes of everything in one build of the whole set.
fn survey(
    lab: &Lab,
    repository: &Path,
    arm: Arm,
    target: Option<(&str, Edit)>,
) -> Result<Survey, String> {
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
    let mut built = chain::built_from_seed(&seed);
    let mut order = Vec::new();
    let mut failed = Vec::new();
    let mut rebuilt = Vec::new();
    for package in all_packages(repository)? {
        let package = match target {
            Some((id, edit)) if package.id == id => edit.apply(&package),
            _ => package,
        };
        match chain::build_one(
            lab,
            repository,
            &package,
            arm,
            &seed,
            &mut sources,
            &mut built,
        ) {
            Ok(Some(prepared)) => {
                fix_up_if_needed(lab, arm, &package, &prepared)?;
                chain::remove(&prepared.build_dir)?;
                chain::finish(lab, &package, arm, &prepared.out, &mut built)?;
                rebuilt.push(package.id.clone());
                order.push(package);
            }
            Ok(None) => order.push(package),
            Err(message) => {
                println!("  {}: {message}", package.id);
                if !message.contains("not built before it") {
                    save_log(lab, arm, &package.id, target.map_or("none", |(id, _)| id));
                }
                failed.push((package.id.clone(), message));
            }
        }
    }
    let by_hash: BTreeMap<String, String> = built
        .paths
        .iter()
        .map(|(id, path)| (path.hash().to_owned(), id.clone()))
        .collect();
    let mut nodes = BTreeMap::new();
    for package in order {
        let path = &built.paths[&package.id];
        let dir = lab.on_host(path);
        let (raw, masked) = tree_hashes(&dir)?;
        let refs = chain::references(&dir)?
            .into_keys()
            .filter_map(|hash| by_hash.get(&hash).cloned())
            .filter(|id| *id != package.id)
            .collect();
        nodes.insert(
            package.id.clone(),
            Node {
                deps: package.dep_ids()?,
                raw,
                masked,
                refs,
            },
        );
    }
    let paths = nodes
        .keys()
        .filter(|id| !chain::BOOTSTRAP_ONLY.contains(&id.as_str()) && !id.ends_with("-stage1"))
        .map(|id| (id.clone(), built.paths[id].clone()))
        .collect();
    Ok(Survey {
        nodes,
        failed,
        rebuilt,
        paths,
    })
}

/// The store paths of the whole set, with a change to one package if `target` says so.
pub(crate) fn system_paths(
    lab: &Lab,
    repository: &Path,
    arm: Arm,
    target: Option<(&str, Edit)>,
) -> Result<BTreeMap<String, StorePath>, String> {
    let found = survey(lab, repository, arm, target)?;
    if let Some((id, message)) = found.failed.first() {
        return Err(format!("{id}: {message}"));
    }
    Ok(found.paths)
}

/// A failed build's log, which the next run of that package would otherwise replace.
fn save_log(lab: &Lab, arm: Arm, id: &str, group: &str) {
    let logs = lab.work.join("logs");
    let dir = logs.join("failed").join(group);
    let _ = fs::create_dir_all(&dir);
    let name = format!("{id}.log");
    let _ = fs::copy(logs.join(arm.name()).join(&name), dir.join(name));
}

/// How many packages each way of keying rebuilds after a change to `root`. `after` is the build
/// with the change, in which every dependent really was rebuilt.
fn policies(
    before: &BTreeMap<String, Node>,
    after: &BTreeMap<String, Node>,
    root: &str,
) -> [usize; 6] {
    let order: Vec<&String> = before.keys().filter(|id| after.contains_key(*id)).collect();
    let masked_changed = |id: &str| before[id].masked != after[id].masked;
    let raw_changed = |id: &str| before[id].raw != after[id].raw;

    // Does this package's content hash change in a content-addressed store? When its own bytes
    // do, once references are ignored, or when anything it refers to does.
    let mut content: BTreeSet<String> = BTreeSet::new();
    loop {
        let before_count = content.len();
        for id in &order {
            if masked_changed(id) || after[*id].refs.iter().any(|d| content.contains(d)) {
                content.insert((*id).clone());
            }
        }
        if content.len() == before_count {
            break;
        }
    }

    let transitive = |policy: &dyn Fn(&str) -> bool| {
        let mut rebuilt = BTreeSet::from([root.to_owned()]);
        loop {
            let before_count = rebuilt.len();
            for id in &order {
                if after[*id]
                    .deps
                    .iter()
                    .any(|d| rebuilt.contains(d) && policy(d))
                {
                    rebuilt.insert((*id).clone());
                }
            }
            if rebuilt.len() == before_count {
                return rebuilt;
            }
        }
    };
    let every = transitive(&|_| true);
    let by_content = transitive(&|d| content.contains(d));
    let by_masked = transitive(&masked_changed);
    // A package skipped under masked keys although its own bytes, references aside, would change.
    let false_cutoffs = every
        .iter()
        .filter(|id| !by_masked.contains(*id) && after.contains_key(*id) && masked_changed(id))
        .count();
    [
        every.len(),
        by_content.len(),
        by_masked.len(),
        order.iter().filter(|id| masked_changed(id)).count(),
        order.iter().filter(|id| raw_changed(id)).count(),
        false_cutoffs,
    ]
}

/// Step 6: applies each change, rebuilds everything downstream of it, and works out what each way
/// of keying would have rebuilt. Every hash goes to `results/raw/spread-<arm>/`.
pub fn spread(lab: &Lab, repository: &Path, arm: Arm) -> Result<(), String> {
    let before = survey(lab, repository, arm, None)?;
    let raw = repository.join(format!("results/raw/spread-{}", arm.name()));
    fs::create_dir_all(&raw).map_err(with_path(&raw))?;
    let save = |name: &str, nodes: &BTreeMap<String, Node>| {
        let path = raw.join(format!("{name}.json"));
        let text = serde_json::to_string_pretty(nodes).map_err(|e| e.to_string())?;
        fs::write(&path, text + "\n").map_err(with_path(&path))
    };
    save("before", &before.nodes)?;
    let mut table = format!(
        "# How far a change spreads, arm {}\n\n{} packages. Everything downstream of each change \
         was really rebuilt, and the columns say how many of them each key would have rebuilt.\n\n\
         | Change | Package | Every dependent | Content-addressed | References masked | Bytes \
         changed, references aside | Paths changed | Skipped, yet bytes differ | Failed |\n\
         |---|---|---|---|---|---|---|---|---|\n",
        arm.name(),
        before.nodes.len(),
    );
    let mut detail = String::new();
    for (name, root, edit) in CHANGES {
        println!("{name}");
        let after = survey(lab, repository, arm, Some((root, edit)))?;
        save(root, &after.nodes)?;
        let [every, content, masked, bytes, paths, skipped] =
            policies(&before.nodes, &after.nodes, root);
        table.push_str(&format!(
            "| {name} | {root} | {every} | {content} | {masked} | {bytes} | {paths} | {skipped} | \
             {} |\n",
            after.failed.len()
        ));
        let changed: Vec<&str> = after
            .nodes
            .iter()
            .filter(|(id, node)| {
                before
                    .nodes
                    .get(*id)
                    .is_some_and(|b| b.masked != node.masked)
            })
            .map(|(id, _)| id.as_str())
            .collect();
        detail.push_str(&format!(
            "\n## {root}\n\nBytes changed, references aside: {}\n",
            changed.join(" ")
        ));
        detail.push_str(&format!(
            "\nReally rebuilt, {} of them: {}\n",
            after.rebuilt.len(),
            after.rebuilt.join(" ")
        ));
        for (id, message) in &after.failed {
            detail.push_str(&format!("\nFailed: {id}: {message}\n"));
        }
    }
    let path = repository.join(format!("results/spread-{}.md", arm.name()));
    fs::write(&path, table + &detail).map_err(with_path(&path))
}

/// Step 6: builds the whole set under another kind of view, and counts what breaks and what comes
/// out different. A package that breaks is built as declared, so what depends on it still gets
/// built under the view being tested.
pub fn narrow(lab: &Lab, repository: &Path, arm: Arm, views: Views) -> Result<(), String> {
    let declared = survey(lab, repository, arm, None)?.nodes;
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
    let mut built = chain::built_from_seed(&seed);
    let (mut broken, mut different, mut total) = (Vec::new(), Vec::new(), 0);
    for package in all_packages(repository)? {
        let mut build = |built: &mut Built| {
            chain::build_one(lab, repository, &package, arm, &seed, &mut sources, built)
        };
        built.views = views;
        let mut result = build(&mut built);
        if let Err(message) = &result {
            if package.is_boot() {
                return Err(message.clone());
            }
            println!("  broke {}: {message}", package.id);
            save_log(lab, arm, &package.id, views.name());
            broken.push(package.id.clone());
            built.views = Views::Declared;
            result = build(&mut built);
        }
        match result {
            Ok(Some(prepared)) => {
                fix_up_if_needed(lab, arm, &package, &prepared)?;
                chain::remove(&prepared.build_dir)?;
            }
            Ok(None) => {}
            Err(message) => {
                println!("  {}: not built at all: {message}", package.id);
                continue;
            }
        }
        if package.is_boot() {
            continue;
        }
        total += 1;
        let (_, masked) = tree_hashes(&lab.on_host(&built.paths[&package.id]))?;
        if declared
            .get(&package.id)
            .is_some_and(|node| node.masked != masked)
        {
            different.push(package.id.clone());
        }
    }
    let text = format!(
        "# Builds under {} views, arm {}\n\nOf {total} packages, {} broke and {} built with \
         different contents, references aside, from the declared view.\n\nBroke: {}\n\nDifferent: \
         {}\n",
        views.name(),
        arm.name(),
        broken.len(),
        different.len(),
        broken.join(" "),
        different.join(" "),
    );
    let path = repository.join(format!("results/narrow-{}-{}.md", arm.name(), views.name()));
    fs::write(&path, text).map_err(with_path(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(lab: &Lab, seed: &str, name: &str, text: &str) -> StorePath {
        let path = StorePath::derive(seed, &[seed.as_bytes()]);
        let dir = lab.on_host(&path);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), text).unwrap();
        path
    }

    fn reproduction() -> Reproduction {
        Reproduction {
            package: "p".to_owned(),
            status: "reproduced".to_owned(),
            differing_files: 0,
            samples: Vec::new(),
        }
    }

    #[test]
    fn the_same_addresses_reproduce_and_keep_their_files() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path().to_str().unwrap());
        let lib = output(&lab, "lib", "a", "same");
        let outputs = Outputs::from([(Class::Lib, lib.clone())]);
        let mut result = reproduction();
        record_differences(&lab, &mut result, &outputs, &outputs).unwrap();
        assert_eq!(result.status, "reproduced");
        assert!(lab.on_host(&lib).join("a").exists());
    }

    #[test]
    fn a_differing_output_names_its_file_and_removes_only_the_second_copy() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path().to_str().unwrap());
        let shared = output(&lab, "lib", "a", "same");
        let one = output(&lab, "first", "b", "one");
        let two = output(&lab, "second", "b", "two");
        let first = Outputs::from([(Class::Lib, shared.clone()), (Class::Out, one.clone())]);
        let second = Outputs::from([(Class::Lib, shared.clone()), (Class::Out, two.clone())]);
        let mut result = reproduction();
        record_differences(&lab, &mut result, &first, &second).unwrap();
        assert_eq!(result.status, "differs");
        assert_eq!(result.samples, ["b"]);
        assert!(lab.on_host(&shared).exists());
        assert!(lab.on_host(&one).exists());
        assert!(!lab.on_host(&two).exists());
    }

    #[test]
    fn an_output_only_one_build_made_differs() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path().to_str().unwrap());
        let out = output(&lab, "out", "a", "same");
        let doc = output(&lab, "doc", "b", "extra");
        let first = Outputs::from([(Class::Out, out.clone())]);
        let second = Outputs::from([(Class::Out, out.clone()), (Class::Doc, doc.clone())]);
        let mut result = reproduction();
        record_differences(&lab, &mut result, &first, &second).unwrap();
        assert_eq!(result.status, "differs");
        assert_eq!(result.samples, ["doc in one build only"]);
        assert!(lab.on_host(&out).exists());
        assert!(!lab.on_host(&doc).exists());
    }

    fn node(deps: &[&str], raw: &str, masked: &str, refs: &[&str]) -> Node {
        Node {
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            raw: raw.to_owned(),
            masked: masked.to_owned(),
            refs: refs.iter().map(|d| (*d).to_owned()).collect(),
        }
    }

    fn map(nodes: &[(&str, Node)]) -> BTreeMap<String, Node> {
        nodes
            .iter()
            .map(|(id, n)| ((*id).to_owned(), n.clone()))
            .collect()
    }

    #[test]
    fn store_hashes_are_masked_and_nothing_else_is() {
        let path = format!("x/store/{}/lib y", "a".repeat(HASH_LENGTH));
        let masked = String::from_utf8(mask_references(path.as_bytes())).unwrap();

        assert_eq!(masked, format!("x/store/{}/lib y", "0".repeat(HASH_LENGTH)));
        let short = b"/store/abc/lib";
        assert_eq!(mask_references(short), short);
    }

    #[test]
    fn a_change_that_alters_no_bytes_stops_at_the_changed_package() {
        let before = map(&[
            ("lib", node(&[], "r1", "m1", &[])),
            ("app", node(&["lib"], "r2", "m2", &["lib"])),
        ]);
        let after = map(&[
            ("lib", node(&[], "r1", "m1", &[])),
            ("app", node(&["lib"], "r2", "m2", &["lib"])),
        ]);

        let [every, content, masked, ..] = policies(&before, &after, "lib");
        assert_eq!((every, content, masked), (2, 1, 1));
    }

    #[test]
    fn a_library_change_reaches_what_refers_to_it_unless_keys_ignore_references() {
        let before = map(&[
            ("lib", node(&[], "r1", "m1", &[])),
            ("app", node(&["lib"], "r2", "m2", &["lib"])),
            ("top", node(&["app"], "r3", "m3", &["app"])),
        ]);
        let after = map(&[
            ("lib", node(&[], "R1", "M1", &[])),
            ("app", node(&["lib"], "R2", "m2", &["lib"])),
            ("top", node(&["app"], "R3", "m3", &["app"])),
        ]);

        let [every, content, masked, bytes, paths, false_cutoffs] =
            policies(&before, &after, "lib");
        assert_eq!((every, content, masked), (3, 3, 2));
        assert_eq!((bytes, paths, false_cutoffs), (1, 3, 0));
    }

    #[test]
    fn a_dependent_whose_own_bytes_change_is_a_false_cutoff_when_skipped() {
        let before = map(&[
            ("lib", node(&[], "r1", "m1", &[])),
            ("mid", node(&["lib"], "r2", "m2", &[])),
            ("top", node(&["mid"], "r3", "m3", &[])),
        ]);
        let after = map(&[
            ("lib", node(&[], "R1", "M1", &[])),
            ("mid", node(&["lib"], "R2", "m2", &[])),
            ("top", node(&["mid"], "R3", "M3", &[])),
        ]);

        let [.., false_cutoffs] = policies(&before, &after, "lib");
        assert_eq!(false_cutoffs, 1);
    }
}
