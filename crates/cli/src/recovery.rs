//! Recovery keys for a machine's encrypted disks: each made by systemd-cryptenroll, which unlocks
//! its disk wherever a passphrase is asked for, and kept where `unlock.recovery` says.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Where on the ESP the recovery keys are kept, encrypted to the admins.
pub const ON_ESP: &str = "wrath/recovery-keys.age";

/// The SSH host key's public half on a machine, which the admins are told apart from.
const HOST_PUBLIC: &str = "etc/ssh/ssh_host_ed25519_key.pub";

/// Where a machine's recovery keys go, from `unlock.recovery`.
#[derive(Debug, PartialEq)]
pub enum Keep {
    /// Encrypted to the admins on the ESP, and shown once.
    Esp,
    /// Shown once.
    Print,
    /// Handed to a program on its standard input.
    Command(Vec<String>),
    /// No recovery key.
    None,
}

impl Keep {
    /// `keep` and `command` as the option has them: a command, when there is one, takes the keys
    /// instead.
    pub fn of(keep: &str, command: Vec<String>) -> Self {
        match keep {
            _ if !command.is_empty() => Self::Command(command),
            "print" => Self::Print,
            "none" => Self::None,
            _ => Self::Esp,
        }
    }
}

/// The machine's recovery keys are kept for: who they're encrypted to and where.
pub struct Machine<'a> {
    /// The machine's name, a command's argument.
    pub name: &'a str,
    /// The machine's root, where its host key is.
    pub root: &'a Path,
    /// The machine's ESP, mounted, when it has one.
    pub esp: Option<&'a Path>,
    /// `system.secret_recipients`.
    pub recipients: &'a [String],
}

/// Makes a new recovery key for `device`, unlocked with the passphrase in `key_file`, replacing
/// any it had, and gives it.
pub fn enroll(device: &str, key_file: &Path) -> Result<String, String> {
    let output = Command::new("systemd-cryptenroll")
        .args(["--recovery-key", "--wipe-slot=recovery"])
        .arg(format!("--unlock-key-file={}", key_file.display()))
        .arg(device)
        .env("SYSTEMD_COLORS", "0")
        .env("SYSTEMD_EMOJI", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run systemd-cryptenroll: {e}"))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(format!(
            "cannot make a recovery key for {device}: {}",
            said.trim()
        ));
    }
    find_key(&said).ok_or_else(|| {
        format!("systemd-cryptenroll made a recovery key for {device} but didn't show it")
    })
}

/// systemd's recovery key in what it printed: eight groups of eight modhex characters.
fn find_key(said: &str) -> Option<String> {
    const MODHEX: &str = "cbdefghijklnrtuv";
    said.split_whitespace()
        .find(|word| {
            let groups: Vec<&str> = word.split('-').collect();
            groups.len() == 8
                && groups
                    .iter()
                    .all(|group| group.len() == 8 && group.chars().all(|c| MODHEX.contains(c)))
        })
        .map(str::to_owned)
}

/// Keeps `keys`, each volume's name with its recovery key, as `keep` says.
pub fn keep(keys: &BTreeMap<String, String>, keep: &Keep, machine: &Machine) -> Result<(), String> {
    let lines: String = keys
        .iter()
        .map(|(name, key)| format!("{name} {key}\n"))
        .collect();
    match keep {
        Keep::None => Ok(()),
        Keep::Print => {
            show(keys);
            Ok(())
        }
        Keep::Command(command) => hand_over(command, machine.name, &lines),
        Keep::Esp => {
            let admins = admins(machine);
            match machine.esp {
                Some(esp) if !admins.is_empty() => {
                    encrypt(&admins, lines.as_bytes(), &esp.join(ON_ESP))?;
                    println!(
                        "kept the recovery keys on the ESP, encrypted to {} admin key(s); \
                         `wrath unlock recovery-key --from DIR` reads them",
                        admins.len()
                    );
                }
                Some(_) => crate::style::note(
                    "nobody but the machine is among `system.secret_recipients`, so the recovery \
                     keys are only shown",
                ),
                None => {
                    crate::style::note("no ESP to keep the recovery keys on; they're only shown")
                }
            }
            show(keys);
            Ok(())
        }
    }
}

/// `system.secret_recipients` but the machine's own host key.
fn admins(machine: &Machine) -> Vec<String> {
    let host = std::fs::read_to_string(machine.root.join(HOST_PUBLIC)).unwrap_or_default();
    let host = key_of(&host);
    machine
        .recipients
        .iter()
        .filter(|recipient| host.is_empty() || key_of(recipient) != host)
        .cloned()
        .collect()
}

/// A public key's type and key, without its comment.
fn key_of(public: &str) -> String {
    public
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Prints `keys`, each volume's name with its recovery key.
fn show(keys: &BTreeMap<String, String>) {
    println!(
        "recovery keys, each of which unlocks its disk wherever a passphrase is asked for; keep \
         them somewhere safe:"
    );
    for (name, key) in keys {
        println!("  {name}  {key}");
    }
}

/// Runs `command` with the machine's name as its last argument, `lines` on its standard input.
fn hand_over(command: &[String], name: &str, lines: &str) -> Result<(), String> {
    let (program, args) = command.split_first().expect("a command has a program");
    let mut child = Command::new(program)
        .args(args)
        .arg(name)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {program}, which keeps the recovery keys: {e}"))?;
    let written = child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(lines.as_bytes());
    let status = child.wait().map_err(|e| format!("{program}: {e}"))?;
    if written.is_err() || !status.success() {
        return Err(format!("{program} didn't take the recovery keys"));
    }
    println!("handed the recovery keys to {program}");
    Ok(())
}

/// Encrypts `plaintext` to `recipients` into `file`, replacing it in one step.
fn encrypt(recipients: &[String], plaintext: &[u8], file: &Path) -> Result<(), String> {
    let dir = file.parent().expect("in a directory");
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let partial = file.with_extension("partial");
    let mut command = Command::new("age");
    command.arg("--encrypt").arg("--output").arg(&partial);
    for recipient in recipients {
        command.arg("--recipient").arg(recipient);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run age: {e}"))?;
    let written = child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(plaintext);
    let status = child.wait().map_err(|e| format!("age: {e}"))?;
    if written.is_err() || !status.success() {
        return Err(format!("cannot encrypt {}", file.display()));
    }
    std::fs::rename(&partial, file).map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_found_in_what_systemd_printed() {
        let key = "fhvjkrbg-icelbbvk-ulretndv-kgtfnnjr-trbkiucg-bndfnrtj-cgbbiilt-trccnlhd";
        let said = format!(
            "A secret recovery key has been generated for this volume:\n\n    {key}\n\n\
             Please save this secret recovery key at a secure location.\n\
             New recovery key enrolled as key slot 2.\n"
        );
        assert_eq!(find_key(&said).as_deref(), Some(key));
        assert_eq!(find_key("no key-here-at-all"), None);
    }

    #[test]
    fn a_command_takes_the_keys_instead_of_where_keep_says() {
        assert_eq!(Keep::of("esp", vec![]), Keep::Esp);
        assert_eq!(Keep::of("print", vec![]), Keep::Print);
        assert_eq!(Keep::of("none", vec![]), Keep::None);
        assert_eq!(
            Keep::of("none", vec!["store".into()]),
            Keep::Command(vec!["store".into()])
        );
    }

    #[test]
    fn the_admins_are_the_recipients_but_the_machine() {
        let root = tempfile::tempdir().unwrap();
        let recipients = [
            "ssh-ed25519 AAAAhost".to_owned(),
            "ssh-ed25519 AAAAme me".to_owned(),
        ];
        let machine = Machine {
            name: "t",
            root: root.path(),
            esp: None,
            recipients: &recipients,
        };
        assert_eq!(admins(&machine).len(), 2);
        let host = root.path().join(HOST_PUBLIC);
        std::fs::create_dir_all(host.parent().unwrap()).unwrap();
        std::fs::write(&host, "ssh-ed25519 AAAAhost root@t\n").unwrap();
        assert_eq!(admins(&machine), ["ssh-ed25519 AAAAme me"]);
    }

    #[test]
    fn a_command_gets_the_keys_and_the_machines_name() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let command = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            format!("{{ echo \"$0\"; cat; }} > {}", out.display()),
        ];
        hand_over(&command, "t-05", "main0-system abc\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "t-05\nmain0-system abc\n"
        );
    }
}
