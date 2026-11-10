//! What an update has to download under each way of storing packages.
//!
//! Compares a system before and after each change from the spread experiment: whole packages,
//! individual files, content-defined chunks and binary deltas, with and without counting bytes
//! that differ only in store references.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use crate::chain::{self, Arm};
use crate::experiments::{self, CHANGES, mask_references};
use crate::pilot::files_under;
use crate::split;
use crate::store::{Lab, StorePath, with_path};

const MIN_CHUNK: usize = 16 * 1024;
const MAX_CHUNK: usize = 256 * 1024;
/// A boundary comes about every 64 KiB, once past the minimum.
const BOUNDARY_MASK: u64 = (1 << 16) - 1;

type Digests = HashSet<[u8; 32]>;
/// A package's files by relative path, with a link's target as its contents.
pub(crate) type Tree = BTreeMap<String, Vec<u8>>;
pub(crate) type Trees = BTreeMap<StorePath, Tree>;

#[derive(Clone, Default)]
struct Held {
    files: Digests,
    masked_files: Digests,
    chunks: Digests,
}

#[derive(Default)]
struct Missing<'a> {
    files: Vec<&'a [u8]>,
    masked_files: Vec<&'a [u8]>,
    chunks: Vec<&'a [u8]>,
}

impl Held {
    fn add<'a>(&mut self, trees: &'a Trees, table: &[u64; 256]) -> Missing<'a> {
        let mut missing = Missing::default();
        for bytes in trees.values().flat_map(Tree::values) {
            if self.files.insert(digest(bytes)) {
                missing.files.push(bytes.as_slice());
            }
            if self.masked_files.insert(digest(&mask_references(bytes))) {
                missing.masked_files.push(bytes.as_slice());
            }
            for chunk in chunks(bytes, table) {
                if self.chunks.insert(digest(chunk)) {
                    missing.chunks.push(chunk);
                }
            }
        }
        missing
    }
}

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn gear_table() -> [u64; 256] {
    let mut table = [0; 256];
    for (index, slot) in table.iter_mut().enumerate() {
        let hash = Sha256::digest([index as u8]);
        *slot = u64::from_le_bytes(hash[..8].try_into().unwrap_or_default());
    }
    table
}

/// Splits `bytes` where its content says to, so an insertion only moves nearby boundaries.
fn chunks<'a>(bytes: &'a [u8], table: &[u64; 256]) -> Vec<&'a [u8]> {
    let mut found = Vec::new();
    let mut start = 0;
    let mut hash = 0u64;
    for (at, byte) in bytes.iter().enumerate() {
        hash = (hash << 1).wrapping_add(table[usize::from(*byte)]);
        let length = at + 1 - start;
        if length >= MAX_CHUNK || (length >= MIN_CHUNK && hash & BOUNDARY_MASK == 0) {
            found.push(&bytes[start..=at]);
            start = at + 1;
            hash = 0;
        }
    }
    if start < bytes.len() {
        found.push(&bytes[start..]);
    }
    found
}

fn compressed_size(bytes: &[u8]) -> u64 {
    zstd::bulk::compress(bytes, 3).map_or(bytes.len() as u64, |out| out.len() as u64)
}

/// The total compressed size of `blobs`, each compressed on its own.
pub(crate) fn compress_each<B: AsRef<[u8]> + Sync>(blobs: &[B]) -> u64 {
    let threads = std::thread::available_parallelism().map_or(1, |count| count.get());
    let portion = blobs.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let workers: Vec<_> = blobs
            .chunks(portion)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .map(|blob| compressed_size(blob.as_ref()))
                        .sum::<u64>()
                })
            })
            .collect();
        workers.into_iter().map(|w| w.join().unwrap_or(0)).sum()
    })
}

fn read_tree(dir: &Path) -> Result<Tree, String> {
    let mut tree = Tree::new();
    for file in files_under(dir)? {
        let relative = file
            .strip_prefix(dir)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .into_owned();
        let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
        let bytes = if meta.is_symlink() {
            let target = fs::read_link(&file).map_err(with_path(&file))?;
            target.to_string_lossy().into_owned().into_bytes()
        } else {
            fs::read(&file).map_err(with_path(&file))?
        };
        tree.insert(relative, bytes);
    }
    Ok(tree)
}

fn joined(tree: &Tree) -> Vec<u8> {
    let mut all = Vec::new();
    for (name, bytes) in tree {
        all.extend_from_slice(name.as_bytes());
        all.push(0);
        all.extend_from_slice(bytes);
    }
    all
}

fn masked_equal(left: &Tree, right: &Tree) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|((a_name, a), (b_name, b))| {
            mask_references(a_name.as_bytes()) == mask_references(b_name.as_bytes())
                && mask_references(a) == mask_references(b)
        })
}

/// Copies `paths` to `lab` and strips them, since nobody would ship the debug info.
pub(crate) fn prepare(from: &Lab, lab: &Lab, paths: &BTreeSet<StorePath>) -> Result<(), String> {
    chain::remove(&lab.work)?;
    let store = lab.work.join("store");
    fs::create_dir_all(&store).map_err(with_path(&store))?;
    for path in paths {
        let status = Command::new("cp")
            .args(["-a", "--reflink=auto"])
            .arg(from.on_host(path))
            .arg(&store)
            .status()
            .map_err(with_path(&store))?;
        if !status.success() {
            return Err(format!("copying {}", path.hash()));
        }
    }
    split::strip(lab, paths)?;
    Ok(())
}

pub(crate) fn load(lab: &Lab, paths: &BTreeSet<StorePath>) -> Result<Trees, String> {
    paths
        .iter()
        .map(|path| Ok((path.clone(), read_tree(&lab.on_host(path))?)))
        .collect()
}

fn patch_size(work: &Path, old: &Tree, new: &Tree) -> Result<u64, String> {
    let (old_file, new_file, out) = (
        work.join("patch.old"),
        work.join("patch.new"),
        work.join("patch.zst"),
    );
    fs::write(&old_file, joined(old)).map_err(with_path(&old_file))?;
    fs::write(&new_file, joined(new)).map_err(with_path(&new_file))?;
    let status = Command::new("zstd")
        .args(["-q", "-3", "-f", "--long=27"])
        .arg(format!("--patch-from={}", old_file.display()))
        .arg(&new_file)
        .arg("-o")
        .arg(&out)
        .status()
        .map_err(with_path(&out))?;
    if !status.success() {
        return Err("zstd --patch-from failed".to_owned());
    }
    Ok(fs::metadata(&out).map_err(with_path(&out))?.len())
}

pub(crate) fn mib(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

fn total_len(parts: &[&[u8]]) -> u64 {
    parts.iter().map(|part| part.len() as u64).sum()
}

/// Measures every change against `before` and writes `results/layouts-fetch.md`.
pub fn fetch_sizes(lab: &Lab, repository: &Path, arm: Arm) -> Result<(), String> {
    let table = gear_table();
    let root = lab.work.join("layouts");
    let before = experiments::system_paths(lab, repository, arm, None)?;
    let before_set: BTreeSet<StorePath> = before.values().cloned().collect();
    let before_lab = Lab::new(root.join("before"));
    prepare(lab, &before_lab, &before_set)?;
    let before_trees = load(&before_lab, &before_set)?;

    let total_bytes: u64 = before_trees
        .values()
        .flat_map(Tree::values)
        .map(|bytes| bytes.len() as u64)
        .sum();
    let mut held = Held::default();
    let distinct = held.add(&before_trees, &table);

    let mut report = format!(
        "# What an update downloads\n\n{} packages, stripped: {} MiB in all, {} MiB of distinct \
         files, {} MiB of distinct chunks.\n\nMiB compressed with zstd -3. Every column except \
         the last counts what a machine holding the old system still needs.\n\n| Change | \
         Paths rebuilt | Whole packages | Packages, references aside | Files | Files, \
         references aside | Chunks | Binary deltas | The whole system |\n\
         |---|---|---|---|---|---|---|---|---|\n",
        before.len(),
        mib(total_bytes),
        mib(total_len(&distinct.files)),
        mib(total_len(&distinct.chunks)),
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
        let after_lab = Lab::new(root.join(target));
        prepare(lab, &after_lab, &new_set)?;
        let new_trees = load(&after_lab, &new_set)?;

        let mut package_blobs = Vec::new();
        let mut masked_packages = Vec::new();
        for (old, new) in &changed {
            let blob = joined(&new_trees[*new]);
            if !masked_equal(&before_trees[*old], &new_trees[*new]) {
                masked_packages.push(blob.clone());
            }
            package_blobs.push(blob);
        }
        let missing = held.clone().add(&new_trees, &table);

        let mut deltas = 0;
        for (old, new) in &changed {
            deltas += patch_size(&after_lab.work, &before_trees[*old], &new_trees[*new])?;
        }

        let everything: Vec<Vec<u8>> = after
            .values()
            .map(|path| {
                new_trees
                    .get(path)
                    .or_else(|| before_trees.get(path))
                    .map(joined)
                    .unwrap_or_default()
            })
            .collect();
        report.push_str(&format!(
            "| {name} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            changed.len(),
            mib(compress_each(&package_blobs)),
            mib(compress_each(&masked_packages)),
            mib(compress_each(&missing.files)),
            mib(compress_each(&missing.masked_files)),
            mib(compress_each(&missing.chunks)),
            mib(deltas),
            mib(compress_each(&everything)),
        ));
    }
    chain::remove(&root)?;
    let path = repository.join("results/layouts-fetch.md");
    fs::write(&path, report).map_err(with_path(&path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(length: usize, seed: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(length);
        let mut state = u32::from(seed) + 1;
        for _ in 0..length {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            out.push((state >> 16) as u8);
        }
        out
    }

    #[test]
    fn chunks_cover_the_input_within_their_limits() {
        let table = gear_table();
        let input = data(2_000_000, 1);
        let found = chunks(&input, &table);
        assert_eq!(found.concat(), input);
        assert!(found.len() > 2);
        for chunk in &found[..found.len() - 1] {
            assert!((MIN_CHUNK..=MAX_CHUNK).contains(&chunk.len()));
        }
    }

    #[test]
    fn an_edit_near_the_start_leaves_later_chunks_alone() {
        let table = gear_table();
        let input = data(2_000_000, 2);
        let mut edited = input.clone();
        edited[100] ^= 0xff;
        edited.splice(200..200, [1, 2, 3]);
        let old: BTreeSet<&[u8]> = chunks(&input, &table).into_iter().collect();
        let new = chunks(&edited, &table);
        let shared = new.iter().filter(|chunk| old.contains(*chunk)).count();
        assert!(shared >= new.len() - 2, "{shared} of {}", new.len());
    }

    #[test]
    fn trees_differing_only_in_references_are_masked_equal() {
        let tree = |hash: char| {
            let path = format!("/store/{}/lib\0", hash.to_string().repeat(40));
            Tree::from([("a".to_owned(), path.into_bytes())])
        };
        assert!(masked_equal(&tree('a'), &tree('b')));
    }

    #[test]
    fn a_real_difference_is_not_masked() {
        let tree = |text: &str| Tree::from([("a".to_owned(), text.as_bytes().to_vec())]);
        assert!(!masked_equal(&tree("one"), &tree("two")));
    }
}
