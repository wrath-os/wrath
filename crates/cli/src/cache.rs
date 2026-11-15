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
        /// Where machines will fetch from, for the configuration this prints
        #[arg(long)]
        url: Option<String>,
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
        CacheCommand::Key { name, file, url } => {
            if file.exists() {
                return Err(format!("{} is there already", file.display()));
            }
            let key = SigningKeyFile::generate(&name)?;
            write_private(&file, &key.secret())?;
            eprintln!(
                "wrote the key to {}; only whoever pushes should be able to read it. A machine \
                 fetches what it signs with this in its configuration:\n",
                file.display()
            );
            print!("{}", paste_ready(&name, url.as_deref(), &key.public()));
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

/// Makes builds fetch from `caches` what a key in `keys` signed, or a key a configuration this
/// command reads trusts.
pub fn use_caches(caches: &[String], keys: &[String]) -> Result<(), String> {
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

/// Makes builds fetch from the caches `evaluation` lists, trusting the keys it gives them. A
/// cache's directory is relative to the configuration at `config`, when it has one.
pub fn use_configured(evaluation: &wrath_eval::Evaluation, config: Option<&Path>) {
    let caches = evaluation.declared_caches().values();
    let locations = caches
        .clone()
        .map(|cache| match (Location::parse(&cache.url), config) {
            (Location::Dir(dir), Some(config)) if dir.is_relative() => {
                Location::Dir(config.join(dir))
            }
            (location, _) => location,
        })
        .collect();
    let trusted = caches
        .flat_map(|cache| &cache.keys)
        .filter_map(|key| {
            TrustedKey::parse(key)
                .inspect_err(|e| crate::style::warning(e))
                .ok()
        })
        .collect();
    wrath_build::cache::use_caches(locations, trusted);
}

/// The `[caches]` table a system's configuration trusts `public` with, for a cache at `url`. Without
/// one, the line for it is there to fill in, and the configuration says it's missing until then.
fn paste_ready(name: &str, url: Option<&str>, public: &str) -> String {
    let url = match url {
        Some(url) => format!("url = {}", toml_string(url)),
        None => "# url = \"https://...\"   where machines fetch from".to_owned(),
    };
    format!("[caches.{name}]\n{url}\nkeys = [{}]\n", toml_string(public))
}

fn toml_string(text: &str) -> String {
    toml_edit::Value::from(text).to_string().trim().to_owned()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_key_prints_the_table_a_machine_trusts_it_with() {
        let key = SigningKeyFile::generate("home-1").unwrap();
        let block = paste_ready("home-1", Some("https://cache.lan/\"x\""), &key.public());
        let table: toml_edit::DocumentMut = block.parse().unwrap();
        let cache = &table["caches"]["home-1"];
        assert_eq!(cache["url"].as_str(), Some("https://cache.lan/\"x\""));
        assert_eq!(cache["keys"][0].as_str(), Some(key.public().as_str()));

        let without = paste_ready("home-1", None, &key.public());
        let table: toml_edit::DocumentMut = without.parse().unwrap();
        assert!(table["caches"]["home-1"].get("url").is_none());
        assert!(without.contains("# url = "), "{without}");
    }
}
