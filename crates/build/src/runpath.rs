//! Cuts each ELF file's run path down to the directories it can load from.
//!
//! The compiler wrapper gives every program a run path entry per declared dependency, and each
//! entry counts as a run-time reference. The string is overwritten in place, always with something
//! shorter, so no section has to grow and nothing like `patchelf` is needed.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use memchr::memmem;

use crate::store::{Lab, StorePath, with_path};

const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;
const DT_RPATH: u64 = 15;
const DT_RUNPATH: u64 = 29;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const PT_NOTE: u32 = 4;
const NT_GNU_BUILD_ID: u32 = 3;

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

fn string_at(bytes: &[u8], at: usize) -> Option<String> {
    let rest = bytes.get(at..)?;
    let end = rest.iter().position(|b| *b == 0)?;
    String::from_utf8(rest[..end].to_vec()).ok()
}

struct Dynamic {
    needed: Vec<String>,
    runpath_at: usize,
    runpath: String,
}

/// The parts of a 64-bit little-endian ELF file that matter here, if it has a run path.
fn parse(bytes: &[u8]) -> Option<Dynamic> {
    if bytes.get(..6)? != b"\x7fELF\x02\x01" {
        return None;
    }
    let table = usize::try_from(u64_at(bytes, 32)?).ok()?;
    let size = usize::from(u16_at(bytes, 54)?);
    let count = usize::from(u16_at(bytes, 56)?);
    let mut loads = Vec::new();
    let mut dynamic = None;
    for index in 0..count {
        let header = table + index * size;
        let offset = u64_at(bytes, header + 8)?;
        let address = u64_at(bytes, header + 16)?;
        let length = u64_at(bytes, header + 32)?;
        match u32_at(bytes, header)? {
            PT_LOAD => loads.push((address, offset, length)),
            PT_DYNAMIC => dynamic = Some((usize::try_from(offset).ok()?, length)),
            _ => {}
        }
    }
    let (start, length) = dynamic?;
    let (mut needed, mut strtab, mut runpath, mut rpath) = (Vec::new(), None, None, None);
    for entry in (start..start + usize::try_from(length).ok()?).step_by(16) {
        let (tag, value) = (u64_at(bytes, entry)?, u64_at(bytes, entry + 8)?);
        match tag {
            0 => break,
            DT_NEEDED => needed.push(value),
            DT_STRTAB => strtab = Some(value),
            DT_RUNPATH => runpath = Some(value),
            DT_RPATH => rpath = Some(value),
            _ => {}
        }
    }
    let strtab = strtab?;
    let strtab = loads.iter().find_map(|&(address, offset, length)| {
        (strtab >= address && strtab < address + length).then(|| strtab - address + offset)
    })?;
    let strtab = usize::try_from(strtab).ok()?;
    let runpath_at = strtab + usize::try_from(runpath.or(rpath)?).ok()?;
    Some(Dynamic {
        needed: needed
            .into_iter()
            .map(|at| string_at(bytes, strtab + usize::try_from(at).ok()?))
            .collect::<Option<_>>()?,
        runpath_at,
        runpath: string_at(bytes, runpath_at)?,
    })
}

fn library_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'.')
}

/// Every `lib*.so*` name written anywhere in the file, which covers `dlopen` of a literal name and
/// systemd's `.note.dlopen`. A name put together at run time is out of reach.
fn named_libraries(bytes: &[u8]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for at in memmem::find_iter(bytes, b".so") {
        let start = bytes[..at]
            .iter()
            .rposition(|b| !library_byte(*b))
            .map_or(0, |p| p + 1);
        let end = bytes[at..]
            .iter()
            .position(|b| !library_byte(*b))
            .map_or(bytes.len(), |p| at + p);
        let name = &bytes[start..end];
        if name.starts_with(b"lib")
            && let Ok(name) = std::str::from_utf8(name)
        {
            names.insert(name.to_owned());
        }
    }
    names
}

/// The run path to keep: what isn't in the store, the package's own directories, and directories
/// holding a library the file needs or names.
fn keep<'a>(
    runpath: &'a str,
    own: &str,
    wanted: &BTreeSet<String>,
    provides: &mut impl FnMut(&str) -> BTreeSet<String>,
) -> Vec<&'a str> {
    runpath
        .split(':')
        .filter(|dir| {
            !dir.starts_with("/store/")
                || dir.starts_with(own)
                || !provides(dir).is_disjoint(wanted)
        })
        .collect()
}

/// The new contents, if the run path gets shorter.
fn shrink(
    bytes: &[u8],
    own: &str,
    provides: &mut impl FnMut(&str) -> BTreeSet<String>,
) -> Option<(Vec<u8>, Shrunk)> {
    let dynamic = parse(bytes)?;
    let mut wanted = named_libraries(bytes);
    wanted.extend(dynamic.needed.iter().cloned());
    let kept = keep(&dynamic.runpath, own, &wanted, provides);
    let before = dynamic.runpath.split(':').count();
    if kept.len() == before {
        return None;
    }
    let new = kept.join(":");
    let mut out = bytes.to_vec();
    let slot = &mut out[dynamic.runpath_at..dynamic.runpath_at + dynamic.runpath.len()];
    slot.fill(0);
    slot[..new.len()].copy_from_slice(new.as_bytes());
    let shrunk = Shrunk {
        files: 1,
        entries_before: before,
        entries_after: kept.len(),
    };
    Some((out, shrunk))
}

fn headers(bytes: &[u8]) -> Option<Vec<(u32, usize, usize)>> {
    if bytes.get(..6)? != b"\x7fELF\x02\x01" {
        return None;
    }
    let table = usize::try_from(u64_at(bytes, 32)?).ok()?;
    let size = usize::from(u16_at(bytes, 54)?);
    let count = usize::from(u16_at(bytes, 56)?);
    (0..count)
        .map(|index| {
            let header = table + index * size;
            Some((
                u32_at(bytes, header)?,
                usize::try_from(u64_at(bytes, header + 8)?).ok()?,
                usize::try_from(u64_at(bytes, header + 32)?).ok()?,
            ))
        })
        .collect()
}

/// Overwrites the string at `at`, of length `old`, with `new`, which must be no longer.
fn overwrite(bytes: &mut [u8], at: usize, old: usize, new: &str) {
    let slot = &mut bytes[at..at + old];
    slot.fill(0);
    slot[..new.len()].copy_from_slice(new.as_bytes());
}

/// Renames the directories in a file's run path and its interpreter, wherever `rename` says to
/// and the new name fits.
pub fn rename_libraries(bytes: &mut [u8], rename: &dyn Fn(&str) -> Option<String>) {
    if let Some(dynamic) = parse(bytes) {
        let new = dynamic
            .runpath
            .split(':')
            .map(|entry| rename(entry).unwrap_or_else(|| entry.to_owned()))
            .collect::<Vec<_>>()
            .join(":");
        if new.len() <= dynamic.runpath.len() {
            overwrite(bytes, dynamic.runpath_at, dynamic.runpath.len(), &new);
        }
    }
    for (kind, offset, length) in headers(bytes).unwrap_or_default() {
        if kind != PT_INTERP || length == 0 {
            continue;
        }
        if let Some(old) = string_at(bytes, offset)
            && let Some(new) = rename(&old).filter(|new| new.len() <= old.len())
        {
            overwrite(bytes, offset, old.len(), &new);
        }
    }
}

/// Blanks a file's build ID, which is a hash over everything linked in.
pub fn blank_build_id(bytes: &mut [u8]) {
    for (kind, offset, length) in headers(bytes).unwrap_or_default() {
        if kind != PT_NOTE {
            continue;
        }
        let mut at = offset;
        while at + 12 <= offset + length {
            let (Some(name), Some(desc), Some(note)) = (
                u32_at(bytes, at),
                u32_at(bytes, at + 4),
                u32_at(bytes, at + 8),
            ) else {
                break;
            };
            let name_at = at + 12;
            let desc_at = name_at + (name as usize).next_multiple_of(4);
            let is_build_id =
                note == NT_GNU_BUILD_ID && bytes.get(name_at..name_at + 4) == Some(b"GNU\0");
            if is_build_id && let Some(id) = bytes.get_mut(desc_at..desc_at + desc as usize) {
                id.fill(0);
            }
            at = desc_at + (desc as usize).next_multiple_of(4);
        }
    }
}

/// Where a file's run path string is, and what it says.
pub fn location(bytes: &[u8]) -> Option<(usize, String)> {
    let dynamic = parse(bytes)?;
    Some((dynamic.runpath_at, dynamic.runpath))
}

/// The libraries an ELF file needs, with its run path.
pub fn needed_and_run_path(bytes: &[u8]) -> Option<(Vec<String>, String)> {
    let dynamic = parse(bytes)?;
    Some((dynamic.needed, dynamic.runpath))
}

/// The libraries `bytes` needs that aren't in any directory of its run path, leaving out the
/// dynamic loader itself. Files with no run path have nothing to check.
pub fn unresolved(lab: &Lab, bytes: &[u8]) -> Vec<String> {
    let Some(dynamic) = parse(bytes) else {
        return Vec::new();
    };
    let dirs: Vec<BTreeSet<String>> = dynamic
        .runpath
        .split(':')
        .map(|dir| libraries_in(lab, dir))
        .collect();
    dynamic
        .needed
        .into_iter()
        .filter(|name| !name.starts_with("ld-linux"))
        .filter(|name| !dirs.iter().any(|dir| dir.contains(name)))
        .collect()
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Shrunk {
    pub files: usize,
    pub entries_before: usize,
    pub entries_after: usize,
}

impl Shrunk {
    pub fn add(&mut self, other: Self) {
        self.files += other.files;
        self.entries_before += other.entries_before;
        self.entries_after += other.entries_after;
    }
}

fn libraries_in(lab: &Lab, dir: &str) -> BTreeSet<String> {
    let Some(rest) = dir.strip_prefix("/store/") else {
        return BTreeSet::new();
    };
    let (hash, relative) = rest.split_once('/').unwrap_or((rest, ""));
    let host = lab.on_host(&StorePath::from_hash(hash)).join(relative);
    let Ok(entries) = fs::read_dir(host) else {
        return BTreeSet::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".so"))
        .collect()
}

/// Shrinks the run path of every ELF file in `files`, which all belong to `out`.
pub fn shrink_files(
    lab: &Lab,
    out: &StorePath,
    files: &[PathBuf],
    write: impl Fn(&Path, &[u8]) -> Result<(), String>,
) -> Result<Shrunk, String> {
    let own = format!("{}/", out.inside());
    let mut cache = BTreeMap::new();
    let mut provides = |dir: &str| {
        cache
            .entry(dir.to_owned())
            .or_insert_with(|| libraries_in(lab, dir))
            .clone()
    };
    let mut total = Shrunk::default();
    for file in files {
        let meta = fs::symlink_metadata(file).map_err(with_path(file))?;
        if !meta.is_file() {
            continue;
        }
        let bytes = fs::read(file).map_err(with_path(file))?;
        if let Some((new, shrunk)) = shrink(&bytes, &own, &mut provides) {
            write(file, &new)?;
            total.add(shrunk);
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ELF file: one load segment at address 0, a dynamic section and a string table.
    fn elf(runpath: &str, needed: &[&str], extra: &[u8]) -> (Vec<u8>, usize) {
        let mut strings = vec![0u8];
        let runpath_index = strings.len();
        strings.extend_from_slice(runpath.as_bytes());
        strings.push(0);
        let needed: Vec<usize> = needed
            .iter()
            .map(|name| {
                let at = strings.len();
                strings.extend_from_slice(name.as_bytes());
                strings.push(0);
                at
            })
            .collect();
        let headers = 64 + 2 * 56;
        let dynamic_length = (needed.len() + 3) * 16;
        let strtab = headers + dynamic_length;
        let mut bytes = vec![0u8; headers];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&2u16.to_le_bytes());
        let total = (strtab + strings.len() + extra.len()) as u64;
        let mut header = |at: usize, kind: u32, offset: u64, length: u64| {
            bytes[at..at + 4].copy_from_slice(&kind.to_le_bytes());
            bytes[at + 8..at + 16].copy_from_slice(&offset.to_le_bytes());
            bytes[at + 16..at + 24].copy_from_slice(&offset.to_le_bytes());
            bytes[at + 32..at + 40].copy_from_slice(&length.to_le_bytes());
        };
        header(64, PT_LOAD, 0, total);
        header(120, PT_DYNAMIC, headers as u64, dynamic_length as u64);
        let mut push = |tag: u64, value: u64| {
            bytes.extend_from_slice(&tag.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        };
        for at in needed {
            push(DT_NEEDED, at as u64);
        }
        push(DT_STRTAB, strtab as u64);
        push(DT_RUNPATH, runpath_index as u64);
        push(0, 0);
        bytes.extend_from_slice(&strings);
        bytes.extend_from_slice(extra);
        (bytes, strtab + runpath_index)
    }

    fn provider(dir: &str) -> BTreeSet<String> {
        let names: &[&str] = match dir {
            "/store/a/lib" => &["libz.so.1"],
            "/store/b/lib" => &["libkmod.so.2"],
            "/store/c/lib" => &["libunused.so.3"],
            _ => &[],
        };
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    fn runpath_of(bytes: &[u8], at: usize) -> String {
        string_at(bytes, at).unwrap()
    }

    #[test]
    fn keeps_what_is_needed_or_named_and_drops_the_rest() {
        let path = "/store/a/lib:/store/b/lib:/store/c/lib";
        let (bytes, at) = elf(path, &["libz.so.1"], b"\0libkmod.so.2\0");
        let (new, shrunk) = shrink(&bytes, "/store/self/", &mut provider).unwrap();
        assert_eq!(runpath_of(&new, at), "/store/a/lib:/store/b/lib");
        assert_eq!((shrunk.entries_before, shrunk.entries_after), (3, 2));
        assert_eq!(new.len(), bytes.len());
    }

    #[test]
    fn keeps_own_directories_and_anything_outside_the_store() {
        let path = "$ORIGIN/../lib:/store/self/lib/private:/store/c/lib";
        let (bytes, at) = elf(path, &[], b"");
        let (new, ..) = shrink(&bytes, "/store/self/", &mut provider).unwrap();
        assert_eq!(
            runpath_of(&new, at),
            "$ORIGIN/../lib:/store/self/lib/private"
        );
    }

    #[test]
    fn leaves_a_file_alone_when_everything_is_used() {
        let (bytes, _) = elf("/store/a/lib", &["libz.so.1"], b"");
        assert!(shrink(&bytes, "/store/self/", &mut provider).is_none());
    }

    #[test]
    fn can_empty_a_run_path() {
        let (bytes, at) = elf("/store/c/lib", &[], b"");
        let (new, ..) = shrink(&bytes, "/store/self/", &mut provider).unwrap();
        assert_eq!(runpath_of(&new, at), "");
    }

    #[test]
    fn renames_run_path_entries_that_fit() {
        let (mut bytes, at) = elf("/store/a/lib:/store/longer-name/lib", &[], b"");
        rename_libraries(&mut bytes, &|entry| {
            (entry == "/store/a/lib").then(|| "/s/a".to_owned())
        });
        assert_eq!(runpath_of(&bytes, at), "/s/a:/store/longer-name/lib");
    }

    #[test]
    fn leaves_a_run_path_that_would_grow() {
        let (mut bytes, at) = elf("/a", &[], b"");
        rename_libraries(&mut bytes, &|_| Some("/much/longer".to_owned()));
        assert_eq!(runpath_of(&bytes, at), "/a");
    }

    #[test]
    fn ignores_what_is_not_elf() {
        assert!(parse(b"#!/bin/sh\necho hello\n").is_none());
        assert!(parse(b"\x7fELF").is_none());
    }

    #[test]
    fn finds_library_names_in_any_bytes() {
        let names = named_libraries(b"x\0libfoo.so.2\0{\"soname\":[\"libbar-1.so\"]} not.so.1");
        assert!(names.contains("libfoo.so.2"));
        assert!(names.contains("libbar-1.so"));
        assert!(!names.contains("not.so.1"));
    }
}
