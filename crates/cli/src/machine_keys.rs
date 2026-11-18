//! A machine's keys, made before it's installed and kept encrypted in its configuration under
//! `secrets/machines/NAME/`: each once to the configuration's recipients, for whoever has one of
//! their keys, and once to a passphrase, for installing where none of those keys is, such as at
//! the machine's console. Install decrypts them into the new system, so its host key is known
//! before it first boots and secrets encrypted to it work from the start.

use std::path::{Path, PathBuf};

use age::secrecy::SecretString;

/// Each key a machine may have made ahead, and where install puts it in the new system.
pub const KEYS: [(&str, &str); 2] = [
    ("ssh_host_ed25519_key", "etc/ssh/ssh_host_ed25519_key"),
    ("initrd_ed25519_key", "etc/ssh/initrd_ed25519_key"),
];

/// The SSH host key, the one `wrath secret machine` makes.
pub const HOST_KEY: &str = KEYS[0].0;

/// Where the configuration at `config` keeps machine `name`'s keys.
pub fn dir(config: &Path, name: &str) -> PathBuf {
    config.join("secrets/machines").join(name)
}

/// `key` encrypted to the configuration's recipients.
pub fn to_recipients(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.age"))
}

/// `key` encrypted to a passphrase.
pub fn to_passphrase(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.passphrase.age"))
}

/// How hard a passphrase is made to guess, as scrypt's log2 of its cost: about a second on a
/// laptop. Fixed, as age would otherwise pick it by how fast the machine encrypting is, and a
/// slower one installing would then refuse to decrypt.
const WORK: u8 = 18;

/// The most work a passphrase may ask of whoever decrypts, which also accepts what `age
/// --passphrase` made on a fast machine.
const MOST_WORK: u8 = 20;

/// `plaintext` encrypted to `passphrase`, as `age --passphrase` does.
pub fn lock(plaintext: &[u8], passphrase: &str) -> Result<Vec<u8>, String> {
    let mut recipient = age::scrypt::Recipient::new(SecretString::from(passphrase.to_owned()));
    recipient.set_work_factor(WORK);
    age::encrypt(&recipient, plaintext).map_err(|e| format!("cannot encrypt: {e}"))
}

/// What `lock` encrypted to `passphrase`, or `None` when it's the wrong passphrase.
pub fn unlock(ciphertext: &[u8], passphrase: &str) -> Result<Option<Vec<u8>>, String> {
    let mut identity = age::scrypt::Identity::new(SecretString::from(passphrase.to_owned()));
    identity.set_max_work_factor(MOST_WORK);
    match age::decrypt(&identity, ciphertext) {
        Ok(plaintext) => Ok(Some(plaintext)),
        Err(age::DecryptError::DecryptionFailed | age::DecryptError::NoMatchingKeys) => Ok(None),
        Err(e) => Err(format!("cannot decrypt: {e}")),
    }
}

/// A new passphrase, asked for twice on the terminal so a typo can't lock anything away.
pub fn new_passphrase(what: &str) -> Result<String, String> {
    use crate::host::read_password as ask;
    let passphrase = ask(&format!("Passphrase for {what}: "))?;
    if passphrase.is_empty() || passphrase != ask("The same passphrase again: ")? {
        return Err("the passphrases were empty or didn't match".into());
    }
    Ok(passphrase)
}

/// The contents of a passphrase file, without the newline an editor or `echo` leaves.
pub fn read_passphrase(file: &Path) -> Result<String, String> {
    std::fs::read_to_string(file)
        .map(|text| text.trim_end_matches('\n').to_owned())
        .map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_a_passphrase_locks_only_that_passphrase_unlocks() {
        let locked = lock(b"key", "horse battery").unwrap();
        assert_eq!(
            unlock(&locked, "horse battery").unwrap().as_deref(),
            Some(&b"key"[..])
        );
        assert_eq!(unlock(&locked, "staple").unwrap(), None);
        assert!(unlock(b"not age", "staple").is_err());
    }

    #[test]
    fn a_machines_keys_are_beside_each_other_under_its_name() {
        let dir = dir(Path::new("/c"), "framework");
        assert_eq!(dir, Path::new("/c/secrets/machines/framework"));
        assert_eq!(
            to_recipients(&dir, HOST_KEY),
            Path::new("/c/secrets/machines/framework/ssh_host_ed25519_key.age")
        );
        assert_eq!(
            to_passphrase(&dir, HOST_KEY),
            Path::new("/c/secrets/machines/framework/ssh_host_ed25519_key.passphrase.age")
        );
    }
}
