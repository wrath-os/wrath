//! Step 3: building packages on the final toolchain, and finding out whether they work with no
//! `/usr` and what they still say about it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use memchr::memmem;
use serde::{Deserialize, Serialize};

use crate::chain::{self, Arm, Built, Package, Prepared, Sources};
use crate::runpath;
use crate::sandbox::{self, Limits, Mount, Status, Step, View};
use crate::seed;
use crate::store::{Lab, StorePath, with_path};

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Leaks {
    usr_in_elf: usize,
    usr_in_pkgconfig: usize,
    usr_in_cmake: usize,
    usr_in_config_script: usize,
    usr_in_script: usize,
    usr_in_docs: usize,
    usr_in_other: usize,
    sentinel: usize,
    sentinel_directories: usize,
    unresolved_shebangs: usize,
    undeclared_references: Vec<String>,
    mentions_seed: bool,
    samples: Vec<String>,
}

impl Leaks {
    fn sample(&mut self, text: String) {
        if self.samples.len() < 20 {
            self.samples.push(text);
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Check {
    what: String,
    status: Status,
    log: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    package: String,
    arm: String,
    /// `built`, `failed` or `blocked`.
    build: String,
    detail: String,
    smoke: Vec<Check>,
    probes: Vec<Check>,
    check: Option<Check>,
    leaks: Leaks,
    fixups: Fixups,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Fixups {
    la_removed: usize,
    shebangs: usize,
    pc_and_config_rewritten: usize,
    sentinel_rewritten: usize,
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

/// The owner of the longest leading part of `relative` that has one. A path can go on past what's
/// in the view, into what a dependency holds.
fn owner_of_prefix(view: &Path, relative: &str) -> Option<String> {
    let mut prefix = relative;
    loop {
        if let Some(real) = owner_of(view, prefix) {
            return Some(real);
        }
        prefix = prefix.rsplit_once('/')?.0;
    }
}

/// Rewrites paths through the sentinel to where each file really is, in place.
fn rewrite_sentinel(
    out: &Path,
    sentinel: &StorePath,
    view: &Path,
    fixups: &mut Fixups,
    leaks: &mut Leaks,
) -> Result<(), String> {
    let needle = sentinel.inside();
    let finder = memmem::Finder::new(needle.as_bytes());
    for file in files_under(out)? {
        let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
        if meta.is_symlink() {
            let link = fs::read_link(&file).map_err(with_path(&file))?;
            let text = link.to_string_lossy().into_owned();
            if let Some(rest) = text.strip_prefix(&format!("{needle}/"))
                && let Some(real) = owner_of(view, rest)
            {
                fs::remove_file(&file).map_err(with_path(&file))?;
                symlink(real, &file).map_err(with_path(&file))?;
                fixups.sentinel_rewritten += 1;
            }
            continue;
        }
        let mut bytes = fs::read(&file).map_err(with_path(&file))?;
        let positions: Vec<usize> = finder.find_iter(&bytes).collect();
        if positions.is_empty() {
            continue;
        }
        for at in positions {
            let start = at + needle.len();
            let end = bytes[start..]
                .iter()
                .position(|byte| !path_byte(*byte))
                .map_or(bytes.len(), |count| start + count);
            let relative = String::from_utf8_lossy(&bytes[start..end])
                .trim_start_matches('/')
                .to_owned();
            match owner_of_prefix(view, &relative) {
                Some(real) => {
                    let hash = real.as_bytes();
                    bytes[at..at + needle.len()].copy_from_slice(&hash[..needle.len()]);
                    fixups.sentinel_rewritten += 1;
                }
                None => {
                    leaks.sentinel_directories += 1;
                    leaks.sample(format!("view dir: {}", file.display()));
                }
            }
        }
        replace_contents(&file, &bytes)?;
    }
    Ok(())
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
    usr_like: &[String],
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
    let Some(prefix) = usr_like
        .iter()
        .find(|prefix| first.starts_with(prefix.as_str()))
    else {
        return Ok(false);
    };
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

/// Replaces `/usr` as a whole path component with `out`, for arm A's text files.
fn rewrite_usr(text: &str, out: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("/usr") {
        result.push_str(&rest[..at]);
        let after = rest.as_bytes().get(at + 4).copied();
        let before = result.as_bytes().last().copied();
        let whole = !after.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !before.is_some_and(path_byte);
        result.push_str(if whole { out } else { "/usr" });
        rest = &rest[at + 4..];
    }
    result.push_str(rest);
    result
}

/// What every arm does to a package once it's installed, on the host.
pub(crate) fn fix_up(
    lab: &Lab,
    arm: Arm,
    prepared: &Prepared,
    fixups: &mut Fixups,
    leaks: &mut Leaks,
) -> Result<(), String> {
    let out = lab.on_host(&prepared.out);
    let inside = prepared.out.inside();
    let view = &prepared.view.dir;
    let mut usr_like = vec!["/usr".to_owned()];
    if let Mount::Sentinel(sentinel) = &prepared.view.mount {
        rewrite_sentinel(&out, sentinel, view, fixups, leaks)?;
        usr_like.push(sentinel.inside());
    }
    for file in files_under(&out)? {
        let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
        if meta.is_symlink() {
            continue;
        }
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.ends_with(".la") {
            fs::remove_file(&file).map_err(with_path(&file))?;
            fixups.la_removed += 1;
            continue;
        }
        if fix_shebang(&file, &usr_like, &out, &inside, view, leaks)? {
            fixups.shebangs += 1;
        }
        if arm == Arm::A
            && (name.ends_with(".pc") || name.ends_with("-config"))
            && let Ok(text) = fs::read_to_string(&file)
        {
            let new = rewrite_usr(&text, &inside);
            if new != text {
                replace_contents(&file, new.as_bytes())?;
                fixups.pc_and_config_rewritten += 1;
            }
        }
    }
    if arm != Arm::A {
        runpath::shrink_files(lab, &prepared.out, &files_under(&out)?, replace_contents)?;
    }
    if arm.moves_packages() {
        crate::split::strip(lab, &BTreeSet::from([prepared.out.clone()]))?;
    }
    Ok(())
}

fn scan_leaks(
    lab: &Lab,
    prepared: &Prepared,
    seed: &StorePath,
    leaks: &mut Leaks,
) -> Result<(), String> {
    let out = lab.on_host(&prepared.out);
    let finder = memmem::Finder::new(b"/usr/");
    let sentinel = match &prepared.view.mount {
        Mount::Sentinel(path) => Some(path.inside()),
        Mount::Usr => None,
    };
    for file in files_under(&out)? {
        let meta = fs::symlink_metadata(&file).map_err(with_path(&file))?;
        if meta.is_symlink() {
            continue;
        }
        let bytes = fs::read(&file).map_err(with_path(&file))?;
        if let Some(sentinel) = &sentinel {
            leaks.sentinel += memmem::find_iter(&bytes, sentinel.as_bytes()).count();
        }
        let count = finder.find_iter(&bytes).count();
        if count == 0 {
            continue;
        }
        let relative = file
            .strip_prefix(&out)
            .unwrap_or(&file)
            .display()
            .to_string();
        let slot = if relative.starts_with("share/man")
            || relative.starts_with("share/doc")
            || relative.starts_with("share/info")
            || relative.ends_with(".html")
            || relative.ends_with(".txt")
            || relative.ends_with(".pod")
            || relative.ends_with(".md")
        {
            &mut leaks.usr_in_docs
        } else if bytes.starts_with(b"\x7fELF") {
            leaks.sample(format!("elf: {relative}"));
            &mut leaks.usr_in_elf
        } else if relative.ends_with(".pc") {
            &mut leaks.usr_in_pkgconfig
        } else if relative.ends_with(".cmake") {
            &mut leaks.usr_in_cmake
        } else if relative.ends_with("-config") {
            &mut leaks.usr_in_config_script
        } else if bytes.starts_with(b"#!") {
            leaks.sample(format!("script: {relative}"));
            &mut leaks.usr_in_script
        } else {
            &mut leaks.usr_in_other
        };
        *slot += count;
    }

    for hash in chain::references(&out)?.into_keys() {
        let path = StorePath::from_hash(&hash);
        if path == *seed {
            leaks.mentions_seed = true;
        }
        if path != prepared.out && !prepared.closure.contains(&path) && lab.on_host(&path).exists()
        {
            leaks.undeclared_references.push(hash);
        }
    }
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

/// Runs the package's own tests, against the installed package with no `/usr` in arms B and C.
fn run_check(
    lab: &Lab,
    arm: Arm,
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
    let log = lab
        .work
        .join("logs")
        .join(arm.name())
        .join(format!("{}.check.log", package.id));
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
                links: prepared.links.clone(),
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
    let in_view = arm == Arm::A || package.check_in_view;
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
                links: prepared.links.clone(),
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
    let (view, links) = if in_view {
        (Some(prepared.view.clone()), prepared.links.clone())
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
        (Some(tools_only(prepared)?), Vec::new())
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
            view,
            build_dir: prepared.build_dir.clone(),
            log: log.clone(),
            env,
            limits: prepared.limits,
            links,
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
    Ok(View {
        dir,
        mount: prepared.view.mount.clone(),
    })
}

fn record_path(repository: &Path, arm: &str, package: &str) -> PathBuf {
    repository
        .join("results/raw")
        .join(arm)
        .join(format!("{package}.json"))
}

fn write_record(repository: &Path, record: &Record) -> Result<(), String> {
    let path = record_path(repository, &record.arm, &record.package);
    let dir = path.parent().expect("a record is in a directory");
    fs::create_dir_all(dir).map_err(with_path(dir))?;
    let json = serde_json::to_string_pretty(record).map_err(|e| e.to_string())?;
    fs::write(&path, json + "\n").map_err(with_path(&path))
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
    let files = crate::graph::system_recipes(repository)?;
    let names = crate::graph::names_of(&files);
    let (mut built, seed, packages) = build_some(&lab, repository, &names, Arm::F, Some(ids))?;
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
            && let Some(prepared) = chain::rebuild_one(
                &lab,
                repository,
                package,
                Arm::F,
                &seed,
                &mut sources,
                &mut built,
            )?
        {
            let tested = run_check(&lab, Arm::F, package, &prepared, &built);
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

/// Forgets that `package` was built, and builds it again.
fn rebuild(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    arm: Arm,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
) -> Result<Option<Prepared>, String> {
    let done = lab
        .work
        .join("done")
        .join(built.temporary[&package.id].hash());
    fs::remove_file(&done).map_err(with_path(&done))?;
    chain::build_one(lab, repository, package, arm, seed, sources, built)
}

/// Runs `package`'s smoke tests and links its probes, from wherever the package ended up.
/// Returns the smoke results and the probe results.
fn check_installed(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    arm: Arm,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
) -> Result<(Vec<Check>, Vec<Check>), String> {
    let out = &built.paths[&package.id];
    let mut smoke = Vec::new();
    for (index, command) in package.smoke.iter().enumerate() {
        let name = format!("{}-smoke-{index}", package.id);
        smoke.push(run_final(lab, seed, out, &name, command)?);
    }
    let mut probes = Vec::new();
    for library in &package.probes {
        let probe = package.probe(library);
        match chain::build_one(lab, repository, &probe, arm, seed, sources, built) {
            Ok(Some(done)) => {
                chain::remove(&done.build_dir)?;
                fix_up(
                    lab,
                    arm,
                    &done,
                    &mut Fixups::default(),
                    &mut Leaks::default(),
                )?;
                chain::finish(lab, &probe, arm, &done.out, built)?;
            }
            Ok(None) => {}
            Err(message) => {
                probes.push(Check {
                    what: format!("linking -l{library}: {message}"),
                    status: Status::Failed,
                    log: String::new(),
                });
                continue;
            }
        }
        let name = format!("{}-run", probe.id);
        probes.push(run_final(
            lab,
            seed,
            &built.paths[&probe.id],
            &name,
            "bin/probe",
        )?);
    }
    Ok((smoke, probes))
}

fn passed_of(checks: &[Check]) -> String {
    format!(
        "{}/{}",
        checks.iter().filter(|c| passed(c)).count(),
        checks.len()
    )
}

/// Builds and tests one package, recording what happened.
fn pilot_one(
    lab: &Lab,
    repository: &Path,
    package: &Package,
    arm: Arm,
    seed: &StorePath,
    sources: &mut Sources,
    built: &mut Built,
) -> Result<Record, String> {
    let mut record = Record {
        package: package.id.clone(),
        arm: arm.name().to_owned(),
        build: "built".to_owned(),
        detail: String::new(),
        smoke: Vec::new(),
        probes: Vec::new(),
        check: None,
        leaks: Leaks::default(),
        fixups: Fixups::default(),
    };
    let record_path = record_path(repository, arm.name(), &package.id);
    let first = chain::build_one(lab, repository, package, arm, seed, sources, built);
    let result = match first {
        // Built before, but with nothing written down: the tests have to run again.
        Ok(None) if !record_path.exists() => {
            rebuild(lab, repository, package, arm, seed, sources, built)
        }
        other => other,
    };
    let prepared = match result {
        Ok(Some(prepared)) => prepared,
        Ok(None) => {
            let text = fs::read_to_string(&record_path).map_err(with_path(&record_path))?;
            return serde_json::from_str(&text)
                .map_err(|e| format!("{}: {e}", record_path.display()));
        }
        Err(message) => {
            built.paths.remove(&package.id);
            built.closures.remove(&package.id);
            record.build = if message.contains("not built before it") {
                "blocked".to_owned()
            } else {
                "failed".to_owned()
            };
            record.detail = message;
            return Ok(record);
        }
    };
    fix_up(lab, arm, &prepared, &mut record.fixups, &mut record.leaks)?;
    scan_leaks(lab, &prepared, seed, &mut record.leaks)?;
    record.check = run_check(lab, arm, package, &prepared, built)?;
    // A failed suite's own logs are the only way to tell why.
    if record.check.as_ref().is_none_or(passed) {
        chain::remove(&prepared.build_dir)?;
    }
    chain::finish(lab, package, arm, &prepared.out, built)?;
    (record.smoke, record.probes) =
        check_installed(lab, repository, package, arm, seed, sources, built)?;
    Ok(record)
}

/// Builds the toolchain and then `recipes` for `arm`. Returns what was built, the seed and the
/// packages of `recipes`.
pub(crate) fn build_recipes(
    lab: &Lab,
    repository: &Path,
    recipes: &[&str],
    arm: Arm,
) -> Result<(Built, StorePath, Vec<Package>), String> {
    build_some(lab, repository, recipes, arm, None)
}

/// Like [`build_recipes`], building only `wanted` and what it depends on when it says.
pub(crate) fn build_some(
    lab: &Lab,
    repository: &Path,
    recipes: &[&str],
    arm: Arm,
    wanted: Option<&[&str]>,
) -> Result<(Built, StorePath, Vec<Package>), String> {
    let mut built = chain::build(lab, repository, "toolchain.toml", arm)?;
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
    let loaded = match wanted {
        Some(wanted) => chain::load_reached(lab, repository, recipes, wanted)?,
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
        build_and_finish(
            lab,
            repository,
            package,
            arm,
            &seed,
            &mut sources,
            &mut built,
        )
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
    arm: Arm,
    seed: &StorePath,
    sources: &mut chain::Sources,
    built: &mut Built,
) -> Result<(), String> {
    if let Some(prepared) = chain::build_one(lab, repository, package, arm, seed, sources, built)? {
        chain::remove(&prepared.build_dir)?;
        fix_up(
            lab,
            arm,
            &prepared,
            &mut Fixups::default(),
            &mut Leaks::default(),
        )?;
        chain::finish(lab, package, arm, &prepared.out, built)?;
    }
    Ok(())
}

/// Builds the packages in `recipes` for `arm`, and returns whether the arm passes.
pub fn run(
    lab: &Lab,
    repository: &Path,
    label: &str,
    recipes: &[&str],
    arm: Arm,
) -> Result<bool, String> {
    let mut built = chain::build(lab, repository, "toolchain.toml", arm)?;
    let seed = seed::prepare(lab)?;
    let mut sources = Sources::load(repository)?;
    let mut packages = Vec::new();
    for file in recipes {
        packages.extend(chain::load_styled(lab, repository, file)?);
    }
    let mut lines = vec![
        format!("# {label}, arm {}", arm.name()),
        String::new(),
        "| Package | Build | Runs with no /usr | Links with no /usr | Tests | /usr in binaries | \
         /usr elsewhere | Sentinel left | Bad shebangs | Undeclared |"
            .to_owned(),
        "|---|---|---|---|---|---|---|---|---|---|".to_owned(),
    ];
    let mut working = 0;
    let total = packages.len();
    for package in &packages {
        let record = pilot_one(
            lab,
            repository,
            package,
            arm,
            &seed,
            &mut sources,
            &mut built,
        )?;
        write_record(repository, &record)?;
        let tests = match &record.check {
            Some(check) if passed(check) => "pass".to_owned(),
            Some(check) => format!("{:?}", check.status).to_lowercase(),
            None => "-".to_owned(),
        };
        let leaks = &record.leaks;
        let elsewhere = leaks.usr_in_pkgconfig
            + leaks.usr_in_cmake
            + leaks.usr_in_config_script
            + leaks.usr_in_script
            + leaks.usr_in_other;
        let row = format!(
            "| {} | {} | {} | {} | {tests} | {} | {elsewhere} | {} | {} | {} |",
            record.package,
            record.build,
            passed_of(&record.smoke),
            passed_of(&record.probes),
            leaks.usr_in_elf,
            leaks.sentinel + leaks.sentinel_directories,
            leaks.unresolved_shebangs,
            leaks.undeclared_references.len(),
        );
        let works = record.build == "built"
            && record.smoke.iter().all(passed)
            && record.probes.iter().all(passed)
            && !(record.smoke.is_empty() && record.probes.is_empty());
        if works {
            working += 1;
        }
        println!("{row}");
        lines.push(row);
    }
    lines.push(String::new());
    lines.push(format!(
        "{working} of {total} run from their final location."
    ));
    let path = repository.join(format!("results/{label}-{}.md", arm.name()));
    fs::write(&path, lines.join("\n") + "\n").map_err(with_path(&path))?;
    println!(
        "arm {}: {working} of {total} run from their final location",
        arm.name()
    );
    Ok(working >= 10)
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

    #[test]
    fn only_a_whole_usr_is_rewritten() {
        let text = "prefix=/usr\nlib=/usr/lib\nother=/usr2 /a/usr/b fusr/x\n";
        assert_eq!(
            rewrite_usr(text, "/store/x"),
            "prefix=/store/x\nlib=/store/x/lib\nother=/usr2 /a/usr/b fusr/x\n"
        );
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
        let usr_like = vec!["/usr".to_owned()];
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
            assert!(fix_shebang(&file, &usr_like, &out, "/store/self", &view, &mut leaks).unwrap());
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
        let usr_like = vec!["/usr".to_owned()];
        let mut leaks = Leaks::default();

        assert!(!fix_shebang(&file, &usr_like, dir.path(), "/store/s", &view, &mut leaks).unwrap());
        assert_eq!(leaks.unresolved_shebangs, 1);
    }

    #[test]
    fn sentinel_paths_become_each_files_owner_at_the_same_length() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = StorePath::derive("sentinel", &[b"s"]);
        let owner = StorePath::derive("dep", &[b"d"]);
        let view = dir.path().join("view");
        view_with(
            &view,
            "lib/libz.so",
            &format!("{}/lib/libz.so", owner.inside()),
        );
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        let text = format!("x{}/lib/libz.so\0y", sentinel.inside());
        fs::write(out.join("binary"), &text).unwrap();
        let (mut fixups, mut leaks) = (Fixups::default(), Leaks::default());

        rewrite_sentinel(&out, &sentinel, &view, &mut fixups, &mut leaks).unwrap();

        let result = fs::read(out.join("binary")).unwrap();
        assert_eq!(
            result,
            format!("x{}/lib/libz.so\0y", owner.inside()).into_bytes()
        );
        assert_eq!(fixups.sentinel_rewritten, 1);
    }

    #[test]
    fn a_path_going_past_the_view_keeps_what_comes_after() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = StorePath::derive("sentinel", &[b"s"]);
        let owner = StorePath::derive("dep", &[b"d"]);
        let view = dir.path().join("view");
        view_with(
            &view,
            "lib/python",
            &format!("{}/lib/python", owner.inside()),
        );
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        fs::write(
            out.join("f"),
            format!("{}/lib/python/site/x.py", sentinel.inside()),
        )
        .unwrap();
        let (mut fixups, mut leaks) = (Fixups::default(), Leaks::default());

        rewrite_sentinel(&out, &sentinel, &view, &mut fixups, &mut leaks).unwrap();

        assert_eq!(
            fs::read_to_string(out.join("f")).unwrap(),
            format!("{}/lib/python/site/x.py", owner.inside())
        );
    }

    #[test]
    fn a_view_directory_with_no_single_owner_is_a_leak() {
        let dir = tempfile::tempdir().unwrap();
        let sentinel = StorePath::derive("sentinel", &[b"s"]);
        let view = dir.path().join("view");
        fs::create_dir_all(view.join("lib")).unwrap();
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("f"), format!("-L{}/lib\n", sentinel.inside())).unwrap();
        let (mut fixups, mut leaks) = (Fixups::default(), Leaks::default());

        rewrite_sentinel(&out, &sentinel, &view, &mut fixups, &mut leaks).unwrap();

        assert_eq!(leaks.sentinel_directories, 1);
    }
}
