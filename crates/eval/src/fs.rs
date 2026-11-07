//! Where configuration files come from: a directory, or memory in tests.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// Read access to a configuration tree. Paths are relative and use `/`.
pub trait ConfigFs {
    fn read(&self, path: &str) -> io::Result<String>;

    /// A file that may not be text, such as an encrypted secret.
    fn read_bytes(&self, path: &str) -> io::Result<Vec<u8>>;

    /// TOML files under `dir`, recursively, as sorted relative paths. A missing directory has none.
    fn toml_files(&self, dir: &str) -> io::Result<Vec<String>>;

    /// How locations refer to a file.
    fn display(&self, path: &str) -> String;

    /// The files of the source `name`, which follows `url`, for including them. The
    /// configuration's lock says which version.
    fn source(&self, name: &str, url: &str) -> Result<Box<dyn ConfigFs>, String> {
        let _ = url;
        Err(format!("source `{name}` isn't available here"))
    }
}

/// A configuration directory on disk.
#[derive(Debug, Clone)]
pub struct DirFs {
    root: PathBuf,
}

impl DirFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl ConfigFs for DirFs {
    fn read(&self, path: &str) -> io::Result<String> {
        std::fs::read_to_string(self.root.join(path))
    }

    fn read_bytes(&self, path: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.root.join(path))
    }

    fn toml_files(&self, dir: &str) -> io::Result<Vec<String>> {
        let mut files = Vec::new();
        collect_toml(&self.root, Path::new(dir), &mut files)?;
        files.sort();
        Ok(files)
    }

    fn display(&self, path: &str) -> String {
        self.root.join(path).display().to_string()
    }
}

fn collect_toml(root: &Path, dir: &Path, files: &mut Vec<String>) -> io::Result<()> {
    let entries = match std::fs::read_dir(root.join(dir)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let relative = dir.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            collect_toml(root, &relative, files)?;
        } else if relative.extension().is_some_and(|ext| ext == "toml") {
            let relative = relative.to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("`{}` is not valid UTF-8", relative.display()),
                )
            })?;
            files.push(relative.to_owned());
        }
    }
    Ok(())
}

/// An in-memory configuration tree, for tests.
#[derive(Debug, Clone, Default)]
pub struct MemFs {
    files: BTreeMap<String, String>,
    sources: BTreeMap<String, MemFs>,
}

impl MemFs {
    pub fn new<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        Self {
            files: files
                .into_iter()
                .map(|(path, text)| (path.to_owned(), text.to_owned()))
                .collect(),
            sources: BTreeMap::new(),
        }
    }

    /// The tree with a source `name` holding `files`.
    #[must_use]
    pub fn with_source(mut self, name: &str, files: MemFs) -> Self {
        self.sources.insert(name.to_owned(), files);
        self
    }
}

impl ConfigFs for MemFs {
    fn read(&self, path: &str) -> io::Result<String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn read_bytes(&self, path: &str) -> io::Result<Vec<u8>> {
        self.read(path).map(String::into_bytes)
    }

    fn toml_files(&self, dir: &str) -> io::Result<Vec<String>> {
        let prefix = format!("{dir}/");
        Ok(self
            .files
            .keys()
            .filter(|path| path.starts_with(&prefix) && path.ends_with(".toml"))
            .cloned()
            .collect())
    }

    fn display(&self, path: &str) -> String {
        path.to_owned()
    }

    fn source(&self, name: &str, _url: &str) -> Result<Box<dyn ConfigFs>, String> {
        match self.sources.get(name) {
            Some(files) => Ok(Box::new(files.clone())),
            None => Err(format!("no source `{name}` in memory")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_lists_toml_files_recursively_and_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("services/web")).unwrap();
        std::fs::write(dir.path().join("services/web/nginx.toml"), "").unwrap();
        std::fs::write(dir.path().join("services/a.toml"), "").unwrap();
        std::fs::write(dir.path().join("services/notes.txt"), "").unwrap();
        let fs = DirFs::new(dir.path());

        assert_eq!(
            fs.toml_files("services").unwrap(),
            ["services/a.toml", "services/web/nginx.toml"]
        );
        assert!(fs.toml_files("users").unwrap().is_empty());
        assert_eq!(
            fs.read("missing.toml").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn memory_lists_toml_files_under_a_directory() {
        let fs = MemFs::new([
            ("system.toml", ""),
            ("services/a.toml", ""),
            ("services/b/c.toml", ""),
            ("servicesx/d.toml", ""),
        ]);

        assert_eq!(
            fs.toml_files("services").unwrap(),
            ["services/a.toml", "services/b/c.toml"]
        );
    }
}
