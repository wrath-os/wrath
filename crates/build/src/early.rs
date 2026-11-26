//! What a system's disks need, from the `storage` the disks module renders into the model: the
//! fstab and crypttab the system mounts and unlocks with, and the part of them the initrd needs,
//! the root, the store and where a root erased at every boot keeps things, mounted under
//! `/sysroot` before it switches.

use serde_json::Value;

/// The disks of a system, as its files and its initrd's.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Disks {
    /// The system's `/etc/fstab`: every filesystem and swap.
    pub fstab: String,
    /// `/etc/crypttab`, for the system and the initrd alike.
    pub crypttab: String,
    /// The initrd's `/etc/fstab`, under `/sysroot`.
    pub initrd_fstab: String,
    /// Kernel modules the initrd loads.
    pub modules: Vec<String>,
    /// SSH in the initrd, to unlock the disks from elsewhere.
    pub ssh: Option<Ssh>,
}

/// SSH in the initrd: the port it listens on and the keys that may log in.
#[derive(Debug, PartialEq, Eq)]
pub struct Ssh {
    pub port: u16,
    pub authorized_keys: Vec<String>,
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

fn items<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map_or(&[], Vec::as_slice)
}

/// The disks `storage` describes. A system without disks, such as one on a USB stick found by
/// its label, has none, and its initrd mounts the root the kernel's `root=` names.
pub fn disks(storage: &Value) -> Disks {
    let partitions = items(storage, "partitions");
    if partitions.is_empty() {
        return Disks::default();
    }
    let unlock = &storage["unlock"];
    let tpm = unlock["tpm2"].as_bool().unwrap_or(false);
    let keys: Vec<String> = items(&unlock["ssh"], "authorized_keys")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let ssh = !keys.is_empty();
    // Members of a filesystem that boots with one missing, past the first, may be absent.
    let optional: Vec<&str> = items(storage, "spanning")
        .iter()
        .filter(|fs| text(fs, "degraded") == "boot")
        .flat_map(|fs| items(fs, "members").iter().skip(1))
        .filter_map(Value::as_str)
        .map(|member| member.trim_start_matches("/dev/mapper/"))
        .collect();
    let encrypted: Vec<&str> = partitions
        .iter()
        .filter(|partition| partition["encrypt"].as_bool().unwrap_or(false))
        .map(|partition| text(partition, "label"))
        .collect();
    let crypttab: String = encrypted
        .iter()
        .map(|label| {
            let mut options = vec!["luks", "discard"];
            if optional.contains(label) {
                options.push("nofail");
            }
            if tpm {
                options.push("tpm2-device=auto");
            }
            let label = field(label);
            format!(
                "{label} /dev/disk/by-partlabel/{label} none {}\n",
                options.join(",")
            )
        })
        .collect();

    let mounts = items(storage, "mounts");
    let state = text(storage, "state");
    let line = |mount: &Value, prefix: &str| {
        let path = text(mount, "path");
        let format = text(mount, "format");
        let mut options: Vec<&str> = items(mount, "options")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        // A root in memory is ready at once. Holding it back until the disks are unlocked keeps
        // the initrd from moving on while someone types the passphrase.
        if !prefix.is_empty() && format == "tmpfs" && !encrypted.is_empty() {
            options.push("x-systemd.after=cryptsetup.target");
        }
        let options = if options.is_empty() {
            "defaults".to_owned()
        } else {
            options.join(",")
        };
        let at = match (path, prefix) {
            ("/", "") => "/".to_owned(),
            ("/", prefix) => prefix.to_owned(),
            (path, prefix) => format!("{prefix}{path}"),
        };
        // A FAT filesystem, such as the ESP, is checked before it's mounted: the firmware writes to
        // it at boot and can leave it marked as not cleanly unmounted.
        let pass = if format == "vfat" { 2 } else { 0 };
        format!(
            "{} {} {format} {options} 0 {pass}\n",
            field(text(mount, "device")),
            field(&at)
        )
    };
    let (swaps, filesystems): (Vec<&Value>, Vec<&Value>) = mounts
        .iter()
        .partition(|mount| text(mount, "format") == "swap");
    let early: Vec<&Value> = filesystems
        .iter()
        .copied()
        .filter(|mount| {
            let path = text(mount, "path");
            path == "/" || path == "/wrath" || path.starts_with("/wrath/") || path == state
        })
        .collect();
    let mut fstab: String = filesystems.iter().map(|mount| line(mount, "")).collect();
    for swap in swaps {
        fstab.push_str(&format!(
            "{} none swap defaults 0 0\n",
            field(text(swap, "device"))
        ));
    }

    let mut modules: Vec<&str> = Vec::new();
    if !encrypted.is_empty() {
        // device-mapper, and the ciphers and hashes LUKS containers use.
        modules.extend([
            "dm_crypt",
            "dm_mod",
            "aes",
            "aesni_intel",
            "xts",
            "cbc",
            "sha256",
            "sha512",
            "cryptd",
            "af_alg",
            "algif_skcipher",
        ]);
    }
    // The TPM unlocks the disks, or the credentials SSH in the initrd starts with.
    if tpm || ssh {
        modules.extend(["tpm_crb", "tpm_tis"]);
    }
    // Drivers for the disks themselves, on real machines and virtual ones.
    modules.extend([
        "ahci",
        "nvme",
        "sd_mod",
        "usb_storage",
        "uas",
        "xhci_pci",
        "virtio_pci",
        "virtio_blk",
        "virtio_scsi",
    ]);
    if ssh {
        // Packet sockets, which DHCP uses, and network drivers, for unlocking over SSH.
        modules.extend([
            "af_packet",
            "virtio_net",
            "e1000e",
            "igb",
            "igc",
            "ixgbe",
            "r8169",
            "tg3",
            "bnxt_en",
            "mlx5_core",
        ]);
    }
    for mount in &early {
        let format = text(mount, "format");
        if !modules.contains(&format) {
            modules.push(format);
        }
    }
    // btrfs checksums with crc32c, which it loads by name.
    if filesystems
        .iter()
        .any(|mount| text(mount, "format") == "btrfs")
    {
        modules.extend(["crc32c", "crc32c_generic"]);
    }

    Disks {
        fstab,
        crypttab,
        initrd_fstab: early.iter().map(|mount| line(mount, "/sysroot")).collect(),
        modules: modules.into_iter().map(str::to_owned).collect(),
        ssh: ssh.then(|| Ssh {
            port: unlock["ssh"]["port"]
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .unwrap_or(2222),
            authorized_keys: keys,
        }),
    }
}

/// A field of fstab or crypttab, which separate fields with spaces and tabs, with those and the
/// backslash written as their octal escapes, as both files expect.
fn field(text: &str) -> String {
    text.replace('\\', "\\134")
        .replace(' ', "\\040")
        .replace('\t', "\\011")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_field_with_spaces_keeps_its_place_in_fstab() {
        assert_eq!(field("/mnt/my disk"), "/mnt/my\\040disk");
        assert_eq!(field("a\tb\\c"), "a\\011b\\134c");
        assert_eq!(field("/wrath/store"), "/wrath/store");
    }

    /// A root erased at every boot on one encrypted disk, as in the install scenario.
    fn storage() -> Value {
        json!({
            "mounts": [
                {"device": "none", "format": "tmpfs", "options": ["mode=755", "size=1G"],
                 "path": "/"},
                {"device": "/dev/disk/by-partlabel/main0-esp", "format": "vfat", "options": [],
                 "path": "/boot"},
                {"device": "/dev/mapper/main0-system", "format": "btrfs",
                 "options": ["subvol=@keep"], "path": "/keep"},
                {"device": "/dev/mapper/main0-system", "format": "btrfs",
                 "options": ["subvol=@wrath"], "path": "/wrath"},
            ],
            "partitions": [
                {"device": 0, "disk": "main", "encrypt": false, "label": "main0-esp"},
                {"device": 0, "disk": "main", "encrypt": true, "label": "main0-system"},
            ],
            "spanning": [{"degraded": "stop", "members": ["/dev/mapper/main0-system"]}],
            "state": "/keep",
            "unlock": {"ssh": {"authorized_keys": []}, "tpm2": false},
        })
    }

    #[test]
    fn a_system_without_disks_has_nothing_to_unlock_or_mount_early() {
        assert_eq!(disks(&json!({})), Disks::default());
        assert_eq!(disks(&Value::Null), Disks::default());
    }

    #[test]
    fn encrypted_partitions_are_unlocked_by_their_label() {
        assert_eq!(
            disks(&storage()).crypttab,
            "main0-system /dev/disk/by-partlabel/main0-system none luks,discard\n"
        );
        let mut tpm = storage();
        tpm["unlock"]["tpm2"] = json!(true);
        assert!(
            disks(&tpm)
                .crypttab
                .ends_with(" none luks,discard,tpm2-device=auto\n")
        );
    }

    #[test]
    fn the_initrd_mounts_the_root_the_store_and_the_state_under_sysroot() {
        assert_eq!(
            disks(&storage()).initrd_fstab,
            "none /sysroot tmpfs mode=755,size=1G,x-systemd.after=cryptsetup.target 0 0\n\
             /dev/mapper/main0-system /sysroot/keep btrfs subvol=@keep 0 0\n\
             /dev/mapper/main0-system /sysroot/wrath btrfs subvol=@wrath 0 0\n"
        );
    }

    #[test]
    fn the_system_mounts_every_filesystem_and_swap() {
        let mut storage = storage();
        storage["mounts"].as_array_mut().unwrap().push(
            json!({"device": "/dev/mapper/swap", "format": "swap", "options": [],
                         "path": ""}),
        );
        let fstab = disks(&storage).fstab;
        assert!(
            fstab.starts_with("none / tmpfs mode=755,size=1G 0 0\n"),
            "{fstab}"
        );
        assert!(fstab.contains("/dev/disk/by-partlabel/main0-esp /boot vfat defaults 0 2\n"));
        assert!(
            fstab.ends_with("/dev/mapper/swap none swap defaults 0 0\n"),
            "{fstab}"
        );
    }

    #[test]
    fn the_initrd_loads_what_unlocks_and_mounts_the_disks() {
        let modules = disks(&storage()).modules;
        for module in ["dm_crypt", "nvme", "virtio_blk", "tmpfs", "btrfs", "crc32c"] {
            assert!(
                modules.iter().any(|m| m == module),
                "{module} in {modules:?}"
            );
        }
        assert!(!modules.iter().any(|m| m == "tpm_crb"));
    }

    #[test]
    fn a_member_that_may_be_missing_doesnt_hold_up_the_boot() {
        let mut storage = storage();
        storage["partitions"]
            .as_array_mut()
            .unwrap()
            .push(json!({"encrypt": true, "label": "main1-system"}));
        storage["spanning"] = json!([{"degraded": "boot",
            "members": ["/dev/mapper/main0-system", "/dev/mapper/main1-system"]}]);
        let crypttab = disks(&storage).crypttab;
        assert!(
            crypttab.contains("main0-system none luks,discard\n"),
            "{crypttab}"
        );
        assert!(
            crypttab.contains("main1-system none luks,discard,nofail\n"),
            "{crypttab}"
        );
    }

    #[test]
    fn keys_that_may_log_in_bring_ssh_to_the_initrd_with_the_network_and_the_tpm() {
        assert_eq!(disks(&storage()).ssh, None);
        let mut ssh = storage();
        ssh["unlock"]["ssh"] = json!({"port": 2022, "authorized_keys": ["ssh-ed25519 AAAA me"]});
        let found = disks(&ssh);
        assert_eq!(
            found.ssh,
            Some(Ssh {
                port: 2022,
                authorized_keys: vec!["ssh-ed25519 AAAA me".to_owned()],
            })
        );
        for module in ["af_packet", "virtio_net", "tpm_crb"] {
            assert!(found.modules.iter().any(|m| m == module), "{module}");
        }
    }
}
