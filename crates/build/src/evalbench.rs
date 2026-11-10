//! How long resolving one package takes as the set of recipes grows.
//!
//! Generates recipes shaped like packages, each in its own file with a handful of dependencies, and
//! resolves one package: read what it needs, and hash the chain from the bottom up. Only files on
//! the way get read.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sha2::{Digest, Sha256};

use crate::store::{hex, with_path};

fn package_name(id: usize) -> String {
    format!("pkg{id:06}")
}

fn recipe_path(root: &Path, id: usize) -> PathBuf {
    let name = package_name(id);
    root.join(&name[3..6]).join(format!("{name}.toml"))
}

/// A small deterministic generator, so every run has the same graph.
fn next(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1);
    *state >> 33
}

/// Writes `count` recipes. A package depends only on earlier ones, mostly on the common bottom of
/// the graph.
pub fn generate(root: &Path, count: usize) -> Result<(), String> {
    let mut state = 42u64;
    for id in 0..count {
        let path = recipe_path(root, id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(with_path(parent))?;
        }
        let mut deps = Vec::new();
        if id > 0 {
            let range = id.min(1 + id / 4 + 40) as u64;
            for _ in 0..(2 + next(&mut state) % 7) {
                let dep = (next(&mut state) % range) as usize;
                deps.push(format!("\"{}\"", package_name(dep)));
            }
            deps.sort();
            deps.dedup();
        }
        let name = package_name(id);
        let text = format!(
            r#"id = "{name}"
version = "1.{minor}"
sources = [{{ url = "https://example.org/{name}.tar.xz", sha256 = "{sha256}" }}]
deps = [{deps}]
script = '''
./configure --prefix=$PREFIX --disable-static
make -j$NPROC
make install DESTDIR=$DEST
'''
"#,
            minor = id % 40,
            sha256 = hex(&Sha256::digest(id.to_le_bytes())),
            deps = deps.join(", "),
        );
        fs::write(&path, text).map_err(with_path(&path))?;
    }
    Ok(())
}

#[derive(serde::Deserialize)]
struct Recipe {
    deps: Vec<String>,
}

fn resolve_one(
    root: &Path,
    id: &str,
    hashes: &mut BTreeMap<String, String>,
) -> Result<String, String> {
    if let Some(hash) = hashes.get(id) {
        return Ok(hash.clone());
    }
    let number: usize = id[3..].parse().map_err(|_| format!("bad id {id}"))?;
    let path = recipe_path(root, number);
    let text = fs::read_to_string(&path).map_err(with_path(&path))?;
    let recipe: Recipe = toml::from_str(&text).map_err(|e| format!("{id}: {e}"))?;
    let mut hasher = Sha256::new();
    hasher.update(&text);
    for dep in &recipe.deps {
        hasher.update(resolve_one(root, dep, hashes)?);
    }
    let hash = hex(&hasher.finalize());
    hashes.insert(id.to_owned(), hash.clone());
    Ok(hash)
}

/// Generates a set of each size, and times resolving a package near the top of it.
pub fn run(root: &Path, sizes: &[usize]) -> Result<String, String> {
    let mut report = String::from(
        "| Recipes | Closure of the package | Resolve, ms | Files read | Every recipe, ms |\n\
         |---|---|---|---|---|\n",
    );
    for &count in sizes {
        let dir = root.join(count.to_string());
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(with_path(&dir))?;
        }
        generate(&dir, count)?;
        let target = package_name(count - 1);
        let mut times = Vec::new();
        let mut closure = 0;
        for _ in 0..5 {
            let start = Instant::now();
            let mut hashes = BTreeMap::new();
            resolve_one(&dir, &target, &mut hashes)?;
            times.push(start.elapsed());
            closure = hashes.len();
        }
        times.sort();
        let median = times[2];
        let start = Instant::now();
        let mut hashes = BTreeMap::new();
        for id in 0..count {
            resolve_one(&dir, &package_name(id), &mut hashes)?;
        }
        let all = start.elapsed();
        report.push_str(&format!(
            "| {count} | {closure} | {:.1} | {closure} | {} |\n",
            median.as_secs_f64() * 1000.0,
            all.as_millis(),
        ));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolving_reads_only_what_the_package_needs_and_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        generate(dir.path(), 500).unwrap();
        let (mut first, mut second) = (BTreeMap::new(), BTreeMap::new());
        let a = resolve_one(dir.path(), "pkg000100", &mut first).unwrap();
        let b = resolve_one(dir.path(), "pkg000100", &mut second).unwrap();
        assert_eq!(a, b);
        assert!(first.len() < 500);
        assert!(first.contains_key("pkg000100"));
    }

    #[test]
    fn a_changed_dependency_changes_the_hash_above_it() {
        let dir = tempfile::tempdir().unwrap();
        generate(dir.path(), 200).unwrap();
        let mut before = BTreeMap::new();
        let old = resolve_one(dir.path(), "pkg000199", &mut before).unwrap();
        let bottom = recipe_path(dir.path(), 0);
        let mut text = fs::read_to_string(&bottom).unwrap();
        text.push_str("# changed\n");
        fs::write(&bottom, text).unwrap();
        let mut after = BTreeMap::new();
        let new = resolve_one(dir.path(), "pkg000199", &mut after).unwrap();
        assert!(before.contains_key("pkg000000"));
        assert_ne!(old, new);
    }
}
