//! Checks that run the evaluator: bad configs against their golden errors, and modules for
//! determinism.

use std::fs;
use std::path::{Path, PathBuf};

use distro_model::Model;

use crate::system::{self, ModuleOrder};

/// The file in a bad-config case holding the exact error output expected.
const EXPECTED: &str = "expected-error.txt";

/// Evaluates every case in `dir` (one configuration per subdirectory) and compares its errors
/// with the case's `expected-error.txt`. With `bless`, writes the actual output there instead.
pub fn bad_configs(dir: &Path, modules: &Path, bless: bool) -> Result<bool, String> {
    let mut all_passed = true;
    for case in subdirectories(dir)? {
        let name = file_name(&case);
        let actual = match system::evaluate_dir(&case, modules, ModuleOrder::Sorted) {
            Ok(_) => "(no error)\n".to_owned(),
            Err(errors) => errors,
        };
        let expected_path = case.join(EXPECTED);
        if bless {
            fs::write(&expected_path, &actual)
                .map_err(|e| format!("{}: {e}", expected_path.display()))?;
            println!("BLESS {name}");
            continue;
        }
        match fs::read_to_string(&expected_path) {
            Ok(expected) if expected == actual => println!("PASS  {name}"),
            Ok(expected) => {
                all_passed = false;
                println!("FAIL  {name}: the error differs from {EXPECTED}");
                println!("      expected:");
                print_indented(&expected);
                println!("      actual:");
                print_indented(&actual);
            }
            Err(_) => {
                all_passed = false;
                println!("FAIL  {name}: no {EXPECTED}; run with --bless and review it");
            }
        }
    }
    Ok(all_passed)
}

fn print_indented(text: &str) {
    for line in text.lines() {
        println!("        {line}");
    }
}

/// Evaluates every configuration in `configs` `runs` times with the modules in `modules`,
/// loading them fresh and in a different order each time, and checks that every run produces
/// the same model bytes.
pub fn determinism(modules: &Path, configs: &Path, runs: u64) -> Result<bool, String> {
    let mut all_passed = true;
    for config in subdirectories(configs)? {
        let canonical = (0..runs)
            .map(|seed| {
                system::evaluate_dir(&config, modules, ModuleOrder::Shuffled(seed))
                    .map(|evaluation| Model::from_evaluation(&evaluation).canonical())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let stable = canonical.windows(2).all(|pair| pair[0] == pair[1]);
        all_passed &= stable;
        println!(
            "{}  {:<16} {runs} runs",
            if stable { "PASS" } else { "FAIL" },
            file_name(&config)
        );
    }
    Ok(all_passed)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn subdirectories(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}
