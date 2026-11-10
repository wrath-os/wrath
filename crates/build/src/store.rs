//! Fixed-length paths under `/store`, and where they are kept on the host.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub(crate) const HASH_LENGTH: usize = 40;

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Turns an I/O error into a message starting with the `path` it happened on.
pub(crate) fn with_path(path: &Path) -> impl FnOnce(std::io::Error) -> String {
    move |e| format!("{}: {e}", path.display())
}

/// A path under `/store`. Every one has the same length, and none has a name in it, so one can be
/// rewritten to another in place.
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

    pub fn hash(&self) -> &str {
        &self.0
    }

    /// The path as builds see it.
    pub fn inside(&self) -> String {
        format!("/store/{}", self.0)
    }
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
