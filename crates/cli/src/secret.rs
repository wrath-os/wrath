//! `distro secret`: editing secrets and encrypting them to the configuration's recipients with
//! `age`. Decrypted contents only ever exist in memory and in the file the editor opens, which is
//! in a private temporary directory, preferably on the user's runtime tmpfs.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

use distro_eval::{ConfigFs, DirFs, Value};

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
        let config = Editing(DirFs::new(dir));
        let evaluation = system::evaluate(&config, modules, crate::role())?;
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
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let partial = file.with_extension("age.partial");
        let mut command = Command::new("age");
        command.arg("--encrypt").arg("--output").arg(&partial);
        for recipient in &self.recipients {
            command.arg("--recipient").arg(recipient);
        }
        age(&mut command, plaintext)?;
        std::fs::rename(&partial, file).map_err(|e| format!("{}: {e}", file.display()))?;
        let count = self.recipients.len();
        let noun = if count == 1 {
            "recipient"
        } else {
            "recipients"
        };
        println!("encrypted {name} to {count} {noun}");
        Ok(())
    }
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
    let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) => tempfile::tempdir_in(runtime),
        None => tempfile::tempdir(),
    }
    .map_err(|e| format!("cannot create a private directory: {e}"))?;
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

/// A configuration whose secrets may not exist yet, as when creating one.
struct Editing(DirFs);

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

    fn display(&self, path: &str) -> String {
        self.0.display(path)
    }
}
