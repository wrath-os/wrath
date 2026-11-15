//! Step 3: building packages on the final toolchain, fixing up what they say about `/usr`, and
//! checking them with their smoke commands and tests.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::sync::Mutex;

use crate::chain::{self, Built, Package, Prepared, Sources};
use crate::fixup;
use crate::sandbox::{self, Limits, Status, Step, View};
use crate::seed;
use crate::store::{Lab, StorePath, with_path};

#[derive(Debug)]
struct Check {
    what: String,
    status: Status,
    log: String,
}

/// Runs `command` from the package's final location with no view and no `/usr`.
fn run_final(
    lab: &Lab,
    seed: &StorePath,
    out: &StorePath,
    name: &str,
    command: &str,
) -> Result<Check, String> {
    let dir = lab.work.join("alone").join(out.hash()).join(name);
    chain::remove(&dir)?;
    let mut inputs = chain::runtime_closure(lab, out)?;
    inputs.insert(seed.clone());
    let bins = format!("{0}/bin:{1}/bin", out.inside(), seed.inside());
    let log = dir.join("log");
    let outcome = sandbox::run(
        lab,
        &Step {
            name: name.to_owned(),
            argv: vec![
                "/bin/sh".to_owned(),
                "-euc".to_owned(),
                match command.strip_prefix("sh: ") {
                    Some(script) => script.to_owned(),
                    // From the package, so `bin/jq --version` and a pipeline into it both work.
                    None => format!("cd {} && {command}", out.inside()),
                },
            ],
            inputs: inputs.into_iter().collect(),
            output: None,
            view: None,
            build_dir: dir.join("build"),
            log: log.clone(),
            env: BTreeMap::from([("PATH".to_owned(), bins), ("OUT".to_owned(), out.inside())]),
            limits: Limits::DEFAULT,
            links: vec![(format!("{}/bin/bash", seed.inside()), "/bin/sh".to_owned())],
            at: None,
        },
    )?;
    Ok(Check {
        what: command.to_owned(),
        status: outcome.status,
        log: log.display().to_string(),
    })
}

/// Prints a suite's own logs when it fails, since the build tree doesn't outlive the step.
const SHOW_TEST_LOGS: &str = concat!(
    r#"trap 'status=$?; [ $status -eq 0 ] || PATH=$PATH:/usr/bin find /build "#,
    r#"\( -name test-suite.log -o -name testsuite.log -o -name "*.failed" \) -type f "#,
    r#"-exec sh -c "for f; do echo \"=== \$f\"; head -c 100000 \"\$f\"; done" _ {} +; "#,
    r#"exit $status' EXIT"#,
);

/// Runs the package's own tests, against the installed package with no `/usr` unless they
/// compile as they run.
fn run_check(
    lab: &Lab,
    package: &Package,
    prepared: &Prepared,
    built: &Built,
) -> Result<Option<Check>, String> {
    let mut extra_paths = Vec::new();
    let mut extra_inputs = BTreeSet::new();
    for id in &package.check_deps {
        let path = built
            .paths
            .get(id)
            .ok_or_else(|| format!("`{}`'s tests need `{id}`, not built before it", package.id))?;
        extra_inputs.insert(path.clone());
        extra_inputs.extend(built.closures[id].iter().cloned());
        link_tools(lab, path, &prepared.view)?;
        extra_paths.push(format!("{}/bin", path.inside()));
    }
    let shell = |command: &str| {
        vec![
            "/bin/sh".to_owned(),
            "-euc".to_owned(),
            format!("cd /build/src\n{command}"),
        ]
    };
    let log = chain::logs_dir(lab).join(format!("{}.check.log", package.id));
    if let Some(command) = &package.check_build {
        let outcome = sandbox::run(
            lab,
            &Step {
                name: format!("{} check build", package.id),
                argv: shell(command),
                inputs: prepared
                    .closure
                    .iter()
                    .chain([&prepared.out])
                    .cloned()
                    .collect(),
                output: None,
                view: Some(prepared.view.clone()),
                build_dir: prepared.build_dir.clone(),
                log: log.clone(),
                env: prepared.env.clone(),
                limits: prepared.limits,
                links: Vec::new(),
                at: Some((prepared.out.clone(), prepared.out.temporary())),
            },
        )?;
        if outcome.status != Status::Passed {
            return Ok(Some(Check {
                what: format!("building tests: {command}"),
                status: outcome.status,
                log: log.display().to_string(),
            }));
        }
    }
    let Some(command) = &package.check else {
        return Ok(None);
    };
    let in_view = package.check_in_view;
    if !in_view {
        // Depfiles name headers in /usr, which isn't there, and make would rebuild from them.
        let blank = "find . \\( -name '*.Po' -o -name '*.Plo' \\) -type f -exec sh -c \
                     'for f; do : > \"$f\"; done' _ {} +";
        let outcome = sandbox::run(
            lab,
            &Step {
                name: format!("{} blank depfiles", package.id),
                argv: shell(blank),
                inputs: prepared.closure.iter().cloned().collect(),
                output: None,
                view: Some(prepared.view.clone()),
                build_dir: prepared.build_dir.clone(),
                log: log.with_extension("blank.log"),
                env: prepared.env.clone(),
                limits: prepared.limits,
                links: Vec::new(),
                at: None,
            },
        )?;
        if outcome.status != Status::Passed {
            return Err(format!("blanking depfiles for {}", package.id));
        }
    }
    let mut env = prepared.env.clone();
    if in_view
        && !extra_paths.is_empty()
        && let Some(path) = env.get_mut("PATH")
    {
        *path = format!("{}:{path}", extra_paths.join(":"));
    }
    let view = if in_view {
        prepared.view.clone()
    } else {
        let mut bins = vec![format!("{}/bin", prepared.out.temporary().inside())];
        bins.extend(extra_paths);
        for dep in &prepared.dep_paths {
            if lab.on_host(dep).join("bin").is_dir() {
                bins.push(format!("{}/bin", dep.inside()));
            }
        }
        env.insert("PATH".to_owned(), bins.join(":"));
        env.remove("WRATH_LINK_FLAGS");
        tools_only(prepared)?
    };
    let outcome = sandbox::run(
        lab,
        &Step {
            name: format!("{} check", package.id),
            argv: shell(&format!("{SHOW_TEST_LOGS}\n{command}")),
            inputs: prepared
                .closure
                .iter()
                .chain([&prepared.out])
                .chain(&extra_inputs)
                .cloned()
                .collect(),
            output: None,
            view: Some(view),
            build_dir: prepared.build_dir.clone(),
            log: log.clone(),
            env,
            limits: prepared.limits,
            links: Vec::new(),
            at: Some((prepared.out.clone(), prepared.out.temporary())),
        },
    )?;
    Ok(Some(Check {
        what: command.clone(),
        status: outcome.status,
        log: log.display().to_string(),
    }))
}

/// Links the programs of `path` into `view`, for a test that names them as `/usr/bin/...`.
fn link_tools(lab: &Lab, path: &StorePath, view: &View) -> Result<(), String> {
    let bin = lab.on_host(path).join("bin");
    for entry in fs::read_dir(&bin).map_err(with_path(&bin))? {
        let entry = entry.map_err(with_path(&bin))?;
        let link = view.dir.join("bin").join(entry.file_name());
        if link.symlink_metadata().is_err() {
            let target = Path::new(&path.inside())
                .join("bin")
                .join(entry.file_name());
            symlink(target, &link).map_err(with_path(&link))?;
        }
    }
    Ok(())
}

/// The view with only its programs, for a test suite whose scripts name tools by path. Libraries,
/// headers and data stay out of reach, so a program that needs them from `/usr` still fails.
fn tools_only(prepared: &Prepared) -> Result<View, String> {
    let dir = prepared.build_dir.join("tools-only");
    chain::remove(&dir)?;
    fs::create_dir_all(dir.join("bin")).map_err(with_path(&dir))?;
    let bin = prepared.view.dir.join("bin");
    for entry in fs::read_dir(&bin).map_err(with_path(&bin))? {
        let entry = entry.map_err(with_path(&bin))?;
        let target = fs::read_link(entry.path()).map_err(with_path(&entry.path()))?;
        symlink(target, dir.join("bin").join(entry.file_name())).map_err(with_path(&dir))?;
    }
    Ok(View { dir })
}

/// One smoke command or test run of a package, from `wrath build --check`.
pub struct Checked {
    pub id: String,
    /// The command, or what was being built.
    pub what: String,
    pub passed: bool,
    /// Where its output is.
    pub log: String,
}

/// Builds the packages `ids` and checks each: its smoke commands run on what was built, and its
/// own tests, when its recipe has them, run in a build of it made again and thrown away.
pub fn check_packages(
    repository: &Path,
    work: &Path,
    ids: &[&str],
) -> Result<Vec<Checked>, String> {
    unknown(ids, &crate::graph::recipe_ids(repository)?)?;
    let lab = Lab::new(work);
    let (mut built, seed, packages) = build_some(&lab, repository, Some(ids))?;
    let mut sources = Sources::load(repository)?;
    let mut checked = Vec::new();
    for id in ids {
        let package = packages
            .iter()
            .find(|package| package.id == *id)
            .ok_or_else(|| format!("there's no recipe for `{id}`"))?;
        let mut checks = Vec::new();
        for (index, command) in package.smoke.iter().enumerate() {
            let name = format!("{id}-smoke-{index}");
            checks.push(run_final(&lab, &seed, &built.paths[*id], &name, command)?);
        }
        if package.check.is_some()
            && let Some(prepared) =
                chain::rebuild_one(&lab, repository, package, &seed, &mut sources, &mut built)?
        {
            let tested = run_check(&lab, package, &prepared, &built);
            chain::remove(&prepared.build_dir)?;
            chain::remove(&lab.on_host(&prepared.out))?;
            checks.extend(tested?);
        }
        checked.extend(checks.into_iter().map(|check| Checked {
            id: (*id).to_owned(),
            passed: passed(&check),
            what: check.what,
            log: check.log,
        }));
    }
    Ok(checked)
}

fn passed(check: &Check) -> bool {
    check.status == Status::Passed
}

/// Builds the toolchain and then the system recipes, only `wanted` and what it depends on when
/// given. Returns what was built, the seed and the packages built.
pub(crate) fn build_some(
    lab: &Lab,
    repository: &Path,
    wanted: Option<&[&str]>,
) -> Result<(Built, StorePath, Vec<Package>), String> {
    let files = chain::recipe_files(repository)?;
    let recipes = chain::names_of(&files);
    let built = chain::build_toolchain(lab, repository)?;
    let seed = seed::path(lab)?;
    let sources = Sources::load(repository)?;
    let loaded = match wanted {
        Some(wanted) => chain::load_reached(lab, repository, &recipes, wanted)?,
        None => {
            let mut loaded = Vec::new();
            for file in recipes {
                loaded.extend(chain::load_styled(lab, repository, file)?);
            }
            loaded
        }
    };
    // The toolchain is built already.
    let loaded = loaded
        .into_iter()
        .filter(|package| !built.paths.contains_key(&package.id))
        .collect();
    let loaded = chain::in_order(loaded, |id| built.paths.contains_key(id))?;
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let shared = Mutex::new((built, sources));
    let packages = carry_on(loaded, slots(cores), |package, running| {
        build_shared(
            lab,
            repository,
            package,
            &seed,
            &shared,
            cores / running.max(1),
        )
    })?;
    let (built, _) = shared
        .into_inner()
        .map_err(|_| "a build stopped partway".to_owned())?;
    Ok((built, seed, packages))
}

/// How many packages build at once on a machine with `cores`: `WRATH_BUILDS` when it's set, or
/// one for every eight cores. Each build is told to run as many jobs as it has cores to share.
fn slots(cores: usize) -> usize {
    std::env::var("WRATH_BUILDS")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(cores / 8)
        .max(1)
}

/// [`build_and_finish`] for builds running side by side: what's built and the sources are
/// shared, and held only while a build is worked out and while it's finished, never while it
/// runs. It runs `jobs` jobs at once.
fn build_shared(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    seed: &StorePath,
    shared: &Mutex<(Built, Sources)>,
    jobs: usize,
) -> Result<(), String> {
    let lock = || {
        shared
            .lock()
            .map_err(|_| "another build stopped partway".to_owned())
    };
    let begun = {
        let mut held = lock()?;
        let (built, sources) = &mut *held;
        chain::begin(lab, repository, package, seed, sources, built, false)?
    };
    let chain::Begun::Run(mut pending) = begun else {
        return Ok(());
    };
    pending
        .step
        .env
        .insert("NPROC".to_owned(), jobs.max(2).to_string());
    let outcome = chain::run(lab, &pending)?;
    let prepared = {
        let mut held = lock()?;
        let (built, _) = &mut *held;
        let prepared = chain::end(lab, package, *pending, &outcome, built)?;
        crate::settle::settle(lab, package, &prepared, built)?;
        prepared
    };
    // Its files are its own, so they're fixed up while other builds go on.
    chain::remove(&prepared.build_dir)?;
    if package.fixed_up() {
        fixup::fix_up(
            lab,
            &prepared,
            &mut fixup::Fixups::default(),
            &mut fixup::Leaks::default(),
        )?;
    }
    let mut held = lock()?;
    let (built, _) = &mut *held;
    fixup::finish(lab, package, &prepared.out, built)
}

/// That every package someone named, `wanted`, has a recipe among `ids`, before anything builds.
/// A system's packages aren't checked here: some are builds its configuration declares.
pub(crate) fn unknown(wanted: &[&str], ids: &BTreeSet<String>) -> Result<(), String> {
    let missing: Vec<String> = wanted
        .iter()
        .filter(|name| !ids.contains(**name))
        .map(|name| format!("there's no recipe for `{name}`"))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(missing.join("\n"))
    }
}

/// Builds `packages`, given in an order where each comes after what it needs, with `build`: each
/// starts as soon as what it needs among them is built, up to `slots` at once. `build` is told how
/// many are running with it. One that fails doesn't stop the others: what doesn't need it is
/// still built, so a run shows every failure at once, and what needs it is skipped. The error
/// lists the failures, in the order given, and how many packages waited on them.
fn carry_on(
    packages: Vec<Package>,
    slots: usize,
    build: impl Fn(&Package, usize) -> Result<(), String> + Sync,
) -> Result<Vec<Package>, String> {
    let ids: BTreeSet<&str> = packages.iter().map(|package| package.id.as_str()).collect();
    let mut needs = Vec::with_capacity(packages.len());
    for package in &packages {
        let deps: BTreeSet<String> = package
            .dep_ids()?
            .into_iter()
            .filter(|dep| ids.contains(dep.as_str()))
            .collect();
        needs.push(deps);
    }
    let mut waiting: Vec<usize> = (0..packages.len()).collect();
    let (mut built, mut broken) = (BTreeSet::new(), BTreeSet::new());
    let (mut done, mut failures) = (Vec::new(), Vec::new());
    let mut skipped = 0;
    std::thread::scope(|scope| {
        let (sender, results) = std::sync::mpsc::channel();
        let mut running = 0;
        loop {
            let mut still = Vec::with_capacity(waiting.len());
            for index in waiting {
                let package = &packages[index];
                if needs[index].iter().any(|dep| broken.contains(dep)) {
                    broken.insert(package.id.clone());
                    skipped += 1;
                } else if running < slots.max(1) && needs[index].is_subset(&built) {
                    running += 1;
                    let (sender, build) = (sender.clone(), &build);
                    scope.spawn(move || {
                        let _counted = crate::progress::running();
                        let _ = sender.send((index, build(package, running)));
                    });
                } else {
                    still.push(index);
                }
            }
            waiting = still;
            if running == 0 {
                break;
            }
            let Ok((index, result)) = results.recv() else {
                break;
            };
            running -= 1;
            match result {
                Ok(()) => {
                    built.insert(packages[index].id.clone());
                    done.push(index);
                }
                Err(error) => {
                    broken.insert(packages[index].id.clone());
                    failures.push((index, error));
                }
            }
        }
    });
    done.sort_unstable();
    failures.sort_unstable();
    let mut failures: Vec<String> = failures.into_iter().map(|(_, error)| error).collect();
    let mut packages: Vec<Option<Package>> = packages.into_iter().map(Some).collect();
    let done = done
        .into_iter()
        .filter_map(|index| packages[index].take())
        .collect();
    match failures.len() {
        0 => Ok(done),
        1 if skipped == 0 => Err(failures.remove(0)),
        count => {
            let waited = if skipped == 0 {
                String::new()
            } else {
                format!(", and {skipped} that need them weren't built")
            };
            Err(format!("{count} failed{waited}:\n{}", failures.join("\n")))
        }
    }
}

/// Builds `package` when it hasn't been, fixes it up, and moves it to where its contents put it,
/// so the next run has it.
pub(crate) fn build_and_finish(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    seed: &StorePath,
    sources: &mut chain::Sources,
    built: &mut Built,
) -> Result<(), String> {
    if let Some(prepared) = chain::build_one(lab, repository, package, seed, sources, built)? {
        chain::remove(&prepared.build_dir)?;
        fixup::run(lab, package, &prepared, built, package.fixed_up())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packages `zlib`, `curl` (needing zlib), `jq` and `git` (needing curl), in that order.
    fn chain_of_four(dir: &Path) -> Vec<Package> {
        fs::create_dir_all(dir.join("recipes")).unwrap();
        let recipe = |id: &str, deps: &str| {
            format!(
                "[[package]]\nid = \"{id}\"\nscope = \"runtime\"\nscript = \"true\"\n\
                 deps = [{deps}]\n"
            )
        };
        let text = [
            recipe("zlib", ""),
            recipe("curl", "\"zlib\""),
            recipe("jq", ""),
            recipe("git", "\"curl\""),
        ]
        .concat();
        fs::write(dir.join("recipes/r.toml"), text).unwrap();
        chain::load(dir, "r.toml").unwrap()
    }

    #[test]
    fn a_name_without_a_recipe_is_refused_before_anything_builds() {
        let ids: BTreeSet<String> = ["jq".to_owned(), "ripgrep".to_owned()].into();
        assert!(unknown(&["jq", "ripgrep"], &ids).is_ok());
        assert_eq!(
            unknown(&["jq", "ripgrp", "nope"], &ids).unwrap_err(),
            "there's no recipe for `ripgrp`\nthere's no recipe for `nope`"
        );
    }

    #[test]
    fn a_failed_package_stops_only_what_needs_it() {
        let dir = tempfile::tempdir().unwrap();
        let tried = Mutex::new(Vec::new());
        let error = carry_on(chain_of_four(dir.path()), 1, |package, _| {
            tried.lock().unwrap().push(package.id.clone());
            match package.id.as_str() {
                "zlib" => Err("`zlib` failed".to_owned()),
                _ => Ok(()),
            }
        })
        .unwrap_err();
        assert_eq!(tried.into_inner().unwrap(), ["zlib", "jq"]);
        assert_eq!(
            error,
            "1 failed, and 2 that need them weren't built:\n`zlib` failed"
        );
    }

    #[test]
    fn every_failure_is_reported_and_one_alone_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let failing = |ids: &'static [&'static str]| {
            move |package: &Package, _: usize| match ids.contains(&package.id.as_str()) {
                true => Err(format!("`{}` failed", package.id)),
                false => Ok(()),
            }
        };
        for slots in [1, 4] {
            let packages = || chain_of_four(dir.path());
            let error = carry_on(packages(), slots, failing(&["jq", "git"])).unwrap_err();
            assert_eq!(error, "2 failed:\n`jq` failed\n`git` failed");
            let error = carry_on(packages(), slots, failing(&["git"])).unwrap_err();
            assert_eq!(error, "`git` failed");
            let built = carry_on(packages(), slots, failing(&[])).unwrap();
            assert_eq!(built.len(), 4);
        }
    }

    /// Packages `zlib`, `curl` (needing zlib), `jq` and `git` (needing curl): jq needs nothing,
    /// so with room it builds beside zlib, and nothing starts before what it needs is built.
    #[test]
    fn a_package_starts_once_what_it_needs_is_built_beside_others() {
        let dir = tempfile::tempdir().unwrap();
        let events = Mutex::new(Vec::new());
        let most = Mutex::new(0);
        let built = carry_on(chain_of_four(dir.path()), 4, |package, running| {
            events.lock().unwrap().push(format!("start {}", package.id));
            let mut most = most.lock().unwrap();
            *most = (*most).max(running);
            drop(most);
            std::thread::sleep(std::time::Duration::from_millis(50));
            events.lock().unwrap().push(format!("end {}", package.id));
            Ok(())
        })
        .unwrap();
        assert_eq!(built.len(), 4);
        let events = events.into_inner().unwrap();
        let at = |event: &str| events.iter().position(|e| e == event).unwrap();
        assert!(at("start jq") < at("end zlib"), "{events:?}");
        for (needed, by) in [("zlib", "curl"), ("curl", "git")] {
            assert!(
                at(&format!("end {needed}")) < at(&format!("start {by}")),
                "{events:?}"
            );
        }
        assert!(most.into_inner().unwrap() >= 2, "{events:?}");
    }

    #[test]
    fn only_what_is_wanted_and_its_dependencies_are_needed() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("recipes")).unwrap();
        let recipe = |id: &str, deps: &str| {
            format!(
                "[[package]]\nid = \"{id}\"\nscope = \"runtime\"\nscript = \"true\"\n\
                 deps = [{deps}]\n"
            )
        };
        let text = [
            recipe("zlib", ""),
            recipe("curl", "\"zlib\""),
            recipe("jq", ""),
            recipe("git", "\"curl\", \"glibc\""),
        ]
        .concat();
        fs::write(dir.path().join("recipes/r.toml"), text).unwrap();
        let lab = Lab::new(dir.path().join("work"));
        let needed = |wanted: &[&str]| -> BTreeSet<String> {
            let found = chain::load_reached(&lab, dir.path(), &["r.toml"], wanted).unwrap();
            found.into_iter().map(|package| package.id).collect()
        };
        assert_eq!(
            needed(&["git"]),
            BTreeSet::from(["curl".into(), "git".into(), "zlib".into()])
        );
        assert_eq!(needed(&["jq"]), BTreeSet::from(["jq".into()]));
    }
}
