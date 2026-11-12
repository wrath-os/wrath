//! Step 3: building packages on the final toolchain, fixing up what they say about `/usr`, and
//! checking them with their smoke commands and tests.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use crate::chain::{self, Built, Package, Prepared, Sources};
use crate::runpath;
use crate::sandbox::{self, Limits, Status, Step, View};
use crate::seed;
use crate::store::{Lab, StorePath, with_path};

#[derive(Debug, Default)]
pub(crate) struct Leaks {
    unresolved_shebangs: usize,
    samples: Vec<String>,
}

impl Leaks {
    fn sample(&mut self, text: String) {
        if self.samples.len() < 20 {
            self.samples.push(text);
        }
    }
}

#[derive(Debug)]
struct Check {
    what: String,
    status: Status,
    log: String,
}

#[derive(Debug, Default)]
pub(crate) struct Fixups {
    la_removed: usize,
    shebangs: usize,
}

pub(crate) fn files_under(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).map_err(with_path(dir))? {
        let entry = entry.map_err(with_path(dir))?;
        let kind = entry.file_type().map_err(with_path(&entry.path()))?;
        if kind.is_dir() {
            found.extend(files_under(&entry.path())?);
        } else {
            found.push(entry.path());
        }
    }
    Ok(found)
}

/// Writes `bytes` to a file that may be read-only, keeping its mode.
pub(crate) fn replace_contents(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mode = fs::metadata(path)
        .map_err(with_path(path))?
        .permissions()
        .mode();
    fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o200)).map_err(with_path(path))?;
    fs::write(path, bytes).map_err(with_path(path))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(with_path(path))
}

pub(crate) fn path_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"._+/-".contains(&byte)
}

/// Where `relative`, a path in the view, really is in the store, if some dependency provides it.
fn owner_of(view: &Path, relative: &str) -> Option<String> {
    let target = fs::read_link(view.join(relative)).ok()?;
    target.to_str().map(str::to_owned)
}

/// Where a script's interpreter really is, or `None` when nothing provides it.
fn real_interpreter(
    path: &str,
    prefix: &str,
    out_host: &Path,
    out_inside: &str,
    view: &Path,
) -> Option<String> {
    let rest = path.strip_prefix(prefix)?.trim_start_matches('/');
    if let Some(real) = owner_of(view, rest) {
        return Some(real);
    }
    out_host
        .join(rest)
        .exists()
        .then(|| format!("{out_inside}/{rest}"))
}

/// Points a script's first line at the store. Returns whether it changed.
fn fix_shebang(
    file: &Path,
    out_host: &Path,
    out_inside: &str,
    view: &Path,
    leaks: &mut Leaks,
) -> Result<bool, String> {
    let bytes = fs::read(file).map_err(with_path(file))?;
    if !bytes.starts_with(b"#!") {
        return Ok(false);
    }
    let end = bytes
        .iter()
        .position(|b| *b == b'\n')
        .unwrap_or(bytes.len());
    let Ok(line) = std::str::from_utf8(&bytes[2..end]) else {
        return Ok(false);
    };
    let mut words = line.split_whitespace();
    let Some(first) = words.next() else {
        return Ok(false);
    };
    let rest: Vec<&str> = words.collect();
    // /bin/sh is provided everywhere. Anything else in /bin is really a program of the view's.
    let moved = format!("/usr{first}");
    let first = if first.starts_with("/bin/") && first != "/bin/sh" {
        moved.as_str()
    } else {
        first
    };
    let prefix = "/usr";
    if !first.starts_with(prefix) {
        return Ok(false);
    }
    let (target, arguments): (String, Vec<&str>) = if first.ends_with("/bin/env") {
        match rest.iter().position(|word| !word.starts_with('-')) {
            Some(at) => (
                format!("{prefix}/bin/{}", rest[at]),
                rest[at + 1..].to_vec(),
            ),
            None => return Ok(false),
        }
    } else {
        (first.to_owned(), rest)
    };
    let Some(real) = real_interpreter(&target, prefix, out_host, out_inside, view) else {
        leaks.unresolved_shebangs += 1;
        leaks.sample(format!("shebang {first}: {}", file.display()));
        return Ok(false);
    };
    let mut line = format!("#!{real}");
    for word in arguments {
        line.push(' ');
        line.push_str(word);
    }
    let mut new = line.into_bytes();
    new.extend_from_slice(&bytes[end..]);
    replace_contents(file, &new)?;
    Ok(true)
}

/// What's done to a package once it's installed, on the host.
pub(crate) fn fix_up(
    lab: &Lab,
    prepared: &Prepared,
    fixups: &mut Fixups,
    leaks: &mut Leaks,
) -> Result<(), String> {
    let out = lab.on_host(&prepared.out);
    let inside = prepared.out.inside();
    let view = &prepared.view.dir;
    for file in files_under(&out)? {
        let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
        if meta.is_symlink() {
            continue;
        }
        if file
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".la"))
        {
            fs::remove_file(&file).map_err(with_path(&file))?;
            fixups.la_removed += 1;
            continue;
        }
        if fix_shebang(&file, &out, &inside, view, leaks)? {
            fixups.shebangs += 1;
        }
    }
    runpath::shrink_files(lab, &prepared.out, &files_under(&out)?, replace_contents)?;
    crate::split::strip(lab, &BTreeSet::from([prepared.out.clone()]))?;
    Ok(())
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
        let mut bins = vec![format!("{}/bin", prepared.out.inside())];
        bins.extend(extra_paths);
        for dep in &prepared.dep_paths {
            if lab.on_host(dep).join("bin").is_dir() {
                bins.push(format!("{}/bin", dep.inside()));
            }
        }
        env.insert("PATH".to_owned(), bins.join(":"));
        env.remove("LAB_LINK_FLAGS");
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

/// One smoke command or test run of a package, from `distro build --check`.
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
pub fn check_packages(repository: &Path, ids: &[&str]) -> Result<Vec<Checked>, String> {
    let lab = Lab::new(repository.join("work"));
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
    let files = crate::graph::system_recipes(repository)?;
    let recipes = crate::graph::names_of(&files);
    let mut built = chain::build(lab, repository, "toolchain.toml")?;
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
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
    let loaded = chain::in_order(loaded, |id| built.paths.contains_key(id))?;
    let packages = carry_on(loaded, |package| {
        build_and_finish(lab, repository, package, &seed, &mut sources, &mut built)
    })?;
    Ok((built, seed, packages))
}

/// Builds each of `packages`, in order, with `build`. One that fails doesn't stop the others:
/// what doesn't need it is still built, so a run shows every failure at once, and what needs it
/// is skipped. The error lists the failures and how many packages waited on them.
fn carry_on(
    packages: Vec<Package>,
    mut build: impl FnMut(&Package) -> Result<(), String>,
) -> Result<Vec<Package>, String> {
    let mut done = Vec::with_capacity(packages.len());
    let mut broken = BTreeSet::new();
    let mut failures = Vec::new();
    let mut skipped = 0;
    for package in packages {
        if package.dep_ids()?.iter().any(|dep| broken.contains(dep)) {
            broken.insert(package.id.clone());
            skipped += 1;
            continue;
        }
        match build(&package) {
            Ok(()) => done.push(package),
            Err(error) => {
                broken.insert(package.id.clone());
                failures.push(error);
            }
        }
    }
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
        fix_up(
            lab,
            &prepared,
            &mut Fixups::default(),
            &mut Leaks::default(),
        )?;
        chain::finish(lab, package, &prepared.out, built)?;
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
    fn a_failed_package_stops_only_what_needs_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut tried = Vec::new();
        let error = carry_on(chain_of_four(dir.path()), |package| {
            tried.push(package.id.clone());
            match package.id.as_str() {
                "zlib" => Err("`zlib` failed".to_owned()),
                _ => Ok(()),
            }
        })
        .unwrap_err();
        assert_eq!(tried, ["zlib", "jq"]);
        assert_eq!(
            error,
            "1 failed, and 2 that need them weren't built:\n`zlib` failed"
        );
    }

    #[test]
    fn every_failure_is_reported_and_one_alone_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let failing = |ids: &'static [&'static str]| {
            move |package: &Package| match ids.contains(&package.id.as_str()) {
                true => Err(format!("`{}` failed", package.id)),
                false => Ok(()),
            }
        };
        let error = carry_on(chain_of_four(dir.path()), failing(&["jq", "git"])).unwrap_err();
        assert_eq!(error, "2 failed:\n`jq` failed\n`git` failed");
        let error = carry_on(chain_of_four(dir.path()), failing(&["git"])).unwrap_err();
        assert_eq!(error, "`git` failed");
        let built = carry_on(chain_of_four(dir.path()), failing(&[])).unwrap();
        assert_eq!(built.len(), 4);
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

    fn view_with(dir: &Path, relative: &str, target: &str) {
        let at = dir.join(relative);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        symlink(target, at).unwrap();
    }

    #[test]
    fn a_shebang_through_env_or_usr_points_at_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let view = dir.path().join("view");
        view_with(&view, "bin/python3", "/store/aaa/bin/python3");
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        let mut leaks = Leaks::default();
        for (before, after) in [
            (
                "#!/usr/bin/python3 -E\nprint\n",
                "#!/store/aaa/bin/python3 -E\nprint\n",
            ),
            (
                "#!/usr/bin/env python3\nprint\n",
                "#!/store/aaa/bin/python3\nprint\n",
            ),
        ] {
            let file = out.join("script");
            fs::write(&file, before).unwrap();
            assert!(fix_shebang(&file, &out, "/store/self", &view, &mut leaks).unwrap());
            assert_eq!(fs::read_to_string(&file).unwrap(), after);
        }
    }

    #[test]
    fn a_shebang_nothing_provides_is_a_leak() {
        let dir = tempfile::tempdir().unwrap();
        let view = dir.path().join("view");
        fs::create_dir_all(&view).unwrap();
        let file = dir.path().join("script");
        fs::write(&file, "#!/usr/bin/nothing\n").unwrap();
        let mut leaks = Leaks::default();

        assert!(!fix_shebang(&file, dir.path(), "/store/s", &view, &mut leaks).unwrap());
        assert_eq!(leaks.unresolved_shebangs, 1);
    }
}
