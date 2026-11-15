//! What a system does on the machine at every boot and switch, because it
//! needs the machine's keys or accounts: decrypting secrets, filling in files with secrets or
//! machine values, and linking users' files. The system says what in its `activation.json`.
//! Secrets and filled-in files live on ramfs and are replaced in one step. One that fails is left
//! out with an error, and the others still happen, so a bad secret can't stop a machine booting.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::activate::replace_link;
use crate::state::State;
use crate::templates;

const SECRETS: &str = wrath_eval::SECRETS_DIR;
const TEMPLATES: &str = wrath_build::graph::FILLED;

/// Does what `system` asks of the machine it's being switched on: the machine's part, then
/// users' homes.
pub fn apply(system: &Path) -> Result<(), String> {
    at_boot(system)?;
    homes(system)
}

fn activation_of(system: &Path) -> Result<Value, String> {
    let path = system.join("activation.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// The machine's part of what `system` asks, which the first program does at boot: secrets,
/// filled-in files and the hostname. Homes wait for `wrath-homes.service`, since /home may be a
/// filesystem of its own that isn't mounted yet.
pub fn at_boot(system: &Path) -> Result<(), String> {
    let activation = activation_of(system)?;
    // Programs from the system being activated, which may not be on the path yet at boot.
    let sw = system.join("bin");
    if activation["secrets"]
        .as_object()
        .is_some_and(|all| !all.is_empty())
    {
        replace_on_ramfs(&sw, Path::new(SECRETS), |dir| {
            decrypt(system, &activation, dir);
            Ok(())
        })?;
    }
    if activation["templates"]
        .as_array()
        .is_some_and(|all| !all.is_empty())
    {
        fs::create_dir_all("/run/wrath").map_err(|e| format!("/run/wrath: {e}"))?;
        replace_on_ramfs(&sw, Path::new(TEMPLATES), |dir| {
            fill(system, &activation, dir)
        })?;
    }
    if activation["hostname_filled"] == Value::Bool(true) {
        set_hostname();
    }
    Ok(())
}

/// Users' homes as `system` has them: their directories, what's kept of them, and their files
/// linked in. At boot this runs once /home is mounted.
pub fn homes(system: &Path) -> Result<(), String> {
    let activation = activation_of(system)?;
    let sw = system.join("bin");
    run(
        Command::new(sw.join("systemd-tmpfiles"))
            .args(["--create", "/etc/tmpfiles.d/wrath-homes.conf"]),
        "home directories",
    );
    if let Some(state) = State::of(&crate::activate::model_of(system)?) {
        state.keep_homes(system);
    }
    link_homes(system, &activation);
    Ok(())
}

/// Runs `command`, saying so when it fails, since one failure doesn't stop the others.
fn run(command: &mut Command, what: &str) -> bool {
    match command.stdin(Stdio::null()).output() {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let errors = String::from_utf8_lossy(&output.stderr);
            eprintln!("wrath: {what} failed: {}", errors.trim());
            false
        }
        Err(e) => {
            eprintln!("wrath: {what} failed: {e}");
            false
        }
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

/// Fills a new directory on ramfs and makes `link` point at it in one step, removing the sets
/// before it.
fn replace_on_ramfs(
    sw: &Path,
    link: &Path,
    fill: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let store = PathBuf::from(format!("{}.d", link.display()));
    fs::create_dir_all(&store).map_err(|e| format!("{}: {e}", store.display()))?;
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    if !mounts.contains(&format!(" {} ramfs ", store.display())) {
        let mounted = run(
            Command::new(sw.join("mount"))
                .args(["-t", "ramfs", "none"])
                .arg(&store)
                .args(["-o", "nodev,nosuid,mode=0751"]),
            "mounting ramfs",
        );
        if !mounted {
            return Err(format!(
                "cannot keep secrets off the disk in {}",
                store.display()
            ));
        }
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    let generation = store.join(nanos.to_string());
    fs::create_dir(&generation).map_err(|e| format!("{}: {e}", generation.display()))?;
    fs::set_permissions(&generation, fs::Permissions::from_mode(0o751))
        .map_err(|e| format!("{}: {e}", generation.display()))?;
    fill(&generation)?;
    replace_link(&generation, link)?;
    for entry in fs::read_dir(&store).map_err(|e| format!("{}: {e}", store.display()))? {
        let path = entry
            .map_err(|e| format!("{}: {e}", store.display()))?
            .path();
        if path != generation {
            let _ = fs::remove_dir_all(path);
        }
    }
    Ok(())
}

/// Gives `path` its owner, as `chown` takes it, and its mode, in octal.
fn own(sw: &Path, path: &Path, owner: &str, mode: &str) {
    run(
        Command::new(sw.join("chown")).arg(owner).arg(path),
        &format!("giving {} its owner", path.display()),
    );
    let mode = u32::from_str_radix(mode, 8).unwrap_or(0o400);
    if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(mode)) {
        eprintln!("wrath: {}: {e}", path.display());
    }
}

/// Decrypts each secret `system` has into `dir`. Its encrypted file is named relative to the system.
fn decrypt(system: &Path, activation: &Value, dir: &Path) {
    let sw = &system.join("bin");
    let identities: Vec<&str> = items(&activation["identities"])
        .filter_map(Value::as_str)
        .filter(|identity| Path::new(identity).exists())
        .collect();
    let age = activation["age"].as_str().unwrap_or("age");
    for (name, secret) in activation["secrets"].as_object().into_iter().flatten() {
        let file = dir.join(name);
        // Readable only by root from the start, before it has anything in it.
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file);
        let decrypted = output.is_ok_and(|output| {
            let mut command = Command::new(age);
            command.arg("-d");
            for identity in &identities {
                command.args(["-i", identity]);
            }
            run(
                command
                    .arg(system.join(text(secret, "source")))
                    .stdout(Stdio::from(output)),
                &format!("decrypting secret {name}"),
            )
        });
        if decrypted {
            own(sw, &file, text(secret, "owner"), text(secret, "mode"));
        } else {
            let _ = fs::remove_file(&file);
        }
        let link = text(secret, "link");
        if !link.is_empty() {
            link_secret(name, Path::new(link));
        }
    }
}

/// Makes `link`, where a configuration wants a secret, point to it. Links stay valid as
/// `/run/wrath/secrets` moves between sets.
fn link_secret(name: &str, link: &Path) {
    if let Some(parent) = link.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::remove_file(link);
    if let Err(e) = symlink(Path::new(SECRETS).join(name), link) {
        eprintln!("wrath: {}: {e}", link.display());
    }
}

fn fill(system: &Path, activation: &Value, dir: &Path) -> Result<(), String> {
    let sw = &system.join("bin");
    let spec = activation["templates_spec"]
        .as_str()
        .ok_or("the system doesn't say what to fill in")?;
    let spec = system.join(spec);
    let machine = templates::machine_values()?;
    // A file that can't be filled in has already said so, and the others are still there.
    let _ = templates::fill(&spec, Path::new(SECRETS), &machine, dir);
    for template in items(&activation["templates"]) {
        let file = dir.join(text(template, "name"));
        if file.exists() {
            own(sw, &file, text(template, "owner"), text(template, "mode"));
        }
    }
    Ok(())
}

/// A hostname filled in from the machine's name takes effect now; systemd reads it at boot.
fn set_hostname() {
    let name = fs::read_to_string("/etc/hostname").unwrap_or_default();
    let name = name.trim();
    if !name.is_empty()
        && let Err(e) = fs::write("/proc/sys/kernel/hostname", name)
    {
        eprintln!("wrath: setting the hostname: {e}");
    }
}

/// Links each home's files from `system` into the home, through the running system.
fn link_homes(system: &Path, activation: &Value) {
    let users = crate::activate::ids(Path::new("/etc/passwd"));
    let groups = crate::activate::ids(Path::new("/etc/group"));
    for home in items(&activation["homes"]) {
        let name = text(home, "name");
        let owner = users.get(name).zip(groups.get(text(home, "group")));
        let source = system.join("users").join(name).join("files");
        let through = crate::homes::through_running(name);
        let files = crate::homes::Files {
            user: name,
            home: Path::new(text(home, "home")),
            owner: owner.map(|(uid, gid)| (*uid, *gid)),
            source: &source,
            through: &through,
        };
        let record = Path::new(crate::homes::HOME_RECORDS).join(name);
        match crate::homes::link(Path::new("/"), &files, &record) {
            Ok(notes) => notes.iter().for_each(|note| eprintln!("wrath: {note}")),
            Err(e) => eprintln!("wrath: linking {name}'s files failed: {e}"),
        }
    }
}
