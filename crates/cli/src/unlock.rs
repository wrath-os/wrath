//! How a machine's encrypted disks unlock: enrolling its TPM again, for when what the TPM checks
//! changed, such as the first boot of a unified kernel image or a firmware update.

use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

use serde_json::Value;

#[derive(clap::Subcommand)]
pub enum UnlockCommand {
    /// Let the TPM unlock the disks again, as the machine boots now: it asks for the disks'
    /// passphrase and replaces their TPM key, after the boot measured differently and asked
    Enroll,
}

/// The running system's model, which says which PCRs unlocking depends on.
const MODEL: &str = "/run/system/etc/wrath/model.json";

pub fn run(command: UnlockCommand) -> Result<ExitCode, String> {
    match command {
        UnlockCommand::Enroll => enroll(Path::new("/")),
    }
}

/// Replaces the TPM key of every disk the machine at `root` unlocks with its TPM by one sealed to
/// the PCRs as they are now, the passphrase unlocking it for that.
fn enroll(root: &Path) -> Result<ExitCode, String> {
    let crypttab = fs::read_to_string(root.join("etc/crypttab")).unwrap_or_default();
    let devices = tpm_devices(&crypttab);
    if devices.is_empty() {
        return Err("no disk here unlocks with the TPM; set `unlock.tpm2 = true` first".to_owned());
    }
    let model = fs::read_to_string(root.join(MODEL.trim_start_matches('/')))
        .map_err(|e| format!("{MODEL}: {e}"))?;
    let model: Value = serde_json::from_str(&model).map_err(|e| format!("{MODEL}: {e}"))?;
    let mut policy = vec![
        "--tpm2-device=auto".to_owned(),
        format!("--tpm2-pcrs={}", pcrs(&model["base"]["storage"]["unlock"])),
    ];
    if model["base"]["boot"]["secure_boot"] == Value::Bool(true) {
        policy.extend(signed_policy(root)?);
    }
    let policy: Vec<&str> = policy.iter().map(String::as_str).collect();
    for device in devices {
        println!("letting the TPM unlock {device} again, as it boots now");
        // The old key goes first: systemd-cryptenroll leaves a key for the same PCRs alone, even
        // when it's sealed to values they no longer have. The passphrase stays.
        cryptenroll(&device, &["--wipe-slot=tpm2"])?;
        cryptenroll(&device, &policy)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The firmware's Secure Boot state: its last byte is 1 when it's on.
const SECURE_BOOT_STATE: &str =
    "sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";

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
    let on = fs::read(root.join(SECURE_BOOT_STATE)).is_ok_and(|state| state.last() == Some(&1));
    if !on {
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

fn cryptenroll(device: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new("systemd-cryptenroll")
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

/// The devices in `crypttab` that unlock with the TPM.
fn tpm_devices(crypttab: &str) -> Vec<String> {
    crypttab
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let options = fields.get(3)?;
            options
                .split(',')
                .any(|option| option.starts_with("tpm2-device="))
                .then(|| fields[1].to_owned())
        })
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
            tpm_devices(crypttab),
            ["/dev/disk/by-partlabel/main0-system"]
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
        let state = root.path().join(SECURE_BOOT_STATE);
        fs::create_dir_all(state.parent().unwrap()).unwrap();
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
