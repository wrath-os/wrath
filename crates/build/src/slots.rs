//! Finding libraries through stable slots instead of store hashes.
//!
//! A program's run path and interpreter name `/system/libs/<package>/lib`, a link the system sets.
//! A library then changes without changing anything that uses it. The rewrite happens after
//! linking here, where a real tool would do it at link time.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;

use crate::chain::{self, Arm};
use crate::experiments::{self, CHANGES, mask_references};
use crate::layouts::{Tree, Trees, compress_each, digest, load, mib, prepare};
use crate::runpath;
use crate::store::{Lab, StorePath, with_path};

const HASH_LENGTH: usize = 40;

/// The slot for a store path inside `packages`, if it has one.
pub(crate) fn to_slot(entry: &str, packages: &BTreeMap<String, String>) -> Option<String> {
    let rest = entry.strip_prefix("/store/")?;
    let hash = rest.get(..HASH_LENGTH)?;
    let id = packages.get(hash)?;
    Some(format!("/system/libs/{id}{}", &rest[HASH_LENGTH..]))
}

fn apply(trees: &mut Trees, packages: &BTreeMap<String, String>) {
    for bytes in trees.values_mut().flat_map(Tree::values_mut) {
        runpath::rename_libraries(bytes, &|entry| to_slot(entry, packages));
        runpath::blank_build_id(bytes);
    }
}

/// Blanks a package's references to itself, as a path not derived from its inputs would.
fn blank_own_path(trees: &mut Trees) {
    for (path, tree) in trees.iter_mut() {
        let hash = path.hash().as_bytes();
        for bytes in tree.values_mut() {
            let found: Vec<usize> = memchr::memmem::find_iter(bytes, hash).collect();
            for at in found {
                bytes[at..at + HASH_LENGTH].fill(b'0');
            }
        }
    }
}

fn packages_of(paths: &BTreeMap<String, StorePath>) -> BTreeMap<String, String> {
    paths
        .iter()
        .map(|(id, path)| (path.hash().to_owned(), id.clone()))
        .collect()
}

/// Copies `paths` into `work` and moves them to slots. Returns those trees, and a copy of them
/// with each package's own path blanked as well.
fn slotted_trees(
    from: &Lab,
    work: &Path,
    paths: &BTreeSet<StorePath>,
    system: &BTreeMap<String, StorePath>,
) -> Result<(Trees, Trees), String> {
    let lab = Lab::new(work);
    prepare(from, &lab, paths)?;
    let mut trees = load(&lab, paths)?;
    apply(&mut trees, &packages_of(system));
    let mut own = trees.clone();
    blank_own_path(&mut own);
    Ok((trees, own))
}

fn contents(trees: &Trees) -> impl Iterator<Item = &Vec<u8>> {
    trees.values().flat_map(Tree::values)
}

/// Measures every spread change with libraries found through slots, into
/// `results/slots-fetch.md`.
pub fn fetch_sizes(lab: &Lab, repository: &Path, arm: Arm) -> Result<(), String> {
    let root = lab.work.join("layouts-slots");
    let before = experiments::system_paths(lab, repository, arm, None)?;
    let before_set: BTreeSet<StorePath> = before.values().cloned().collect();
    let (before_trees, before_own) =
        slotted_trees(lab, &root.join("before"), &before_set, &before)?;

    let held: HashSet<_> = contents(&before_trees).map(|bytes| digest(bytes)).collect();
    let held_own: HashSet<_> = contents(&before_own).map(|bytes| digest(bytes)).collect();
    let held_masked: HashSet<_> = contents(&before_trees)
        .map(|bytes| digest(&mask_references(bytes)))
        .collect();

    let mut report = String::from(
        "# What an update downloads with libraries in slots\n\nMiB compressed with zstd -3. \
         Build IDs are blanked, since linking against slots would not vary them.\n\n| Change | \
         Paths rebuilt today | Packages still changing | Files changing | MiB, slots | MiB, slots \
         and own path not from inputs | MiB, no store paths at all |\n\
         |---|---|---|---|---|---|---|\n",
    );
    for (name, target, edit) in CHANGES {
        println!("{name}");
        let after = experiments::system_paths(lab, repository, arm, Some((target, edit)))?;
        let changed: Vec<(&StorePath, &StorePath)> = after
            .iter()
            .filter_map(|(id, new)| Some((before.get(id)?, new)))
            .filter(|(old, new)| old != new)
            .collect();
        let new_set: BTreeSet<StorePath> = changed.iter().map(|&(_, new)| new.clone()).collect();
        let (new_trees, new_own) = slotted_trees(lab, &root.join(target), &new_set, &after)?;

        let packages_changing = changed
            .iter()
            .filter(|&&(old, new)| before_trees[old] != new_trees[new])
            .count();
        let mut seen = held.clone();
        let (mut files, mut real) = (Vec::new(), Vec::new());
        for bytes in contents(&new_trees) {
            if seen.insert(digest(bytes)) {
                files.push(bytes.as_slice());
                if !held_masked.contains(&digest(&mask_references(bytes))) {
                    real.push(bytes.as_slice());
                }
            }
        }
        let mut seen_own = held_own.clone();
        let own: Vec<&[u8]> = contents(&new_own)
            .filter(|bytes| seen_own.insert(digest(bytes)))
            .map(Vec::as_slice)
            .collect();
        report.push_str(&format!(
            "| {name} | {} | {packages_changing} | {} | {} | {} | {} |\n",
            changed.len(),
            files.len(),
            mib(compress_each(&files)),
            mib(compress_each(&own)),
            mib(compress_each(&real)),
        ));
    }
    chain::remove(&root)?;
    let path = repository.join("results/slots-fetch.md");
    fs::write(&path, report).map_err(with_path(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_library_directory_gets_its_slot() {
        let hash = "a".repeat(HASH_LENGTH);
        let packages = BTreeMap::from([(hash.clone(), "zlib".to_owned())]);
        assert_eq!(
            to_slot(&format!("/store/{hash}/lib"), &packages).as_deref(),
            Some("/system/libs/zlib/lib")
        );
    }

    #[test]
    fn other_paths_are_left_alone() {
        let packages = BTreeMap::new();
        assert_eq!(to_slot("$ORIGIN/../lib", &packages), None);
        let unknown = format!("/store/{}/lib", "b".repeat(HASH_LENGTH));
        assert_eq!(to_slot(&unknown, &packages), None);
    }
}
