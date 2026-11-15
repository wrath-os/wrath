//! A binary cache (decision 0038): what was built, under the key it was built by, in a directory
//! or behind HTTP, so a build fetches a package instead of building it.
//!
//! A cache holds a trace per build, signed, under `traces/<key>/<rules>/<signer>`, and each store
//! path once, as `paths/<path>.tar.zst`. A trace says what the build's fix-up record is and, for
//! each path it made, the digest of its archive and the paths it refers to. A fetched archive
//! must match the digest a trusted signer gave, so the cache itself needs no trust.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::store::{Lab, StorePath, hex, with_path};

/// One build: the fix-up record its key and rules led to, and the paths it made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Trace {
    key: String,
    rules: String,
    record: String,
    paths: BTreeMap<String, Archived>,
}

/// A store path as a cache keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Archived {
    /// The SHA-256 of `paths/<path>.tar.zst`.
    archive: String,
    /// The store paths it refers to, other than itself.
    refs: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct Signed {
    trace: Trace,
    signature: String,
}

impl Trace {
    fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a trace serializes")
    }
}

/// A key that signs traces, as `name:hex` of its secret.
pub struct SigningKeyFile {
    name: String,
    key: SigningKey,
}

/// A key a trace's signature is checked with, as `name:hex`.
#[derive(Debug, Clone)]
pub struct TrustedKey {
    name: String,
    key: VerifyingKey,
}

fn named_bytes<const N: usize>(text: &str, what: &str) -> Result<(String, [u8; N]), String> {
    let invalid = || format!("{what} `{text}` isn't `name:hex` of {N} bytes");
    let (name, hex_text) = text.trim().split_once(':').ok_or_else(invalid)?;
    let valid_name = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    match from_hex(hex_text) {
        Some(bytes) if valid_name => Ok((name.to_owned(), bytes)),
        _ => Err(invalid()),
    }
}

/// The `N` bytes `text` spells in hex, if it spells exactly that many.
fn from_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != 2 * N || !text.is_ascii() {
        return None;
    }
    let mut bytes = [0; N];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(bytes)
}

impl SigningKeyFile {
    /// A new key called `name`, from the system's randomness.
    pub fn generate(name: &str) -> Result<Self, String> {
        let mut secret = [0; 32];
        fs::File::open("/dev/urandom")
            .and_then(|mut random| random.read_exact(&mut secret))
            .map_err(|e| format!("/dev/urandom: {e}"))?;
        Self::parse(&format!("{name}:{}", hex(&secret)))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let (name, secret) = named_bytes::<32>(text, "a signing key")?;
        Ok(Self {
            name,
            key: SigningKey::from_bytes(&secret),
        })
    }

    pub fn read(file: &Path) -> Result<Self, String> {
        Self::parse(&fs::read_to_string(file).map_err(with_path(file))?)
            .map_err(|e| format!("{}: {e}", file.display()))
    }

    /// The key as its file holds it.
    pub fn secret(&self) -> String {
        format!("{}:{}\n", self.name, hex(&self.key.to_bytes()))
    }

    /// The key a machine trusts to accept what this one signs.
    pub fn public(&self) -> String {
        format!(
            "{}:{}",
            self.name,
            hex(&self.key.verifying_key().to_bytes())
        )
    }

    fn sign(&self, trace: Trace) -> Signed {
        let signature = hex(&self.key.sign(&trace.bytes()).to_bytes());
        Signed { trace, signature }
    }
}

impl TrustedKey {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (name, public) = named_bytes::<32>(text, "a cache key")?;
        let key = VerifyingKey::from_bytes(&public)
            .map_err(|_| format!("a cache key `{text}` isn't a valid public key"))?;
        Ok(Self { name, key })
    }

    fn verifies(&self, signed: &Signed) -> bool {
        from_hex::<64>(&signed.signature)
            .map(|bytes| Signature::from_bytes(&bytes))
            .is_some_and(|signature| {
                self.key
                    .verify_strict(&signed.trace.bytes(), &signature)
                    .is_ok()
            })
    }
}

/// Where a cache is: a directory, or an HTTP URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Dir(PathBuf),
    Http(String),
}

impl Location {
    pub fn parse(url: &str) -> Self {
        if url.starts_with("http://") || url.starts_with("https://") {
            Self::Http(url.trim_end_matches('/').to_owned())
        } else {
            Self::Dir(PathBuf::from(url.strip_prefix("file://").unwrap_or(url)))
        }
    }

    /// Fetches `name` into `file`, or says there's no such thing.
    fn download(&self, name: &str, file: &Path) -> Result<bool, String> {
        match self {
            Self::Dir(dir) => match fs::copy(dir.join(name), file) {
                Ok(_) => Ok(true),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(e) => Err(format!("{}: {e}", dir.join(name).display())),
            },
            Self::Http(base) => crate::http::download(&format!("{base}/{name}"), file),
        }
    }

    fn read(&self, name: &str, scratch: &Path) -> Result<Option<Vec<u8>>, String> {
        if !self.download(name, scratch)? {
            return Ok(None);
        }
        let bytes = fs::read(scratch).map_err(with_path(scratch))?;
        let _ = fs::remove_file(scratch);
        Ok(Some(bytes))
    }

    fn shown(&self) -> String {
        match self {
            Self::Dir(dir) => dir.display().to_string(),
            Self::Http(url) => url.clone(),
        }
    }

    fn dir(&self) -> Result<&Path, String> {
        match self {
            Self::Dir(dir) => Ok(dir),
            Self::Http(url) => Err(format!("{url}: only a directory can be pushed to so far")),
        }
    }
}

/// Writes `bytes` to `file` whole or not at all.
fn write_whole(file: &Path, bytes: &[u8]) -> Result<(), String> {
    let dir = file.parent().expect("a cache file is in a directory");
    fs::create_dir_all(dir).map_err(with_path(dir))?;
    let partial = file.with_extension(format!("partial{}", std::process::id()));
    fs::write(&partial, bytes).map_err(with_path(&partial))?;
    fs::rename(&partial, file).map_err(with_path(file))
}

fn sha256_of(file: &Path) -> Result<String, String> {
    let mut reader = fs::File::open(file).map_err(with_path(file))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut reader, &mut hasher).map_err(with_path(file))?;
    Ok(hex(&hasher.finalize()))
}

fn trace_name(key: &str, rules: &str, signer: &str) -> String {
    format!("traces/{key}/{rules}/{signer}")
}

fn archive_name(path: &str) -> String {
    format!("paths/{path}.tar.zst")
}

/// The caches builds fetch from and the keys they trust, set once for the command.
static CACHES: OnceLock<(Vec<Location>, Vec<TrustedKey>)> = OnceLock::new();

/// Makes builds fetch from `caches` what a key in `trusted` signed, before building it.
pub fn use_caches(caches: Vec<Location>, trusted: Vec<TrustedKey>) {
    let _ = CACHES.set((caches, trusted));
}

/// Fetches the build at `out` under fix-up `rules` from the caches in use, writing its fix-up
/// record at `fixed` once every path it made is in the store. Says whether it did. A cache that
/// can't be read, or holds something that doesn't match its trace, is warned about and skipped.
pub(crate) fn fetch(lab: &Lab, out: &StorePath, rules: &str, fixed: &Path) -> Result<bool, String> {
    let Some((caches, trusted)) = CACHES.get() else {
        return Ok(false);
    };
    for cache in caches {
        match fetch_from(lab, cache, trusted, out.hash(), rules, fixed) {
            Ok(true) => return Ok(true),
            Ok(false) => {}
            Err(message) => eprintln!("warning: cache {}: {message}", cache.shown()),
        }
    }
    Ok(false)
}

fn fetch_from(
    lab: &Lab,
    cache: &Location,
    trusted: &[TrustedKey],
    key: &str,
    rules: &str,
    fixed: &Path,
) -> Result<bool, String> {
    let scratch = lab.work.join("fetching");
    fs::create_dir_all(&scratch).map_err(with_path(&scratch))?;
    let partial = scratch.join(format!("{key}.{}", std::process::id()));
    for signer in trusted {
        let Some(bytes) = cache.read(&trace_name(key, rules, &signer.name), &partial)? else {
            continue;
        };
        let signed: Signed = serde_json::from_slice(&bytes)
            .map_err(|e| format!("the trace of {key} by {}: {e}", signer.name))?;
        if !signer.verifies(&signed) || signed.trace.key != key || signed.trace.rules != rules {
            return Err(format!(
                "the trace of {key} isn't signed by {} for that key",
                signer.name
            ));
        }
        for (path, archived) in &signed.trace.paths {
            unpack(lab, cache, path, &archived.archive, &partial)?;
        }
        write_whole(fixed, signed.trace.record.as_bytes())?;
        eprintln!("  fetched {key}");
        return Ok(true);
    }
    Ok(false)
}

/// Puts the store path `path` from `cache` into the store, unless it's there, checking its
/// archive is the one with `digest`.
fn unpack(
    lab: &Lab,
    cache: &Location,
    path: &str,
    digest: &str,
    partial: &Path,
) -> Result<(), String> {
    let target = lab.on_host(&StorePath::from_hash(path));
    if target.exists() {
        return Ok(());
    }
    if !cache.download(&archive_name(path), partial)? {
        return Err(format!("it has a trace naming {path}, but not the path"));
    }
    let actual = sha256_of(partial)?;
    if actual != digest {
        let _ = fs::remove_file(partial);
        return Err(format!("{path} is {actual}, and its trace says {digest}"));
    }
    // Unpacked beside where it goes, so moving it there is one rename within the store.
    let unpacked = target.with_extension(format!("fetching{}", std::process::id()));
    fs::create_dir_all(&unpacked).map_err(with_path(&unpacked))?;
    let unpack_from = |archive: fs::File| -> std::io::Result<()> {
        let mut entries = tar::Archive::new(zstd::Decoder::new(archive)?);
        entries.set_preserve_permissions(true);
        entries.set_preserve_mtime(true);
        entries.set_unpack_xattrs(false);
        entries.unpack(&unpacked)
    };
    let unpacked_now = fs::File::open(partial).and_then(unpack_from);
    let _ = fs::remove_file(partial);
    unpacked_now.map_err(|e| format!("unpacking {path}: {e}"))?;
    fs::rename(&unpacked, &target).map_err(with_path(&target))
}

/// What a push did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pushed {
    pub traces: usize,
    pub archives: usize,
    /// Traces the cache had already, from this signer, saying something else.
    pub disagreeing: Vec<String>,
}

/// Pushes to the directory `cache` every build `lab` keeps under the current fix-up rules whose
/// paths are all in the store, signed by `signer`. Paths go up before the trace naming them.
pub fn push(lab: &Lab, cache: &Location, signer: &SigningKeyFile) -> Result<Pushed, String> {
    let dir = cache.dir()?;
    let current = &crate::fixup::rules()[..16];
    let records = lab.work.join("fixed");
    let mut pushed = Pushed::default();
    let mut entries: Vec<_> = fs::read_dir(&records)
        .map_err(with_path(&records))?
        .collect::<Result<_, _>>()
        .map_err(with_path(&records))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        // The rules come last, after the key's path, whose name has dashes of its own.
        let Some((key, rules)) = name.rsplit_once('-') else {
            continue;
        };
        if rules != current && rules != "built" {
            continue;
        }
        let record = fs::read_to_string(entry.path()).map_err(with_path(&entry.path()))?;
        let paths: Vec<&str> = if rules == "built" {
            vec![key]
        } else {
            record
                .split(' ')
                .filter_map(|part| part.split_once('=').map(|(_, path)| path))
                .collect()
        };
        let in_store = |path: &&str| lab.on_host(&StorePath::from_hash(path)).is_dir();
        if paths.is_empty() || !paths.iter().all(in_store) {
            continue;
        }
        let mut archived = BTreeMap::new();
        for path in paths {
            let archive = dir.join(archive_name(path));
            if !archive.exists() {
                pack(lab, path, &archive)?;
                pushed.archives += 1;
            }
            let refs = crate::chain::references_of(lab, &StorePath::from_hash(path))?
                .iter()
                .map(|r| r.hash().to_owned())
                .collect();
            archived.insert(
                path.to_owned(),
                Archived {
                    archive: sha256_of(&archive)?,
                    refs,
                },
            );
        }
        let trace = Trace {
            key: key.to_owned(),
            rules: rules.to_owned(),
            record,
            paths: archived,
        };
        let file = dir.join(trace_name(key, rules, &signer.name));
        if let Ok(bytes) = fs::read(&file) {
            let theirs: Option<Signed> = serde_json::from_slice(&bytes).ok();
            if theirs.is_none_or(|signed| signed.trace != trace) {
                pushed.disagreeing.push(key.to_owned());
            }
            continue;
        }
        let signed = serde_json::to_vec(&signer.sign(trace)).expect("a trace serializes");
        write_whole(&file, &signed)?;
        pushed.traces += 1;
    }
    Ok(pushed)
}

/// Archives the store path `path` at `archive`, as [`unpack`] reads it.
fn pack(lab: &Lab, path: &str, archive: &Path) -> Result<(), String> {
    let dir = archive.parent().expect("an archive is in a directory");
    fs::create_dir_all(dir).map_err(with_path(dir))?;
    let partial = archive.with_extension(format!("partial{}", std::process::id()));
    let output = Command::new("tar")
        .args(["--zstd", "--create", "--sort=name", "--numeric-owner"])
        .arg("--file")
        .arg(&partial)
        .arg("--directory")
        .arg(lab.on_host(&StorePath::from_hash(path)))
        .arg(".")
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "archiving {path}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    fs::rename(&partial, archive).map_err(with_path(archive))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "abc123def4567890abc123def4567890abc12345-hello-1.0";
    const PATH: &str = "0123456789abcdef0123456789abcdef01234567-hello-1.0";

    /// A lab with one built package at `KEY`, moved to `path`, holding one file.
    fn lab_with(dir: &Path, path: &str) -> Lab {
        let lab = Lab::new(dir);
        let store = lab.work.join("store").join(path);
        fs::create_dir_all(store.join("bin")).unwrap();
        fs::write(store.join("bin/hello"), "#!/bin/sh\necho hello\n").unwrap();
        let fixed = lab.work.join("fixed");
        fs::create_dir_all(&fixed).unwrap();
        let rules = &crate::fixup::rules()[..16];
        fs::write(fixed.join(format!("{KEY}-{rules}")), format!("out={path}")).unwrap();
        lab
    }

    #[test]
    fn a_key_reads_back_as_it_was_written_and_signs_what_its_public_key_verifies() {
        let key = SigningKeyFile::generate("home-1").unwrap();
        let again = SigningKeyFile::parse(&key.secret()).unwrap();
        assert_eq!(again.public(), key.public());
        let trusted = TrustedKey::parse(&key.public()).unwrap();
        let trace = Trace {
            key: KEY.into(),
            rules: "r".into(),
            record: format!("out={PATH}"),
            paths: Default::default(),
        };
        let mut signed = key.sign(trace);
        assert!(trusted.verifies(&signed));
        signed.trace.record = "out=someone-elses".into();
        assert!(!trusted.verifies(&signed));
        let other = TrustedKey::parse(&SigningKeyFile::generate("x").unwrap().public()).unwrap();
        assert!(!other.verifies(&key.sign(signed.trace)));
    }

    #[test]
    fn a_key_that_isnt_a_name_and_hex_is_refused() {
        for text in ["nohex", ":00", "a b:00", "k:zz", "k:0011"] {
            assert!(TrustedKey::parse(text).is_err(), "{text}");
        }
    }

    #[test]
    fn what_one_lab_pushes_another_fetches_by_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let built = lab_with(&dir.path().join("built"), PATH);
        let cache = Location::Dir(dir.path().join("cache"));
        let key = SigningKeyFile::generate("home").unwrap();
        let pushed = push(&built, &cache, &key).unwrap();
        assert_eq!((pushed.traces, pushed.archives), (1, 1));
        // Pushing again changes nothing.
        assert_eq!(push(&built, &cache, &key).unwrap(), Pushed::default());

        let fresh = Lab::new(dir.path().join("fresh"));
        let rules = &crate::fixup::rules()[..16];
        let fixed = fresh.work.join("fixed").join(format!("{KEY}-{rules}"));
        let trusted = vec![TrustedKey::parse(&key.public()).unwrap()];
        let out = StorePath::from_hash(KEY);
        assert!(fetch_from(&fresh, &cache, &trusted, out.hash(), rules, &fixed).unwrap());
        assert_eq!(fs::read_to_string(&fixed).unwrap(), format!("out={PATH}"));
        let file = fresh.work.join("store").join(PATH).join("bin/hello");
        assert_eq!(fs::read_to_string(file).unwrap(), "#!/bin/sh\necho hello\n");
    }

    #[test]
    fn a_trace_by_a_key_not_trusted_or_an_archive_not_matching_it_is_never_used() {
        let dir = tempfile::tempdir().unwrap();
        let built = lab_with(&dir.path().join("built"), PATH);
        let cache = Location::Dir(dir.path().join("cache"));
        let key = SigningKeyFile::generate("home").unwrap();
        push(&built, &cache, &key).unwrap();
        let fresh = Lab::new(dir.path().join("fresh"));
        let rules = &crate::fixup::rules()[..16];
        let fixed = fresh.work.join("fixed").join(format!("{KEY}-{rules}"));

        // Another key of the same name doesn't verify it.
        let impostor = SigningKeyFile::generate("home").unwrap();
        let trusted = vec![TrustedKey::parse(&impostor.public()).unwrap()];
        let error = fetch_from(&fresh, &cache, &trusted, KEY, rules, &fixed).unwrap_err();
        assert!(error.contains("isn't signed"), "{error}");
        // A key of another name finds no trace of its own.
        let stranger = SigningKeyFile::generate("elsewhere").unwrap();
        let trusted = vec![TrustedKey::parse(&stranger.public()).unwrap()];
        assert!(!fetch_from(&fresh, &cache, &trusted, KEY, rules, &fixed).unwrap());

        // An archive swapped for another is refused, and nothing reaches the store.
        let archive = dir.path().join("cache").join(archive_name(PATH));
        fs::write(&archive, b"something else").unwrap();
        let trusted = vec![TrustedKey::parse(&key.public()).unwrap()];
        let error = fetch_from(&fresh, &cache, &trusted, KEY, rules, &fixed).unwrap_err();
        assert!(error.contains("its trace says"), "{error}");
        assert!(!fresh.work.join("store").join(PATH).exists());
        assert!(!fixed.exists());
    }

    /// Serves the files in `dir` over HTTP, one request per connection, until the test ends;
    /// with `status`, answers every request with it instead.
    fn serve(dir: PathBuf, status: Option<u16>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut lines = BufReader::new(stream.try_clone().unwrap()).lines();
                let request = lines.next().unwrap().unwrap();
                while lines.next().is_some_and(|line| !line.unwrap().is_empty()) {}
                let name = request.split(' ').nth(1).unwrap().trim_start_matches('/');
                let (code, body) = match (status, fs::read(dir.join(name))) {
                    (Some(code), _) => (code, Vec::new()),
                    (None, Ok(bytes)) => (200, bytes),
                    (None, Err(_)) => (404, Vec::new()),
                };
                let head = format!(
                    "HTTP/1.1 {code} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        url
    }

    #[test]
    fn a_cache_over_http_gives_a_build_says_what_it_lacks_and_fails_on_anything_else() {
        let dir = tempfile::tempdir().unwrap();
        let built = lab_with(&dir.path().join("built"), PATH);
        let key = SigningKeyFile::generate("home").unwrap();
        push(&built, &Location::Dir(dir.path().join("cache")), &key).unwrap();
        let trusted = vec![TrustedKey::parse(&key.public()).unwrap()];
        let rules = &crate::fixup::rules()[..16];
        let fresh = Lab::new(dir.path().join("fresh"));
        let fixed = fresh.work.join("fixed").join(format!("{KEY}-{rules}"));
        let cache = Location::parse(&serve(dir.path().join("cache"), None));

        let missing = "ffffffffffffffffffffffffffffffffffffffff-nothing-1.0";
        assert!(!fetch_from(&fresh, &cache, &trusted, missing, rules, &fixed).unwrap());
        assert!(fetch_from(&fresh, &cache, &trusted, KEY, rules, &fixed).unwrap());
        let file = fresh.work.join("store").join(PATH).join("bin/hello");
        assert_eq!(fs::read_to_string(file).unwrap(), "#!/bin/sh\necho hello\n");

        let broken = Location::parse(&serve(dir.path().join("cache"), Some(500)));
        let error = fetch_from(&fresh, &broken, &trusted, missing, rules, &fixed).unwrap_err();
        assert!(error.ends_with(": 500"), "{error}");
    }

    #[test]
    fn a_fetched_path_keeps_its_modes_links_and_read_only_directories() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let built = lab_with(&dir.path().join("built"), PATH);
        let store = built.work.join("store").join(PATH);
        let mode = |path: &Path, mode| fs::set_permissions(path, PermissionsExt::from_mode(mode));
        fs::create_dir(store.join("lib")).unwrap();
        fs::write(store.join("lib/libhello.so.1"), "elf").unwrap();
        symlink("libhello.so.1", store.join("lib/libhello.so")).unwrap();
        symlink(
            "/wrath/store/elsewhere/lib/libc.so.6",
            store.join("lib/libc.so.6"),
        )
        .unwrap();
        fs::hard_link(store.join("bin/hello"), store.join("bin/hi")).unwrap();
        mode(&store.join("bin/hello"), 0o555).unwrap();
        mode(&store.join("lib/libhello.so.1"), 0o444).unwrap();
        mode(&store.join("lib"), 0o555).unwrap();
        let key = SigningKeyFile::generate("home").unwrap();
        let cache = Location::Dir(dir.path().join("cache"));
        push(&built, &cache, &key).unwrap();

        let fresh = Lab::new(dir.path().join("fresh"));
        let rules = &crate::fixup::rules()[..16];
        let fixed = fresh.work.join("fixed").join(format!("{KEY}-{rules}"));
        let trusted = vec![TrustedKey::parse(&key.public()).unwrap()];
        assert!(fetch_from(&fresh, &cache, &trusted, KEY, rules, &fixed).unwrap());
        let got = fresh.work.join("store").join(PATH);
        let mode_of = |path: &str| fs::symlink_metadata(got.join(path)).unwrap().mode() & 0o7777;
        assert_eq!(mode_of("bin/hello"), 0o555);
        assert_eq!(mode_of("lib/libhello.so.1"), 0o444);
        assert_eq!(mode_of("lib"), 0o555);
        let link = |path: &str| fs::read_link(got.join(path)).unwrap();
        assert_eq!(link("lib/libhello.so"), Path::new("libhello.so.1"));
        assert_eq!(
            link("lib/libc.so.6"),
            Path::new("/wrath/store/elsewhere/lib/libc.so.6")
        );
        let inode = |path: &str| fs::metadata(got.join(path)).unwrap().ino();
        assert_eq!(inode("bin/hi"), inode("bin/hello"));

        for lib in [store.join("lib"), got.join("lib")] {
            mode(&lib, 0o755).unwrap();
        }
    }
}
