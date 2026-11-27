//! Checks that run the evaluator: bad configs against their golden errors, configurations
//! against the files they write, and modules for determinism.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use wrath_eval::RuntimePart;
use wrath_model::Model;

use crate::system::{self, ModuleOrder};

/// The file in a bad-config case holding the exact error output expected.
const EXPECTED: &str = "expected-error.txt";

/// The directory in a case holding the files the configuration should write.
const RENDERED: &str = "expected";

/// Checks every case in `dir`, one configuration per subdirectory. A case with an `expected/`
/// directory renders: what the configuration writes is compared with the files there. Any other
/// case is a bad configuration whose errors are compared with its `expected-error.txt`. With
/// `bless`, what each case actually gives is written in place of what it expected.
pub fn cases(dir: &Path, modules: &Path, bless: bool) -> Result<bool, String> {
    let mut all_passed = true;
    for case in subdirectories(dir)? {
        all_passed &= if case.join(RENDERED).is_dir() {
            renders(&case, modules, bless)?
        } else {
            bad_config(&case, modules, bless)?
        };
    }
    Ok(all_passed)
}

fn bad_config(case: &Path, modules: &Path, bless: bool) -> Result<bool, String> {
    let name = file_name(case);
    let actual = match system::evaluate_dir(case, modules, ModuleOrder::Sorted, None) {
        Ok(_) => "(no error)\n".to_owned(),
        Err(errors) => errors,
    };
    let expected_path = case.join(EXPECTED);
    if bless {
        fs::write(&expected_path, &actual)
            .map_err(|e| format!("{}: {e}", expected_path.display()))?;
        println!("BLESS {name}");
        return Ok(true);
    }
    match fs::read_to_string(&expected_path) {
        Ok(expected) if expected == actual => {
            println!("PASS  {name}");
            Ok(true)
        }
        Ok(expected) => {
            println!("FAIL  {name}: the error differs from {EXPECTED}");
            print_difference(&expected, &actual);
            Ok(false)
        }
        Err(_) => {
            println!("FAIL  {name}: no {EXPECTED}; run with --bless and review it");
            Ok(false)
        }
    }
}

/// Compares each file under the case's `expected/` with what the configuration writes there.
fn renders(case: &Path, modules: &Path, bless: bool) -> Result<bool, String> {
    let name = file_name(case);
    let evaluation = match system::evaluate_dir(case, modules, ModuleOrder::Sorted, None) {
        Ok(evaluation) => evaluation,
        Err(errors) => {
            println!("FAIL  {name}: the configuration doesn't evaluate");
            print_indented(&errors);
            return Ok(false);
        }
    };
    let written = rendered(&Model::from_evaluation(&evaluation));
    let expected_dir = case.join(RENDERED);
    let expected = files_under(&expected_dir)?;
    if expected.is_empty() {
        println!("FAIL  {name}: {RENDERED}/ is empty; add the files to check, then --bless");
        return Ok(false);
    }
    let mut passed = true;
    for relative in &expected {
        let path = expected_dir.join(relative);
        let Some(actual) = written.get(relative) else {
            println!("FAIL  {name}: the configuration doesn't write {relative}");
            passed = false;
            continue;
        };
        if bless {
            fs::write(&path, actual).map_err(|e| format!("{}: {e}", path.display()))?;
            continue;
        }
        let wanted = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if wanted != *actual {
            println!("FAIL  {name}: {relative} differs");
            print_difference(&wanted, actual);
            passed = false;
        }
    }
    if bless {
        println!("BLESS {name}");
    } else if passed {
        println!("PASS  {name}");
    }
    Ok(passed)
}

/// What `model` writes, by where it goes under the root: files, those filled in on the machine
/// with each secret and machine value as `${...}`, units in `etc/systemd/system`, and each
/// user's files and units in `home/<user>`.
fn rendered(model: &Model) -> BTreeMap<String, String> {
    let mut written = BTreeMap::new();
    for (path, file) in &model.files {
        written.insert(path.trim_start_matches('/').to_owned(), file.text.clone());
    }
    for (path, template) in &model.templates {
        let text = template.parts.iter().map(placeholder).collect();
        written.insert(path.trim_start_matches('/').to_owned(), text);
    }
    for (name, unit) in &model.units {
        written.insert(format!("etc/systemd/system/{name}"), unit.text());
    }
    for (user, home) in &model.homes {
        for (path, file) in &home.files {
            let relative = path.trim_start_matches("~/");
            written.insert(format!("home/{user}/{relative}"), file.text.clone());
        }
        for (name, unit) in &home.units {
            let path = format!("home/{user}/.config/systemd/user/{name}");
            written.insert(path, unit.text());
        }
    }
    written
}

/// A part of a file filled in on the machine, as a configuration writes it.
fn placeholder(part: &RuntimePart) -> String {
    match part {
        RuntimePart::Text(text) => text.clone(),
        RuntimePart::Secret(name) | RuntimePart::QuotedSecret(name) => {
            format!("${{secret.{name}}}")
        }
        RuntimePart::Machine(path) | RuntimePart::QuotedMachine(path) => format!("${{{path}}}"),
    }
}

/// The files under `dir`, relative to it, sorted.
fn files_under(dir: &Path) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(at) = pending.pop() {
        for entry in fs::read_dir(&at).map_err(|e| format!("{}: {e}", at.display()))? {
            let path = entry.map_err(|e| format!("{}: {e}", at.display()))?.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(dir) {
                found.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    Ok(found)
}

fn print_difference(expected: &str, actual: &str) {
    println!("      expected:");
    print_indented(expected);
    println!("      actual:");
    print_indented(actual);
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
        for role in crate::all_roles(&config)? {
            let canonical = (0..runs)
                .map(|seed| {
                    let order = ModuleOrder::Shuffled(seed);
                    system::evaluate_dir(&config, modules, order, role.as_deref())
                        .map(|evaluation| Model::from_evaluation(&evaluation).canonical())
                })
                .collect::<Result<Vec<_>, _>>()?;
            let stable = canonical.windows(2).all(|pair| pair[0] == pair[1]);
            all_passed &= stable;
            let name = match &role {
                Some(role) => format!("{}:{role}", file_name(&config)),
                None => file_name(&config),
            };
            let status = if stable { "PASS" } else { "FAIL" };
            println!("{status}  {name:<16} {runs} runs");
        }
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
