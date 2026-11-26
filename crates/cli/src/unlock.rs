//! How a machine's encrypted disks unlock: enrolling its TPM again, for when what the TPM checks
//! changed, such as the first boot of a unified kernel image or a firmware update.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::Value;

use crate::recovery::Keep;

#[derive(clap::Subcommand)]
pub enum UnlockCommand {
    /// Let the TPM unlock the disks again, as the machine boots now: it asks for the disks'
    /// passphrase once, replaces their TPM key and records their volume keys, which the images a
    /// machine with Secure Boot signs pin them to. For when a boot measured differently and
    /// asked for the passphrase
    Enroll,
    /// Make a new recovery key for each encrypted disk, replacing the old, and keep them as
    /// `unlock.recovery` says. It asks for the disks' passphrase once
    Recovery,
    /// Show the recovery keys a machine keeps on its ESP, decrypted with an admin's key, from any
    /// machine
    RecoveryKey {
        /// The ESP, mounted, or the file on it
        #[arg(long, default_value = "/boot")]
        from: PathBuf,
        /// A private key that decrypts them [default: ~/.ssh/id_ed25519]
        #[arg(long)]
        identity: Option<PathBuf>,
    },
}

/// The running system's model, which says which PCRs unlocking depends on.
const MODEL: &str = "/run/system/etc/wrath/model.json";

pub fn run(command: UnlockCommand) -> Result<ExitCode, String> {
    match command {
        UnlockCommand::Enroll => enroll(Path::new("/")),
        UnlockCommand::Recovery => recovery(Path::new("/")),
        UnlockCommand::RecoveryKey { from, identity } => recovery_key(&from, identity),
    }
}

/// The running system's model, from the machine at `root`.
fn model(root: &Path) -> Result<Value, String> {
    let model = fs::read_to_string(root.join(MODEL.trim_start_matches('/')))
        .map_err(|e| format!("{MODEL}: {e}"))?;
    serde_json::from_str(&model).map_err(|e| format!("{MODEL}: {e}"))
}

/// The disks' passphrase, from `$PASSWORD` or asked for once, in a file in a directory only root
/// reads, in memory, gone with the directory.
fn passphrase_file(root: &Path) -> Result<(tempfile::TempDir, PathBuf), String> {
    let passphrase = match std::env::var("PASSWORD") {
        Ok(passphrase) => passphrase,
        Err(_) => crate::host::read_password("Passphrase for the disks: ")?,
    };
    let run = root.join("run");
    let work = tempfile::Builder::new()
        .prefix("wrath-unlock")
        .tempdir_in(&run)
        .map_err(|e| format!("cannot make a directory under {}: {e}", run.display()))?;
    let key = work.path().join("passphrase");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key)
        .and_then(|mut file| file.write_all(passphrase.as_bytes()))
        .map_err(|e| format!("{}: {e}", key.display()))?;
    Ok((work, key))
}

/// Makes a new recovery key for every encrypted disk of the machine at `root` and keeps them as
/// its `unlock.recovery` says.
fn recovery(root: &Path) -> Result<ExitCode, String> {
    let crypttab = fs::read_to_string(root.join("etc/crypttab")).unwrap_or_default();
    let volumes = volumes(&crypttab);
    if volumes.is_empty() {
        return Err("no disk here is encrypted".to_owned());
    }
    let model = model(root)?;
    let recovery = &model["base"]["storage"]["unlock"]["recovery"];
    let command = strings(&recovery["command"]);
    let keep = Keep::of(recovery["keep"].as_str().unwrap_or_default(), command);
    if keep == Keep::None {
        return Err("`unlock.recovery.keep` is \"none\"".to_owned());
    }
    let (_work, key) = passphrase_file(root)?;
    let mut keys = BTreeMap::new();
    for (name, device) in volumes {
        println!("making a new recovery key for {device}");
        keys.insert(name, crate::recovery::enroll(&device, &key)?);
    }
    let esp = model["base"]["storage"]["mounts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|mount| mount["format"] == "vfat")
        .and_then(|mount| mount["path"].as_str())
        .map(|path| root.join(path.trim_start_matches('/')));
    let recipients = strings(&model["base"]["secrets"]["recipients"]);
    let machine = crate::recovery::Machine {
        name: model["options"]["system.hostname"]
            .as_str()
            .unwrap_or_default(),
        root,
        esp: esp.as_deref(),
        recipients: &recipients,
    };
    crate::recovery::keep(&keys, &keep, &machine)?;
    Ok(ExitCode::SUCCESS)
}

/// Prints the recovery keys kept on the ESP at `from`, or in the file `from`, decrypted with
/// `identity`.
fn recovery_key(from: &Path, identity: Option<PathBuf>) -> Result<ExitCode, String> {
    let file = if from.is_dir() {
        from.join(crate::recovery::ON_ESP)
    } else {
        from.to_path_buf()
    };
    let identity = match identity {
        Some(identity) => identity,
        None => {
            Path::new(&std::env::var("HOME").map_err(|_| "HOME isn't set")?).join(".ssh/id_ed25519")
        }
    };
    let output = Command::new("age")
        .arg("--decrypt")
        .arg("--identity")
        .arg(&identity)
        .arg(&file)
        .output()
        .map_err(|e| format!("cannot run age: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cannot decrypt {} with {}: {}",
            file.display(),
            identity.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    print!("{}", String::from_utf8_lossy(&output.stdout));
    Ok(ExitCode::SUCCESS)
}

/// Replaces the TPM key of every disk the machine at `root` unlocks with its TPM by one sealed to
/// the PCRs as they are now, the passphrase, from `$PASSWORD` or asked for once, unlocking it for
/// that. Each disk's volume key id is recorded too, which pins the disk in the images the machine
/// signs.
fn enroll(root: &Path) -> Result<ExitCode, String> {
    let crypttab = fs::read_to_string(root.join("etc/crypttab")).unwrap_or_default();
    let volumes = tpm_volumes(&crypttab);
    if volumes.is_empty() {
        return Err("no disk here unlocks with the TPM; set `unlock.tpm2 = true` first".to_owned());
    }
    let model = model(root)?;
    let mut policy = vec![
        "--tpm2-device=auto".to_owned(),
        format!("--tpm2-pcrs={}", pcrs(&model["base"]["storage"]["unlock"])),
    ];
    if model["base"]["boot"]["secure_boot"] == Value::Bool(true) {
        policy.extend(signed_policy(root)?);
    }
    let (work, key) = passphrase_file(root)?;
    let pin = match model["base"]["storage"]["unlock"]["pin"] {
        Value::Bool(true) => {
            policy.push("--tpm2-with-pin=yes".to_owned());
            Some(new_pin()?)
        }
        _ => None,
    };
    policy.push(format!("--unlock-key-file={}", key.display()));
    let policy: Vec<&str> = policy.iter().map(String::as_str).collect();
    let mut ids = BTreeMap::new();
    for (name, device) in volumes {
        // Read first, so a wrong passphrase stops before any key is wiped.
        let id = crate::volume_keys::id_of(&device, &name, &key, work.path())?;
        ids.insert(name, id);
        println!("letting the TPM unlock {device} again, as it boots now");
        // The old key goes first: systemd-cryptenroll leaves a key for the same PCRs alone, even
        // when it's sealed to values they no longer have. The passphrase stays.
        cryptenroll(&device, &["--wipe-slot=tpm2"], None)?;
        cryptenroll(&device, &policy, pin.as_deref())?;
    }
    let before = crate::volume_keys::recorded(root);
    crate::volume_keys::record(root, &ids)?;
    // Images signed before pin no volume, or other keys.
    if crate::volume_keys::recorded(root) != before {
        crate::activate::boot_entries(root, false)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// With Secure Boot, the TPM also checks PCR 11 against a policy signed with the machine's PCR
/// key, which every image it signs carries: the disks then unlock only in the initrd of an image
/// the machine signed. PCR 7 changes when the firmware turns Secure Boot on, so enrolling before
/// then would only ask again.
fn signed_policy(root: &Path) -> Result<Vec<String>, String> {
    let key = root.join(crate::secure_boot::PCR_PUBLIC_KEY);
    if !key.is_file() {
        return Err(
            "this machine boots with Secure Boot, but no switch has signed what it boots yet; \
             `wrath apply` first"
                .to_owned(),
        );
    }
    // Empty when the kernel can't show them: efivarfs isn't there.
    let variables = Path::new(crate::secure_boot::SECURE_BOOT_STATE)
        .parent()
        .expect("in a directory");
    let shown =
        fs::read_dir(root.join(variables)).is_ok_and(|mut entries| entries.next().is_some());
    if !shown {
        return Err(format!(
            "cannot tell whether Secure Boot is on: the firmware's variables aren't in /{}",
            variables.display()
        ));
    }
    if !crate::secure_boot::firmware_enforces(root) {
        return Err(
            "Secure Boot isn't on in the firmware yet: put it in setup mode, enroll the keys from \
             the boot menu, and enroll the disks once it's back up"
                .to_owned(),
        );
    }
    Ok(vec![
        format!("--tpm2-public-key={}", key.display()),
        "--tpm2-public-key-pcrs=11".to_owned(),
    ])
}

/// The PIN a TPM key is sealed with too, from `$NEWPIN`, as systemd-cryptenroll takes it, or
/// asked for twice.
pub(crate) fn new_pin() -> Result<String, String> {
    use crate::host::read_password as ask;
    if let Ok(pin) = std::env::var("NEWPIN") {
        return Ok(pin);
    }
    let pin = ask("PIN to unlock the disks with, along with the TPM: ")?;
    if pin.is_empty() || pin != ask("The same PIN again: ")? {
        return Err("the PINs were empty or didn't match".into());
    }
    Ok(pin)
}

/// Runs systemd-cryptenroll on `device`, sealing with `pin` when there is one.
fn cryptenroll(device: &str, args: &[&str], pin: Option<&str>) -> Result<(), String> {
    let mut command = Command::new("systemd-cryptenroll");
    if let Some(pin) = pin {
        command.env("NEWPIN", pin);
    }
    let status = command
        .args(args)
        .arg(device)
        .status()
        .map_err(|e| format!("cannot run systemd-cryptenroll: {e}"))?;
    if !status.success() {
        return Err(format!(
            "systemd-cryptenroll {} {device} failed",
            args.join(" ")
        ));
    }
    Ok(())
}

/// The volumes in `crypttab` that unlock with the TPM, each as its name and device.
pub(crate) fn tpm_volumes(crypttab: &str) -> Vec<(String, String)> {
    entries(crypttab)
        .filter(|(.., options)| {
            options
                .split(',')
                .any(|option| option.starts_with("tpm2-device="))
        })
        .map(|(name, device, _)| (name, device))
        .collect()
}

/// Every volume in `crypttab`, as its name and device.
fn volumes(crypttab: &str) -> Vec<(String, String)> {
    entries(crypttab)
        .map(|(name, device, _)| (name, device))
        .collect()
}

/// The lines of `crypttab`, each as its name, device and options.
fn entries(crypttab: &str) -> impl Iterator<Item = (String, String, String)> + '_ {
    crypttab
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (name, device) = (fields.first()?, fields.get(1)?);
            let options = fields.get(3).copied().unwrap_or_default();
            Some((name.to_string(), device.to_string(), options.to_owned()))
        })
}

/// The strings in the array `value`, or none when it isn't one.
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The PCRs `unlock` names, as systemd-cryptenroll takes them, or 7 when it names none.
fn pcrs(unlock: &Value) -> String {
    let named: Vec<String> = unlock["pcrs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .map(|pcr| pcr.to_string())
        .collect();
    if named.is_empty() {
        "7".to_owned()
    } else {
        named.join("+")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_disks_the_tpm_unlocks_are_found_in_crypttab() {
        let crypttab = "# a comment tpm2-device=auto\n\
             main0-system /dev/disk/by-partlabel/main0-system none luks,discard,tpm2-device=auto\n\
             data /dev/disk/by-partlabel/data none luks,discard\n";
        assert_eq!(
            tpm_volumes(crypttab),
            [(
                "main0-system".to_owned(),
                "/dev/disk/by-partlabel/main0-system".to_owned()
            )]
        );
    }

    #[test]
    fn the_pcrs_come_from_the_model_or_are_the_secure_boot_state() {
        assert_eq!(pcrs(&json!({"pcrs": [7, 15]})), "7+15");
        assert_eq!(pcrs(&json!({})), "7");
    }

    #[test]
    fn with_secure_boot_the_disks_enroll_to_the_signed_policy_once_the_firmware_has_it_on() {
        let root = tempfile::tempdir().unwrap();
        let refused = signed_policy(root.path()).unwrap_err();
        assert!(refused.contains("no switch has signed"), "{refused}");

        let key = root.path().join(crate::secure_boot::PCR_PUBLIC_KEY);
        fs::create_dir_all(key.parent().unwrap()).unwrap();
        fs::write(&key, "key").unwrap();
        let state = root.path().join(crate::secure_boot::SECURE_BOOT_STATE);
        fs::create_dir_all(state.parent().unwrap()).unwrap();
        let refused = signed_policy(root.path()).unwrap_err();
        assert!(
            refused.contains("cannot tell whether Secure Boot is on"),
            "{refused}"
        );

        fs::write(&state, [6, 0, 0, 0, 0]).unwrap();
        let refused = signed_policy(root.path()).unwrap_err();
        assert!(
            refused.contains("isn't on in the firmware yet"),
            "{refused}"
        );

        fs::write(&state, [6, 0, 0, 0, 1]).unwrap();
        assert_eq!(
            signed_policy(root.path()).unwrap(),
            [
                format!("--tpm2-public-key={}", key.display()),
                "--tpm2-public-key-pcrs=11".to_owned()
            ]
        );
    }
}
