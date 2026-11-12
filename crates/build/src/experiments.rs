//! Step 5: how many packages reproduce when built again, and when built somewhere else.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use memchr::memmem;
use sha2::{Digest, Sha256};

use crate::chain::{self, Arm, Built, Package, Prepared, Sources};
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

    #[test]
    fn store_hashes_are_masked_and_nothing_else_is() {
        let path = format!("x/store/{}/lib y", "a".repeat(HASH_LENGTH));
        let masked = String::from_utf8(mask_references(path.as_bytes())).unwrap();

        assert_eq!(masked, format!("x/store/{}/lib y", "0".repeat(HASH_LENGTH)));
        let short = b"/store/abc/lib";
        assert_eq!(mask_references(short), short);
    }
}
