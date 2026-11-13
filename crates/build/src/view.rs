//! A view: the dependencies of a build merged into one tree of links, which the sandbox shows as
//! `/usr` or at a path of its own.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use crate::store::{Lab, StorePath, with_path};

/// Merges `dependencies`, each a name for error messages and a store path, into a new tree at
/// `dir`. Two dependencies providing the same path is an error naming both.
pub fn compose(lab: &Lab, dir: &Path, dependencies: &[(&str, &StorePath)]) -> Result<(), String> {
    let whole: Vec<_> = dependencies
        .iter()
        .map(|(name, path)| (*name, *path, &[] as &[String]))
        .collect();
    compose_skipping(lab, dir, &whole)
}

/// Like [`compose`], leaving out what each dependency has at or below the paths given with it.
pub fn compose_skipping(
    lab: &Lab,
    dir: &Path,
    dependencies: &[(&str, &StorePath, &[String])],
) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(with_path(dir))?;
    let mut providers = BTreeMap::new();
    let mut ordered: Vec<_> = dependencies.iter().collect();
    ordered.sort_by_key(|(_, _, skip)| !skip.is_empty());
    for (name, dependency, skip) in ordered {
        let from = lab.on_host(dependency);
        // What's left of the seed is only there until something better is declared.
        let source = Source {
            from: &from,
            name,
            dependency,
            skip,
            yields: !skip.is_empty(),
        };
        merge(&source, Path::new(""), dir, &mut providers)?;
    }
    Ok(())
}

struct Source<'a, 'n> {
    from: &'a Path,
    name: &'n str,
    dependency: &'a StorePath,
    skip: &'a [String],
    /// Whether another dependency's file replaces this one's instead of being a conflict.
    yields: bool,
}

fn merge<'n>(
    dep: &Source<'_, 'n>,
    relative: &Path,
    dir: &Path,
    providers: &mut BTreeMap<PathBuf, &'n str>,
) -> Result<(), String> {
    let name = dep.name;
    let source = dep.from.join(relative);
    let mut entries = fs::read_dir(&source)
        .map_err(with_path(&source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(with_path(&source))?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = relative.join(entry.file_name());
        if path == Path::new(OWN) || dep.skip.iter().any(|skipped| path.starts_with(skipped)) {
            continue;
        }
        let target = dir.join(&path);
        let kind = entry.file_type().map_err(with_path(&path))?;
        if kind.is_dir() {
            if let Some(other) = providers.get(&path) {
                return Err(conflict(&path, other, name));
            }
            fs::create_dir_all(&target).map_err(with_path(&target))?;
            merge(dep, &path, dir, providers)?;
            continue;
        }
        if dep.yields && providers.contains_key(&path) {
            continue;
        }
        if let Some(other) = providers.insert(path.clone(), name) {
            return Err(conflict(&path, other, name));
        }
        if target.is_dir() {
            return Err(format!(
                "`{}` is a file in `{name}` and a directory in another dependency",
                path.display()
            ));
        }
        let inside = Path::new(&dep.dependency.inside()).join(&path);
        symlink(&inside, &target).map_err(with_path(&target))?;
    }
    Ok(())
}

/// What a package keeps for wrath itself, such as the store paths of its runtime dependencies,
/// which builds have no use for and every package names the same.
const OWN: &str = "lib/distro";

fn conflict(path: &Path, first: &str, second: &str) -> String {
    format!(
        "`{}` is provided by both `{first}` and `{second}`",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dependency(lab: &Lab, name: &str, files: &[&str]) -> StorePath {
        let path = StorePath::derive("test", &[name.as_bytes()]);
        for file in files {
            let at = lab.on_host(&path).join(file);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, name).unwrap();
        }
        path
    }

    #[test]
    fn dependencies_merge_into_links_to_where_they_are_in_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let a = dependency(&lab, "a", &["bin/a", "lib/liba.so"]);
        let b = dependency(&lab, "b", &["bin/b"]);
        let view = dir.path().join("view");

        compose(&lab, &view, &[("a", &a), ("b", &b)]).unwrap();

        assert_eq!(
            fs::read_link(view.join("bin/a")).unwrap(),
            Path::new(&a.inside()).join("bin/a")
        );
        assert_eq!(
            fs::read_link(view.join("bin/b")).unwrap(),
            Path::new(&b.inside()).join("bin/b")
        );
        assert!(view.join("lib").is_dir());
    }

    #[test]
    fn two_dependencies_with_one_file_is_an_error_naming_both() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let a = dependency(&lab, "a", &["bin/tool"]);
        let b = dependency(&lab, "b", &["bin/tool"]);

        let error = compose(&lab, &dir.path().join("view"), &[("a", &a), ("b", &b)]).unwrap_err();

        assert_eq!(error, "`bin/tool` is provided by both `a` and `b`");
    }

    #[test]
    fn what_packages_keep_for_wrath_itself_stays_out_of_views() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let a = dependency(&lab, "a", &["bin/a", "lib/distro/runtime-deps"]);
        let b = dependency(&lab, "b", &["lib/distro/runtime-deps"]);
        let view = dir.path().join("view");

        compose(&lab, &view, &[("a", &a), ("b", &b)]).unwrap();

        assert!(fs::read_link(view.join("bin/a")).is_ok());
        assert!(!view.join("lib/distro").exists());
    }

    #[test]
    fn a_file_where_another_has_a_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let a = dependency(&lab, "a", &["share/thing/file"]);
        let b = dependency(&lab, "b", &["share/thing"]);

        let error = compose(&lab, &dir.path().join("view"), &[("a", &a), ("b", &b)]).unwrap_err();

        assert!(error.contains("`share/thing`"), "{error}");
    }

    #[test]
    fn skipped_paths_stay_out_of_the_view() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let seed = dependency(&lab, "seed", &["bin/gcc", "bin/make", "lib/libc.so"]);
        let skip = ["bin/gcc".to_owned(), "lib".to_owned()];
        let view = dir.path().join("view");

        compose_skipping(&lab, &view, &[("seed", &seed, &skip)]).unwrap();

        assert!(view.join("bin/make").symlink_metadata().is_ok());
        assert!(view.join("bin/gcc").symlink_metadata().is_err());
        assert!(!view.join("lib").exists());
    }

    #[test]
    fn links_inside_a_dependency_go_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let lab = Lab::new(dir.path());
        let a = dependency(&lab, "a", &["lib/liba.so.1"]);
        symlink("liba.so.1", lab.on_host(&a).join("lib/liba.so")).unwrap();
        let view = dir.path().join("view");

        compose(&lab, &view, &[("a", &a)]).unwrap();

        assert_eq!(
            fs::read_link(view.join("lib/liba.so")).unwrap(),
            Path::new(&a.inside()).join("lib/liba.so")
        );
    }
}
