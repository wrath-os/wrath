//! `wrath secret`: editing secrets and encrypting them to the configuration's recipients with
//! `age`. Decrypted contents only ever exist in memory and in the file the editor opens, which is
//! in a private temporary directory, preferably on the user's runtime tmpfs.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use wrath_eval::{ConfigFs, DirFs, Value};

use crate::fetch::Sourced;
use crate::system;

#[derive(Debug, clap::Subcommand)]
pub enum SecretCommand {
    /// Open a secret in $EDITOR and encrypt it again, or create it
    Edit {
        /// The secret, as declared under `[secrets.<name>]`
        name: String,
        #[command(flatten)]
        options: Options,
    },
    /// Encrypt every secret again to the current recipients, such as after adding a machine
    Rekey {
        #[command(flatten)]
        options: Options,
    },
    /// Make the keys a machine doesn't have yet, kept encrypted in `secrets/machines/NAME`: its SSH
    /// host key, made before installing it so its secrets decrypt from its first boot, and its
    /// Secure Boot keys
    Machine {
        /// The machine, as `wrath install --name` or its hostname calls it
        name: String,
        /// A file with the passphrase install asks for, instead of asking for it here
        #[arg(long, value_name = "FILE")]
        passphrase_file: Option<PathBuf>,
    },
}

#[derive(Debug, clap::Args)]
pub struct Options {
    /// A private key that decrypts the secrets [default: ~/.ssh/id_ed25519]
    #[arg(long)]
    identity: Vec<PathBuf>,
}

pub fn run(command: SecretCommand, config: &Path, modules: &Path) -> Result<ExitCode, String> {
    match command {
        SecretCommand::Edit { name, options } => {
            let secrets = Secrets::load(config, modules)?;
            secrets.edit(&name, &identities(options.identity)?)
        }
        SecretCommand::Rekey { options } => {
            let secrets = Secrets::load(config, modules)?;
            secrets.rekey(&identities(options.identity)?)
        }
        SecretCommand::Machine {
            name,
            passphrase_file,
        } => {
            let secrets = Secrets::load(config, modules)?;
            secrets.make_machine_keys(config, &name, passphrase_file.as_deref())
        }
    }
}

/// The identities given, or the user's SSH key when there are none.
fn identities(given: Vec<PathBuf>) -> Result<Vec<PathBuf>, String> {
    if !given.is_empty() {
        return Ok(given);
    }
    let home = std::env::var_os("HOME").ok_or("no --identity given and HOME is not set")?;
    Ok(vec![Path::new(&home).join(".ssh/id_ed25519")])
}

/// The configuration's secrets and who they are encrypted to.
struct Secrets {
    recipients: Vec<String>,
    /// Each secret's encrypted file.
    files: BTreeMap<String, PathBuf>,
}

impl Secrets {
    fn load(dir: &Path, modules: &Path) -> Result<Self, String> {
        let config = Editing(Sourced::dir(dir));
        let evaluation = system::evaluate(&config, modules, crate::role(dir))?;
        let recipients = match evaluation.get("system.secret_recipients") {
            Some(Value::List(items)) => items
                .iter()
                .filter_map(|item| match item {
                    Value::Str(recipient) => Some(recipient.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let files = evaluation
            .modules()
            .iter()
            .flat_map(|rendered| &rendered.output.secrets)
            .map(|(name, secret)| (name.clone(), dir.join(&secret.file)))
            .collect();
        Ok(Self { recipients, files })
    }

    fn edit(&self, name: &str, identities: &[PathBuf]) -> Result<ExitCode, String> {
        let file = self.files.get(name).ok_or_else(|| {
            let declared: Vec<&str> = self.files.keys().map(String::as_str).collect();
            let mut message = format!("there is no secret `{name}`");
            if !declared.is_empty() {
                message.push_str(&format!("; the secrets are {}", declared.join(", ")));
            }
            format!("{message}\nhelp: declare it under `[secrets.{name}]` first")
        })?;
        self.check_recipients()?;
        let current = if file.exists() {
            Some(decrypt(file, identities)?)
        } else {
            None
        };
        let edited = edit_in_editor(name, current.as_deref().unwrap_or_default())?;
        if current.as_deref() == Some(edited.as_slice()) {
            println!("{name} is unchanged");
            return Ok(ExitCode::SUCCESS);
        }
        self.encrypt(name, &edited, file)?;
        Ok(ExitCode::SUCCESS)
    }

    fn rekey(&self, identities: &[PathBuf]) -> Result<ExitCode, String> {
        self.check_recipients()?;
        for (name, file) in &self.files {
            if !file.exists() {
                println!("skipped {name}: {} does not exist", file.display());
                continue;
            }
            self.encrypt(name, &decrypt(file, identities)?, file)?;
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Makes the keys machine `name` doesn't have yet under `secrets/machines/NAME` in the
    /// configuration at `config`: its SSH host key and the key its initrd's SSH server presents,
    /// and its Secure Boot keys. A machine with every key already gets nothing new.
    fn make_machine_keys(
        &self,
        config: &Path,
        name: &str,
        passphrase_file: Option<&Path>,
    ) -> Result<ExitCode, String> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return Err(format!("`{name}` isn't a machine's name"));
        }
        let dir = crate::machine_keys::dir(config, name);
        let host = crate::machine_keys::HOST_KEY;
        let has_host_key = crate::machine_keys::to_recipients(&dir, host).exists()
            || crate::machine_keys::to_passphrase(&dir, host).exists();
        let has_secure_boot = dir.join(crate::secure_boot::MACHINE_ARCHIVE).exists();
        if has_host_key && has_secure_boot {
            return Err(format!(
                "{name} has its keys already, in {}; remove them first to make new ones",
                dir.display()
            ));
        }
        self.check_recipients()?;
        let made = private_dir()?;
        if !has_host_key {
            self.make_host_keys(&dir, name, passphrase_file, made.path())?;
        }
        if !has_secure_boot {
            self.make_secure_boot_keys(&dir, name, made.path())?;
        }
        Ok(ExitCode::SUCCESS)
    }

    /// Makes machine `name`'s SSH host key and the key its initrd's SSH server presents, in
    /// `made`, and keeps them in `dir`, encrypted to the recipients and to a passphrase, with their
    /// public halves beside them.
    fn make_host_keys(
        &self,
        dir: &Path,
        name: &str,
        passphrase_file: Option<&Path>,
        made: &Path,
    ) -> Result<(), String> {
        let host = crate::machine_keys::HOST_KEY;
        let passphrase = match passphrase_file {
            Some(file) => crate::machine_keys::read_passphrase(file)?,
            None => crate::machine_keys::new_passphrase(&format!("{name}'s keys at install"))?,
        };
        let mut publics = Vec::new();
        for (key, _) in crate::machine_keys::KEYS {
            let private = made.join(key);
            let comment = if key == host {
                format!("root@{name}")
            } else {
                format!("initrd@{name}")
            };
            let generated = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", "", "-C", &comment, "-f"])
                .arg(&private)
                .output()
                .map_err(|e| format!("cannot run ssh-keygen: {e}"))?;
            if !generated.status.success() {
                return Err(format!(
                    "ssh-keygen: {}",
                    String::from_utf8_lossy(&generated.stderr).trim()
                ));
            }
            let secret =
                std::fs::read(&private).map_err(|e| format!("{}: {e}", private.display()))?;
            let public = std::fs::read_to_string(private.with_extension("pub"))
                .map_err(|e| format!("{key}.pub: {e}"))?;
            let encrypted = crate::machine_keys::to_recipients(dir, key);
            self.encrypt(&format!("{name}'s {key}"), &secret, &encrypted)?;
            let locked = crate::machine_keys::to_passphrase(dir, key);
            let partial = locked.with_extension("partial");
            std::fs::write(&partial, crate::machine_keys::lock(&secret, &passphrase)?)
                .and_then(|()| std::fs::rename(&partial, &locked))
                .map_err(|e| format!("{}: {e}", locked.display()))?;
            let pub_file = dir.join(format!("{key}.pub"));
            std::fs::write(&pub_file, &public)
                .map_err(|e| format!("{}: {e}", pub_file.display()))?;
            // The public key without its comment, as recipients are written.
            publics.push(
                public
                    .split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        println!(
            "encrypted them to the passphrase too, for installing where none of their keys is"
        );
        println!("{name}'s host key is {}", publics[0]);
        println!(
            "its initrd presents {}, when its disks unlock over SSH",
            publics[1]
        );
        println!(
            "add the host key to `system.secret_recipients`, in place of any key {name} had \
             before, and `wrath secret rekey` encrypts its secrets to it"
        );
        Ok(())
    }

    /// Makes machine `name`'s Secure Boot keys in `made` and keeps them in `dir`: what the machine
    /// signs with encrypted to the recipients, and the PK and KEK to every recipient but the
    /// machine, so only its admins can change what its firmware trusts.
    fn make_secure_boot_keys(&self, dir: &Path, name: &str, made: &Path) -> Result<(), String> {
        let host_public = without_comment(
            &std::fs::read_to_string(dir.join(format!("{}.pub", crate::machine_keys::HOST_KEY)))
                .unwrap_or_default(),
        );
        let admins: Vec<String> = self
            .recipients
            .iter()
            .filter(|recipient| without_comment(recipient) != host_public)
            .cloned()
            .collect();
        if admins.is_empty() {
            return Err(format!(
                "only {name} itself is among `system.secret_recipients`, so nobody would keep the \
                 keys that change what its firmware trusts\nhelp: add your own key to them"
            ));
        }
        let made = made.join("secure-boot");
        std::fs::create_dir(&made).map_err(|e| format!("{}: {e}", made.display()))?;
        let keys = crate::secure_boot::make(name, &made)?;
        // The machine decrypts these when it switches, so they're encrypted to its host key even
        // before it's among the recipients, as a host key made just now isn't.
        let mut recipients = self.recipients.clone();
        // Every recipient is an admin when the host key isn't among them.
        if !host_public.is_empty() && admins.len() == self.recipients.len() {
            recipients.push(host_public);
        }
        let machine = dir.join(crate::secure_boot::MACHINE_ARCHIVE);
        encrypt_to(
            &recipients,
            &format!("{name}'s Secure Boot keys"),
            &keys.machine,
            &machine,
        )?;
        let owner = dir.join(crate::secure_boot::OWNER_ARCHIVE);
        encrypt_to(
            &admins,
            &format!("{name}'s firmware owner keys"),
            &keys.owner,
            &owner,
        )?;
        println!(
            "{name} enrolls its Secure Boot keys when its firmware is in setup mode, once \
             `boot.secure_boot` is on"
        );
        Ok(())
    }

    fn check_recipients(&self) -> Result<(), String> {
        if self.recipients.is_empty() {
            return Err(
                "there is no one to encrypt to\nhelp: set `system.secret_recipients` to \
                        the machine's public host key and yours"
                    .to_owned(),
            );
        }
        Ok(())
    }

    /// Encrypts `plaintext` to the recipients and replaces `file` with the result in one step.
    fn encrypt(&self, name: &str, plaintext: &[u8], file: &Path) -> Result<(), String> {
        encrypt_to(&self.recipients, name, plaintext, file)
    }
}

/// Encrypts `plaintext` to `recipients` and replaces `file` with the result in one step.
fn encrypt_to(
    recipients: &[String],
    name: &str,
    plaintext: &[u8],
    file: &Path,
) -> Result<(), String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let partial = file.with_extension("age.partial");
    let mut command = Command::new("age");
    command.arg("--encrypt").arg("--output").arg(&partial);
    for recipient in recipients {
        command.arg("--recipient").arg(recipient);
    }
    age(&mut command, plaintext)?;
    std::fs::rename(&partial, file).map_err(|e| format!("{}: {e}", file.display()))?;
    let count = recipients.len();
    let noun = if count == 1 {
        "recipient"
    } else {
        "recipients"
    };
    println!("encrypted {name} to {count} {noun}");
    Ok(())
}

/// A public key as recipients are compared: its type and key, without the comment.
fn without_comment(key: &str) -> String {
    key.split_whitespace().take(2).collect::<Vec<_>>().join(" ")
}

fn decrypt(file: &Path, identities: &[PathBuf]) -> Result<Vec<u8>, String> {
    let mut command = Command::new("age");
    command.arg("--decrypt");
    for identity in identities {
        command.arg("--identity").arg(identity);
    }
    command.arg(file);
    age(&mut command, &[]).map_err(|e| format!("cannot decrypt {}: {e}", file.display()))
}

/// Runs `age` with `input` on stdin and returns its output.
fn age(command: &mut Command, input: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run age: {e}"))?;
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(input)
        .map_err(|e| format!("cannot write to age: {e}"))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("age failed: {e}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Opens `contents` in $VISUAL or $EDITOR and returns what was saved.
fn edit_in_editor(name: &str, contents: &[u8]) -> Result<Vec<u8>, String> {
    let dir = private_dir()?;
    let path = dir.path().join(name);
    std::fs::write(&path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_owned());
    // The editor may have arguments, as in `code --wait`, so the shell splits it.
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(&path)
        .status()
        .map_err(|e| format!("cannot run the editor: {e}"))?;
    if !status.success() {
        return Err(format!("the editor failed ({status}); {name} is unchanged"));
    }
    std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))
}

/// A directory only this user can read, preferably on the user's runtime tmpfs, for decrypted
/// contents.
fn private_dir() -> Result<tempfile::TempDir, String> {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => tempfile::tempdir_in(runtime),
        None => tempfile::tempdir(),
    }
    .map_err(|e| format!("cannot create a private directory: {e}"))
}

/// A configuration whose secrets may not exist yet, as when creating one.
struct Editing(Sourced<DirFs>);

impl ConfigFs for Editing {
    fn read(&self, path: &str) -> io::Result<String> {
        self.0.read(path)
    }

    fn read_bytes(&self, path: &str) -> io::Result<Vec<u8>> {
        match self.0.read_bytes(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
            read => read,
        }
    }

    fn toml_files(&self, dir: &str) -> io::Result<Vec<String>> {
        self.0.toml_files(dir)
    }

    fn subdirs(&self, dir: &str) -> io::Result<Vec<String>> {
        self.0.subdirs(dir)
    }

    fn display(&self, path: &str) -> String {
        self.0.display(path)
    }

    fn source(&self, name: &str, url: &str) -> Result<Box<dyn ConfigFs>, String> {
        self.0.source(name, url)
    }
}
