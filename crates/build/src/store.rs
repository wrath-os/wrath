//! Fixed-length paths under `/store`, and where they are kept on the host.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub(crate) const HASH_LENGTH: usize = 40;

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Sets the permissions files are made with to the usual 022, whatever the host's are, so a
/// package's files, which its address covers, come out the same everywhere.
pub(crate) fn usual_umask() {
    rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o022));
}

/// Turns an I/O error into a message starting with the `path` it happened on.
pub(crate) fn with_path(path: &Path) -> impl FnOnce(std::io::Error) -> String {
    move |e| format!("{}: {e}", path.display())
}

/// A path under `/store`: a hash of the same length for every path, then for a package a name,
/// as `<hash>-jq-1.8.1`. A package keeps its name while it moves from where it was built to where
/// its contents put it, so one path can be rewritten to the other in place.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StorePath(String);

impl StorePath {
    /// The path for a `kind` of thing described by `parts`. Each part is hashed with its length,
    /// so different splits of the same bytes differ.
    pub fn derive(kind: &str, parts: &[&[u8]]) -> Self {
        let mut hasher = Sha256::new();
        for part in std::iter::once(kind.as_bytes()).chain(parts.iter().copied()) {
            hasher.update((part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        Self(hex(&hasher.finalize())[..HASH_LENGTH].to_owned())
    }

    /// The path with this hash.
    pub fn from_hash(hash: &str) -> Self {
        Self(hash.to_owned())
    }

    /// The path's entry in `/store`: its hash, and its name when it has one.
    pub fn hash(&self) -> &str {
        &self.0
    }

    /// This path's hash with `name` after it.
    pub fn named(&self, name: &str) -> Self {
        Self(format!("{}-{name}", self.digest()))
    }

    /// The hash the path starts with.
    pub fn digest(&self) -> &str {
        &self.0[..HASH_LENGTH.min(self.0.len())]
    }

    /// The name after the hash, when the path has one.
    pub fn name(&self) -> Option<&str> {
        self.0.get(HASH_LENGTH + 1..)
    }

    /// The path as builds see it.
    pub fn inside(&self) -> String {
        format!("/store/{}", self.0)
    }
}

/// The length of the store entry `bytes` start with: a hash, and a name after it when there is
/// one, up to the first byte a name can't have. `None` when they don't start with a hash.
pub fn entry_length(bytes: &[u8]) -> Option<usize> {
    let hash = bytes.get(..HASH_LENGTH)?;
    if !hash.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let name = match bytes.get(HASH_LENGTH) {
        Some(b'-') => bytes[HASH_LENGTH + 1..]
            .iter()
            .take_while(|b| name_byte(**b))
            .count(),
        _ => 0,
    };
    Some(if name == 0 {
        HASH_LENGTH
    } else {
        HASH_LENGTH + 1 + name
    })
}

/// Whether `byte` can be in a store path's name.
pub(crate) fn name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"._+-".contains(&byte)
}

/// Whether `name` is a whole store entry, such as a directory in `/store`.
pub fn is_entry(name: &str) -> bool {
    entry_length(name.as_bytes()) == Some(name.len())
}

/// The harness's working directory on the host.
#[derive(Debug, Clone)]
pub struct Lab {
    pub work: PathBuf,
}

impl Lab {
    pub fn new(work: impl AsRef<Path>) -> Self {
        Self {
            work: work.as_ref().to_path_buf(),
        }
    }

    /// Where `path` is kept on the host.
    pub fn on_host(&self, path: &StorePath) -> PathBuf {
        self.work.join("store").join(path.hash())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_made_with_the_usual_permissions_whatever_the_hosts() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        // The umask is the whole process's, and other tests make files whose modes their
        // addresses cover, so this runs again in a process of its own.
        const ALONE: &str = "WRATH_TEST_UMASK_ALONE";
        if std::env::var_os(ALONE).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "--quiet"])
                .arg("store::tests::files_are_made_with_the_usual_permissions_whatever_the_hosts")
                .env(ALONE, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }
        rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
        usual_umask();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f");
        fs::write(&file, "").unwrap();
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[test]
    fn an_entry_is_a_hash_and_maybe_a_name() {
        let hash = "a".repeat(HASH_LENGTH);
        assert_eq!(
            entry_length(format!("{hash}/bin").as_bytes()),
            Some(HASH_LENGTH)
        );
        let named = format!("{hash}-jq-1.8.1");
        assert_eq!(
            entry_length(format!("{named}/bin/jq").as_bytes()),
            Some(named.len())
        );
        assert_eq!(
            entry_length(format!("{hash}-/x").as_bytes()),
            Some(HASH_LENGTH)
        );
        assert_eq!(entry_length(b"not a hash"), None);
        assert!(is_entry(&named) && is_entry(&hash));
        assert!(!is_entry(".system") && !is_entry(&format!("{named}/bin")));
    }

    #[test]
    fn every_path_has_the_same_length() {
        let short = StorePath::derive("a", &[b"x"]);
        let long = StorePath::derive("a much longer kind", &[&[7; 4096]]);

        assert_eq!(short.inside().len(), long.inside().len());
        assert_eq!(short.inside().len(), "/store/".len() + HASH_LENGTH);
    }

    #[test]
    fn splitting_the_same_bytes_differently_gives_another_path() {
        assert_ne!(
            StorePath::derive("a", &[b"xy", b"z"]),
            StorePath::derive("a", &[b"x", b"yz"])
        );
    }

    #[test]
    fn the_kind_is_part_of_the_path() {
        assert_ne!(
            StorePath::derive("a", &[b"x"]),
            StorePath::derive("b", &[b"x"])
        );
    }
}
