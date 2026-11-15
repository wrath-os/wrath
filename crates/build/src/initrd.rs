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
            "wrath/store/abc/bin/sh",
            Node::File {
                mode: 0o755,
                bytes: b"#!x\n".to_vec(),
            },
        );
        insert(
            &mut nodes,
            "bin",
            Node::Link("wrath/store/abc/bin".to_owned()),
        );
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
        assert_eq!(nodes["wrath/store/abc"], Node::Dir);
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

/// Directories and files a running program never reads: headers, static libraries and manuals.
fn for_building(name: &str) -> bool {
    matches!(name, "include" | "man" | "doc" | "info" | "pkgconfig")
        || name.ends_with(".a")
        || name.ends_with(".la")
}

/// Adds each of `roots`, store paths by hash in the store at `store`, without what's only for
/// building, and the shared libraries their programs load, file by file, wherever those are.
/// Whole closures would bring in every library a package was built against, compilers included.
pub fn add_programs(nodes: &mut Nodes, store: &Path, roots: &[&str]) -> Result<(), String> {
    let mut adder = Adder { nodes, store };
    for root in roots {
        adder.tree(&store.join(root), &format!("wrath/store/{root}"))?;
    }
    Ok(())
}

struct Adder<'a> {
    nodes: &'a mut Nodes,
    store: &'a Path,
}

impl Adder<'_> {
    fn tree(&mut self, from: &Path, to: &str) -> Result<(), String> {
        insert(self.nodes, to, Node::Dir);
        for entry in fs::read_dir(from).map_err(with_path(from))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if for_building(&name) {
                continue;
            }
            let path = entry.path();
            let inside = format!("{to}/{name}");
            if fs::symlink_metadata(&path)
                .map_err(with_path(&path))?
                .is_dir()
            {
                self.tree(&path, &inside)?;
            } else {
                self.file(&path, &inside)?;
            }
        }
        Ok(())
    }

    /// Adds one file, or a link and what it points to, and the libraries a program loads.
    fn file(&mut self, from: &Path, to: &str) -> Result<(), String> {
        if self.nodes.contains_key(to) {
            return Ok(());
        }
        let meta = fs::symlink_metadata(from).map_err(with_path(from))?;
        if meta.is_symlink() {
            let target = fs::read_link(from).map_err(with_path(from))?;
            insert(
                self.nodes,
                to,
                Node::Link(target.to_string_lossy().into_owned()),
            );
            let (host, inside) = match target.strip_prefix("/wrath/store/") {
                Ok(rest) => (
                    self.store.join(rest),
                    format!("wrath/store/{}", rest.display()),
                ),
                Err(_) => {
                    let parent = to.rsplit_once('/').map_or("", |(parent, _)| parent);
                    let from_parent = from.parent().unwrap_or(Path::new("/"));
                    (
                        from_parent.join(&target),
                        format!("{parent}/{}", target.display()),
                    )
                }
            };
            if host.is_file() && !inside.contains("..") {
                self.file(&host, &inside)?;
            }
            return Ok(());
        }
        let bytes = fs::read(from).map_err(with_path(from))?;
        let mode = meta.permissions().mode() & 0o7777;
        let wanted = crate::runpath::needed_and_run_path(&bytes);
        let names = loaded_by_name(&bytes);
        insert(self.nodes, to, Node::File { mode, bytes });
        if let Some((needed, runpath)) = wanted {
            let dirs: Vec<&str> = runpath
                .split(':')
                .filter(|dir| dir.starts_with("/wrath/store/"))
                .collect();
            let mut sonames = needed;
            sonames.extend(names);
            if sonames.iter().any(|name| name == "libc.so.6") {
                // glibc loads the module that reads /etc/passwd only when asked for an account.
                sonames.push("libnss_files.so.2".to_owned());
            }
            for soname in sonames {
                let found = dirs.iter().find_map(|dir| {
                    let rest = dir.trim_start_matches("/wrath/store/");
                    let host = self.store.join(rest).join(&soname);
                    host.exists()
                        .then(|| (host, format!("wrath/store/{rest}/{soname}")))
                });
                if let Some((host, inside)) = found {
                    self.file(&host, &inside)?;
                }
            }
        }
        Ok(())
    }
}

/// The libraries a program names in its text to load when it needs them, such as
/// `libcryptsetup.so.12`, which systemd opens only to unlock a disk.
fn loaded_by_name(bytes: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    for (at, _) in bytes.windows(3).enumerate().filter(|(_, w)| *w == b"lib") {
        if at > 0 && bytes[at - 1] != 0 {
            continue;
        }
        let end = bytes[at..]
            .iter()
            .position(|b| *b == 0)
            .map_or(bytes.len(), |n| at + n);
        let Ok(name) = std::str::from_utf8(&bytes[at..end]) else {
            continue;
        };
        let Some((stem, version)) = name.split_once(".so.") else {
            continue;
        };
        let plain = |c: char| c.is_ascii_alphanumeric() || "_+-".contains(c);
        if name.len() < 64
            && stem.chars().all(plain)
            && !version.is_empty()
            && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod program_tests {
    use super::*;

    #[test]
    fn names_of_libraries_to_load_later_are_found_in_a_programs_text() {
        let bytes = b"\0libcryptsetup.so.12\0libkmod.so.2\0not libc.so.6\0libx.so\0\0";
        assert_eq!(
            loaded_by_name(bytes),
            ["libcryptsetup.so.12", "libkmod.so.2"]
        );
    }

    #[test]
    fn a_package_comes_without_what_is_only_for_building() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path();
        let root = store.join("a".repeat(40));
        for path in [
            "bin/tool",
            "include/tool.h",
            "lib/libtool.a",
            "share/man/tool.1",
        ] {
            let file = root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, "x").unwrap();
        }
        let mut nodes = Nodes::new();
        add_programs(&mut nodes, store, &[&"a".repeat(40)]).unwrap();
        let files: Vec<&String> = nodes
            .iter()
            .filter(|(_, node)| matches!(node, Node::File { .. }))
            .map(|(path, _)| path)
            .collect();
        assert_eq!(files, [&format!("wrath/store/{}/bin/tool", "a".repeat(40))]);
    }
}
