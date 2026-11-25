//! `wrath cache`: the keys that sign what goes into a binary cache, pushing builds to one, and
//! serving it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// Serve a cache in a directory over HTTP, for machines on the network to fetch from
    Serve {
        /// The cache: a directory `wrath cache push` filled
        cache: PathBuf,
        /// The address and port to listen on
        #[arg(long, default_value = "0.0.0.0:8080")]
        listen: String,
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
            let seeds = match pushed.seeds {
                0 => String::new(),
                n => format!(", and {}", crate::counted(n, "seed")),
            };
            println!(
                "pushed {} and {}{seeds}",
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
        CacheCommand::Serve { cache, listen } => serve(&cache, &listen),
    }
}

/// Serves `cache` on `listen` until stopped.
fn serve(cache: &Path, listen: &str) -> Result<ExitCode, String> {
    if !cache.join("traces").is_dir() {
        return Err(format!(
            "{} isn't a cache: `wrath cache push` fills one",
            cache.display()
        ));
    }
    let listener =
        std::net::TcpListener::bind(listen).map_err(|e| format!("listening on {listen}: {e}"))?;
    println!(
        "serving {} on http://{listen} until stopped",
        cache.display()
    );
    wrath_build::http::serve_dir(listener, cache);
    Ok(ExitCode::SUCCESS)
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

/// Whether this command asks no cache for anything, as `--offline` says.
static OFFLINE: AtomicBool = AtomicBool::new(false);

/// The names configurations give caches, by where each one is, for saying where things come from.
static NAMES: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Makes this command ask no cache for anything, whatever the configuration lists.
pub fn go_offline() {
    OFFLINE.store(true, Ordering::Relaxed);
}

pub fn offline() -> bool {
    OFFLINE.load(Ordering::Relaxed)
}

/// The name a configuration gives the cache at `shown`, or `shown` itself.
pub fn name_of(shown: &str) -> String {
    let names = NAMES.lock().expect("the cache names are readable");
    names
        .get(shown)
        .cloned()
        .unwrap_or_else(|| shown.to_owned())
}

/// Makes builds fetch from the caches `evaluation` lists, trusting the keys it gives them. A
/// cache's directory is relative to the configuration at `config`, when it has one.
pub fn use_configured(evaluation: &wrath_eval::Evaluation, config: Option<&Path>) {
    if offline() {
        return;
    }
    let mut names = NAMES.lock().expect("the cache names are writable");
    let mut locations = Vec::new();
    let mut trusted = Vec::new();
    for (name, cache) in evaluation.declared_caches() {
        let location = match (Location::parse(&cache.url), config) {
            (Location::Dir(dir), Some(config)) if dir.is_relative() => {
                Location::Dir(config.join(dir))
            }
            (location, _) => location,
        };
        if let Location::Dir(dir) = &location
            && !dir.join("traces").is_dir()
        {
            crate::style::warning(&format!(
                "cache {name}: {} holds no traces, so nothing comes from it",
                dir.display()
            ));
        }
        names.insert(location.shown(), name.clone());
        locations.push(location);
        trusted.extend(cache.keys.iter().filter_map(|key| {
            TrustedKey::parse(key)
                .inspect_err(|e| crate::style::warning(e))
                .ok()
        }));
    }
    wrath_build::cache::use_caches(locations, trusted);
}

/// Says where `package` would come from: the store, a cache, or a build, and when it would be
/// built, why. Nothing is fetched or built.
pub(crate) fn where_from(package: &str, work: &Path) -> Result<(), String> {
    use wrath_build::graph::Found;
    let grapes = crate::places::grapes()?;
    let (key, found) = wrath_build::graph::package_check(&grapes, work, package)?;
    match key {
        Some(key) => println!("where it comes from, key {}:", &key[..12.min(key.len())]),
        None => println!("where it comes from:"),
    }
    let asked = wrath_build::cache::asked();
    // A cache that couldn't be reached may have it all the same.
    let nowhere = if asked.iter().any(|(_, down)| *down) {
        "no cache that answered has"
    } else {
        "no cache has"
    };
    match found {
        Found::Here => println!("  here already"),
        Found::Fetched(from) => {
            for (cache, (count, bytes)) in from {
                let size = crate::generations::size(bytes);
                let paths = crate::counted(count, "path");
                println!("  fetched from {}: {paths} ({size})", name_of(&cache));
            }
        }
        Found::Built(chain) if chain.is_empty() => println!("  built here: {nowhere} it"),
        Found::Built(chain) => println!(
            "  built here: it's built with {}, which {nowhere}",
            chain.join(", which is built with ")
        ),
    }
    if asked.is_empty() {
        println!("  no cache is listed; a system's configuration lists them under `[caches.NAME]`");
    }
    for (cache, down) in asked {
        let reached = if down { "couldn't be reached" } else { "asked" };
        match name_of(&cache) {
            name if name == cache => println!("  {reached}: {cache}"),
            name => println!("  {reached}: {name} ({cache})"),
        }
    }
    Ok(())
}

/// Says how much the command fetched from caches since this was last called, if anything.
pub fn say_fetched() {
    let (count, bytes) = wrath_build::cache::fetched();
    if count > 0 {
        let size = crate::generations::size(bytes);
        eprintln!("fetched {} ({size})", crate::counted(count, "path"));
    }
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
    let dir = file.parent().filter(|dir| !dir.as_os_str().is_empty());
    dir.map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(file)
        })
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
