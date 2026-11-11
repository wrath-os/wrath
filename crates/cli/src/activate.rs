//! Switching the running machine to a system: /etc, accounts, tmpfiles and
//! units, as the plan between the two systems' models says. `plan` shows the same activation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use distro_eval::Unit;
use distro_model::{Activation, Model};

use crate::read_model;
use crate::state::State;

/// What a switch does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Switch now and boot into it.
    Switch,
    /// Switch now; the next boot still starts the current generation.
    Test,
    /// Boot into it next time, without switching now.
    Boot,
    /// Put it in place as the booted system's first program, then start systemd.
    Init,
    /// Install the boot loader on a machine being installed, with this as its first generation.
    Install,
}

/// Accounts are copied into /etc rather than linked, so systemd-sysusers can add the system's
/// own users to them.
const ACCOUNTS: [&str; 3] = ["passwd", "group", "shadow"];

/// Switches the machine at `root`, normally `/`, to `system`. Needs root.
pub fn activate(system: &Path, mode: Mode, root: &Path) -> Result<(), String> {
    match mode {
        Mode::Boot => boot_entries(root),
        // The entries come first, so a switch that fails partway, such as on a unit that needs the
        // new kernel, still boots into the new generation next time.
        Mode::Switch => boot_entries(root).and_then(|()| switch(root, system)),
        Mode::Test => switch(root, system),
        Mode::Init => init(system),
        Mode::Install => {
            let esp = root.join("boot");
            run(Command::new("bootctl")
                .arg(format!("--esp-path={}", esp.display()))
                .args(["--graceful", "install"]))?;
            boot_entries(root)
        }
    }
}

/// Where a machine from distro's store keeps its generations, under the root: each a link named
/// by its number to a system in `/store`, kept beside the store so a root erased at every boot
/// keeps them.
pub(crate) const GENERATIONS: &str = "store/.system/generations";

/// The link to the current generation, under the root, as `generations/N`.
pub(crate) const CURRENT: &str = "store/.system/current";

/// Writes a boot loader entry for each generation of the system at `root`, with its kernel and
/// initrd copied to the ESP at `/boot`, and makes the current generation the default. Kernels
/// and entries no generation has any more are removed. A machine without an ESP, such as a test
/// VM booted directly, has nothing to write.
fn boot_entries(root: &Path) -> Result<(), String> {
    let esp = root.join("boot");
    if !esp.join("loader").is_dir() {
        return Ok(());
    }
    let (generations, current) = generations(root)?;
    let kernels = esp.join("EFI/distro");
    let entries = esp.join("loader/entries");
    for dir in [&kernels, &entries] {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut kept = BTreeSet::new();
    let mut written = BTreeSet::new();
    for (number, link) in generations {
        let system = inside(root, &link);
        let boot = Boot::of(&system)?;
        let mut files = Vec::new();
        for path in [&boot.kernel, &boot.initrd] {
            // Named after the store path, so a kernel shared by generations is copied once.
            let name = path
                .strip_prefix("/store")
                .unwrap_or(path)
                .to_string_lossy()
                .replace('/', "-");
            let copy = kernels.join(&name);
            if !copy.exists() {
                let under_root = root.join(path.strip_prefix("/").unwrap_or(path));
                copy_replacing(&inside(root, &under_root), &copy)?;
            }
            kept.insert(name.clone());
            files.push(name);
        }
        // Secrets the initrd needs from the disk, such as its SSH host key, go in an initrd of
        // their own made here, so they never go in the store.
        let secrets = boot
            .secrets
            .iter()
            .map(|path| {
                let file = root.join(path.trim_start_matches('/'));
                fs::read(&file)
                    .map(|bytes| (path.as_str(), bytes))
                    .map_err(|e| format!("{}: {e}", file.display()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !secrets.is_empty() {
            let name = format!("secrets-{number}.cpio");
            let archive = kernels.join(&name);
            let temporary = archive.with_extension("distro-new");
            fs::write(&temporary, cpio(&secrets))
                .and_then(|()| fs::rename(&temporary, &archive))
                .map_err(|e| format!("{}: {e}", archive.display()))?;
            kept.insert(name.clone());
            files.push(name);
        }
        let mut params = boot.params.join(" ");
        // A system from distro's store can't name itself in its own files, so the entry does.
        if !params.split(' ').any(|param| param.starts_with("init=")) {
            let named = fs::read_link(&link).map_err(|e| format!("{}: {e}", link.display()))?;
            params.push_str(&format!(" init={}/init", named.display()));
        }
        // An entry the boot loader is counting tries for keeps its name, which holds the count.
        // A new one gets its tries; a boot that comes up marks it good, and one that runs out of
        // tries is booted only when nothing else is left.
        let entry = counted_entry(&entries, number)?
            .unwrap_or_else(|| format!("distro-{number}+{BOOT_TRIES}.conf"));
        let initrds: String = files[1..]
            .iter()
            .map(|file| format!("initrd /EFI/distro/{file}\n"))
            .collect();
        // The current generation sorts first, so the boot loader starts it unless it ran out of
        // tries, and then the newest of the others.
        let sort_key = if number == current {
            "distro-current"
        } else {
            "distro-other"
        };
        let text = format!(
            "title distro\nversion generation {number}\nsort-key {sort_key}\n\
             linux /EFI/distro/{}\n{initrds}options {}\n",
            files[0], params
        );
        write_replacing(&entries.join(&entry), &text, 0o644)?;
        written.insert(entry);
    }
    write_replacing(
        &esp.join("loader/loader.conf"),
        "default distro-*\ntimeout 3\neditor no\n",
        0o644,
    )?;
    remove_others(&entries, |name| {
        name.starts_with("distro-") && !written.contains(name)
    })?;
    remove_others(&kernels, |name| !kept.contains(name))?;
    sync_tree(&kernels)?;
    sync_tree(&esp.join("loader"))
}

/// How many times the boot loader tries a new generation before it falls back to another.
const BOOT_TRIES: u32 = 3;

/// The file in `entries` that holds generation `number`'s entry, with or without the boot
/// loader's count of tries in its name: `distro-4.conf`, `distro-4+3.conf`, `distro-4+1-2.conf`.
fn counted_entry(entries: &Path, number: u32) -> Result<Option<String>, String> {
    let error = |e: io::Error| format!("{}: {e}", entries.display());
    let prefix = format!("distro-{number}");
    for entry in fs::read_dir(entries).map_err(error)? {
        let name = entry
            .map_err(error)?
            .file_name()
            .to_string_lossy()
            .into_owned();
        let Some(rest) = name
            .strip_prefix(&prefix)
            .and_then(|r| r.strip_suffix(".conf"))
        else {
            continue;
        };
        let counted = rest.strip_prefix('+').is_some_and(|count| {
            let (left, done) = count.split_once('-').unwrap_or((count, "0"));
            [left, done]
                .iter()
                .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        });
        if rest.is_empty() || counted {
            return Ok(Some(name));
        }
    }
    Ok(None)
}

/// What boots a generation, from its `kernel` and `initrd` links, its `kernel-params` and its
/// `initrd-secrets`.
#[derive(Debug, PartialEq)]
struct Boot {
    kernel: PathBuf,
    initrd: PathBuf,
    params: Vec<String>,
    /// Files on the machine that go in an initrd of their own.
    secrets: Vec<String>,
}

impl Boot {
    fn of(system: &Path) -> Result<Self, String> {
        let at = |name: &str| system.join(name);
        let error = |name: &str| {
            let path = at(name);
            move |e: io::Error| format!("{}: {e}", path.display())
        };
        let params = fs::read_to_string(at("kernel-params")).map_err(error("kernel-params"))?;
        let secrets = fs::read_to_string(at("initrd-secrets")).unwrap_or_default();
        Ok(Self {
            kernel: fs::read_link(at("kernel")).map_err(error("kernel"))?,
            initrd: fs::read_link(at("initrd")).map_err(error("initrd"))?,
            params: params.split_whitespace().map(str::to_owned).collect(),
            secrets: secrets
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
        })
    }
}

/// Writes everything under `dir` to the disk itself. The ESP's filesystem leaves that to whoever
/// wrote the files, and an entry that was only in the disk's cache is gone after a power cut.
fn sync_tree(dir: &Path) -> Result<(), String> {
    let sync = |path: &Path| {
        fs::File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.is_dir() {
            sync_tree(&path)?;
        } else {
            sync(&path)?;
        }
    }
    sync(dir)
}

/// An uncompressed cpio archive, in the `newc` format the kernel unpacks an initrd from, holding
/// `files` by absolute path, readable only by root, with the directories they're in.
fn cpio(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    fn entry(out: &mut Vec<u8>, name: &str, mode: u32, data: &[u8]) {
        let (size, name_size) = (data.len() as u32, name.len() as u32 + 1);
        let fields = [0, mode, 0, 0, 1, 0, size, 0, 0, 0, 0, name_size, 0];
        out.extend_from_slice(b"070701");
        for field in fields {
            out.extend_from_slice(format!("{field:08x}").as_bytes());
        }
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.resize(out.len().next_multiple_of(4), 0);
        out.extend_from_slice(data);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    let mut out = Vec::new();
    let mut directories = BTreeSet::new();
    for (path, data) in files {
        let path = path.trim_start_matches('/');
        let parents: Vec<&Path> = Path::new(path)
            .ancestors()
            .skip(1)
            .filter(|dir| !dir.as_os_str().is_empty())
            .collect();
        for dir in parents.into_iter().rev() {
            if directories.insert(dir) {
                entry(&mut out, &dir.to_string_lossy(), 0o040_755, &[]);
            }
        }
        entry(&mut out, path, 0o100_600, data);
    }
    entry(&mut out, "TRAILER!!!", 0, &[]);
    out
}

/// The generations of the machine at `root`, by number with the link to each, oldest first, and
/// the current one's number.
pub(crate) fn generations(root: &Path) -> Result<(Vec<(u32, PathBuf)>, u32), String> {
    let (dir, current) = (root.join(GENERATIONS), root.join(CURRENT));
    let number_of = |name: &str| -> Option<u32> {
        name.strip_prefix("generations/")
            .unwrap_or(name)
            .parse()
            .ok()
    };
    let mut generations: Vec<(u32, PathBuf)> = fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            Some((
                number_of(&entry.file_name().to_string_lossy())?,
                entry.path(),
            ))
        })
        .collect();
    generations.sort();
    let link = fs::read_link(&current).map_err(|e| format!("{}: {e}", current.display()))?;
    let current = number_of(&link.to_string_lossy())
        .ok_or_else(|| format!("{} names no generation", current.display()))?;
    Ok((generations, current))
}

/// Where `link` points, within `root`: absolute targets are under it.
fn inside(root: &Path, link: &Path) -> PathBuf {
    let mut path = link.to_path_buf();
    while let Ok(target) = fs::read_link(&path) {
        path = match target.strip_prefix("/") {
            Ok(relative) => root.join(relative),
            Err(_) => path.parent().unwrap_or(Path::new("/")).join(target),
        };
    }
    path
}

fn copy_replacing(from: &Path, to: &Path) -> Result<(), String> {
    let temporary = to.with_extension("distro-new");
    fs::copy(from, &temporary).map_err(|e| format!("{}: {e}", from.display()))?;
    fs::rename(&temporary, to).map_err(|e| format!("{}: {e}", to.display()))
}

/// Removes the files in `dir` whose names `unwanted` says.
fn remove_others(dir: &Path, unwanted: impl Fn(&str) -> bool) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        if unwanted(&entry.file_name().to_string_lossy()) {
            fs::remove_file(entry.path())
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Runs as the booted system's init: puts `system` in place and hands over to systemd, which
/// starts even when part of that failed, so the machine still comes up to be fixed.
fn init(system: &Path) -> Result<(), String> {
    if let Err(message) = put_in_place(Path::new("/"), system) {
        eprintln!("distro: {message}; starting systemd anyway");
    }
    let systemd = system.join("systemd/lib/systemd/systemd");
    let error = Command::new(&systemd).exec();
    Err(format!("cannot start {}: {error}", systemd.display()))
}

/// Puts `system` in place at boot. The initrd's /dev, /proc, /sys and /run came along when it
/// switched to this root.
fn put_in_place(root: &Path, system: &Path) -> Result<(), String> {
    for (dir, mode) in [
        ("etc", 0o755),
        ("var", 0o755),
        ("tmp", 0o1777),
        ("root", 0o700),
        ("home", 0o755),
        ("bin", 0o755),
        ("usr/bin", 0o755),
        ("run/wrappers", 0o755),
    ] {
        let dir = root.join(dir);
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(mode))
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    point_at(root, system, &[RUNNING, BOOTED])?;
    let model = model_of(system)?;
    let state = State::of(&model);
    if let Some(state) = &state {
        state.restore_accounts(root);
        state.keep_system(root, system, None);
    }
    // A root on disk still has /etc from the system it last ran, whose files may have gone since.
    let old = fs::read_link(root.join(ETC_LINK))
        .ok()
        .and_then(|etc| etc.parent().map(Path::to_path_buf));
    switch_etc(root, old.as_deref(), system)?;
    let programs = system.join("bin");
    replace_link(&programs.join("bash"), &root.join("bin/sh"))?;
    replace_link(&programs.join("env"), &root.join("usr/bin/env"))?;
    link_kernel_modules(root, system)?;
    link_foreign(root, system)?;
    // /run doesn't allow setuid programs, so the wrappers get a filesystem of their own.
    run(Command::new(programs.join("mount")).args([
        "-t",
        "tmpfs",
        "-o",
        "mode=755,nodev",
        "wrappers",
        "/run/wrappers",
    ]))?;
    run(&mut Command::new(programs.join("systemd-sysusers")))?;
    if let Some(state) = &state {
        state.save_accounts(root);
    }
    warn(make_wrappers(root, system, &model)?);
    crate::on_machine::apply(system)
}

/// The running system, under the root.
pub(crate) const RUNNING: &str = "run/system";

/// The system that booted, which differs from the running one after a live switch.
pub(crate) const BOOTED: &str = "run/system-booted";

/// The link every file in /etc goes through to the running system's, under the root.
const ETC_LINK: &str = "etc/.system";

/// Points each of `links`, under `root`, at `system`.
fn point_at(root: &Path, system: &Path, links: &[&str]) -> Result<(), String> {
    for link in links {
        replace_link(system, &root.join(link))?;
    }
    Ok(())
}

/// Links /lib/modules to the modules of the kernel that booted, where kmod looks for them. A
/// system whose kernel modules aren't in it leaves /lib/modules alone.
fn link_kernel_modules(root: &Path, system: &Path) -> Result<(), String> {
    if !system.join("kernel-modules").exists() {
        return Ok(());
    }
    let lib = root.join("lib");
    fs::create_dir_all(&lib).map_err(|e| format!("{}: {e}", lib.display()))?;
    replace_link(
        &Path::new("/")
            .join(BOOTED)
            .join("kernel-modules/lib/modules"),
        &lib.join("modules"),
    )
}

/// Links /lib64 and /run/ld.so.cache to the running generation's loader and library cache for
/// programs built elsewhere (decision 0015), or removes the links when it has none.
fn link_foreign(root: &Path, system: &Path) -> Result<(), String> {
    let links = [
        ("lib64", "/run/system/lib64"),
        ("run/ld.so.cache", "/run/system/foreign/ld.so.cache"),
    ];
    let has_foreign = system.join("lib64").exists();
    for (link, target) in links {
        let link = root.join(link);
        if has_foreign {
            replace_link(Path::new(target), &link)?;
        } else if fs::symlink_metadata(&link).is_ok_and(|m| m.file_type().is_symlink()) {
            fs::remove_file(&link).map_err(|e| format!("{}: {e}", link.display()))?;
        }
    }
    Ok(())
}

fn switch(root: &Path, system: &Path) -> Result<(), String> {
    let current = root.join(RUNNING);
    let old = fs::read_link(&current)
        .or_else(|_| fs::read_link(root.join("run/current-system")))
        .ok();
    let before = match &old {
        Some(old) => model_of(old)?,
        None => Model::default(),
    };
    let after = model_of(system)?;
    let activation = distro_model::plan(&before, &after).activation;

    systemctl("stop", &activation.stop)?;
    let state = State::of(&after);
    if let Some(state) = &state {
        state.keep_system(root, system, State::of(&before).as_ref());
    }
    switch_etc(root, old.as_deref(), system)?;
    run(&mut Command::new("systemd-sysusers"))?;
    if let Some(state) = &state {
        state.save_accounts(root);
    }
    warn(make_wrappers(root, system, &after)?);
    crate::on_machine::apply(system)?;
    run(Command::new("systemd-tmpfiles").arg("--create"))?;
    run(Command::new("systemctl").arg("daemon-reload"))?;
    systemctl("try-restart", &activation.restart)?;
    systemctl("try-reload-or-restart", &activation.reload)?;
    systemctl("start", &installed(&activation, &after))?;
    point_at(root, system, &[RUNNING])?;
    switch_user_managers(root, &before, &after);
    link_foreign(root, system)
}

/// The user units enabled for every user that `after` adds and those it drops, against `before`.
fn user_unit_changes(before: &Model, after: &Model) -> (Vec<String>, Vec<String>) {
    let added = after
        .enabled_user
        .keys()
        .filter(|unit| !before.enabled_user.contains_key(*unit))
        .cloned()
        .collect();
    let dropped = before
        .enabled_user
        .keys()
        .filter(|unit| !after.enabled_user.contains_key(*unit))
        .cloned()
        .collect();
    (added, dropped)
}

/// The names of the users whose service managers `listing` shows running, as
/// `systemctl list-units --plain 'user@*.service'` prints them, by their uid in `passwd`.
fn running_users(listing: &str, passwd: &str) -> Vec<String> {
    let names: BTreeMap<&str, &str> = passwd
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            Some((fields.nth(1)?, name))
        })
        .collect();
    listing
        .lines()
        .filter_map(|line| line.split_whitespace().next()?.strip_prefix("user@"))
        .filter_map(|unit| unit.strip_suffix(".service"))
        .filter_map(|uid| names.get(uid).map(|name| (*name).to_owned()))
        .collect()
}

/// Brings the service managers of users logged in to the new generation: each reloads, starts the
/// user units newly enabled for a target it has reached, such as a shell for the graphical session,
/// and stops those no longer enabled. A user's session going wrong doesn't fail the switch, so
/// failures are warnings.
fn switch_user_managers(root: &Path, before: &Model, after: &Model) {
    let (added, dropped) = user_unit_changes(before, after);
    let listing = Command::new("systemctl")
        .args([
            "list-units",
            "--type=service",
            "--state=active",
            "--no-legend",
            "--plain",
            "user@*.service",
        ])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    let passwd = fs::read_to_string(root.join("etc/passwd")).unwrap_or_default();
    for user in running_users(&listing, &passwd) {
        let manager = format!("{user}@");
        let user_systemctl = |args: &[&str]| {
            Command::new("systemctl")
                .args(["--user", "--machine", &manager])
                .args(args)
                .output()
        };
        let warn = |what: &str, args: &[&str]| match user_systemctl(args) {
            Ok(out) if out.status.success() => {}
            _ => eprintln!("distro: {user}'s session: couldn't {what}"),
        };
        warn("reload its units", &["daemon-reload"]);
        for unit in &dropped {
            warn(&format!("stop {unit}"), &["stop", unit]);
        }
        for unit in &added {
            let wanted_by = user_systemctl(&["show", "--property=WantedBy", "--value", unit])
                .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
                .unwrap_or_default();
            let reached = wanted_by.split_whitespace().any(|target| {
                user_systemctl(&["is-active", "--quiet", target]).is_ok_and(|o| o.status.success())
            });
            if reached {
                warn(&format!("start {unit}"), &["start", "--no-block", unit]);
            }
        }
    }
}

/// Says what activation left out, and carries on.
fn warn(left_out: Vec<String>) {
    for message in left_out {
        eprintln!("distro: {message}");
    }
}

pub(crate) fn model_of(system: &Path) -> Result<Model, String> {
    read_model(&system.join("etc/distro/model.json"))
}

/// The units the activation starts that something wants, so they'd run after a boot too. Others
/// start when something asks for them, such as through a socket or a timer.
fn installed(activation: &Activation, after: &Model) -> BTreeSet<String> {
    activation
        .start
        .iter()
        .filter(|name| {
            after.units.get(*name).is_some_and(wanted) || after.enabled.contains_key(*name)
        })
        .cloned()
        .collect()
}

fn wanted(unit: &Unit) -> bool {
    unit.sections
        .iter()
        .filter(|section| section.name == "Install")
        .flat_map(|section| &section.entries)
        .any(|(key, _)| key == "WantedBy" || key == "RequiredBy")
}

/// Points /etc at `system`'s files: every file links through /etc/.system, which moves in one
/// step, and links to files `old` had and `system` doesn't are removed. Accounts are merged.
fn switch_etc(root: &Path, old: Option<&Path>, system: &Path) -> Result<(), String> {
    let etc = root.join("etc");
    let new_etc = system.join("etc");
    let new_files = files_in(&new_etc).map_err(|e| format!("{}: {e}", system.display()))?;
    let old_files = match old {
        Some(old) => files_in(&old.join("etc")).unwrap_or_default(),
        None => BTreeSet::new(),
    };
    let new_modes = etc_modes(system);
    let old_modes = old.map(etc_modes).unwrap_or_default();
    replace_link(&new_etc, &root.join(ETC_LINK))?;
    // A link in the system's /etc is made as it is: through /etc/.system it would be two steps,
    // and systemd takes a unit reached in two steps for a unit of its own, so `default.target`
    // would stop being `multi-user.target`. Files are linked through /etc/.system.
    let target_in = |system_etc: &Path, path: &Path| {
        fs::read_link(system_etc.join(path))
            .unwrap_or_else(|_| Path::new("/").join(ETC_LINK).join(path))
    };
    for path in &new_files {
        if ACCOUNTS.iter().any(|name| path == Path::new(name)) {
            continue;
        }
        let target = target_in(&new_etc, path);
        let link = etc.join(path);
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        // A file with its own mode is a copy, since a link into the store can't have one.
        if let Some(mode) = new_modes.get(path) {
            let text =
                fs::read(new_etc.join(path)).map_err(|e| format!("{}: {e}", path.display()))?;
            write_replacing(&link, text, *mode)?;
            continue;
        }
        if fs::read_link(&link).is_ok_and(|current| current == target) {
            continue;
        }
        replace_link(&target, &link)?;
    }
    for path in old_files.difference(&new_files) {
        let link = etc.join(path);
        let old_target = old.map(|old| target_in(&old.join("etc"), path));
        if old_modes.contains_key(path)
            || fs::read_link(&link).is_ok_and(|current| Some(current) == old_target)
        {
            fs::remove_file(&link).map_err(|e| format!("{}: {e}", link.display()))?;
        }
    }
    for name in ACCOUNTS {
        let read = |dir: &Path| fs::read_to_string(dir.join(name)).unwrap_or_default();
        let merged = merge_accounts(
            &read(&etc),
            &old.map(|old| read(&old.join("etc"))).unwrap_or_default(),
            &read(&new_etc),
            name == "shadow",
        );
        let path = etc.join(name);
        let mode = if name == "shadow" { 0o600 } else { 0o644 };
        write_replacing(&path, &merged, mode)?;
    }
    Ok(())
}

/// The files in `system`'s /etc that have their own mode, by path under /etc.
fn etc_modes(system: &Path) -> BTreeMap<PathBuf, u32> {
    let text = fs::read_to_string(system.join("activation.json")).unwrap_or_default();
    let activation: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    activation["etc_modes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(path, mode)| {
            let mode = u32::from_str_radix(mode.as_str()?, 8).ok()?;
            Some((PathBuf::from(path), mode))
        })
        .collect()
}

/// Makes `/run/wrappers/bin` hold `model`'s wrappers, copied from `system`, each with its owner and
/// mode. The new set replaces the old one in one step. A wrapper that can't be made, such as one
/// for a program the system lacks, is left out, and returned with why, so the rest still work.
fn make_wrappers(root: &Path, system: &Path, model: &Model) -> Result<Vec<String>, String> {
    let wrappers = root.join("run/wrappers");
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let dir = wrappers.join(format!("set-{nanos}"));
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let users = ids(&root.join("etc/passwd"));
    let groups = ids(&root.join("etc/group"));
    let id = |ids: &BTreeMap<String, u32>, wrapper: &str, account: &str, what: &str| {
        ids.get(account).copied().ok_or(format!(
            "wrapper `{wrapper}`: there's no {what} `{account}`"
        ))
    };
    let mut left_out = Vec::new();
    for (name, wrapper) in &model.wrappers {
        // Sources name the running system, which is `system` once this switch is done.
        let source = match wrapper
            .source
            .strip_prefix("/run/system/")
            .or_else(|| wrapper.source.strip_prefix("/run/current-system/"))
        {
            Some(inside) => system.join(inside),
            None => PathBuf::from(&wrapper.source),
        };
        let path = dir.join(name);
        let made = fs::copy(&source, &path)
            .map_err(|e| format!("{}: {e}", source.display()))
            .and_then(|_| {
                std::os::unix::fs::chown(
                    &path,
                    Some(id(&users, name, &wrapper.owner, "user")?),
                    Some(id(&groups, name, &wrapper.group, "group")?),
                )
                .map_err(|e| format!("{}: {e}", path.display()))
            })
            // After the owner, which clears setuid.
            .and_then(|()| {
                fs::set_permissions(&path, fs::Permissions::from_mode(wrapper.mode))
                    .map_err(|e| format!("{}: {e}", path.display()))
            });
        if let Err(message) = made {
            let _ = fs::remove_file(&path);
            left_out.push(format!("wrapper `{name}`: {message}"));
        }
    }
    replace_link(&dir, &wrappers.join("bin"))?;
    for entry in fs::read_dir(&wrappers).map_err(|e| format!("{}: {e}", wrappers.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", wrappers.display()))?;
        let path = entry.path();
        if path != dir && entry.file_name().to_string_lossy().starts_with("set-") {
            let _ = fs::remove_dir_all(path);
        }
    }
    Ok(left_out)
}

/// Names to ids in an accounts file such as `/etc/passwd`.
pub(crate) fn ids(path: &Path) -> BTreeMap<String, u32> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let id = fields.nth(1)?.parse().ok()?;
            Some((name.to_owned(), id))
        })
        .collect()
}

/// The files under `dir`, relative to it. A link to a directory, such as systemd's units, is one
/// file.
pub(crate) fn files_in(dir: &Path) -> io::Result<BTreeSet<PathBuf>> {
    let mut files = BTreeSet::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        for entry in fs::read_dir(dir.join(&relative))? {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                pending.push(path);
            } else {
                files.insert(path);
            }
        }
    }
    Ok(files)
}

/// An accounts file, `passwd`, `group` or `shadow`: the new system's entries, and the machine's
/// own, such as users systemd-sysusers made, that neither system declares. A `shadow` entry the
/// machine already has keeps its password.
fn merge_accounts(current: &str, old: &str, new: &str, keep_passwords: bool) -> String {
    let name = |line: &str| line.split(':').next().unwrap_or_default().to_owned();
    let declared: BTreeSet<String> = old.lines().chain(new.lines()).map(name).collect();
    let mut lines: Vec<String> = new
        .lines()
        .map(|line| {
            current
                .lines()
                .find(|existing| keep_passwords && name(existing) == name(line))
                .unwrap_or(line)
                .to_owned()
        })
        .collect();
    lines.extend(
        current
            .lines()
            .filter(|line| !line.is_empty() && !declared.contains(&name(line)))
            .map(str::to_owned),
    );
    lines.iter().map(|line| format!("{line}\n")).collect()
}

/// Makes `link` point at `target`, replacing what's there in one step.
pub(crate) fn replace_link(target: &Path, link: &Path) -> Result<(), String> {
    let temporary = link.with_extension("distro-new");
    let _ = fs::remove_file(&temporary);
    symlink(target, &temporary).map_err(|e| format!("{}: {e}", temporary.display()))?;
    fs::rename(&temporary, link).map_err(|e| format!("{}: {e}", link.display()))
}

fn write_replacing(path: &Path, text: impl AsRef<[u8]>, mode: u32) -> Result<(), String> {
    let temporary = path.with_extension("distro-new");
    fs::write(&temporary, text).map_err(|e| format!("{}: {e}", temporary.display()))?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("{}: {e}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn systemctl(verb: &str, units: &BTreeSet<String>) -> Result<(), String> {
    if units.is_empty() {
        return Ok(());
    }
    run(Command::new("systemctl").arg(verb).args(units))
}

fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|e| format!("cannot run {program}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "{program} failed ({}): {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine from distro's store, with generations `numbered` by store name, each with its own
    /// kernel and the same initrd, and `current` the current one.
    fn store_machine(root: &Path, numbered: &[(u32, &str)], current: u32) {
        let store = root.join("store");
        for (name, file) in [("k1", "bzImage"), ("k2", "bzImage"), ("i1", "initrd")] {
            fs::create_dir_all(store.join(name)).unwrap();
            fs::write(store.join(name).join(file), name).unwrap();
        }
        let generations = root.join(GENERATIONS);
        fs::create_dir_all(&generations).unwrap();
        for (number, name) in numbered {
            let dir = store.join(name);
            fs::create_dir_all(&dir).unwrap();
            let kernel = if *number == 1 { "k1" } else { "k2" };
            symlink(format!("/store/{kernel}/bzImage"), dir.join("kernel")).unwrap();
            symlink("/store/i1/initrd", dir.join("initrd")).unwrap();
            fs::write(dir.join("kernel-params"), "root=LABEL=distro-root rw\n").unwrap();
            symlink(
                format!("/store/{name}"),
                generations.join(number.to_string()),
            )
            .unwrap();
        }
        symlink(format!("generations/{current}"), root.join(CURRENT)).unwrap();
    }

    #[test]
    fn a_switch_that_fails_partway_still_leaves_the_generation_bootable() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        store_machine(root, &[(1, "aaa"), (2, "bbb")], 2);
        fs::create_dir_all(root.join("boot/loader/entries")).unwrap();
        // The new system has no activation record, so switching to it fails before any unit.
        let system = root.join("store/bbb");
        assert!(activate(&system, Mode::Switch, root).is_err());
        assert!(root.join("boot/loader/entries/distro-2+3.conf").exists());
    }

    #[test]
    fn a_generations_entry_is_found_with_or_without_its_count_of_tries() {
        let work = tempfile::tempdir().unwrap();
        let entries = work.path();
        for name in [
            "distro-10.conf",
            "distro-2+0-3.conf",
            "distro-3+x.conf",
            "other-1.conf",
        ] {
            fs::write(entries.join(name), "").unwrap();
        }
        let found = |number| counted_entry(entries, number).unwrap();
        assert_eq!(found(10).as_deref(), Some("distro-10.conf"));
        assert_eq!(found(2).as_deref(), Some("distro-2+0-3.conf"));
        assert_eq!(found(1), None, "distro-10 isn't generation 1");
        assert_eq!(found(3), None, "a count that isn't numbers isn't one");
    }

    #[test]
    fn a_generation_boots_as_its_files_say() {
        let work = tempfile::tempdir().unwrap();
        let system = work.path();
        symlink("/store/old/bzImage", system.join("kernel")).unwrap();
        symlink("/store/old/initrd", system.join("initrd")).unwrap();
        fs::write(system.join("kernel-params"), "quiet rw\n").unwrap();
        let old = Boot::of(system).unwrap();
        assert_eq!(old.kernel, Path::new("/store/old/bzImage"));
        assert_eq!(old.initrd, Path::new("/store/old/initrd"));
        assert_eq!(old.params, ["quiet", "rw"]);
        assert!(old.secrets.is_empty());

        fs::write(
            system.join("initrd-secrets"),
            "/etc/ssh/initrd_ed25519_key\n\n",
        )
        .unwrap();
        assert_eq!(
            Boot::of(system).unwrap().secrets,
            ["/etc/ssh/initrd_ed25519_key"]
        );
    }

    #[test]
    fn a_store_machine_gets_an_entry_per_generation_that_starts_its_init() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        store_machine(root, &[(1, "aaa"), (2, "bbb"), (10, "ccc")], 2);
        fs::create_dir_all(root.join("boot/loader/entries")).unwrap();

        boot_entries(root).unwrap();

        let esp = root.join("boot");
        assert_eq!(
            fs::read_to_string(esp.join("loader/loader.conf")).unwrap(),
            "default distro-*\ntimeout 3\neditor no\n"
        );
        let entry = fs::read_to_string(esp.join("loader/entries/distro-10+3.conf")).unwrap();
        assert!(entry.contains("linux /EFI/distro/k2-bzImage\n"), "{entry}");
        assert!(entry.contains("initrd /EFI/distro/i1-initrd\n"), "{entry}");
        assert!(
            entry.contains("options root=LABEL=distro-root rw init=/store/ccc/init\n"),
            "{entry}"
        );
        assert_eq!(fs::read_dir(esp.join("EFI/distro")).unwrap().count(), 3);
    }

    #[test]
    fn a_store_machine_lists_its_generations_by_number_with_the_current_one() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        store_machine(root, &[(2, "bbb"), (10, "ccc"), (1, "aaa")], 10);
        let (listed, current) = generations(root).unwrap();
        let numbers: Vec<u32> = listed.iter().map(|(number, _)| *number).collect();
        assert_eq!(numbers, [1, 2, 10]);
        assert_eq!(current, 10);
        assert_eq!(listed[0].1, root.join(GENERATIONS).join("1"));
    }

    #[test]
    fn each_generation_gets_a_boot_entry_and_the_current_one_is_the_default() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path();
        let store = root.join("store");
        let system = |name: &str, kernel: &str| {
            let dir = store.join(name);
            fs::create_dir_all(&dir).unwrap();
            for (file, text) in [("bzImage", "kernel"), ("initrd", "initrd")] {
                let part = store.join(format!("{kernel}-{file}"));
                fs::create_dir_all(&part).unwrap();
                fs::write(part.join(file), text).unwrap();
            }
            symlink(
                format!("/store/{kernel}-bzImage/bzImage"),
                dir.join("kernel"),
            )
            .unwrap();
            symlink(format!("/store/{kernel}-initrd/initrd"), dir.join("initrd")).unwrap();
            fs::write(
                dir.join("kernel-params"),
                format!("init=/store/{name}/init\n"),
            )
            .unwrap();
        };
        system("aaa-distro-system", "k1");
        system("bbb-distro-system", "k1");
        system("ccc-distro-system", "k2");
        fs::write(
            store.join("ccc-distro-system/initrd-secrets"),
            "/etc/ssh/initrd_ed25519_key\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("etc/ssh")).unwrap();
        fs::write(root.join("etc/ssh/initrd_ed25519_key"), "key\n").unwrap();
        let generations = root.join(GENERATIONS);
        fs::create_dir_all(&generations).unwrap();
        for (number, name) in [(1, "aaa"), (2, "bbb"), (3, "ccc")] {
            let link = generations.join(number.to_string());
            symlink(format!("/store/{name}-distro-system"), link).unwrap();
        }
        symlink("generations/2", root.join(CURRENT)).unwrap();
        fs::create_dir_all(root.join("boot/loader/entries")).unwrap();
        fs::write(root.join("boot/loader/entries/distro-9.conf"), "old").unwrap();
        // The boot loader counted down generation 2's tries and 1 was marked good.
        fs::write(root.join("boot/loader/entries/distro-2+1-2.conf"), "old").unwrap();
        fs::write(root.join("boot/loader/entries/distro-1.conf"), "old").unwrap();

        boot_entries(root).unwrap();

        let esp = root.join("boot");
        assert_eq!(
            fs::read_to_string(esp.join("loader/loader.conf")).unwrap(),
            "default distro-*\ntimeout 3\neditor no\n"
        );
        let mut names: Vec<String> = fs::read_dir(esp.join("loader/entries"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["distro-1.conf", "distro-2+1-2.conf", "distro-3+3.conf"]
        );
        let current = fs::read_to_string(esp.join("loader/entries/distro-2+1-2.conf")).unwrap();
        assert!(current.contains("sort-key distro-current\n"), "{current}");
        let entry = fs::read_to_string(esp.join("loader/entries/distro-3+3.conf")).unwrap();
        assert!(entry.contains("sort-key distro-other\n"), "{entry}");
        assert!(
            entry.contains("linux /EFI/distro/k2-bzImage-bzImage\n"),
            "{entry}"
        );
        assert!(
            entry.contains("options init=/store/ccc-distro-system/init\n"),
            "{entry}"
        );
        assert!(!esp.join("loader/entries/distro-9.conf").exists());
        let kernels = fs::read_dir(esp.join("EFI/distro")).unwrap().count();
        assert_eq!(
            kernels, 5,
            "two kernels and two initrds, each copied once, and the secrets"
        );
        assert_eq!(
            fs::read_to_string(esp.join("EFI/distro/k1-initrd-initrd")).unwrap(),
            "initrd"
        );
    }

    #[test]
    fn the_secrets_initrd_is_a_cpio_archive_of_the_files_readable_by_root() {
        let archive = cpio(&[("/etc/ssh/initrd_ed25519_key", b"secret\n".to_vec())]);
        assert!(archive.starts_with(b"070701"));
        assert_eq!(archive.len() % 4, 0);
        let text = String::from_utf8_lossy(&archive);
        let names: Vec<&str> = [
            "etc\0",
            "etc/ssh\0",
            "etc/ssh/initrd_ed25519_key\0",
            "TRAILER!!!\0",
        ]
        .into_iter()
        .filter(|name| text.contains(name))
        .collect();
        assert_eq!(names.len(), 4, "{text:?}");
        assert!(text.contains(&format!("{:08x}", 0o100_600)));
        let work = tempfile::tempdir().unwrap();
        fs::write(work.path().join("a.cpio"), &archive).unwrap();
        if let Ok(output) = Command::new("cpio")
            .args(["-itv", "-F"])
            .arg(work.path().join("a.cpio"))
            .output()
        {
            let listing = String::from_utf8_lossy(&output.stdout);
            assert!(listing.contains("-rw-------"), "{listing}");
            assert!(listing.contains("etc/ssh/initrd_ed25519_key"), "{listing}");
        }
    }

    #[test]
    fn accounts_are_the_systems_plus_the_machines_own() {
        let current = "root:x:0:0::/root:/bin/sh\nold:x:1001:100::/home/old:/bin/sh\n\
                       systemd-network:x:152:152::/:/bin/nologin\n";
        let old = "root:x:0:0::/root:/bin/sh\nold:x:1001:100::/home/old:/bin/sh\n";
        let new = "root:x:0:0::/root:/bin/bash\njohn:x:1000:100::/home/john:/bin/bash\n";
        assert_eq!(
            merge_accounts(current, old, new, false),
            "root:x:0:0::/root:/bin/bash\njohn:x:1000:100::/home/john:/bin/bash\n\
             systemd-network:x:152:152::/:/bin/nologin\n",
            "a user the old system had and the new one doesn't goes, the machine's own stay"
        );
        assert_eq!(
            merge_accounts("john:$y$hash:1::::::\n", "", "john:!:1::::::\n", true),
            "john:$y$hash:1::::::\n"
        );
    }

    #[test]
    fn a_file_with_its_own_mode_is_a_copy_and_goes_when_the_system_drops_it() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path().join("root");
        let system = |name: &str, modes: &str, files: &[&str]| {
            let dir = work.path().join(name);
            fs::create_dir_all(dir.join("etc")).unwrap();
            for file in files {
                fs::write(dir.join("etc").join(file), *file).unwrap();
            }
            fs::write(
                dir.join("activation.json"),
                format!("{{\"etc_modes\": {{{modes}}}}}"),
            )
            .unwrap();
            dir
        };
        let old = system("old", "\"private.conf\": \"0640\"", &["private.conf"]);
        let new = system("new", "", &["plain.conf"]);
        fs::create_dir_all(root.join("etc")).unwrap();

        switch_etc(&root, None, &old).unwrap();
        let private = root.join("etc/private.conf");
        let metadata = fs::symlink_metadata(&private).unwrap();
        assert!(!metadata.is_symlink());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o640);

        switch_etc(&root, Some(&old), &new).unwrap();
        assert!(fs::symlink_metadata(&private).is_err());
    }

    #[test]
    fn wrappers_are_copies_of_the_new_systems_programs_with_their_owner_and_mode() {
        use std::os::unix::fs::MetadataExt;
        let work = tempfile::tempdir().unwrap();
        let root = work.path().join("root");
        let system = work.path().join("system");
        fs::create_dir_all(system.join("bin")).unwrap();
        fs::write(system.join("bin/tool"), "new\n").unwrap();
        fs::create_dir_all(root.join("etc")).unwrap();
        let (uid, gid) = own_ids();
        fs::write(
            root.join("etc/passwd"),
            format!("me:x:{uid}:{gid}::/:/bin/sh\n"),
        )
        .unwrap();
        fs::write(root.join("etc/group"), format!("mine:x:{gid}:\n")).unwrap();
        let mut model = Model::default();
        model.wrappers.insert(
            "tool".into(),
            distro_eval::Wrapper {
                source: "/run/system/bin/tool".into(),
                owner: "me".into(),
                group: "mine".into(),
                mode: 0o4750,
            },
        );
        assert!(make_wrappers(&root, &system, &model).unwrap().is_empty());
        assert!(make_wrappers(&root, &system, &model).unwrap().is_empty());

        let tool = root.join("run/wrappers/bin/tool");
        assert_eq!(fs::read_to_string(&tool).unwrap(), "new\n");
        let metadata = fs::metadata(&tool).unwrap();
        assert_eq!((metadata.uid(), metadata.mode() & 0o7777), (uid, 0o4750));
        let sets = fs::read_dir(root.join("run/wrappers")).unwrap().count();
        assert_eq!(
            sets, 2,
            "the link and one set of wrappers; the old set is gone"
        );
    }

    #[test]
    fn a_wrapper_for_a_program_the_system_lacks_is_left_out_and_the_rest_are_made() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path().join("root");
        let system = work.path().join("system");
        fs::create_dir_all(system.join("bin")).unwrap();
        fs::write(system.join("bin/tool"), "new\n").unwrap();
        fs::create_dir_all(root.join("etc")).unwrap();
        let (uid, gid) = own_ids();
        fs::write(
            root.join("etc/passwd"),
            format!("me:x:{uid}:{gid}::/:/bin/sh\n"),
        )
        .unwrap();
        fs::write(root.join("etc/group"), format!("mine:x:{gid}:\n")).unwrap();
        let mut model = Model::default();
        for name in ["gone", "tool"] {
            model.wrappers.insert(
                name.into(),
                distro_eval::Wrapper {
                    source: format!("/run/system/bin/{name}"),
                    owner: "me".into(),
                    group: "mine".into(),
                    mode: 0o755,
                },
            );
        }
        let left_out = make_wrappers(&root, &system, &model).unwrap();
        assert_eq!(left_out.len(), 1);
        assert!(left_out[0].starts_with("wrapper `gone`: "), "{left_out:?}");
        assert!(root.join("run/wrappers/bin/tool").exists());
        assert!(!root.join("run/wrappers/bin/gone").exists());
    }

    #[test]
    fn links_in_a_systems_etc_are_made_as_they_are_so_systemd_sees_one_step() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path().join("root");
        let system = |name: &str, links: &[(&str, &str)]| {
            let etc = work.path().join(name).join("etc");
            fs::create_dir_all(&etc).unwrap();
            fs::write(etc.join("passwd"), "root:x:0:0::/root:/bin/sh\n").unwrap();
            for (path, target) in links {
                let link = etc.join(path);
                fs::create_dir_all(link.parent().unwrap()).unwrap();
                symlink(target, link).unwrap();
            }
            work.path().join(name)
        };
        let wants = "systemd/system/multi-user.target.wants/web.service";
        let default = "systemd/system/default.target";
        let gone = "systemd/system/sockets.target.wants/old.socket";
        let old = system("old", &[(wants, "../web.service"), (gone, "../old.socket")]);
        let new = system(
            "new",
            &[
                (wants, "../web.service"),
                (
                    default,
                    "/store/systemd/lib/systemd/system/multi-user.target",
                ),
            ],
        );
        fs::create_dir_all(root.join("etc")).unwrap();
        switch_etc(&root, None, &old).unwrap();

        switch_etc(&root, Some(&old), &new).unwrap();

        let etc = root.join("etc");
        assert_eq!(
            fs::read_link(etc.join(wants)).unwrap(),
            Path::new("../web.service")
        );
        assert_eq!(
            fs::read_link(etc.join(default)).unwrap(),
            Path::new("/store/systemd/lib/systemd/system/multi-user.target")
        );
        assert!(fs::symlink_metadata(etc.join(gone)).is_err());
    }

    #[test]
    fn a_switch_starts_user_units_newly_enabled_and_stops_those_dropped() {
        let mut before = Model::default();
        let mut after = Model::default();
        before
            .enabled_user
            .insert("old.service".to_owned(), BTreeSet::new());
        before
            .enabled_user
            .insert("kept.service".to_owned(), BTreeSet::new());
        after
            .enabled_user
            .insert("kept.service".to_owned(), BTreeSet::new());
        after
            .enabled_user
            .insert("noctalia.service".to_owned(), BTreeSet::new());
        assert_eq!(
            user_unit_changes(&before, &after),
            (
                vec!["noctalia.service".to_owned()],
                vec!["old.service".to_owned()]
            )
        );
    }

    #[test]
    fn the_users_whose_service_managers_run_are_named_by_their_uid() {
        let listing = "user@1000.service loaded active running User Manager for UID 1000\n\
                       user@1001.service loaded active running User Manager for UID 1001\n";
        let passwd = "root:x:0:0::/root:/bin/sh\ndemo:x:1000:100::/home/demo:/bin/sh\n";
        // A uid with no account, as for a user gone since, is left out.
        assert_eq!(running_users(listing, passwd), ["demo"]);
    }

    #[test]
    fn lib64_and_the_foreign_cache_follow_whether_the_system_has_them() {
        let work = tempfile::tempdir().unwrap();
        let (root, system) = (work.path().join("root"), work.path().join("system"));
        fs::create_dir_all(system.join("lib64")).unwrap();
        fs::create_dir_all(root.join("run")).unwrap();
        link_foreign(&root, &system).unwrap();
        assert_eq!(
            fs::read_link(root.join("lib64")).unwrap(),
            Path::new("/run/system/lib64")
        );
        assert_eq!(
            fs::read_link(root.join("run/ld.so.cache")).unwrap(),
            Path::new("/run/system/foreign/ld.so.cache")
        );
        fs::remove_dir(system.join("lib64")).unwrap();
        link_foreign(&root, &system).unwrap();
        assert!(fs::symlink_metadata(root.join("lib64")).is_err());
        assert!(fs::symlink_metadata(root.join("run/ld.so.cache")).is_err());
        // A real /lib64 is left alone.
        fs::create_dir(root.join("lib64")).unwrap();
        link_foreign(&root, &system).unwrap();
        assert!(root.join("lib64").is_dir());
    }

    #[test]
    fn lib_modules_is_the_booted_kernels_when_the_system_has_its_modules() {
        let work = tempfile::tempdir().unwrap();
        let (root, system) = (work.path().join("root"), work.path().join("system"));
        fs::create_dir_all(&system).unwrap();
        link_kernel_modules(&root, &system).unwrap();
        assert!(fs::symlink_metadata(root.join("lib/modules")).is_err());
        let linux = work.path().join("linux");
        fs::create_dir_all(&linux).unwrap();
        symlink(&linux, system.join("kernel-modules")).unwrap();
        link_kernel_modules(&root, &system).unwrap();
        assert_eq!(
            fs::read_link(root.join("lib/modules")).unwrap(),
            Path::new("/run/system-booted/kernel-modules/lib/modules")
        );
    }

    /// The user and group this test runs as, which it can give its own files.
    fn own_ids() -> (u32, u32) {
        use std::os::unix::fs::MetadataExt;
        let file = tempfile::NamedTempFile::new().unwrap();
        let metadata = fs::metadata(file.path()).unwrap();
        (metadata.uid(), metadata.gid())
    }

    #[test]
    fn etc_links_through_one_link_and_drops_what_the_old_system_had() {
        let work = tempfile::tempdir().unwrap();
        let root = work.path().join("root");
        let system = |name: &str, files: &[&str]| {
            let dir = work.path().join(name);
            for file in files {
                let path = dir.join("etc").join(file);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, *file).unwrap();
            }
            fs::write(dir.join("etc/passwd"), "root:x:0:0::/root:/bin/sh\n").unwrap();
            dir
        };
        let old = system("old", &["hostname", "ssh/sshd_config", "gone.conf"]);
        let new = system("new", &["hostname", "ssh/sshd_config", "nftables.conf"]);
        fs::create_dir_all(root.join("etc")).unwrap();
        switch_etc(&root, None, &old).unwrap();
        fs::write(root.join("etc/machine-id"), "local\n").unwrap();

        switch_etc(&root, Some(&old), &new).unwrap();

        let etc = root.join("etc");
        assert_eq!(fs::read_link(etc.join(".system")).unwrap(), new.join("etc"));
        assert_eq!(
            fs::read_link(etc.join("ssh/sshd_config")).unwrap(),
            Path::new("/etc/.system/ssh/sshd_config")
        );
        assert!(fs::symlink_metadata(etc.join("nftables.conf")).is_ok());
        assert!(fs::symlink_metadata(etc.join("gone.conf")).is_err());
        assert_eq!(
            fs::read_to_string(etc.join("machine-id")).unwrap(),
            "local\n"
        );
        let passwd = etc.join("passwd");
        assert!(!fs::symlink_metadata(&passwd).unwrap().is_symlink());
        assert_eq!(
            fs::read_to_string(passwd).unwrap(),
            "root:x:0:0::/root:/bin/sh\n"
        );
    }
}
