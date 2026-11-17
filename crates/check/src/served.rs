//! A cache of the host's builds for scenarios that ask for one: pushed once per run of the
//! harness, signed with a key kept for checks, and served over HTTP to guests at [`GUEST`].

use std::fs;
use std::net::TcpListener;
use std::path::Path;
use std::sync::OnceLock;
use std::thread;

use wrath_build::cache::SigningKeyFile;

use crate::vm::Host;

/// Where a guest reaches the cache, as a machine reaches one on its network.
pub const GUEST: &str = "10.0.2.100";

/// The host's cache, served on a port of its own.
pub struct Served {
    pub port: u16,
    /// The `[caches]` table a guest's configuration trusts it with.
    pub table: String,
}

/// Pushes what the host built into its cache for checks and serves it, the first time a
/// scenario asks for it in this run.
pub fn cache(host: Host<'_>) -> Result<&'static Served, String> {
    static SERVED: OnceLock<Result<Served, String>> = OnceLock::new();
    SERVED
        .get_or_init(|| start(host))
        .as_ref()
        .map_err(Clone::clone)
}

fn start(host: Host<'_>) -> Result<Served, String> {
    let dir = host.workdir.join("target/wrath-check");
    let root = dir.join("cache");
    let key_file = dir.join("cache-key");
    let key = key(&key_file)?;
    let (root_arg, key_arg) = (root.display().to_string(), key_file.display().to_string());
    host.wrath(&["cache", "push", &root_arg, "--key", &key_arg])?;
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("serving the cache: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("serving the cache: {e}"))?
        .port();
    thread::spawn(move || wrath_build::http::serve_dir(listener, &root));
    Ok(Served {
        port,
        table: format!(
            "[caches.check]\nurl = \"http://{GUEST}\"\nkeys = [\"{}\"]\n",
            key.public()
        ),
    })
}

/// The key checks sign with, made the first time.
fn key(file: &Path) -> Result<SigningKeyFile, String> {
    if file.exists() {
        return SigningKeyFile::read(file);
    }
    let key = SigningKeyFile::generate("check")?;
    fs::create_dir_all(file.parent().expect("the key is in a directory"))
        .and_then(|()| fs::write(file, key.secret()))
        .map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(key)
}
