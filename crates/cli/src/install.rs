//! Installing a machine: laying out its disks from the `storage` layout the `disks` module
//! renders, then putting the system on them.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use distro_backend::{self as backend, Target};
use distro_eval::Value;
use distro_model::Model;

use crate::{generations, machine};

/// A partition of the layout, as the `disks` module renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub label: String,
    pub disk: String,
    pub device: i64,
    /// Such as `1G`, or `rest`.
    pub size: String,
    /// The filesystem on just this partition, or empty when a filesystem spans it.
    pub format: String,
    pub encrypt: bool,
}

/// The partitions in `storage`, in the order they go on their disks.
pub fn partitions(storage: &Value) -> Vec<Partition> {
    list(storage, "partitions")
        .iter()
        .filter(|item| matches!(item, Value::Table(_)))
        .map(|item| Partition {
            label: text(item, "label"),
            disk: text(item, "disk"),
            device: match field(item, "device") {
                Value::Int(n) => *n,
                _ => 0,
            },
            size: text(item, "size"),
            format: text(item, "format"),
            encrypt: matches!(field(item, "encrypt"), Value::Bool(true)),
        })
        .collect()
}

/// systemd-repart's definition files for device `device` of disk `disk`, by file name: one per
/// partition, numbered so repart lays them out in order. Encrypted partitions repart formats take
/// their key from its `--key-file`.
pub fn repart_definitions(
    partitions: &[Partition],
    disk: &str,
    device: i64,
) -> Vec<(String, String)> {
    partitions
        .iter()
        .filter(|partition| partition.disk == disk && partition.device == device)
        .enumerate()
        .map(|(i, partition)| {
            let kind = match partition.format.as_str() {
                "vfat" => "esp",
                "swap" => "swap",
                _ => "linux-generic",
            };
            let mut text = format!("[Partition]\nType={kind}\nLabel={}\n", partition.label);
            if partition.size != "rest" {
                let bytes = bytes(&partition.size);
                text.push_str(&format!("SizeMinBytes={bytes}\nSizeMaxBytes={bytes}\n"));
            }
            // repart encrypts only what it formats; distro encrypts the partitions a filesystem
            // spans itself, before creating the filesystem across them.
            if !partition.format.is_empty() {
                if partition.encrypt {
                    text.push_str("Encrypt=key-file\n");
                }
                text.push_str(&format!("Format={}\n", partition.format));
            }
            (format!("{:02}-{}.conf", i + 10, partition.label), text)
        })
        .collect()
}

/// A size as the `disks` module checked it, such as `512M`, in bytes.
fn bytes(size: &str) -> u64 {
    let (number, unit) = size.split_at(size.len().saturating_sub(1));
    let shift = match unit {
        "K" => 10,
        "M" => 20,
        "G" => 30,
        "T" => 40,
        _ => 0,
    };
    number.parse::<u64>().unwrap_or(0) << shift
}

/// How `distro install` was asked to install.
pub struct Install<'a> {
    pub config: &'a Path,
    /// Each disk of the layout with its devices, in order.
    pub disks: BTreeMap<String, Vec<PathBuf>>,
    /// The machine's name, recorded on it.
    pub name: Option<&'a str>,
    pub role: Option<&'a str>,
    /// Where the disks' passphrase is, instead of asking for it.
    pub passphrase: Option<&'a Path>,
    /// An age identity that decrypts the machine's keys, which stays where install runs.
    pub identity: Option<&'a Path>,
    /// Where the new system is mounted while it's installed.
    pub target: &'a Path,
    /// Lay out disks that have partitions already, destroying them.
    pub wipe: bool,
    pub yes: bool,
    /// What to build and install: the system, or a test system.
    pub system: Target,
}

/// The devices given with `--disk`, by the disk of the layout they're for. One given without a
/// name, `--disk /dev/vda`, is for the layout's only disk, which [`install`] works out.
pub fn disks(given: &[String]) -> BTreeMap<String, Vec<PathBuf>> {
    let mut disks: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for disk in given {
        let (name, device) = disk.split_once('=').unwrap_or(("", disk));
        disks
            .entry(name.to_owned())
            .or_default()
            .push(PathBuf::from(device));
    }
    disks
}

/// Lays out the disks the model's `storage` describes, mounts them at the target, and installs
/// the system on them.
pub fn install(root: &Path, model: Model, how: &Install) -> Result<ExitCode, String> {
    let storage = model
        .base
        .get("storage")
        .ok_or("the configuration describes no disks to install on; add them under `[disks]`")?;
    let partitions = partitions(storage);
    let disks = if how.disks.is_empty() {
        match_disks(storage, &list_blocks()?, how.wipe)?
    } else {
        named(storage, &how.disks)?
    };
    check_disks(storage, &disks)?;
    let how = &Install { disks, ..*how };
    println!("installing onto:");
    for (disk, devices) in &how.disks {
        for (n, device) in devices.iter().enumerate() {
            let labels: Vec<&str> = partitions
                .iter()
                .filter(|p| &p.disk == disk && p.device == n as i64)
                .map(|p| p.label.as_str())
                .collect();
            println!("  {} as {disk}{n}: {}", device.display(), labels.join(", "));
        }
    }
    let what = if how.wipe {
        "erase these disks and install?"
    } else {
        "install?"
    };
    if !generations::confirmed(how.yes, what)? {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    }
    let work = tempfile::tempdir().map_err(|e| format!("cannot create a work directory: {e}"))?;
    let key = work.path().join("key");
    let passphrase = match how.passphrase {
        Some(file) => std::fs::read_to_string(file)
            .map_err(|e| format!("{}: {e}", file.display()))?
            .trim_end_matches('\n')
            .to_owned(),
        None => ask_passphrase()?,
    };
    // Readable only by root, as systemd-repart and cryptsetup require of a key file.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key)
        .and_then(|mut file| file.write_all(passphrase.as_bytes()))
        .map_err(|e| format!("cannot keep the passphrase: {e}"))?;
    let system = generations::build(root, &model, how.system)?;
    let result = lay_out(&partitions, storage, how, &key, work.path())
        // Before the boot loader: it copies the initrd's SSH key into the initrd.
        .and_then(|()| place_keys(how))
        .and_then(|()| backend::install(&system, how.target))
        .and_then(|()| record(how))
        .and_then(|()| match crate::state::State::of(&model) {
            Some(state) => state.move_into_keep(how.target),
            None => Ok(()),
        });
    let closed = close(&partitions, how.target);
    result?;
    closed?;
    println!(
        "installed {} on the disks; it starts from them at the next boot",
        system.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// `given` with devices given without a name put under the layout's only disk.
fn named(
    storage: &Value,
    given: &BTreeMap<String, Vec<PathBuf>>,
) -> Result<BTreeMap<String, Vec<PathBuf>>, String> {
    let mut given = given.clone();
    let Some(unnamed) = given.remove("") else {
        return Ok(given);
    };
    let names: Vec<String> = match field(storage, "disks") {
        Value::Table(disks) => disks.keys().cloned().collect(),
        _ => Vec::new(),
    };
    match names.as_slice() {
        [only] => {
            given.entry(only.clone()).or_default().extend(unnamed);
            Ok(given)
        }
        _ => Err(format!(
            "the configuration has disks {}; name the one each device is for, as \
             `--disk NAME=/dev/...`",
            names.join(", ")
        )),
    }
}

/// A disk of the machine, as `lsblk` sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    /// How it's attached, such as `nvme` or `sata`, when lsblk knows.
    pub transport: String,
    /// It has a partition table or partitions already.
    pub partitioned: bool,
    /// Something on it is mounted, such as the live system install runs from.
    pub mounted: bool,
}

/// The disks in `lsblk --json --bytes --output NAME,PATH,SIZE,TYPE,TRAN,PTTYPE,MOUNTPOINTS`.
pub fn blocks(json: &str) -> Result<Vec<Block>, String> {
    let parsed: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("cannot read what lsblk says: {e}"))?;
    Ok(parsed["blockdevices"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|device| device["type"] == "disk")
        .map(|device| Block {
            path: PathBuf::from(device["path"].as_str().unwrap_or_default()),
            name: device["name"].as_str().unwrap_or_default().to_owned(),
            size: device["size"].as_u64().unwrap_or(0),
            transport: device["tran"].as_str().unwrap_or_default().to_owned(),
            partitioned: !device["pttype"].is_null()
                || device["children"].as_array().is_some_and(|c| !c.is_empty()),
            mounted: mounted(device),
        })
        .collect())
}

fn mounted(device: &serde_json::Value) -> bool {
    let here = device["mountpoints"]
        .as_array()
        .is_some_and(|points| points.iter().any(|point| !point.is_null()));
    here || device["children"]
        .as_array()
        .is_some_and(|children| children.iter().any(mounted))
}

/// The machine's disks for each disk the layout describes: those it names by path, or those of
/// the kind and size it asks for. Disks with partitions count only with `wipe`, and mounted ones
/// never do. More disks fitting than a description wants is an error, rather than a guess.
pub fn match_disks(
    storage: &Value,
    blocks: &[Block],
    wipe: bool,
) -> Result<BTreeMap<String, Vec<PathBuf>>, String> {
    let Value::Table(disks) = field(storage, "disks") else {
        return Err("the configuration describes no disks".into());
    };
    let mut chosen: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let mut taken: Vec<PathBuf> = Vec::new();
    for (name, disk) in disks {
        let paths = strings(field(disk, "paths"));
        if !paths.is_empty() {
            chosen.insert(name.clone(), paths.iter().map(PathBuf::from).collect());
            continue;
        }
        let count = match field(disk, "count") {
            Value::Int(n) => *n as usize,
            _ => 1,
        };
        let kind = text(disk, "kind");
        let at_least = text(disk, "at_least");
        let least = bytes(&at_least);
        let fits: Vec<&Block> = blocks
            .iter()
            .filter(|block| !block.mounted && (wipe || !block.partitioned))
            .filter(|block| block.size >= least && !taken.contains(&block.path))
            .filter(|block| match kind.as_str() {
                "nvme" => block.transport == "nvme" || block.name.starts_with("nvme"),
                "sata" => block.transport == "sata",
                "virtio" => block.name.starts_with("vd"),
                _ => true,
            })
            .collect();
        if fits.len() != count {
            let wanted = describe(count, &kind, &at_least);
            let listed: Vec<String> = fits
                .iter()
                .map(|block| format!("{} ({}G)", block.path.display(), block.size >> 30))
                .collect();
            let found = if listed.is_empty() {
                "none".to_owned()
            } else {
                listed.join(", ")
            };
            let hint = if fits.len() < count && !wipe {
                "; disks with partitions count with `--wipe`"
            } else {
                ""
            };
            return Err(format!(
                "the disk `{name}` is {wanted}, and this machine has {} that fit: {found}{hint}; \
                 name them with `--disk {name}=/dev/...`",
                fits.len()
            ));
        }
        let paths: Vec<PathBuf> = fits.iter().map(|block| block.path.clone()).collect();
        taken.extend(paths.iter().cloned());
        chosen.insert(name.clone(), paths);
    }
    Ok(chosen)
}

/// What a disk description asks for, such as `2 nvme disks of at least 500G`.
fn describe(count: usize, kind: &str, at_least: &str) -> String {
    let kind = if kind == "any" || kind.is_empty() {
        String::new()
    } else {
        format!("{kind} ")
    };
    let plural = if count == 1 { "" } else { "s" };
    let size = if at_least.is_empty() {
        String::new()
    } else {
        format!(" of at least {at_least}")
    };
    format!("{count} {kind}disk{plural}{size}")
}

/// The machine's disks, as `lsblk` lists them.
fn list_blocks() -> Result<Vec<Block>, String> {
    let output = Command::new("lsblk")
        .args([
            "--json",
            "--bytes",
            "--output",
            "NAME,PATH,SIZE,TYPE,TRAN,PTTYPE,MOUNTPOINTS",
        ])
        .output()
        .map_err(|e| format!("cannot run lsblk: {e}"))?;
    blocks(&String::from_utf8_lossy(&output.stdout))
}

/// Checks there are devices for every disk the layout has, as many as it wants.
fn check_disks(storage: &Value, given: &BTreeMap<String, Vec<PathBuf>>) -> Result<(), String> {
    let Value::Table(storage) = storage else {
        return Err("the disks layout isn't a table".into());
    };
    let Some(Value::Table(disks)) = storage.get("disks") else {
        return Err("the configuration describes no disks".into());
    };
    for (name, disk) in disks {
        let count = match field(disk, "count") {
            Value::Int(n) => *n as usize,
            _ => 1,
        };
        let devices = given.get(name).map_or(0, Vec::len);
        if devices != count {
            return Err(format!(
                "the disk `{name}` has {count} device(s), and {devices} were given; give them \
                 as `--disk {name}=/dev/...`"
            ));
        }
    }
    if let Some(unknown) = given.keys().find(|name| !disks.contains_key(*name)) {
        return Err(format!("the configuration has no disk `{unknown}`"));
    }
    Ok(())
}

fn ask_passphrase() -> Result<String, String> {
    let ask = |prompt: &str| {
        let output = Command::new("systemd-ask-password")
            .arg(prompt)
            .output()
            .map_err(|e| format!("cannot run systemd-ask-password: {e}"))?;
        Ok::<_, String>(
            String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_owned(),
        )
    };
    let passphrase = ask("Passphrase for the disks:")?;
    if passphrase.is_empty() || passphrase != ask("The same passphrase again:")? {
        return Err("the passphrases were empty or didn't match".into());
    }
    Ok(passphrase)
}

/// Partitions each device with systemd-repart, opens what's encrypted, creates the filesystems
/// that span partitions, and mounts everything under the target.
fn lay_out(
    partitions: &[Partition],
    storage: &Value,
    how: &Install,
    key: &Path,
    work: &Path,
) -> Result<(), String> {
    for (disk, devices) in &how.disks {
        for (n, device) in devices.iter().enumerate() {
            let definitions = work.join(format!("{disk}{n}"));
            std::fs::create_dir_all(&definitions).map_err(|e| e.to_string())?;
            for (name, text) in repart_definitions(partitions, disk, n as i64) {
                std::fs::write(definitions.join(name), text).map_err(|e| e.to_string())?;
            }
            run(Command::new("systemd-repart")
                .arg("--dry-run=no")
                .arg(if how.wipe {
                    "--empty=force"
                } else {
                    "--empty=require"
                })
                .arg(format!("--definitions={}", definitions.display()))
                .arg(format!("--key-file={}", key.display()))
                .arg(device))?;
        }
    }
    run(Command::new("udevadm").arg("settle"))?;
    for partition in partitions
        .iter()
        .filter(|p| p.encrypt && p.format.is_empty())
    {
        run(Command::new("cryptsetup")
            .args(["luksFormat", "--batch-mode", "--key-file"])
            .arg(key)
            .arg(format!("/dev/disk/by-partlabel/{}", partition.label)))?;
    }
    for partition in partitions.iter().filter(|p| p.encrypt) {
        run(Command::new("cryptsetup")
            .args(["open", "--key-file"])
            .arg(key)
            .arg(format!("/dev/disk/by-partlabel/{}", partition.label))
            .arg(&partition.label))?;
    }
    let unlock = field(storage, "unlock");
    if matches!(field(unlock, "tpm2"), Value::Bool(true)) {
        let pcrs: Vec<String> = match field(unlock, "pcrs") {
            Value::List(items) => items.iter().map(ToString::to_string).collect(),
            _ => Vec::new(),
        };
        for partition in partitions.iter().filter(|p| p.encrypt) {
            run(Command::new("systemd-cryptenroll")
                .arg(format!("--unlock-key-file={}", key.display()))
                .arg("--tpm2-device=auto")
                .arg(format!("--tpm2-pcrs={}", pcrs.join("+")))
                .arg(format!("/dev/disk/by-partlabel/{}", partition.label)))?;
        }
    }
    for spanning in list(storage, "spanning") {
        let members = strings(field(spanning, "members"));
        let name = text(spanning, "name");
        let raid = text(spanning, "raid");
        run(Command::new("mkfs.btrfs")
            .args([
                "--force",
                "--label",
                &name,
                "--data",
                &raid,
                "--metadata",
                &raid,
            ])
            .args(&members))?;
        // The kernel only mounts a filesystem across disks once it knows every member; udev would
        // tell it in time, but install doesn't wait for udev.
        run(Command::new("modprobe").arg("btrfs"))?;
        run(Command::new("btrfs")
            .args(["device", "scan"])
            .args(&members))?;
        let scratch = work.join(format!("fs-{name}"));
        std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
        run(Command::new("mount").arg(&members[0]).arg(&scratch))?;
        for subvolume in strings(field(spanning, "subvolumes")) {
            run(Command::new("btrfs")
                .args(["subvolume", "create"])
                .arg(scratch.join(&subvolume)))?;
        }
        run(Command::new("umount").arg(&scratch))?;
    }
    let mut mounts = list(storage, "mounts").to_vec();
    // A mount point is made inside the filesystem mounted above it.
    mounts.sort_by_key(|mount| text(mount, "path").matches('/').count());
    for mount in mounts
        .iter()
        .filter(|mount| text(mount, "format") != "swap")
    {
        let at = how.target.join(text(mount, "path").trim_start_matches('/'));
        std::fs::create_dir_all(&at).map_err(|e| format!("{}: {e}", at.display()))?;
        let options = strings(field(mount, "options")).join(",");
        let mut command = Command::new("mount");
        command.args(["-t", &text(mount, "format")]);
        if !options.is_empty() {
            command.args(["-o", &options]);
        }
        run(command.arg(text(mount, "device")).arg(&at))?;
    }
    Ok(())
}

/// The machine's keys, made before install and kept encrypted in the configuration, by the file
/// each is decrypted to on the machine.
const KEYS: [(&str, &str); 2] = [
    ("ssh_host_ed25519_key.age", "etc/ssh/ssh_host_ed25519_key"),
    ("initrd_ed25519_key.age", "etc/ssh/initrd_ed25519_key"),
];

/// Decrypts the machine's keys from `secrets/machines/<name>/` into the new system, with the
/// identity install was given, and writes their public halves beside them. The identity isn't
/// copied anywhere.
fn place_keys(how: &Install) -> Result<(), String> {
    let Some(name) = how.name else {
        return Ok(());
    };
    let dir = how.config.join("secrets/machines").join(name);
    for (encrypted, file) in KEYS {
        let source = dir.join(encrypted);
        if !source.is_file() {
            continue;
        }
        let identity = how.identity.ok_or_else(|| {
            format!(
                "{} is encrypted; give `--identity` with a key that decrypts it",
                source.display()
            )
        })?;
        let key = how.target.join(file);
        let parent = key.parent().expect("keys are in a directory");
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        run(Command::new("age")
            .args(["--decrypt", "--identity"])
            .arg(identity)
            .arg("--output")
            .arg(&key)
            .arg(&source))?;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", key.display()))?;
        let public = Command::new("ssh-keygen")
            .args(["-y", "-f"])
            .arg(&key)
            .output()
            .map_err(|e| format!("cannot run ssh-keygen: {e}"))?;
        if !public.status.success() {
            return Err(format!("{} isn't an SSH key", source.display()));
        }
        let pub_file = key.with_extension("pub");
        std::fs::write(&pub_file, public.stdout)
            .map_err(|e| format!("{}: {e}", pub_file.display()))?;
        println!("placed the machine's {}", key.display());
    }
    Ok(())
}

/// Records the machine's role and name on it, and copies the configuration to where it's
/// applied from.
fn record(how: &Install) -> Result<(), String> {
    let given = machine::Given {
        role: how.role.map(str::to_owned),
        name: how.name.map(str::to_owned),
        values: BTreeMap::new(),
        disks: how
            .disks
            .iter()
            .map(|(disk, devices)| (disk.clone(), devices.iter().map(|d| stable(d)).collect()))
            .collect(),
    };
    let file = how
        .target
        .join(machine::MACHINE_FILE.trim_start_matches('/'));
    machine::record(&file, &given)?;
    let etc = how.target.join("etc");
    std::fs::create_dir_all(&etc).map_err(|e| format!("{}: {e}", etc.display()))?;
    run(Command::new("cp")
        .arg("-rT")
        .arg(how.config)
        .arg(etc.join("distro")))
}

/// A disk's name in `/dev/disk/by-id`, which stays the same across boots and other disks coming
/// and going, or its path when it has none there, as a virtual disk without a serial number.
fn stable(device: &Path) -> String {
    let resolved = device
        .canonicalize()
        .unwrap_or_else(|_| device.to_path_buf());
    let Ok(entries) = std::fs::read_dir("/dev/disk/by-id") else {
        return device.display().to_string();
    };
    let mut names: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.canonicalize().is_ok_and(|target| target == resolved))
        .collect();
    // Names from the disk's model and serial read better than its WWN.
    names.sort_by_key(|path| path.to_string_lossy().contains("/wwn-"));
    names
        .first()
        .map_or(device, PathBuf::as_path)
        .display()
        .to_string()
}

/// Unmounts the target and closes what was opened, after an install or a failed one.
fn close(partitions: &[Partition], target: &Path) -> Result<(), String> {
    let unmounted = run(Command::new("umount").arg("--recursive").arg(target));
    for partition in partitions.iter().filter(|p| p.encrypt) {
        let _ = run(Command::new("cryptsetup")
            .arg("close")
            .arg(&partition.label));
    }
    unmounted
}

/// The longest a disk tool may take. One waiting for something that never comes, such as a device
/// udev never announces, fails the install instead of hanging it.
const TOOL_TIMEOUT: Duration = Duration::from_secs(300);

fn run(command: &mut Command) -> Result<(), String> {
    backend::run_within(command, TOOL_TIMEOUT)
}

fn list<'v>(value: &'v Value, key: &str) -> &'v [Value] {
    match field(value, key) {
        Value::List(items) => items,
        _ => &[],
    }
}

fn field<'v>(value: &'v Value, key: &str) -> &'v Value {
    static NOTHING: Value = Value::Bool(false);
    match value {
        Value::Table(fields) => fields.get(key).unwrap_or(&NOTHING),
        _ => &NOTHING,
    }
}

fn text(value: &Value, key: &str) -> String {
    match field(value, key) {
        Value::Str(text) => text.clone(),
        _ => String::new(),
    }
}

fn strings(value: &Value) -> Vec<String> {
    match value {
        Value::List(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Str(text) => Some(text.clone()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_that_hangs_is_stopped_and_one_that_fails_says_why() {
        let started = std::time::Instant::now();
        let err = backend::run_within(Command::new("sleep").arg("30"), Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(
            err,
            "`sleep 30` didn't finish within 1 seconds, so distro stopped it"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        // A process the tool leaves running doesn't hold up the install.
        let script = "sleep 30 & echo nope >&2; exit 3";
        let err = backend::run_within(
            Command::new("sh").args(["-c", script]),
            Duration::from_secs(5),
        );
        assert_eq!(err.unwrap_err(), "sh failed: nope");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    fn partition(label: &str, device: i64, size: &str, format: &str, encrypt: bool) -> Partition {
        Partition {
            label: label.into(),
            disk: "nvme".into(),
            device,
            size: size.into(),
            format: format.into(),
            encrypt,
        }
    }

    #[test]
    fn each_disk_gets_its_own_partitions_in_order() {
        let partitions = [
            partition("nvme0-esp", 0, "1G", "vfat", false),
            partition("nvme0-swap", 0, "16G", "swap", true),
            partition("nvme0-system", 0, "rest", "", true),
            partition("nvme1-system", 1, "rest", "", true),
        ];

        let first: Vec<String> = repart_definitions(&partitions, "nvme", 0)
            .into_iter()
            .map(|(name, text)| format!("{name}\n{text}"))
            .collect();
        insta::assert_snapshot!(first.join("\n"), @"
        10-nvme0-esp.conf
        [Partition]
        Type=esp
        Label=nvme0-esp
        SizeMinBytes=1073741824
        SizeMaxBytes=1073741824
        Format=vfat

        11-nvme0-swap.conf
        [Partition]
        Type=swap
        Label=nvme0-swap
        SizeMinBytes=17179869184
        SizeMaxBytes=17179869184
        Encrypt=key-file
        Format=swap

        12-nvme0-system.conf
        [Partition]
        Type=linux-generic
        Label=nvme0-system
        ");
        let second = repart_definitions(&partitions, "nvme", 1);
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].0, "10-nvme1-system.conf");
    }

    #[test]
    fn sizes_are_bytes() {
        assert_eq!(bytes("512M"), 512 << 20);
        assert_eq!(bytes("1G"), 1 << 30);
        assert_eq!(bytes("2T"), 2 << 40);
    }

    #[test]
    fn a_machine_whose_keys_are_encrypted_needs_an_identity() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let keys = config.join("secrets/machines/w-01");
        std::fs::create_dir_all(&keys).unwrap();
        let install = |name| Install {
            config: &config,
            disks: BTreeMap::new(),
            name,
            role: None,
            passphrase: None,
            identity: None,
            target: dir.path(),
            wipe: false,
            yes: true,
            system: Target::System,
        };

        assert_eq!(place_keys(&install(Some("w-01"))), Ok(()));
        std::fs::write(keys.join("ssh_host_ed25519_key.age"), "").unwrap();
        let err = place_keys(&install(Some("w-01"))).unwrap_err();
        assert!(err.contains("give `--identity`"), "{err}");
        assert_eq!(place_keys(&install(None)), Ok(()));
    }

    const LSBLK: &str = r#"{"blockdevices": [
      {"name": "sda", "path": "/dev/sda", "size": 32000000000, "type": "disk", "tran": "usb",
       "pttype": "dos", "mountpoints": [null],
       "children": [{"name": "sda1", "path": "/dev/sda1", "size": 31000000000, "type": "part",
         "tran": null, "pttype": "dos", "mountpoints": ["/iso"]}]},
      {"name": "nvme0n1", "path": "/dev/nvme0n1", "size": 1000204886016, "type": "disk",
       "tran": "nvme", "pttype": null, "mountpoints": [null]},
      {"name": "nvme1n1", "path": "/dev/nvme1n1", "size": 1000204886016, "type": "disk",
       "tran": "nvme", "pttype": "gpt", "mountpoints": [null],
       "children": [{"name": "nvme1n1p1", "path": "/dev/nvme1n1p1", "size": 1000, "type": "part",
         "tran": "nvme", "pttype": "gpt", "mountpoints": [null]}]},
      {"name": "sdb", "path": "/dev/sdb", "size": 256000000000, "type": "disk", "tran": "sata",
       "pttype": null, "mountpoints": [null]},
      {"name": "sr0", "path": "/dev/sr0", "size": 1000, "type": "rom", "tran": "sata",
       "pttype": null, "mountpoints": [null]}
    ]}"#;

    fn wanting(disks: &[(&str, i64, &str, &str)]) -> Value {
        let disks = disks
            .iter()
            .map(|(name, count, kind, at_least)| {
                let disk = BTreeMap::from([
                    ("count".to_owned(), Value::Int(*count)),
                    ("kind".to_owned(), Value::Str((*kind).into())),
                    ("at_least".to_owned(), Value::Str((*at_least).into())),
                    ("paths".to_owned(), Value::List(vec![])),
                ]);
                ((*name).to_owned(), Value::Table(disk))
            })
            .collect();
        Value::Table(BTreeMap::from([("disks".to_owned(), Value::Table(disks))]))
    }

    #[test]
    fn lsblk_says_which_disks_are_free() {
        let blocks = blocks(LSBLK).unwrap();
        let names: Vec<(&str, bool, bool)> = blocks
            .iter()
            .map(|b| (b.name.as_str(), b.partitioned, b.mounted))
            .collect();
        assert_eq!(
            names,
            [
                ("sda", true, true),
                ("nvme0n1", false, false),
                ("nvme1n1", true, false),
                ("sdb", false, false)
            ]
        );
    }

    #[test]
    fn disks_are_matched_by_kind_and_size_and_never_guessed() {
        let blocks = blocks(LSBLK).unwrap();
        let matched = |storage: &Value, wipe| match_disks(storage, &blocks, wipe);

        let one = matched(&wanting(&[("main", 1, "nvme", "500G")]), false).unwrap();
        assert_eq!(one["main"], [PathBuf::from("/dev/nvme0n1")]);

        let both = matched(&wanting(&[("nvme", 2, "nvme", "500G")]), true).unwrap();
        assert_eq!(both["nvme"].len(), 2);

        let err = matched(&wanting(&[("nvme", 2, "nvme", "500G")]), false).unwrap_err();
        assert_eq!(
            err,
            "the disk `nvme` is 2 nvme disks of at least 500G, and this machine has 1 that fit: \
             /dev/nvme0n1 (931G); disks with partitions count with `--wipe`; name them with \
             `--disk nvme=/dev/...`"
        );

        // The live system's USB stick is mounted, so it never fits, even with `--wipe`.
        let err = matched(&wanting(&[("main", 1, "any", "")]), true).unwrap_err();
        assert!(err.contains("has 3 that fit"), "{err}");
        assert!(!err.contains("/dev/sda "), "{err}");

        let split = matched(
            &wanting(&[("fast", 1, "nvme", "500G"), ("slow", 1, "sata", "100G")]),
            false,
        )
        .unwrap();
        assert_eq!(split["slow"], [PathBuf::from("/dev/sdb")]);
    }
}
