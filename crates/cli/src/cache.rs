//! `wrath cache`: the keys that sign what goes into a binary cache, and pushing builds to one.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use wrath_build::cache::{Location, SigningKeyFile, TrustedKey};
use wrath_build::store::Lab;

#[derive(Debug, clap::Subcommand)]
pub enum CacheCommand {
    /// Make a key to sign builds with, and print the public key machines trust it by
    Key {
        /// The key's name, which a cache files what it signs under, such as `home-1`
        name: String,
        /// Where to keep the key, which only whoever pushes should be able to read
        file: PathBuf,
    },
    /// Push everything built here to a cache, signed, that it doesn't have yet
    Push {
        /// The cache: a directory
        cache: String,
        /// The key to sign with, as `wrath cache key` made it
        #[arg(long)]
        key: PathBuf,
    },
}

pub fn run(command: CacheCommand, work: &Path) -> Result<ExitCode, String> {
    match command {
        CacheCommand::Key { name, file } => {
            if file.exists() {
                return Err(format!("{} is there already", file.display()));
            }
            let key = SigningKeyFile::generate(&name)?;
            write_private(&file, &key.secret())?;
            println!("{}", key.public());
            Ok(ExitCode::SUCCESS)
        }
        CacheCommand::Push { cache, key } => {
            let key = SigningKeyFile::read(&key)?;
            let pushed = wrath_build::cache::push(&Lab::new(work), &Location::parse(&cache), &key)?;
            println!(
                "pushed {} and {}",
                crate::counted(pushed.traces, "build"),
                crate::counted(pushed.archives, "store path")
            );
            if !pushed.disagreeing.is_empty() {
                crate::style::warning(&format!(
                    "the cache has {} signed by {} that say something else, kept as they were: {}",
                    crate::counted(pushed.disagreeing.len(), "build"),
                    key.public(),
                    pushed.disagreeing.join(", ")
                ));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Makes builds fetch from `caches` what a key in `keys` signed.
pub fn use_caches(caches: &[String], keys: &[String]) -> Result<(), String> {
    if caches.is_empty() {
        return Ok(());
    }
    if keys.is_empty() {
        return Err("`--cache` needs a `--cache-key` to trust what it holds".into());
    }
    let trusted = keys
        .iter()
        .map(|key| TrustedKey::parse(key))
        .collect::<Result<_, _>>()?;
    wrath_build::cache::use_caches(
        caches.iter().map(|url| Location::parse(url)).collect(),
        trusted,
    );
    Ok(())
}

fn write_private(file: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(file)
        .and_then(|mut opened| opened.write_all(text.as_bytes()))
        .map_err(|e| format!("{}: {e}", file.display()))
}
