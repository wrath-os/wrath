//! A cpio archive for the kernel to unpack as its first file system, the same bytes every time.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::store::with_path;

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Dir,
    File { mode: u32, bytes: Vec<u8> },
    Link(String),
    Char { major: u32, minor: u32 },
}

pub type Nodes = BTreeMap<String, Node>;

const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;
const S_IFLNK: u32 = 0o120_000;
const S_IFCHR: u32 = 0o020_000;

/// Adds `path` and the directories above it.
pub fn insert(nodes: &mut Nodes, path: &str, node: Node) {
    let mut parent = path;
    while let Some((above, _)) = parent.rsplit_once('/') {
        nodes.entry(above.to_owned()).or_insert(Node::Dir);
        parent = above;
    }
    nodes.insert(path.to_owned(), node);
}

/// Adds everything under `from` as `to`.
pub fn add_tree(nodes: &mut Nodes, from: &Path, to: &str) -> Result<(), String> {
    insert(nodes, to, Node::Dir);
    for entry in fs::read_dir(from).map_err(with_path(from))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        let inside = format!("{to}/{}", entry.file_name().to_string_lossy());
        let meta = fs::symlink_metadata(&path).map_err(with_path(&path))?;
        if meta.is_symlink() {
            let target = fs::read_link(&path).map_err(with_path(&path))?;
            insert(
                nodes,
                &inside,
                Node::Link(target.to_string_lossy().into_owned()),
            );
        } else if meta.is_dir() {
            add_tree(nodes, &path, &inside)?;
        } else {
            let bytes = fs::read(&path).map_err(with_path(&path))?;
            let mode = meta.permissions().mode() & 0o7777;
            insert(nodes, &inside, Node::File { mode, bytes });
        }
    }
    Ok(())
}

fn pad(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn entry(out: &mut Vec<u8>, ino: u32, mode: u32, rdev: (u32, u32), name: &str, data: &[u8]) {
    let name_size = u32::try_from(name.len() + 1).expect("a short name");
    let size = u32::try_from(data.len()).expect("a file under 4 GiB");
    let nlink = if mode & S_IFDIR == S_IFDIR { 2 } else { 1 };
    let fields = [
        ino, mode, 0, 0, nlink, 0, size, 0, 0, rdev.0, rdev.1, name_size, 0,
    ];
    out.extend_from_slice(b"070701");
    for field in fields {
        out.extend_from_slice(format!("{field:08x}").as_bytes());
    }
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    pad(out);
    out.extend_from_slice(data);
    pad(out);
}

/// The archive in the "new" cpio format, with nothing varying between runs.
pub fn archive(nodes: &Nodes) -> Vec<u8> {
    let mut out = Vec::new();
    for (ino, (name, node)) in (1..).zip(nodes) {
        let (mode, rdev, data): (u32, (u32, u32), &[u8]) = match node {
            Node::Dir => (S_IFDIR | 0o755, (0, 0), &[]),
            Node::File { mode, bytes } => (S_IFREG | mode, (0, 0), bytes),
            Node::Link(target) => (S_IFLNK | 0o777, (0, 0), target.as_bytes()),
            Node::Char { major, minor } => (S_IFCHR | 0o600, (*major, *minor), &[]),
        };
        entry(&mut out, ino, mode, rdev, name, data);
    }
    entry(&mut out, 0, 0, (0, 0), "TRAILER!!!", &[]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Nodes {
        let mut nodes = Nodes::new();
        insert(
            &mut nodes,
            "store/abc/bin/sh",
            Node::File {
                mode: 0o755,
                bytes: b"#!x\n".to_vec(),
            },
        );
        insert(&mut nodes, "bin", Node::Link("store/abc/bin".to_owned()));
        insert(&mut nodes, "dev/console", Node::Char { major: 5, minor: 1 });
        nodes
    }

    #[test]
    fn the_same_files_make_the_same_bytes() {
        assert_eq!(archive(&sample()), archive(&sample()));
    }

    #[test]
    fn parents_are_added_and_every_entry_is_aligned() {
        let nodes = sample();
        assert_eq!(nodes["store/abc"], Node::Dir);
        let bytes = archive(&nodes);
        assert_eq!(bytes.len() % 4, 0);
        assert!(bytes.starts_with(b"070701"));
        let trailer = bytes.windows(10).position(|w| w == b"TRAILER!!!");
        assert!(trailer.is_some());
    }

    #[test]
    fn a_tree_keeps_its_links_and_modes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("run"), "x").unwrap();
        fs::set_permissions(dir.path().join("run"), fs::Permissions::from_mode(0o750)).unwrap();
        std::os::unix::fs::symlink("run", dir.path().join("alias")).unwrap();
        let mut nodes = Nodes::new();
        add_tree(&mut nodes, dir.path(), "here").unwrap();
        assert_eq!(nodes["here/alias"], Node::Link("run".to_owned()));
        let Node::File { mode, .. } = nodes["here/run"] else {
            panic!("not a file");
        };
        assert_eq!(mode, 0o750);
    }
}
