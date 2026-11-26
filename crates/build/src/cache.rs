//! A binary cache (decision 0038): what was built, under the key it was built by, in a directory
//! or behind HTTP, so a build fetches a package instead of building it.
//!
//! A cache holds a trace per build, signed, under `traces/<key>/<rules>/<signer>`, and each store
//! path once, as `paths/<path>.tar.zst`. A trace says what the build's fix-up record is and, for
//! each path it made, the digest of its archive and the paths it refers to. A fetched archive
//! must match the digest a trusted signer gave, so the cache itself needs no trust.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::Object;
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
    /// The size of the archive in bytes.
    size: u64,
    /// What its dependents' keys read of it. A trace from before listings has none, and is
    /// refused: its cache needs pushing to again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listing: Option<Listing>,
}

/// A trace as a cache holds it: the trace's JSON as it was signed, and the signature over those
/// very bytes, so what's checked is what was sent, whatever fields a later wrath adds.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Signed {
    trace: String,
    signature: String,
}

/// What a signature covers before the trace, so nothing else signed with the key can pass for a
/// trace.
const SIGNED_AS: &[u8] = b"wrath-trace-v1\0";

/// The most a trace may be, in bytes.
const TRACE_LIMIT: u64 = 16_000_000;

impl Signed {
    fn bytes(&self) -> Vec<u8> {
        [SIGNED_AS, self.trace.as_bytes()].concat()
    }

    fn trace(&self) -> Result<Trace, String> {
        serde_json::from_str(&self.trace).map_err(|e| format!("a trace wrath can't read: {e}"))
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

    fn sign(&self, trace: &Trace) -> Signed {
        let mut signed = Signed {
            trace: serde_json::to_string(trace).expect("a trace serializes"),
            signature: String::new(),
        };
        signed.signature = hex(&self.key.sign(&signed.bytes()).to_bytes());
        signed
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
            .is_some_and(|signature| self.key.verify_strict(&signed.bytes(), &signature).is_ok())
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

    /// Fetches `name`, at most `limit` bytes, into `file`, or says the cache doesn't have it.
    fn download(&self, name: &str, file: &Path, limit: u64) -> Result<Object, String> {
        match self {
            Self::Dir(dir) => {
                let from = dir.join(name);
                match fs::metadata(&from) {
                    Ok(meta) if meta.len() > limit => Err(format!(
                        "{} is larger than the {limit} bytes expected",
                        from.display()
                    )),
                    Ok(_) => fs::copy(&from, file)
                        .map(|_| Object::Got)
                        .map_err(with_path(&from)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Object::Missing),
                    Err(e) => Err(format!("{}: {e}", from.display())),
                }
            }
            Self::Http(base) => crate::http::object(&format!("{base}/{name}"), file, limit),
        }
    }

    fn read(&self, name: &str, scratch: &Path) -> Result<Option<Vec<u8>>, String> {
        if self.download(name, scratch, TRACE_LIMIT)? == Object::Missing {
            return Ok(None);
        }
        let bytes = fs::read(scratch).map_err(with_path(scratch))?;
        let _ = fs::remove_file(scratch);
        Ok(Some(bytes))
    }

    pub fn shown(&self) -> String {
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

/// The caches builds fetch from and the keys they trust, gathered from the command line and the
/// configurations the command reads.
static CACHES: Mutex<(Vec<Location>, Vec<TrustedKey>)> = Mutex::new((Vec::new(), Vec::new()));

/// Makes builds fetch from `caches`, as well as those already in use, what a key in `trusted`
/// or one trusted already signed, before building it.
pub fn use_caches(caches: Vec<Location>, trusted: Vec<TrustedKey>) {
    let mut using = CACHES.lock().expect("the caches in use are readable");
    for cache in caches {
        if !using.0.contains(&cache) {
            using.0.push(cache);
        }
    }
    for key in trusted {
        if !using.1.iter().any(|known| known.key == key.key) {
            using.1.push(key);
        }
    }
}

/// Fetches the build at `out` under fix-up `rules` from the caches in use, writing its fix-up
/// record at `fixed` once every path it made is in the store. Says whether it did. A cache that
/// can't be read, or holds something that doesn't match its trace, is warned about and skipped.
pub(crate) fn fetch(lab: &Lab, out: &StorePath, rules: &str, fixed: &Path) -> Result<bool, String> {
    let (caches, trusted) = CACHES
        .lock()
        .expect("the caches in use are readable")
        .clone();
    if caches.is_empty() {
        return Ok(false);
    }
    if trusted.is_empty() {
        static SAID: OnceLock<()> = OnceLock::new();
        SAID.get_or_init(|| {
            eprintln!(
                "warning: no cache key is trusted, so nothing is fetched from {}; a system's \
                 configuration trusts one with `keys` under `[caches.NAME]`",
                caches
                    .iter()
                    .map(Location::shown)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        });
        return Ok(false);
    }
    for cache in caches.iter().filter(|cache| !is_down(cache)) {
        match fetch_from(lab, cache, &trusted, out.hash(), rules, fixed) {
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
    // A cache files a trace under its signer's name; keys can share one, as an old and a new.
    let names: BTreeSet<&str> = trusted.iter().map(|key| key.name.as_str()).collect();
    for name in names {
        let bytes = match cache.read(&trace_name(key, rules, name), &partial) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(e) => {
                mark_down(cache, &e);
                return Ok(false);
            }
        };
        let signed: Signed = match serde_json::from_slice(&bytes) {
            Ok(signed) => signed,
            Err(e) => {
                warn_once(cache, &format!("it holds traces wrath can't read ({e})"));
                continue;
            }
        };
        if !signed_by(trusted, name, &signed) {
            warn_once(
                cache,
                &format!("it holds traces signed by a key named {name} that isn't trusted"),
            );
            continue;
        }
        let trace = match signed.trace() {
            Ok(trace) => trace,
            Err(e) => {
                warn_once(cache, &e);
                continue;
            }
        };
        let refused = if !names_store_paths(&trace) {
            Some("it holds a trace naming something that isn't a store path".to_owned())
        } else if trace
            .paths
            .values()
            .any(|archived| archived.listing.is_none())
        {
            Some("it holds traces from an older wrath; push to it again".to_owned())
        } else if trace.key != key || trace.rules != rules {
            Some(format!(
                "a trace filed under {key} is signed for another key"
            ))
        } else {
            None
        };
        if let Some(why) = refused {
            warn_once(cache, &why);
            continue;
        }
        let kept = Kept {
            cache: cache.shown(),
            signer: name.to_owned(),
            signed,
            fixed: fixed.to_path_buf(),
        };
        keep(lab, &kept, &trace)?;
        return Ok(true);
    }
    Ok(false)
}

/// A trace a trusted key signed, kept in the work directory while what it names isn't all in the
/// store: where it came from, who signed it, the trace as it came, and the fix-up record it led to.
#[derive(Serialize, Deserialize)]
struct Kept {
    cache: String,
    signer: String,
    signed: Signed,
    fixed: PathBuf,
}

impl Kept {
    /// The name it's kept under, its key and rules.
    fn id(trace: &Trace) -> String {
        format!("{}-{}", trace.key, trace.rules)
    }

    fn file(lab: &Lab, id: &str) -> PathBuf {
        lab.work.join("traces").join(id)
    }

    fn read(lab: &Lab, id: &str) -> Option<Self> {
        serde_json::from_slice(&fs::read(Self::file(lab, id)).ok()?).ok()
    }
}

/// Keeps the verified `kept` and what it says: each path it names is fetched later, when
/// something runs or builds with it, and its dependents' keys need only its listing and
/// references meanwhile.
fn keep(lab: &Lab, kept: &Kept, trace: &Trace) -> Result<(), String> {
    let id = Kept::id(trace);
    write_whole(
        &Kept::file(lab, &id),
        &serde_json::to_vec(kept).expect("a trace serializes"),
    )?;
    for (path, archived) in &trace.paths {
        let store_path = StorePath::from_hash(path);
        if lab.on_host(&store_path).exists() {
            continue;
        }
        write_whole(&fetchable_record(lab, path), id.as_bytes())?;
        let refs: String = archived.refs.iter().map(|r| format!("{r}\n")).collect();
        write_whole(
            &crate::chain::references_record(lab, &store_path),
            refs.as_bytes(),
        )?;
    }
    write_whole(&kept.fixed, trace.record.as_bytes())
}

/// Undoes what keeping the trace `id` recorded, for the paths it names that aren't in the store,
/// so their package counts as neither built nor fetched and is built the next time.
fn forget(lab: &Lab, id: &str, kept: &Kept) {
    if let Ok(trace) = kept.signed.trace() {
        for path in trace.paths.keys() {
            let store_path = StorePath::from_hash(path);
            if !lab.on_host(&store_path).exists() {
                let _ = fs::remove_file(fetchable_record(lab, path));
                let _ = fs::remove_file(crate::chain::references_record(lab, &store_path));
            }
        }
    }
    let _ = fs::remove_file(&kept.fixed);
    let _ = fs::remove_file(Kept::file(lab, id));
}

/// Where a path traced and not in the store yet says which kept trace names it.
fn fetchable_record(lab: &Lab, path: &str) -> PathBuf {
    lab.work.join("fetchable").join(path)
}

/// The kept trace that names `path`, and what it says of it, when `path` is traced and not in the
/// store yet.
fn fetchable(lab: &Lab, path: &StorePath) -> Option<(String, Kept, Archived)> {
    if lab.on_host(path).exists() {
        return None;
    }
    let id = fs::read_to_string(fetchable_record(lab, path.hash())).ok()?;
    let kept = Kept::read(lab, &id)?;
    let archived = kept.signed.trace().ok()?.paths.remove(path.hash())?;
    Some((id, kept, archived))
}

/// The cache that traced `path` and the size of its archive, when it's traced and not in the
/// store yet.
pub fn traced(lab: &Lab, path: &StorePath) -> Option<(String, u64)> {
    fetchable(lab, path).map(|(_, kept, archived)| (kept.cache, archived.size))
}

/// Whether every path `trace` names, and every one they refer to, is a store path's name: a hash
/// in hex and a name, nothing that could reach outside the store or the work directory.
fn names_store_paths(trace: &Trace) -> bool {
    let shaped = |name: &str| {
        let (hash, rest) = name.split_at(name.len().min(40));
        let named = match rest.strip_prefix('-') {
            Some(rest) => !rest.is_empty() && !rest.contains('/'),
            None => rest.is_empty(),
        };
        hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) && named
    };
    trace
        .paths
        .iter()
        .all(|(path, archived)| shaped(path) && archived.refs.iter().all(|r| shaped(r)))
}

fn signed_by(trusted: &[TrustedKey], name: &str, signed: &Signed) -> bool {
    trusted
        .iter()
        .filter(|key| key.name == name)
        .any(|key| key.verifies(signed))
}

/// Says what's wrong with `cache` once a command, however many traces it shows it in.
fn warn_once(cache: &Location, what: &str) {
    static WARNED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    if WARNED
        .lock()
        .expect("the warnings are writable")
        .insert(cache.shown())
    {
        eprintln!("warning: cache {}: {what}", cache.shown());
    }
}

/// The caches in use, each with whether it couldn't be reached this command.
pub fn asked() -> Vec<(String, bool)> {
    let caches = CACHES.lock().expect("the caches in use are readable");
    caches
        .0
        .iter()
        .map(|cache| (cache.shown(), is_down(cache)))
        .collect()
}

/// The caches that couldn't be reached this command, which aren't asked again.
static DOWN: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

fn is_down(cache: &Location) -> bool {
    !reachable(&cache.shown())
}

/// Whether the cache shown as `shown` hasn't failed to answer this command.
pub fn reachable(shown: &str) -> bool {
    !DOWN
        .lock()
        .expect("the caches down are readable")
        .contains(shown)
}

/// Leaves `cache` out for the rest of the command, saying so the first time.
fn mark_down(cache: &Location, why: &str) {
    let shown = cache.shown();
    if DOWN
        .lock()
        .expect("the caches down are readable")
        .insert(shown.clone())
    {
        eprintln!(
            "warning: cache {shown} can't be reached ({why}); \
             what it has is built instead this time"
        );
    }
}

/// What a cache's trace lists of `path`, when it's traced and not in the store yet.
fn listing(lab: &Lab, path: &StorePath) -> Option<Listing> {
    fetchable(lab, path).and_then(|(_, _, archived)| archived.listing)
}

/// Whether a cache traced `path`, not in the store, as holding nothing, as a main output a split
/// emptied does: nothing refers to it or needs it, so nothing fetches it. Its listing has no
/// directories; a store path keeps what it holds in directories.
pub(crate) fn holds_nothing(lab: &Lab, path: &StorePath) -> bool {
    listing(lab, path).is_some_and(|listing| listing.dirs.is_empty())
}

/// Whether `path` has a directory at `relative`, from the store or a cache's listing of it. One
/// under a linked directory, which a listing doesn't follow, brings the path into the store.
pub(crate) fn has_dir(lab: &Lab, path: &StorePath, relative: &str) -> Result<bool, String> {
    if let Some(listing) = listing(lab, path) {
        let under_link = relative
            .match_indices('/')
            .any(|(at, _)| listing.linked.contains(&relative[..at]));
        if !under_link {
            return Ok(listing.dirs.contains(relative));
        }
        materialize(lab, path)?;
    }
    Ok(lab.on_host(path).join(relative).is_dir())
}

/// Whether `path` has a file `name` directly in `lib`, from the store or a cache's listing of it.
pub(crate) fn has_lib_file(lab: &Lab, path: &StorePath, name: &str) -> bool {
    match listing(lab, path) {
        Some(listing) => listing.lib.contains(name),
        None => lab.on_host(path).join("lib").join(name).exists(),
    }
}

/// The `.pc` files of `path`, as each one's path under it and its text, in order of path, from
/// the store or a cache's listing of it.
pub(crate) fn pc_files(lab: &Lab, path: &StorePath) -> Result<BTreeMap<String, String>, String> {
    if let Some(listing) = listing(lab, path) {
        return Ok(listing.pc);
    }
    pc_of(&lab.on_host(path))
}

/// How many paths, and how many bytes of archives, this command has fetched since it last said.
static FETCHED: Mutex<(usize, u64)> = Mutex::new((0, 0));

/// Puts `path` into the store, when it's traced and not there yet: from the cache that traced it,
/// or any other in use, as long as a key trusted now signed its trace. A path no cache can give
/// takes its trace with it: the package is built the next time, and this command fails saying so.
/// On a terminal one line says what's being fetched; `--verbose` gives a line per path.
pub fn materialize(lab: &Lab, path: &StorePath) -> Result<(), String> {
    let (caches, trusted) = CACHES
        .lock()
        .expect("the caches in use are readable")
        .clone();
    materialize_from(lab, path, &caches, &trusted)
}

/// [`materialize`] from `caches`, trusting `trusted`.
fn materialize_from(
    lab: &Lab,
    path: &StorePath,
    caches: &[Location],
    trusted: &[TrustedKey],
) -> Result<(), String> {
    let Some((id, kept, archived)) = fetchable(lab, path) else {
        return Ok(());
    };
    let name = path.name().unwrap_or(path.hash());
    let (count, _) = *FETCHED.lock().expect("what was fetched is readable");
    match crate::progress::mode() {
        crate::progress::Mode::Live => eprint!("\r\x1b[2K  fetch  {name}  ({count} so far)"),
        crate::progress::Mode::Verbose => eprintln!("  fetch  {name}"),
        crate::progress::Mode::Lines => {}
    }
    if let Err(why) = fetch_path(lab, path, &kept, &archived, caches, trusted) {
        forget(lab, &id, &kept);
        return Err(format!(
            "{name} couldn't be fetched: {why}; what needs it is built the next time instead"
        ));
    }
    // Added to as it is now, since others may have fetched meanwhile.
    let mut fetched = FETCHED.lock().expect("what was fetched is writable");
    *fetched = (fetched.0 + 1, fetched.1 + archived.size);
    drop(fetched);
    let _ = fs::remove_file(fetchable_record(lab, path.hash()));
    Ok(())
}

/// How many paths are fetched at once: enough to keep a link busy while each waits on its
/// request, few enough that a cache serving many machines isn't flooded.
const FETCHING_AT_ONCE: usize = 8;

/// Puts every one of `paths` a cache traced and the store lacks into it, as [`materialize`] does
/// one, a few at a time, since each mostly waits on its request.
pub fn materialize_all<'a>(
    lab: &Lab,
    paths: impl IntoIterator<Item = &'a StorePath>,
) -> Result<(), String> {
    let (caches, trusted) = CACHES
        .lock()
        .expect("the caches in use are readable")
        .clone();
    materialize_all_from(lab, paths, &caches, &trusted)
}

/// [`materialize_all`] from `caches`, trusting `trusted`. The first error is the one returned,
/// once the fetches already under way are done.
fn materialize_all_from<'a>(
    lab: &Lab,
    paths: impl IntoIterator<Item = &'a StorePath>,
    caches: &[Location],
    trusted: &[TrustedKey],
) -> Result<(), String> {
    // The biggest first, so the last to finish isn't a big one started late.
    let mut pending: Vec<(u64, &StorePath)> = paths
        .into_iter()
        .filter_map(|path| traced(lab, path).map(|(_, size)| (size, path)))
        .collect();
    pending.sort_by_key(|&(size, _)| std::cmp::Reverse(size));
    let pending: Vec<&StorePath> = pending.into_iter().map(|(_, path)| path).collect();
    if pending.len() > 1 && crate::progress::mode() == crate::progress::Mode::Verbose {
        let at_once = FETCHING_AT_ONCE.min(pending.len());
        eprintln!("  fetch  {} paths, {at_once} at a time", pending.len());
    }
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..FETCHING_AT_ONCE.min(pending.len()) {
            scope.spawn(|| {
                while failed.lock().is_ok_and(|failed| failed.is_none()) {
                    let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(path) = pending.get(at) else {
                        break;
                    };
                    if let Err(e) = materialize_from(lab, path, caches, trusted)
                        && let Ok(mut failed) = failed.lock()
                    {
                        failed.get_or_insert(e);
                    }
                }
            });
        }
    });
    match failed.into_inner() {
        Ok(Some(e)) => Err(e),
        _ => Ok(()),
    }
}

/// Fetches `path`, which `kept` traced as `archived`, from the caches in use.
fn fetch_path(
    lab: &Lab,
    path: &StorePath,
    kept: &Kept,
    archived: &Archived,
    caches: &[Location],
    trusted: &[TrustedKey],
) -> Result<(), String> {
    if !signed_by(trusted, &kept.signer, &kept.signed) {
        return Err(format!(
            "no key trusted now signed its trace, which {} did",
            kept.signer
        ));
    }
    // The cache that traced it first; any other has the same archive under the same digest.
    let mut order: Vec<&Location> = caches.iter().filter(|cache| !is_down(cache)).collect();
    order.sort_by_key(|cache| cache.shown() != kept.cache);
    if order.is_empty() {
        return Err("no cache is in use to fetch it from".to_owned());
    }
    let scratch = lab.work.join("fetching");
    fs::create_dir_all(&scratch).map_err(with_path(&scratch))?;
    let partial = scratch.join(format!("{}.{}", path.hash(), std::process::id()));
    let mut failed = Vec::new();
    for cache in order {
        match unpack(lab, cache, path.hash(), archived, &partial) {
            Ok(()) => return Ok(()),
            Err(e) => failed.push(format!("{}: {e}", cache.shown())),
        }
    }
    Err(failed.join("; "))
}

/// How many paths, and how many bytes, were fetched since this was last asked, clearing the line
/// that said what was being fetched.
pub fn fetched() -> (usize, u64) {
    let mut fetched = FETCHED.lock().expect("what was fetched is writable");
    if fetched.0 > 0 && crate::progress::mode() == crate::progress::Mode::Live {
        eprint!("\r\x1b[2K");
    }
    std::mem::take(&mut *fetched)
}

/// What a store path holds that its dependents' keys read, so a key can be worked out from a
/// trace without the path itself: its directories, the files directly in `lib`, and its `.pc`
/// files. Links count as what they point to, as reading through them would find.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Listing {
    dirs: BTreeSet<String>,
    /// The directories that are links, which the listing doesn't follow.
    linked: BTreeSet<String>,
    lib: BTreeSet<String>,
    /// Each `.pc` file's text, by its path, under `lib/pkgconfig` or `share/pkgconfig`.
    pc: BTreeMap<String, String>,
}

impl Listing {
    /// The listing of the store path at `dir`.
    pub(crate) fn of(dir: &Path) -> Result<Self, String> {
        let mut listing = Self::default();
        let mut pending = vec![String::new()];
        while let Some(relative) = pending.pop() {
            for (name, full) in entries(&dir.join(&relative))? {
                if !full.is_dir() {
                    continue;
                }
                let path = if relative.is_empty() {
                    name
                } else {
                    format!("{relative}/{name}")
                };
                if full.is_symlink() {
                    listing.linked.insert(path.clone());
                } else {
                    pending.push(path.clone());
                }
                listing.dirs.insert(path);
            }
        }
        for (name, full) in entries(&dir.join("lib"))? {
            if full.exists() && !full.is_dir() {
                listing.lib.insert(name);
            }
        }
        listing.pc = pc_of(dir)?;
        Ok(listing)
    }
}

/// The `.pc` files of the store path at `dir`, by path under it.
fn pc_of(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut found = BTreeMap::new();
    for pc_dir in crate::requires::PC_DIRS {
        for (name, full) in entries(&dir.join(pc_dir))? {
            if name.ends_with(".pc") && full.is_file() {
                let text = fs::read_to_string(&full).map_err(with_path(&full))?;
                found.insert(format!("{pc_dir}/{name}"), text);
            }
        }
    }
    Ok(found)
}

/// The entries of `dir` by name, through a link if it is one, or none when it isn't a directory.
fn entries(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let Ok(read) = fs::read_dir(dir) else {
        return Ok(Vec::new());
    };
    read.map(|entry| {
        let entry = entry.map_err(with_path(dir))?;
        Ok((
            entry.file_name().to_string_lossy().into_owned(),
            entry.path(),
        ))
    })
    .collect()
}

/// Puts the store path `path` from `cache` into the store, unless it's there, checking its
/// archive is the one `archived` describes. Nothing of a failed attempt stays in the store, and
/// another command that put it there first is as good.
fn unpack(
    lab: &Lab,
    cache: &Location,
    path: &str,
    archived: &Archived,
    partial: &Path,
) -> Result<(), String> {
    let target = lab.on_host(&StorePath::from_hash(path));
    if target.exists() {
        return Ok(());
    }
    if cache.download(&archive_name(path), partial, archived.size)? == Object::Missing {
        return Err(format!("it doesn't have {path}"));
    }
    let actual = sha256_of(partial)?;
    if actual != archived.archive {
        let _ = fs::remove_file(partial);
        return Err(format!(
            "{path} is {actual}, and its trace says {}",
            archived.archive
        ));
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
    // A trusted signer still gives no setuid program: privileges come from a system's wrappers.
    let moved = unpacked_now
        .map_err(|e| format!("unpacking {path}: {e}"))
        .and_then(|()| crate::settle::settle_modes(&unpacked))
        .and_then(|()| fs::rename(&unpacked, &target).map_err(with_path(&target)));
    match moved {
        Ok(()) => Ok(()),
        Err(e) => {
            remove_unpacked(&unpacked);
            if target.exists() { Ok(()) } else { Err(e) }
        }
    }
}

/// Removes a directory unpacking left, read-only directories and all.
fn remove_unpacked(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(here) = pending.pop() {
        let _ = fs::set_permissions(&here, fs::Permissions::from_mode(0o700));
        for (_, full) in entries(&here).unwrap_or_default() {
            if full.is_dir() && !full.is_symlink() {
                pending.push(full);
            }
        }
    }
    let _ = fs::remove_dir_all(dir);
}

/// What a push did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pushed {
    pub traces: usize,
    pub archives: usize,
    /// Seed tarballs, which a machine with none fetches to build from.
    pub seeds: usize,
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
                    size: fs::metadata(&archive).map_err(with_path(&archive))?.len(),
                    refs,
                    listing: Some(Listing::of(&lab.on_host(&StorePath::from_hash(path)))?),
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
        // One this wrath can't read, from an older format, is replaced; one that reads and says
        // something else is a build that came out differently, and stays as it was.
        let theirs = fs::read(&file).ok().and_then(|bytes| {
            let signed: Signed = serde_json::from_slice(&bytes).ok()?;
            signed.trace().ok()
        });
        match theirs {
            Some(theirs) if theirs == trace => continue,
            Some(_) => {
                pushed.disagreeing.push(key.to_owned());
                continue;
            }
            None => {}
        }
        let signed = serde_json::to_vec(&signer.sign(&trace)).expect("a trace serializes");
        write_whole(&file, &signed)?;
        pushed.traces += 1;
    }
    pushed.seeds = push_seeds(lab, dir)?;
    Ok(pushed)
}

/// Where a cache keeps the seed tarball whose SHA-256 is `sha256`. The name is the hash, so what's
/// fetched is checked against it and the cache needs no trust.
fn seed_name(sha256: &str) -> String {
    format!("seed/{sha256}.tar.xz")
}

/// The most a seed tarball may be.
const SEED_LIMIT: u64 = 1 << 30;

/// Copies the seed tarballs `lab` has into the cache at `dir`, each only when it's what its name
/// says, so a machine without the seed can fetch it there and build. How many went up.
fn push_seeds(lab: &Lab, dir: &Path) -> Result<usize, String> {
    let seeds = lab.work.join("seed");
    let Ok(entries) = fs::read_dir(&seeds) else {
        return Ok(0);
    };
    let mut pushed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(sha256) = name.strip_suffix(".tar.xz") else {
            continue;
        };
        let target = dir.join(seed_name(sha256));
        let hashed = sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit());
        if !hashed || target.exists() || sha256_of(&entry.path())? != sha256 {
            continue;
        }
        let parent = target.parent().expect("a seed is in a directory");
        fs::create_dir_all(parent).map_err(with_path(parent))?;
        let partial = target.with_extension(format!("partial{}", std::process::id()));
        fs::copy(entry.path(), &partial).map_err(with_path(&partial))?;
        fs::rename(&partial, &target).map_err(with_path(&target))?;
        pushed += 1;
    }
    Ok(pushed)
}

/// Fetches the seed tarball whose SHA-256 is `sha256` into `file` from the first cache in use
/// that has it, which the caller checks against the hash. Whether one had it.
pub fn fetch_seed(sha256: &str, file: &Path) -> Result<bool, String> {
    fetch_seed_from(&caches_in_use(), sha256, file)
}

fn caches_in_use() -> Vec<Location> {
    let using = CACHES.lock().expect("the caches in use are readable");
    using.0.clone()
}

fn fetch_seed_from(caches: &[Location], sha256: &str, file: &Path) -> Result<bool, String> {
    let mut failed = Vec::new();
    for cache in caches.iter().filter(|cache| !is_down(cache)) {
        match cache.download(&seed_name(sha256), file, SEED_LIMIT) {
            Ok(Object::Got) => return Ok(true),
            Ok(Object::Missing) => {}
            Err(e) => failed.push(format!("{}: {e}", cache.shown())),
        }
    }
    if failed.is_empty() {
        Ok(false)
    } else {
        Err(format!("fetching the seed: {}", failed.join("; ")))
    }
}

/// Whether a cache in use has the seed tarball whose SHA-256 is `sha256`, asked without fetching
/// it.
pub fn has_seed(sha256: &str) -> bool {
    let name = seed_name(sha256);
    caches_in_use()
        .iter()
        .filter(|cache| !is_down(cache))
        .any(|cache| match cache {
            Location::Dir(dir) => dir.join(&name).is_file(),
            Location::Http(base) => crate::http::exists(&format!("{base}/{name}")) == Ok(true),
        })
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
        let mut signed = key.sign(&trace);
        assert!(trusted.verifies(&signed));
        assert_eq!(signed.trace().unwrap(), trace);
        // The signature covers the bytes as sent: a field added or one changed doesn't pass.
        let changed = signed
            .trace
            .replacen(&format!("out={PATH}"), "out=someone-elses", 1);
        let added = signed.trace.replacen('{', "{\"extra\":1,", 1);
        for trace in [changed, added] {
            signed.trace = trace;
            assert!(!trusted.verifies(&signed));
        }
        // Nor does the same trace signed without what marks it as one.
        let bare = Signed {
            signature: hex(&key
                .key
                .sign(serde_json::to_string(&trace).unwrap().as_bytes())
                .to_bytes()),
            trace: serde_json::to_string(&trace).unwrap(),
        };
        assert!(!trusted.verifies(&bare));
        let other = TrustedKey::parse(&SigningKeyFile::generate("x").unwrap().public()).unwrap();
        assert!(!other.verifies(&key.sign(&trace)));
    }

    #[test]
    fn a_trace_names_only_store_paths() {
        let trace = |path: &str, refs: &[&str]| Trace {
            key: KEY.into(),
            rules: "r".into(),
            record: String::new(),
            paths: BTreeMap::from([(
                path.to_owned(),
                Archived {
                    archive: String::new(),
                    refs: refs.iter().map(|r| (*r).to_owned()).collect(),
                    size: 0,
                    listing: None,
                },
            )]),
        };
        assert!(names_store_paths(&trace(PATH, &[PATH, &PATH[..40]])));
        for bad in [
            "../../etc",
            "0123-x",
            "0123456789abcdef0123456789abcdef01234567-a/b",
        ] {
            assert!(!names_store_paths(&trace(bad, &[])), "{bad}");
            assert!(!names_store_paths(&trace(PATH, &[bad])), "{bad}");
        }
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
        // Its trace is enough for a key; the path itself comes when something needs it.
        let path = StorePath::from_hash(PATH);
        assert!(!fresh.on_host(&path).exists());
        assert!(has_dir(&fresh, &path, "bin").unwrap());
        assert!(!has_dir(&fresh, &path, "lib").unwrap());
        let archive = dir.path().join("cache").join(archive_name(PATH));
        let size = fs::metadata(archive).unwrap().len();
        assert_eq!(traced(&fresh, &path), Some((cache.shown(), size)));
        assert!(!holds_nothing(&fresh, &path));
        materialize_from(&fresh, &path, std::slice::from_ref(&cache), &trusted).unwrap();
        let file = fresh.work.join("store").join(PATH).join("bin/hello");
        assert_eq!(fs::read_to_string(file).unwrap(), "#!/bin/sh\necho hello\n");
        assert!(!fetchable_record(&fresh, PATH).exists());
        assert_eq!(traced(&fresh, &path), None);
    }

    #[test]
    fn what_a_cache_traced_is_fetched_together_and_what_is_here_is_left() {
        const LIB: &str = "89abcdef0123456789abcdef0123456789abcdef-hello-1.0";
        let dir = tempfile::tempdir().unwrap();
        let built = lab_with(&dir.path().join("built"), PATH);
        let lib = built.work.join("store").join(LIB).join("lib");
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join("libhello.so"), "elf").unwrap();
        let rules = &crate::fixup::rules()[..16];
        let record = built.work.join("fixed").join(format!("{KEY}-{rules}"));
        fs::write(&record, format!("out={PATH} lib={LIB}")).unwrap();
        let cache = Location::Dir(dir.path().join("cache"));
        let key = SigningKeyFile::generate("home").unwrap();
        push(&built, &cache, &key).unwrap();

        let fresh = Lab::new(dir.path().join("fresh"));
        let fixed = fresh.work.join("fixed").join(format!("{KEY}-{rules}"));
        let trusted = vec![TrustedKey::parse(&key.public()).unwrap()];
        assert!(fetch_from(&fresh, &cache, &trusted, KEY, rules, &fixed).unwrap());
        let paths = [StorePath::from_hash(PATH), StorePath::from_hash(LIB)];
        assert!(paths.iter().all(|path| !fresh.on_host(path).exists()));
        materialize_all_from(&fresh, &paths, std::slice::from_ref(&cache), &trusted).unwrap();
        assert!(paths.iter().all(|path| fresh.on_host(path).exists()));
        let library = fresh.work.join("store").join(LIB).join("lib/libhello.so");
        assert_eq!(fs::read_to_string(library).unwrap(), "elf");
        // Asked again, everything is here and nothing is fetched.
        materialize_all_from(&fresh, &paths, &[], &trusted).unwrap();
    }

    #[test]
    fn the_seed_goes_up_with_a_push_and_comes_back_by_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let built = lab_with(&dir.path().join("built"), PATH);
        let seeds = built.work.join("seed");
        fs::create_dir_all(&seeds).unwrap();
        let tarball = seeds.join("good.tar.xz");
        fs::write(&tarball, "compilers").unwrap();
        let sha256 = sha256_of(&tarball).unwrap();
        fs::rename(&tarball, seeds.join(format!("{sha256}.tar.xz"))).unwrap();
        // One that isn't what its name says stays behind.
        let wrong = "0".repeat(64);
        fs::write(seeds.join(format!("{wrong}.tar.xz")), "something else").unwrap();
        let cache = Location::Dir(dir.path().join("cache"));
        let key = SigningKeyFile::generate("home").unwrap();
        assert_eq!(push(&built, &cache, &key).unwrap().seeds, 1);
        assert_eq!(push(&built, &cache, &key).unwrap().seeds, 0);

        let file = dir.path().join("fetched");
        assert!(fetch_seed_from(std::slice::from_ref(&cache), &sha256, &file).unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), "compilers");
        assert!(!fetch_seed_from(std::slice::from_ref(&cache), &wrong, &file).unwrap());
        assert!(!fetch_seed_from(&[], &sha256, &file).unwrap());
    }

    #[test]
    fn a_listing_answers_what_a_key_reads_as_the_path_itself_would() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let lab = lab_with(&dir.path().join("built"), PATH);
        let store = lab.work.join("store").join(PATH);
        fs::create_dir_all(store.join("lib64/pkgconfig")).unwrap();
        fs::write(store.join("lib64/pkgconfig/hello.pc"), "Requires: zlib\n").unwrap();
        fs::write(store.join("lib64/ld-linux-x86-64.so.2"), "elf").unwrap();
        symlink("lib64", store.join("lib")).unwrap();
        fs::create_dir_all(store.join("share/pkgconfig")).unwrap();
        fs::write(store.join("share/pkgconfig/data.pc"), "Name: data\n").unwrap();
        symlink("missing", store.join("share/gone")).unwrap();

        let listing = Listing::of(&store).unwrap();
        assert_eq!(listing.linked, BTreeSet::from(["lib".to_owned()]));
        assert_eq!(
            listing.lib,
            BTreeSet::from(["ld-linux-x86-64.so.2".to_owned()])
        );
        assert_eq!(pc_of(&store).unwrap(), listing.pc);
        assert_eq!(
            listing.pc.keys().collect::<Vec<_>>(),
            ["lib/pkgconfig/hello.pc", "share/pkgconfig/data.pc"]
        );
        for dir in [
            "bin",
            "lib",
            "share",
            "lib64/pkgconfig",
            "share/gone",
            "nothing",
        ] {
            let found = store.join(dir).is_dir();
            assert_eq!(listing.dirs.contains(dir), found, "{dir}");
        }
        // What a split emptied lists no directories, which is how a cache says it holds nothing.
        let emptied = dir.path().join("emptied");
        fs::create_dir(&emptied).unwrap();
        assert!(Listing::of(&emptied).unwrap().dirs.is_empty());
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
        let trust = |keys: &[&SigningKeyFile]| -> Vec<TrustedKey> {
            keys.iter()
                .map(|key| TrustedKey::parse(&key.public()).unwrap())
                .collect()
        };

        // Another key of the same name doesn't verify it, and the cache is only passed over.
        let impostor = SigningKeyFile::generate("home").unwrap();
        assert!(!fetch_from(&fresh, &cache, &trust(&[&impostor]), KEY, rules, &fixed).unwrap());
        assert!(!fixed.exists());
        // Next to the key that signed it, as when a key is being replaced, it's found.
        let both = trust(&[&impostor, &key]);
        assert!(fetch_from(&fresh, &cache, &both, KEY, rules, &fixed).unwrap());
        // A key no longer trusted when the path is fetched takes the trace with it.
        let path = StorePath::from_hash(PATH);
        let error = materialize_from(&fresh, &path, std::slice::from_ref(&cache), &[]).unwrap_err();
        assert!(error.contains("no key trusted now"), "{error}");
        assert!(!fixed.exists() && traced(&fresh, &path).is_none());
        // A key of another name finds no trace of its own.
        let stranger = SigningKeyFile::generate("elsewhere").unwrap();
        assert!(!fetch_from(&fresh, &cache, &trust(&[&stranger]), KEY, rules, &fixed).unwrap());

        // An archive swapped for another is refused when it's fetched, nothing reaches the store,
        // and the package counts as neither built nor fetched, to be built the next time.
        let good = dir.path().join("good");
        fs::create_dir_all(good.join("paths")).unwrap();
        let archive = dir.path().join("cache").join(archive_name(PATH));
        fs::copy(&archive, good.join(archive_name(PATH))).unwrap();
        fs::write(&archive, b"something else").unwrap();
        let trusted = trust(&[&key]);
        assert!(fetch_from(&fresh, &cache, &trusted, KEY, rules, &fixed).unwrap());
        let error =
            materialize_from(&fresh, &path, std::slice::from_ref(&cache), &trusted).unwrap_err();
        assert!(error.contains("its trace says"), "{error}");
        assert!(error.contains("built the next time"), "{error}");
        assert!(!fresh.work.join("store").join(PATH).exists());
        assert!(!fixed.exists() && traced(&fresh, &path).is_none());
        // Another cache with the archive gives it, whichever traced it.
        assert!(fetch_from(&fresh, &cache, &trusted, KEY, rules, &fixed).unwrap());
        let caches = [cache.clone(), Location::Dir(good)];
        materialize_from(&fresh, &path, &caches, &trusted).unwrap();
        assert!(fresh.on_host(&path).join("bin/hello").exists());
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
    fn a_cache_over_http_gives_a_build_says_what_it_lacks_and_is_left_out_on_anything_else() {
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
        materialize_from(
            &fresh,
            &StorePath::from_hash(PATH),
            std::slice::from_ref(&cache),
            &trusted,
        )
        .unwrap();
        let file = fresh.work.join("store").join(PATH).join("bin/hello");
        assert_eq!(fs::read_to_string(file).unwrap(), "#!/bin/sh\necho hello\n");

        let broken = Location::parse(&serve(dir.path().join("cache"), Some(500)));
        // A cache that answers with an error is left out for the rest of the command.
        assert!(!fetch_from(&fresh, &broken, &trusted, missing, rules, &fixed).unwrap());
        assert!(is_down(&broken));
        assert!(!is_down(&cache));
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
        materialize_from(
            &fresh,
            &StorePath::from_hash(PATH),
            std::slice::from_ref(&cache),
            &trusted,
        )
        .unwrap();
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
