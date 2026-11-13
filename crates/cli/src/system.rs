//! Evaluating a configuration directory with the modules in a module directory.

use std::fs;
use std::path::Path;

use wrath_eval::{ConfigFs, DirFs, Evaluation, Input, Module, Schema, Sources, render};

/// The order modules are loaded and run in. The result must not depend on it.
#[derive(Debug, Clone, Copy)]
pub enum ModuleOrder {
    /// By file name.
    Sorted,
    /// A permutation chosen by the seed, for checking that order doesn't matter.
    Shuffled(u64),
}

/// Evaluates `config` with every module in `modules`. Errors come back rendered.
pub fn evaluate_dir(
    config: &Path,
    modules: &Path,
    order: ModuleOrder,
    role: Option<&str>,
) -> Result<Evaluation, String> {
    evaluate_with(&crate::fetch::Sourced::dir(config), modules, order, role)
}

/// Evaluates the configuration at `config` as the system an image of it boots: with
/// `image.live = true`, from the image's own root and without the disks it describes, which
/// that image installs the configured system on.
pub fn evaluate_live(
    config: &Path,
    modules: &Path,
    role: Option<&str>,
) -> Result<Evaluation, String> {
    let sourced = crate::fetch::Sourced::dir(config);
    let start = wrath_eval::starting_file(&sourced, role).map_err(|e| e.message)?;
    let live = LiveImage {
        inner: &sourced,
        start,
    };
    evaluate_with(&live, modules, ModuleOrder::Sorted, role)
}

/// A configuration with `image.live = true` added at the end of its starting file, as its author
/// could write it, so errors still point at its own lines.
struct LiveImage<'a> {
    inner: &'a dyn ConfigFs,
    start: String,
}

/// What the starting file of [`LiveImage`] ends with.
const LIVE: &str = "\n[image]\nlive = true\n";

impl ConfigFs for LiveImage<'_> {
    fn read(&self, path: &str) -> std::io::Result<String> {
        let text = self.inner.read(path)?;
        Ok(if path == self.start {
            text + LIVE
        } else {
            text
        })
    }

    fn read_bytes(&self, path: &str) -> std::io::Result<Vec<u8>> {
        self.inner.read_bytes(path)
    }

    fn toml_files(&self, dir: &str) -> std::io::Result<Vec<String>> {
        self.inner.toml_files(dir)
    }

    fn display(&self, path: &str) -> String {
        self.inner.display(path)
    }

    fn source(&self, name: &str, url: &str) -> Result<Box<dyn ConfigFs>, String> {
        self.inner.source(name, url)
    }

    fn dir(&self) -> Option<&Path> {
        self.inner.dir()
    }
}

/// Evaluates the project at `config` as everyone else has it, without `wrath.local.toml`.
pub fn evaluate_shared(config: &Path, modules: &Path) -> Result<Evaluation, String> {
    let shared = crate::fetch::Sourced::new(crate::project::Shared(DirFs::new(config)), config);
    evaluate_with(&shared, modules, ModuleOrder::Sorted, None)
}

/// Like [`evaluate_dir`], for a configuration from anywhere, with modules in sorted order.
pub fn evaluate(
    config: &dyn ConfigFs,
    modules: &Path,
    role: Option<&str>,
) -> Result<Evaluation, String> {
    evaluate_with(config, modules, ModuleOrder::Sorted, role)
}

fn evaluate_with(
    config: &dyn ConfigFs,
    modules: &Path,
    order: ModuleOrder,
    role: Option<&str>,
) -> Result<Evaluation, String> {
    let mut sources = Sources::default();
    let kind = wrath_eval::kind_of(config);
    let mut modules = crate::platforms::modules(config, role, modules, &mut sources, order)?;
    modules.retain(|module| module.configurations().contains(&kind));
    // A home belongs to whoever runs the command.
    let user = std::env::var("USER").ok();
    wrath_eval::evaluate(Input {
        sources,
        config,
        kind,
        user: user.as_deref(),
        role,
        schema: Schema::default(),
        modules: &modules,
    })
    .map_err(|e| e.to_string())
}

/// Loads every Starlark module (`*.star`) in `dir`.
pub fn load_modules(
    dir: &Path,
    sources: &mut Sources,
    order: ModuleOrder,
) -> Result<Vec<Box<dyn Module>>, String> {
    let mut paths = fs::read_dir(dir)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::io::Result<Vec<_>>>()
        })
        .map_err(|e| format!("cannot read modules in {}: {e}", dir.display()))?;
    paths.sort();
    if let ModuleOrder::Shuffled(seed) = order {
        shuffle(&mut paths, seed);
    }
    let mut modules = Vec::new();
    let mut errors = Vec::new();
    for path in paths {
        if path.extension().is_none_or(|extension| extension != "star") {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        match wrath_starlark::load(sources, &path.display().to_string(), text, dir) {
            Ok(module) => modules.push(module),
            Err(diagnostic) => errors.push(diagnostic),
        }
    }
    if errors.is_empty() {
        Ok(modules)
    } else {
        Err(render(&errors, sources))
    }
}

/// A Fisher-Yates shuffle driven by a small xorshift generator, so a seed always gives the same
/// permutation.
fn shuffle<T>(items: &mut [T], seed: u64) {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    for i in (1..items.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bound = u64::try_from(i + 1).expect("fewer than 2^64 items");
        let j = usize::try_from(state % bound).expect("the index is below the length");
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use wrath_eval::Value;
    use wrath_model::Model;

    use super::{ModuleOrder, evaluate_dir, evaluate_live, shuffle};

    fn repository() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    /// A configuration with an encrypted disk and a root erased at every boot, as installed and as
    /// the system an image of it boots.
    fn installed_and_live() -> (Model, Model) {
        let config = repository().join("scenarios/fixtures/native-state");
        let modules = repository().join("modules");
        let installed = evaluate_dir(&config, &modules, ModuleOrder::Sorted, None).unwrap();
        let live = evaluate_live(&config, &modules, None).unwrap();
        (
            Model::from_evaluation(&installed),
            Model::from_evaluation(&live),
        )
    }

    fn kernel_params(model: &Model) -> Vec<String> {
        let Value::Table(boot) = &model.base["boot"] else {
            panic!("boot is a table");
        };
        let Some(Value::List(params)) = boot.get("kernel_params") else {
            panic!("the kernel's command line is a list");
        };
        params
            .iter()
            .map(|param| match param {
                Value::Str(text) => text.clone(),
                other => panic!("{other:?} isn't a string"),
            })
            .collect()
    }

    #[test]
    fn an_installed_system_unlocks_and_mounts_its_disks_and_keeps_its_state() {
        let (installed, _) = installed_and_live();
        assert!(installed.base.contains_key("storage"));
        assert!(installed.base.contains_key("state"));
        let params = kernel_params(&installed);
        assert!(!params.iter().any(|p| p.starts_with("root=")), "{params:?}");
        assert!(!installed.files.contains_key("/etc/grow-root/50-root.conf"));
    }

    #[test]
    fn the_system_an_image_boots_has_only_the_images_root() {
        let (_, live) = installed_and_live();
        assert!(!live.base.contains_key("storage"), "no disks to wait for");
        assert!(
            !live.base.contains_key("state"),
            "nothing kept on disks it doesn't have"
        );
        assert_eq!(kernel_params(&live)[..2], ["root=LABEL=wrath-root", "rw"]);
        assert!(live.files.contains_key("/etc/grow-root/50-root.conf"));
    }

    #[test]
    fn the_system_an_image_boots_is_otherwise_the_configured_one() {
        let (installed, live) = installed_and_live();
        assert_eq!(
            live.packages, installed.packages,
            "the tools to install with are there"
        );
        assert_eq!(live.base["system"], installed.base["system"]);
        assert_eq!(live.base["users"], installed.base["users"]);
    }

    #[test]
    fn shuffles_are_permutations_that_depend_on_the_seed() {
        let original: Vec<u32> = (0..8).collect();
        let shuffled = |seed| {
            let mut items = original.clone();
            shuffle(&mut items, seed);
            items
        };

        let mut sorted = shuffled(1);
        sorted.sort_unstable();
        assert_eq!(sorted, original);
        assert_eq!(shuffled(1), shuffled(1));
        assert_ne!(shuffled(1), shuffled(2));
    }
}
