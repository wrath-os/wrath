//! Switching the running machine to a system built without NixOS: /etc, accounts, tmpfiles and
//! units, as the plan between the two systems' models says. `plan` shows the same activation.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
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
    }
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
        .filter(|name| after.units.get(*name).is_some_and(wanted))
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
