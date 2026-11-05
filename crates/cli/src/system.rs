//! Evaluating a configuration directory with the modules in a module directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use distro_eval::{DirFs, Evaluation, Input, Module, Schema, Sources, Value, evaluate, render};

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
) -> Result<Evaluation, String> {
    let mut sources = Sources::default();
    let modules = load_modules(modules, &mut sources, order)?;
    evaluate(Input {
        sources,
        config: &DirFs::new(config),
        schema: Schema::default(),
        modules: &modules,
        facts: &host_facts(),
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
        match distro_starlark::load(sources, &path.display().to_string(), text) {
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

/// Facts about the machine `distro` runs on, for `${facts.…}` references.
fn host_facts() -> Value {
    let cpus = std::thread::available_parallelism().map_or(1, usize::from);
    let cpu = BTreeMap::from([(
        "count".to_owned(),
        Value::Int(i64::try_from(cpus).unwrap_or(i64::MAX)),
    )]);
    Value::Table(BTreeMap::from([("cpu".to_owned(), Value::Table(cpu))]))
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
    use super::shuffle;

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
