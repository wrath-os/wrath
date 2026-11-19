//! Each encrypted disk's volume key, pinned by its id so the initrd attaches only the volume the
//! machine made: a volume swapped in with the same label and UUID, which the TPM would hand its
//! key to all the same, is refused (systemd's `fixate-volume-key=`). The ids are the machine's
//! own while crypttab is the system's, so the switch writes them into each image it signs.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Each volume's name and id, one per line, under the root. Kept with an erased root.
pub const VOLUME_KEYS: &str = "var/lib/wrath/volume-keys";

/// systemd's id for the volume key of volume `name` with LUKS UUID `uuid`: HMAC-SHA256 keyed with
/// the volume key over `cryptsetup:NAME:UUID`, with `:` in the name escaped.
fn key_id(volume_key: &[u8], name: &str, uuid: &str) -> String {
    let escaped: String = name
        .chars()
        .map(|c| match c {
            ':' | '\\' | '\0'..=' ' | '\x7f' => format!("\\x{:02x}", c as u32),
            c => c.to_string(),
        })
        .collect();
    let mut mac = Hmac::<Sha256>::new_from_slice(volume_key).expect("HMAC takes any key");
    mac.update(format!("cryptsetup:{escaped}:{uuid}").as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The id of the volume key of `device`, attached as `name`, which `key_file` unlocks. The key
/// goes through a file only root reads under `work`, removed at once.
pub fn id_of(device: &str, name: &str, key_file: &Path, work: &Path) -> Result<String, String> {
    let output = |command: &mut Command| {
        let out = command
            .output()
            .map_err(|e| format!("cannot run cryptsetup: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "cryptsetup could not read {device}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    let uuid = output(Command::new("cryptsetup").arg("luksUUID").arg(device))?;
    let dir = tempfile::Builder::new()
        .prefix("wrath-volume-key")
        .tempdir_in(work)
        .map_err(|e| format!("cannot make a directory under {}: {e}", work.display()))?;
    let file = dir.path().join("key");
    output(
        Command::new("cryptsetup")
            .args(["luksDump", "--batch-mode", "--dump-volume-key"])
            .arg("--volume-key-file")
            .arg(&file)
            .arg("--key-file")
            .arg(key_file)
            .arg(device),
    )?;
    let key = fs::read(&file).map_err(|e| format!("{}: {e}", file.display()));
    let _ = fs::remove_file(&file);
    Ok(key_id(&key?, name, &uuid))
}

/// The ids recorded at `root`, by volume name.
pub fn recorded(root: &Path) -> BTreeMap<String, String> {
    fs::read_to_string(root.join(VOLUME_KEYS))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(name, id)| (name.to_owned(), id.to_owned()))
        .collect()
}

/// Records `ids` at `root`, along with those of other volumes recorded before.
pub fn record(root: &Path, ids: &BTreeMap<String, String>) -> Result<(), String> {
    let mut all = recorded(root);
    all.extend(ids.clone());
    let file = root.join(VOLUME_KEYS);
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let text: String = all
        .iter()
        .map(|(name, id)| format!("{name} {id}\n"))
        .collect();
    fs::write(&file, text).map_err(|e| format!("{}: {e}", file.display()))
}

/// `crypttab` with each volume that has an id in `ids` attached only with that volume key.
pub fn fixated(crypttab: &str, ids: &BTreeMap<String, String>) -> String {
    crypttab
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if let [name, device, key, options, ..] = fields.as_slice()
                && !name.starts_with('#')
                && let Some(id) = ids.get(*name)
            {
                return format!("{name} {device} {key} {options},fixate-volume-key={id}\n");
            }
            format!("{line}\n")
        })
        .collect()
}

/// An archive with `system`'s crypttab pinned to `ids`, for the kernel to unpack over the initrd
/// it was made for, when `ids` pins any volume of it.
pub fn initrd_crypttab(system: &Path, ids: &BTreeMap<String, String>) -> Option<Vec<u8>> {
    use wrath_build::initrd::{Node, Nodes, archive, insert};
    let crypttab = fs::read_to_string(system.join("etc/crypttab")).ok()?;
    let pinned = fixated(&crypttab, ids);
    if pinned == crypttab {
        return None;
    }
    let mut nodes = Nodes::new();
    let bytes = pinned.into_bytes();
    insert(
        &mut nodes,
        "etc/crypttab",
        Node::File { mode: 0o644, bytes },
    );
    Some(archive(&nodes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_volume_key_id_is_the_one_systemd_works_out() {
        // printf 'cryptsetup:main0-system:1234' | openssl dgst -sha256 -hmac secret
        assert_eq!(
            key_id(b"secret", "main0-system", "1234"),
            "c06e3d59603a8502a62e7bea4df1d7d5aeaaf2eb67642cf6f081ddb6e75c2c37"
        );
        assert_ne!(
            key_id(b"secret", "a:b", "1234"),
            key_id(b"secret", "a", "b:1234"),
            "a colon in the name is escaped"
        );
    }

    #[test]
    fn recorded_ids_pin_their_volumes_in_crypttab_and_others_stay() {
        let root = tempfile::tempdir().unwrap();
        assert!(recorded(root.path()).is_empty());
        let ids = BTreeMap::from([("main0-system".to_owned(), "abc".to_owned())]);
        record(root.path(), &ids).unwrap();
        record(
            root.path(),
            &BTreeMap::from([("data".to_owned(), "def".to_owned())]),
        )
        .unwrap();
        assert_eq!(recorded(root.path()).len(), 2);

        let crypttab = "# main0-system x none luks\n\
             main0-system /dev/disk/by-partlabel/main0-system none luks,discard\n\
             other /dev/disk/by-partlabel/other none luks\n";
        assert_eq!(
            fixated(crypttab, &recorded(root.path())),
            "# main0-system x none luks\n\
             main0-system /dev/disk/by-partlabel/main0-system none \
             luks,discard,fixate-volume-key=abc\n\
             other /dev/disk/by-partlabel/other none luks\n"
        );
    }

    #[test]
    fn an_initrd_gets_a_crypttab_only_when_a_volume_of_it_is_pinned() {
        let system = tempfile::tempdir().unwrap();
        let ids = BTreeMap::from([("main0-system".to_owned(), "abc".to_owned())]);
        assert_eq!(initrd_crypttab(system.path(), &ids), None);
        fs::create_dir(system.path().join("etc")).unwrap();
        let crypttab = system.path().join("etc/crypttab");
        fs::write(&crypttab, "data /dev/disk/by-partlabel/data none luks\n").unwrap();
        assert_eq!(initrd_crypttab(system.path(), &ids), None);

        fs::write(&crypttab, "main0-system /dev/vda2 none luks\n").unwrap();
        let archive = initrd_crypttab(system.path(), &ids).unwrap();
        let text = String::from_utf8_lossy(&archive);
        assert!(text.starts_with("070701"), "{text}");
        assert!(
            text.contains("etc/crypttab\0") && text.contains("luks,fixate-volume-key=abc\n"),
            "{text}"
        );
    }
}
