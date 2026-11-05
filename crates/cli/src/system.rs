//! Evaluating a configuration directory with the modules in a module directory.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use distro_eval::{
    Diagnostic, DirFs, Evaluation, Input, Module, Schema, Sources, Value, evaluate, render,
};

/// The module languages, by file extension.
const LOADERS: [(&str, Loader); 2] = [("luau", distro_luau::load), ("star", distro_starlark::load)];

type Loader = fn(&mut Sources, &str, String) -> Result<Box<dyn Module>, Diagnostic>;

/// Evaluates `config` with every module in `modules`. Errors come back rendered.
pub fn evaluate_dir(config: &Path, modules: &Path) -> Result<Evaluation, String> {
    let mut sources = Sources::default();
    let modules = load_modules(modules, &mut sources)?;
    evaluate(Input {
        sources,
        config: &DirFs::new(config),
        schema: Schema::default(),
        modules: &modules,
        facts: &host_facts(),
    })
    .map_err(|e| e.to_string())
}

/// Loads every module file in `dir`, in file name order, by extension.
fn load_modules(dir: &Path, sources: &mut Sources) -> Result<Vec<Box<dyn Module>>, String> {
    let mut paths = fs::read_dir(dir)
        .and_then(|entries| {
            entries
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<std::io::Result<Vec<_>>>()
        })
        .map_err(|e| format!("cannot read modules in {}: {e}", dir.display()))?;
    paths.sort();
    let mut modules = Vec::new();
    let mut errors = Vec::new();
    for path in paths {
        let Some(loader) = path
            .extension()
            .and_then(|extension| LOADERS.iter().find(|(known, _)| extension == *known))
            .map(|(_, loader)| loader)
        else {
            continue;
        };
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        match loader(sources, &path.display().to_string(), text) {
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
