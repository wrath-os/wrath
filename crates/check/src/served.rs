//! A cache of the host's builds for scenarios that ask for one: pushed once per run of the
//! harness, signed with a key kept for checks, and served over HTTP to guests at [`GUEST`].

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let root = root.clone();
            thread::spawn(move || {
                let _ = answer(stream, &root);
            });
        }
    });
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

/// Answers one request for a file under `root`, as a static file server does.
fn answer(mut stream: TcpStream, root: &Path) -> io::Result<()> {
    let mut lines = BufReader::new(stream.try_clone()?).lines();
    let request = lines.next().transpose()?.unwrap_or_default();
    while lines
        .next()
        .transpose()?
        .is_some_and(|line| !line.is_empty())
    {}
    match file(root, &request).and_then(|path| fs::File::open(path).ok()) {
        Some(mut file) => {
            let length = file.metadata()?.len();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n"
            )?;
            io::copy(&mut file, &mut stream)?;
        }
        None => write!(
            stream,
            "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        )?,
    }
    Ok(())
}

/// The file a `GET` request line names under `root`, if it names one there.
fn file(root: &Path, request: &str) -> Option<PathBuf> {
    let path = request.strip_prefix("GET ")?.split(' ').next()?;
    let relative = path.strip_prefix('/')?;
    let inside = relative
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..");
    inside.then(|| root.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_name_files_under_the_cache_and_nothing_outside_it() {
        let root = Path::new("/c");
        assert_eq!(
            file(root, "GET /traces/ab HTTP/1.1"),
            Some(PathBuf::from("/c/traces/ab"))
        );
        assert_eq!(file(root, "GET /../etc/passwd HTTP/1.1"), None);
        assert_eq!(file(root, "GET /a//b HTTP/1.1"), None);
        assert_eq!(file(root, "POST /a HTTP/1.1"), None);
        assert_eq!(file(root, ""), None);
    }
}
