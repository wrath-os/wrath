//! Switching the running machine to a system built without NixOS: /etc, accounts, tmpfiles and
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

/// What a switch does, as NixOS's `switch-to-configuration` takes it.
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
}

/// Accounts are copied into /etc rather than linked, so systemd-sysusers can add the system's
/// own users to them.
const ACCOUNTS: [&str; 3] = ["passwd", "group", "shadow"];

/// Switches the machine to `system`. Needs root.
pub fn activate(system: &Path, mode: Mode) -> Result<(), String> {
    match mode {
        // There's no boot loader for these systems yet; the harness boots them directly.
        Mode::Boot => Ok(()),
        Mode::Switch | Mode::Test => switch(Path::new("/"), system),
        Mode::Init => init(system),
    }
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
    replace_link(system, &root.join("run/current-system"))?;
    replace_link(system, &root.join("run/booted-system"))?;
    switch_etc(root, None, system)?;
    replace_link(&system.join("sw/bin/bash"), &root.join("bin/sh"))?;
    replace_link(&system.join("sw/bin/env"), &root.join("usr/bin/env"))?;
    // /run doesn't allow setuid programs, so the wrappers get a filesystem of their own.
    run(Command::new(system.join("sw/bin/mount")).args([
        "-t",
        "tmpfs",
        "-o",
        "mode=755,nodev",
        "wrappers",
        "/run/wrappers",
    ]))?;
    make_wrappers(root, system, &model_of(system)?)
}

fn switch(root: &Path, system: &Path) -> Result<(), String> {
    let current = root.join("run/current-system");
    let old = fs::read_link(&current).ok();
    let before = match &old {
        Some(old) => model_of(old)?,
        None => Model::default(),
    };
    let after = model_of(system)?;
    let activation = distro_model::plan(&before, &after).activation;

    systemctl("stop", &activation.stop)?;
    switch_etc(root, old.as_deref(), system)?;
    run(&mut Command::new("systemd-sysusers"))?;
    make_wrappers(root, system, &after)?;
    run(Command::new("systemd-tmpfiles").arg("--create"))?;
    run(Command::new("systemctl").arg("daemon-reload"))?;
    systemctl("try-restart", &activation.restart)?;
    systemctl("try-reload-or-restart", &activation.reload)?;
    systemctl("start", &installed(&activation, &after))?;
    replace_link(system, &current)
}

fn model_of(system: &Path) -> Result<Model, String> {
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

/// Points /etc at `system`'s files: every file links through /etc/static, which moves in one
/// step, and links to files `old` had and `system` doesn't are removed. Accounts are merged.
fn switch_etc(root: &Path, old: Option<&Path>, system: &Path) -> Result<(), String> {
    let etc = root.join("etc");
    let new_etc = system.join("etc");
    let new_files = files_in(&new_etc).map_err(|e| format!("{}: {e}", system.display()))?;
    let old_files = match old {
        Some(old) => files_in(&old.join("etc")).unwrap_or_default(),
        None => BTreeSet::new(),
    };
    replace_link(&new_etc, &etc.join("static"))?;
    let in_static = |path: &Path| Path::new("/etc/static").join(path);
    for path in &new_files {
        if ACCOUNTS.iter().any(|name| path == Path::new(name)) {
            continue;
        }
        let target = in_static(path);
        let link = etc.join(path);
        if fs::read_link(&link).is_ok_and(|current| current == target) {
            continue;
        }
        if let Some(parent) = link.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        replace_link(&target, &link)?;
    }
    for path in old_files.difference(&new_files) {
        let link = etc.join(path);
        if fs::read_link(&link).is_ok_and(|current| current == in_static(path)) {
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

/// Makes `/run/wrappers/bin` hold `model`'s wrappers, copied from `system`, each with its owner and
/// mode. The new set replaces the old one in one step.
fn make_wrappers(root: &Path, system: &Path, model: &Model) -> Result<(), String> {
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
    for (name, wrapper) in &model.wrappers {
        // Sources name the running system, which is `system` once this switch is done.
        let source = match wrapper.source.strip_prefix("/run/current-system/") {
            Some(inside) => system.join(inside),
            None => PathBuf::from(&wrapper.source),
        };
        let path = dir.join(name);
        fs::copy(&source, &path).map_err(|e| format!("{}: {e}", source.display()))?;
        std::os::unix::fs::chown(
            &path,
            Some(id(&users, name, &wrapper.owner, "user")?),
            Some(id(&groups, name, &wrapper.group, "group")?),
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        // After the owner, which clears setuid.
        fs::set_permissions(&path, fs::Permissions::from_mode(wrapper.mode))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    replace_link(&dir, &wrappers.join("bin"))?;
    for entry in fs::read_dir(&wrappers).map_err(|e| format!("{}: {e}", wrappers.display()))? {
        let entry = entry.map_err(|e| format!("{}: {e}", wrappers.display()))?;
        let path = entry.path();
        if path != dir && entry.file_name().to_string_lossy().starts_with("set-") {
            let _ = fs::remove_dir_all(path);
        }
    }
    Ok(())
}

/// Names to ids in an accounts file such as `/etc/passwd`.
fn ids(path: &Path) -> BTreeMap<String, u32> {
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
fn files_in(dir: &Path) -> io::Result<BTreeSet<PathBuf>> {
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
fn replace_link(target: &Path, link: &Path) -> Result<(), String> {
    let temporary = link.with_extension("distro-new");
    let _ = fs::remove_file(&temporary);
    symlink(target, &temporary).map_err(|e| format!("{}: {e}", temporary.display()))?;
    fs::rename(&temporary, link).map_err(|e| format!("{}: {e}", link.display()))
}

fn write_replacing(path: &Path, text: &str, mode: u32) -> Result<(), String> {
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
    fn wrappers_are_copies_of_the_new_systems_programs_with_their_owner_and_mode() {
        use std::os::unix::fs::MetadataExt;
        let work = tempfile::tempdir().unwrap();
        let root = work.path().join("root");
        let system = work.path().join("system");
        fs::create_dir_all(system.join("sw/bin")).unwrap();
        fs::write(system.join("sw/bin/tool"), "new\n").unwrap();
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
                source: "/run/current-system/sw/bin/tool".into(),
                owner: "me".into(),
                group: "mine".into(),
                mode: 0o4750,
            },
        );
        make_wrappers(&root, &system, &model).unwrap();
        make_wrappers(&root, &system, &model).unwrap();

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

    /// The user and group this test runs as, which it can give its own files.
    fn own_ids() -> (u32, u32) {
        use std::os::unix::fs::MetadataExt;
        let file = tempfile::NamedTempFile::new().unwrap();
        let metadata = fs::metadata(file.path()).unwrap();
        (metadata.uid(), metadata.gid())
    }

    #[test]
    fn etc_links_through_static_and_drops_what_the_old_system_had() {
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
        assert_eq!(fs::read_link(etc.join("static")).unwrap(), new.join("etc"));
        assert_eq!(
            fs::read_link(etc.join("ssh/sshd_config")).unwrap(),
            Path::new("/etc/static/ssh/sshd_config")
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
