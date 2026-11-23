//! Modules that each need another to have run first, found from their code before any of them
//! runs (0044). A module that reads something another writes runs after it.

use std::collections::{BTreeSet, VecDeque};

use crate::diag::Diagnostic;
use crate::module::{Module, OptionUse};
use crate::path::OptionPath;

/// One module having to run after another: what it reads, and the other's write that reaches it.
struct After<'a> {
    module: usize,
    read: &'a OptionUse,
    write: &'a OptionUse,
}

/// An error for each cycle between `modules`.
pub(crate) fn cycles(modules: &[Box<dyn Module>]) -> Vec<Diagnostic> {
    let sites: Vec<_> = modules.iter().map(|module| module.option_uses()).collect();
    let overlap = |a: &OptionPath, b: &OptionPath| a.starts_with(b) || b.starts_with(a);
    let after: Vec<Vec<After>> = sites
        .iter()
        .enumerate()
        .map(|(reader, own)| {
            sites
                .iter()
                .enumerate()
                .filter(|&(writer, _)| writer != reader)
                .filter_map(|(writer, theirs)| {
                    own.reads.iter().find_map(|read| {
                        let write = theirs
                            .writes
                            .iter()
                            .find(|w| overlap(&read.path, &w.path))?;
                        Some(After {
                            module: writer,
                            read,
                            write,
                        })
                    })
                })
                .collect()
        })
        .collect();
    let mut seen = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for start in 0..modules.len() {
        let Some(cycle) = cycle_from(start, &after) else {
            continue;
        };
        let members: BTreeSet<usize> = cycle.iter().map(|&(from, _)| from).collect();
        if seen.insert(members) {
            diagnostics.push(report(modules, &after, &cycle));
        }
    }
    diagnostics
}

/// The shortest way back to `start` through modules that have to run first, as each step's module
/// and the index of its edge.
fn cycle_from(start: usize, after: &[Vec<After>]) -> Option<Vec<(usize, usize)>> {
    let mut previous: Vec<Option<(usize, usize)>> = vec![None; after.len()];
    let mut queue = VecDeque::from([start]);
    let mut visited = vec![false; after.len()];
    visited[start] = true;
    while let Some(module) = queue.pop_front() {
        for (edge, step) in after[module].iter().enumerate() {
            if step.module == start {
                let mut path = vec![(module, edge)];
                let mut at = module;
                while let Some((from, from_edge)) = previous[at] {
                    path.push((from, from_edge));
                    at = from;
                }
                path.reverse();
                return Some(path);
            }
            if !visited[step.module] {
                visited[step.module] = true;
                previous[step.module] = Some((module, edge));
                queue.push_back(step.module);
            }
        }
    }
    None
}

fn report(
    modules: &[Box<dyn Module>],
    after: &[Vec<After>],
    cycle: &[(usize, usize)],
) -> Diagnostic {
    let names: Vec<String> = cycle
        .iter()
        .map(|&(module, _)| format!("`{}`", modules[module].name()))
        .collect();
    let mut diagnostic = Diagnostic::error(format!(
        "modules {} each need another to have run first",
        names.join(", ")
    ));
    for &(module, edge) in cycle {
        let step = &after[module][edge];
        let (reader, writer) = (modules[module].name(), modules[step.module].name());
        diagnostic = diagnostic
            .label_opt(
                step.read.at,
                format!("`{reader}` reads `{}` here", step.read.path),
            )
            .label_opt(
                step.write.at,
                format!("`{writer}` writes `{}` here", step.write.path),
            )
            .note(format!(
                "`{reader}` reads `{}`, which `{writer}` writes",
                step.read.path
            ));
    }
    diagnostic
}
