//! What an apply keeps so the system can still explain itself after the TOML has moved on: the
//! files as they were written, the lock, where each option's value came from, and what each
//! module read and produced. None of it goes into the model or any hash.

use std::path::Path;

use distro_eval::{Contribution, Evaluation, origin, outputs_of};
use distro_model::LOCK_FILE;
use serde_json::{Map, Value, json};

use crate::project::LOCAL_LOCK;

fn contributions(evaluation: &Evaluation, list: &[Contribution]) -> Value {
    list.iter()
        .map(|contribution| {
            json!({
                "origin": origin(evaluation, contribution),
                "value": contribution.value.to_string(),
            })
        })
        .collect()
}

fn options(evaluation: &Evaluation) -> Map<String, Value> {
    evaluation
        .options()
        .iter()
        .map(|(path, resolved)| {
            let option = json!({
                "value": resolved.value.to_string(),
                "set_by": contributions(evaluation, &resolved.set_by),
                "overridden": contributions(evaluation, &resolved.overridden),
                "removed_by": contributions(evaluation, &resolved.removed_by),
                "uses": resolved.uses.iter().map(|used| used.path.to_string()).collect::<Vec<_>>(),
            });
            (path.to_string(), option)
        })
        .collect()
}

fn modules(evaluation: &Evaluation) -> Map<String, Value> {
    evaluation
        .modules()
        .iter()
        .map(|rendered| {
            let reads: Vec<String> = rendered.reads.iter().map(ToString::to_string).collect();
            let module = json!({"reads": reads, "outputs": outputs_of(rendered)});
            (rendered.name.clone(), module)
        })
        .collect()
}

fn locks(config: &Path) -> Result<Map<String, Value>, String> {
    let mut locks = Map::new();
    for name in [LOCK_FILE, LOCAL_LOCK] {
        let path = config.join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                locks.insert(name.to_owned(), text.into());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(locks)
}

/// The record of `evaluation`, whose configuration is in `config`.
pub fn record(evaluation: &Evaluation, config: &Path) -> Result<Value, String> {
    let sources = evaluation.sources();
    let files: Map<String, Value> = sources
        .files()
        .map(|file| (sources.name(file).to_owned(), sources.text(file).into()))
        .collect();
    Ok(json!({
        "sources": files,
        "lock": locks(config)?,
        "explanation": {"options": options(evaluation), "modules": modules(evaluation)},
    }))
}
