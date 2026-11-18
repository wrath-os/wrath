//! Fetching sources as trees of files, without Nix: a GitHub repository at a commit, an archive at
//! a URL, or a local directory. A tree is pinned by the hash of its files in a fixed order, so the
//! same files have the same hash however they were fetched, and a pinned tree is kept in a cache
//! by that hash.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use wrath_model::Source;

/// How a source's hash is written, so a hash from before wrath fetched sources itself, which
/// was Nix's, is told apart.
const HASH_PREFIX: &str = "sha256-";

/// The hash of the files under `dir`: each path, whether it's executable, and its contents, or
/// for a link its target, in sorted order. Times and owners don't count.
pub fn tree_hash(dir: &Path) -> Result<String, String> {
    let mut hasher = Sha256::new();
    let mut paths = Vec::new();
    collect(dir, Path::new(""), &mut paths)?;
    paths.sort();
    for relative in paths {
        let path = dir.join(&relative);
        let meta = fs::symlink_metadata(&path).map_err(at(&path))?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        if meta.is_symlink() {
            let target = fs::read_link(&path).map_err(at(&path))?;
            hasher.update(b"link\0");
            hasher.update(target.to_string_lossy().as_bytes());
        } else {
            let executable = meta.permissions().mode() & 0o111 != 0;
            hasher.update(if executable { b"exec\0" } else { b"file\0" });
            let bytes = fs::read(&path).map_err(at(&path))?;
            hasher.update(Sha256::digest(&bytes));
        }
        hasher.update(b"\n");
    }
    Ok(format!("{HASH_PREFIX}{:x}", hasher.finalize()))
}

/// Every file and link under `dir`, relative to the tree's top.
fn collect(top: &Path, relative: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    let dir = top.join(relative);
    for entry in fs::read_dir(&dir).map_err(at(&dir))? {
        let entry = entry.map_err(at(&dir))?;
        let path = relative.join(entry.file_name());
        let kind = entry.file_type().map_err(at(&path))?;
        if kind.is_dir() {
            collect(top, &path, paths)?;
        } else {
            paths.push(path);
        }
    }
    Ok(())
}

/// An I/O error, saying which path it happened at.
pub(crate) fn at(path: &Path) -> impl FnOnce(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

/// Where a source at a URL comes from.
#[derive(Debug, PartialEq, Eq)]
pub enum Origin {
    /// A repository on GitHub, at a branch, tag or commit, or its default branch.
    GitHub {
        owner: String,
        repo: String,
        reference: Option<String>,
    },
    /// An archive at an `https://` URL.
    Archive(String),
    /// A directory on this machine.
    Path(PathBuf),
}

/// Where `url` comes from: `github:owner/repo[/ref]`, an `https://` archive, or `path:dir`.
pub fn origin(url: &str) -> Result<Origin, String> {
    if let Some(rest) = url.strip_prefix("github:") {
        let rest = rest.split('?').next().unwrap_or(rest);
        let mut parts = rest.splitn(3, '/');
        let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
            return Err(format!(
                "`{url}` names no repository, as `github:owner/repo`"
            ));
        };
        return Ok(Origin::GitHub {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            reference: parts.next().map(str::to_owned),
        });
    }
    if let Some(path) = url.strip_prefix("path:") {
        return Ok(Origin::Path(PathBuf::from(path)));
    }
    if url.starts_with("https://") {
        return Ok(Origin::Archive(url.to_owned()));
    }
    Err(format!(
        "wrath can't fetch `{url}` yet: it fetches `github:owner/repo`, `https://` archives \
         and `path:` directories"
    ))
}

/// The commit and its date, as `2026-10-30`, from what GitHub's API says of a commit.
fn commit_of(json: &str) -> Result<(String, Option<String>), String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("GitHub's answer: {e}"))?;
    let sha = value["sha"]
        .as_str()
        .ok_or("GitHub's answer has no commit")?
        .to_owned();
    let date = value["commit"]["committer"]["date"]
        .as_str()
        .map(|date| date.chars().take(10).collect());
    Ok((sha, date))
}

fn cache() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/var/cache"));
    base.join("wrath/sources")
}

/// Where GitHub's API is, the one place a GitHub token is sent.
const GITHUB_API: &str = "https://api.github.com/";

/// A token to read GitHub with, for private repositories: `GH_TOKEN` or `GITHUB_TOKEN`, the one
/// the running system's configuration gives in `updates.token`, or else the one `gh` is logged in
/// with.
fn github_token() -> Option<&'static str> {
    static TOKEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    TOKEN
        .get_or_init(|| {
            let given = ["GH_TOKEN", "GITHUB_TOKEN"]
                .iter()
                .filter_map(|name| std::env::var(name).ok())
                .map(|token| token.trim().to_owned())
                .find(|token| !token.is_empty());
            given
                .or_else(crate::config_from::running_token)
                .or_else(|| {
                    let out = std::process::Command::new("gh")
                        .args(["auth", "token"])
                        .stderr(std::process::Stdio::null())
                        .output()
                        .ok()
                        .filter(|out| out.status.success())?;
                    let token = String::from_utf8(out.stdout).ok()?.trim().to_owned();
                    (!token.is_empty()).then_some(token)
                })
        })
        .as_deref()
}

fn download(url: &str, output: &Path) -> Result<(), String> {
    let token = url.starts_with(GITHUB_API).then(github_token).flatten();
    let tried = match wrath_build::http::download_with(url, token, output) {
        Ok(true) => return Ok(()),
        Ok(false) => nothing_at(url, token.is_some()),
        Err(e) => format!("couldn't download {e}"),
    };
    // GitHub refuses a token that may only read other repositories even where none is needed,
    // as for a public one, so it's tried once without; what that says is only kept when it works.
    if token.is_some() && wrath_build::http::download_with(url, None, output) == Ok(true) {
        return Ok(());
    }
    Err(tried)
}

/// What to say when `url` has nothing there, which on GitHub is also how a private repository
/// looks to someone who can't read it.
fn nothing_at(url: &str, token: bool) -> String {
    let mut message = format!("couldn't download {url}: there's nothing there (404)");
    if url.starts_with(GITHUB_API) || url.starts_with("https://github.com/") {
        message.push_str(if token {
            "; if the repository is private, the GitHub token wrath used can't read it"
        } else {
            "; if the repository is private, give a token that can read it in GH_TOKEN or a \
             machine's `updates.token`, or log in with `gh auth login`"
        });
    }
    message
}

/// Unpacks the archive at `url` into a new directory in the cache, dropping its top directory.
fn unpack(url: &str) -> Result<PathBuf, String> {
    let cache = cache();
    fs::create_dir_all(&cache).map_err(at(&cache))?;
    let work = tempfile::tempdir_in(&cache).map_err(at(&cache))?;
    let archive = work.path().join("archive");
    download(url, &archive)?;
    let unpacked = work.path().join("unpacked");
    fs::create_dir(&unpacked).map_err(at(&unpacked))?;
    extract(&archive, &unpacked).map_err(|e| format!("{url}: {e}"))?;
    let tops: Vec<_> = fs::read_dir(&unpacked)
        .map_err(at(&unpacked))?
        .collect::<Result<_, _>>()
        .map_err(at(&unpacked))?;
    let [top] = tops.as_slice() else {
        return Err(format!("{url} doesn't hold everything in one directory"));
    };
    let tree = work.path().join("tree");
    fs::rename(top.path(), &tree).map_err(at(&tree))?;
    Ok(work.keep().join("tree"))
}

/// Extracts the tar archive at `archive`, compressed with gzip or zstd or not at all, into `dir`.
fn extract(archive: &Path, dir: &Path) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Read};
    let file = fs::File::open(archive).map_err(at(archive))?;
    let mut reader = BufReader::new(file);
    let magic = reader.fill_buf().map_err(at(archive))?;
    let decoded: Box<dyn Read> = if magic.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::bufread::MultiGzDecoder::new(reader))
    } else if magic.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Box::new(zstd::Decoder::with_buffer(reader).map_err(at(archive))?)
    } else if magic.get(257..262) == Some(b"ustar") {
        Box::new(reader)
    } else {
        return Err("it isn't a tar archive, plain or compressed with gzip or zstd".into());
    };
    tar::Archive::new(decoded)
        .unpack(dir)
        .map_err(|e| format!("unpacking it: {e}"))
}

/// Moves a fetched tree into the cache by its hash, and returns where it is and the hash.
fn keep(tree: &Path) -> Result<(PathBuf, String), String> {
    let hash = tree_hash(tree)?;
    let cache = cache();
    let kept = cache.join(&hash);
    if !kept.exists() {
        fs::rename(tree, &kept).map_err(at(&kept))?;
    }
    if let Some(work) = tree.parent()
        && work.starts_with(&cache)
    {
        let _ = fs::remove_dir_all(work);
    }
    Ok((kept, hash))
}

/// The archive URL of `origin` at `rev`, when it has one. With a GitHub token it's the API's,
/// which reads private repositories too; without one it's the public archive, which has no
/// rate limit.
fn archive_of(origin: &Origin, rev: Option<&str>, token: bool) -> Option<String> {
    let rev = rev.unwrap_or("HEAD");
    match origin {
        Origin::GitHub { owner, repo, .. } if token => {
            Some(format!("{GITHUB_API}repos/{owner}/{repo}/tarball/{rev}"))
        }
        Origin::GitHub { owner, repo, .. } => Some(format!(
            "https://github.com/{owner}/{repo}/archive/{rev}.tar.gz"
        )),
        Origin::Archive(url) => Some(url.clone()),
        Origin::Path(_) => None,
    }
}

/// Where the cache notes which tree each commit fetched had, so a commit seen before isn't
/// downloaded again.
fn revs() -> PathBuf {
    cache().join("revs")
}

/// The tree a commit fetched before had, from the cache, and its hash.
fn kept_at(rev: &str) -> Option<(PathBuf, String)> {
    let hash = fs::read_to_string(revs().join(rev)).ok()?;
    let hash = hash.trim();
    let kept = cache().join(hash);
    kept.is_dir().then(|| (kept, hash.to_owned()))
}

fn remember_rev(rev: &str, hash: &str) {
    let dir = revs();
    if rev.chars().all(|c| c.is_ascii_hexdigit()) && fs::create_dir_all(&dir).is_ok() {
        let _ = fs::write(dir.join(rev), hash);
    }
}

/// The newest version of the source at `url`, pinned, and where its files are.
pub fn follow(url: &str) -> Result<(Source, PathBuf), String> {
    let origin = origin(url)?;
    let (rev, date) = match &origin {
        Origin::GitHub {
            owner,
            repo,
            reference,
        } => {
            let api = format!(
                "https://api.github.com/repos/{owner}/{repo}/commits/{}",
                reference.as_deref().unwrap_or("HEAD")
            );
            let work = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
            download(&api, work.path())?;
            let text = fs::read_to_string(work.path()).map_err(|e| e.to_string())?;
            let (sha, date) = commit_of(&text)?;
            (Some(sha), date)
        }
        Origin::Archive(_) | Origin::Path(_) => (None, None),
    };
    let (path, hash) = match &origin {
        Origin::Path(dir) => (dir.clone(), tree_hash(dir)?),
        Origin::GitHub { .. } if let Some(kept) = rev.as_deref().and_then(kept_at) => kept,
        Origin::GitHub { .. } | Origin::Archive(_) => {
            let url = archive_of(&origin, rev.as_deref(), github_token().is_some())
                .ok_or("no archive to fetch")?;
            let (path, hash) = keep(&unpack(&url)?)?;
            if let Some(rev) = &rev {
                remember_rev(rev, &hash);
            }
            (path, hash)
        }
    };
    let source = Source {
        url: url.to_owned(),
        rev,
        hash,
        date,
    };
    Ok((source, path))
}

/// The tree `source` pins, from the cache when it's there already. A source pinned before
/// wrath fetched sources itself is pinned by its revision, and its files are taken from it.
pub fn fetch(source: &Source) -> Result<PathBuf, String> {
    let origin = origin(&source.url)?;
    if let Origin::Path(dir) = &origin {
        return Ok(dir.clone());
    }
    let ours = source.hash.starts_with(HASH_PREFIX);
    let cached = cache().join(&source.hash);
    if ours && cached.is_dir() {
        return Ok(cached);
    }
    let url = archive_of(&origin, source.rev.as_deref(), github_token().is_some())
        .ok_or("no archive to fetch")?;
    let (path, hash) = keep(&unpack(&url)?)?;
    if ours && hash != source.hash {
        return Err("its files changed since the lock pinned them".into());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("modules")).unwrap();
        fs::write(dir.path().join("system.toml"), "[system]\n").unwrap();
        fs::write(dir.path().join("modules/a.star"), "x = 1\n").unwrap();
        dir
    }

    #[test]
    fn the_same_files_hash_the_same_whenever_they_were_written() {
        let (one, two) = (tree(), tree());
        Command::new("touch")
            .args(["-d", "2001-01-01"])
            .arg(two.path().join("system.toml"))
            .status()
            .unwrap();
        let hash = tree_hash(one.path()).unwrap();
        assert!(hash.starts_with("sha256-"), "{hash}");
        assert_eq!(tree_hash(two.path()).unwrap(), hash);
    }

    #[test]
    fn a_changed_file_or_mode_changes_the_hash() {
        let dir = tree();
        let before = tree_hash(dir.path()).unwrap();
        let file = dir.path().join("modules/a.star");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        let executable = tree_hash(dir.path()).unwrap();
        assert_ne!(executable, before);
        fs::write(&file, "x = 2\n").unwrap();
        assert_ne!(tree_hash(dir.path()).unwrap(), executable);
    }

    #[test]
    fn urls_say_where_a_source_comes_from() {
        assert_eq!(
            origin("github:johnae/wrath-machines").unwrap(),
            Origin::GitHub {
                owner: "johnae".into(),
                repo: "wrath-machines".into(),
                reference: None,
            }
        );
        assert_eq!(
            origin("github:johnae/wrath/v1").unwrap(),
            Origin::GitHub {
                owner: "johnae".into(),
                repo: "wrath".into(),
                reference: Some("v1".into()),
            }
        );
        assert_eq!(
            origin("path:/srv/config").unwrap(),
            Origin::Path("/srv/config".into())
        );
        let error = origin("git+ssh://example.org/x").unwrap_err();
        assert!(error.contains("can't fetch"), "{error}");
    }

    #[test]
    fn a_github_commit_gives_its_revision_and_day() {
        let json =
            r#"{"sha": "abc123", "commit": {"committer": {"date": "2026-10-30T12:00:00Z"}}}"#;
        assert_eq!(
            commit_of(json).unwrap(),
            ("abc123".to_owned(), Some("2026-10-30".to_owned()))
        );
        let github = origin("github:o/r").unwrap();
        assert_eq!(
            archive_of(&github, Some("abc123"), false).unwrap(),
            "https://github.com/o/r/archive/abc123.tar.gz"
        );
        assert_eq!(
            archive_of(&github, Some("abc123"), true).unwrap(),
            "https://api.github.com/repos/o/r/tarball/abc123"
        );
    }

    #[test]
    fn nothing_on_github_says_how_to_read_a_private_repository() {
        let api = "https://api.github.com/repos/o/r/commits/HEAD";
        assert!(
            nothing_at(api, false).ends_with("`updates.token`, or log in with `gh auth login`")
        );
        assert!(nothing_at(api, true).ends_with("the GitHub token wrath used can't read it"));
        let public = "https://github.com/o/r/archive/HEAD.tar.gz";
        assert!(nothing_at(public, false).contains("GH_TOKEN"));
        assert!(!nothing_at("https://example.com/a.tar.gz", false).contains("private"));
    }

    #[test]
    fn a_local_source_is_its_directory_pinned_by_its_files() {
        let dir = tree();
        let url = format!("path:{}", dir.path().display());
        let (source, path) = follow(&url).unwrap();
        assert_eq!(path, dir.path());
        assert_eq!(source.hash, tree_hash(dir.path()).unwrap());
        assert_eq!(fetch(&source).unwrap(), dir.path());
    }

    /// A tar archive of `top/modules/a.star`, as GitHub's archives hold a repository.
    fn archive() -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_ustar();
        header.set_size(6);
        header.set_mode(0o755);
        builder
            .append_data(&mut header, "top/modules/a.star", &b"x = 1\n"[..])
            .unwrap();
        builder.into_inner().unwrap()
    }

    #[test]
    fn an_archive_plain_or_compressed_with_gzip_or_zstd_extracts_and_anything_else_is_refused() {
        use std::io::Write;
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&archive()).unwrap();
        let formats = [
            ("plain", archive()),
            ("gzip", gzip.finish().unwrap()),
            ("zstd", zstd::encode_all(&archive()[..], 3).unwrap()),
        ];
        for (format, bytes) in formats {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("archive");
            fs::write(&file, bytes).unwrap();
            let out = dir.path().join("out");
            fs::create_dir(&out).unwrap();
            extract(&file, &out).unwrap();
            let star = out.join("top/modules/a.star");
            assert_eq!(fs::read_to_string(&star).unwrap(), "x = 1\n", "{format}");
            let mode = fs::metadata(&star).unwrap().permissions().mode();
            assert_ne!(mode & 0o111, 0, "{format}");
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("archive");
        fs::write(&file, b"<html>not found</html>").unwrap();
        let error = extract(&file, dir.path()).unwrap_err();
        assert!(error.contains("isn't a tar archive"), "{error}");
    }
}
