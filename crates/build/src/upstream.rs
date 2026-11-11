//! What upstream has released since a recipe's version, worked out from where the recipe
//! downloads its source: a git host's tags, GNOME's release index, PyPI, or the directory the
//! file is in. The first step towards keeping recipes current with little human work.

use std::cmp::Ordering;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use crate::chain;

/// How to find a project's releases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// A git repository's tags, each `prefix` and a version.
    Git { repository: String, prefix: String },
    /// GNOME's index of a project's releases.
    Gnome { name: String },
    /// A project's releases on PyPI.
    PyPI { name: String },
    /// A directory listing with files named `before`, a version, then `after`.
    Listing {
        dir: String,
        before: String,
        after: String,
    },
}

impl Check {
    /// Where the check looks, for people.
    pub fn place(&self) -> String {
        match self {
            Self::Git { repository, .. } => repository.clone(),
            Self::Gnome { name } => format!("download.gnome.org: {name}"),
            Self::PyPI { name } => format!("pypi.org: {name}"),
            Self::Listing { dir, .. } => dir.clone(),
        }
    }
}

const VERSION: &str = "{version}";
const GIT_HOSTS: [&str; 4] = [
    "github.com",
    "gitlab.freedesktop.org",
    "gitlab.gnome.org",
    "git.sr.ht",
];

/// How to check for releases of a source downloaded from `url`, where `{version}` stands for the
/// version, and `version` is the recipe's. `None` when the URL says nothing usable.
pub fn check_for(url: &str, version: &str) -> Option<Check> {
    let (scheme, rest) = url.split_once("://")?;
    let (host, path) = rest.split_once('/')?;
    let segments: Vec<&str> = path.split('/').collect();
    let file = segments.last()?;
    if GIT_HOSTS.contains(&host) {
        // `owner/repo`, or on GitLab everything before `/-/`, and on sourcehut `~owner/repo`.
        let project: Vec<&str> = match segments.iter().position(|s| *s == "-") {
            Some(end) => segments[..end].to_vec(),
            None => segments.iter().take(2).copied().collect(),
        };
        // The tag is the first segment with the version in it, as `v{version}.tar.gz`.
        let tagged = segments.iter().find(|s| s.contains(VERSION))?;
        let prefix = tagged.split(VERSION).next().unwrap_or_default().to_owned();
        return Some(Check::Git {
            repository: format!("{scheme}://{host}/{}", project.join("/")),
            prefix,
        });
    }
    if host == "download.gnome.org" && segments.first() == Some(&"sources") {
        return Some(Check::Gnome {
            name: (*segments.get(1)?).to_owned(),
        });
    }
    if host == "files.pythonhosted.org" {
        let name = file.split(&format!("-{VERSION}")).next()?;
        return Some(Check::PyPI {
            name: name.to_owned(),
        });
    }
    let dir = &url[..url.len() - file.len()];
    // A directory per release, as `python/3.13.7/`: its parent lists the releases, each linked as
    // `href="<before>3.13.7<after>/"`.
    if let Some(at) = dir.find(VERSION) {
        let start = dir[..at].rfind('/')? + 1;
        let end = at + dir[at..].find('/')?;
        let (before, after) = dir[start..end].split_once(VERSION)?;
        if after.contains(VERSION) || dir[end..].contains(VERSION) {
            return None;
        }
        return Some(Check::Listing {
            dir: dir[..start].to_owned(),
            before: format!("\"{before}"),
            after: format!("{after}/"),
        });
    }
    let (before, after) = file.split_once(VERSION)?;
    let after = after.replace(VERSION, version);
    Some(Check::Listing {
        dir: dir.to_owned(),
        before: before.to_owned(),
        after,
    })
}

/// Runs a command and gives what it printed, for checks to read.
pub type Fetch<'a> = &'a (dyn Fn(&[&str]) -> Result<String, String> + Sync);

/// Every release `check` finds, as upstream names them.
pub fn releases(check: &Check, fetch: Fetch<'_>) -> Result<Vec<String>, String> {
    let versions = match check {
        Check::Git { repository, prefix } => {
            let text = fetch(&["git", "ls-remote", "--tags", "--refs", repository])?;
            text.lines()
                .filter_map(|line| line.split_once("refs/tags/"))
                .filter_map(|(_, tag)| tag.strip_prefix(prefix.as_str()))
                .map(str::to_owned)
                .collect()
        }
        Check::Gnome { name } => {
            let url = format!("https://download.gnome.org/sources/{name}/cache.json");
            download_json(&url, fetch)?[2][name.as_str()]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .filter(|v| gnome_stable(v))
                .collect()
        }
        Check::PyPI { name } => {
            let url = format!("https://pypi.org/pypi/{name}/json");
            download_json(&url, fetch)?["releases"]
                .as_object()
                .into_iter()
                .flat_map(|releases| releases.keys().cloned())
                .collect()
        }
        Check::Listing { dir, before, after } => listed(&download(dir, fetch)?, before, after),
    };
    Ok(versions)
}

fn download(url: &str, fetch: Fetch<'_>) -> Result<String, String> {
    fetch(&[
        "curl",
        "-fsSL",
        "--retry",
        "3",
        "--retry-all-errors",
        "--max-time",
        "30",
        url,
    ])
}

fn download_json(url: &str, fetch: Fetch<'_>) -> Result<serde_json::Value, String> {
    serde_json::from_str(&download(url, fetch)?).map_err(|e| format!("{url}: {e}"))
}

/// The newest of `releases` a recipe at `current` could move to: a release, on the series it
/// `track`s if it says, and numbered like it, so a tag from 2000 such as `20001016` doesn't beat
/// `4.18.0`.
fn newest(releases: &[String], current: &str, track: Option<&str>) -> Option<String> {
    let width = |v: &str| v.split('.').next().unwrap_or_default().len();
    releases
        .iter()
        .filter(|v| is_release(v))
        .filter(|v| track.is_none_or(|series| v.starts_with(series)))
        .filter(|v| width(v).abs_diff(width(current)) <= 2)
        .max_by(|a, b| compare(a, b))
        .cloned()
}

/// The versions in a listing's text of files named `before`, a version, then `after`.
fn listed(text: &str, before: &str, after: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(before) {
        rest = &rest[at + before.len()..];
        // What sits between `before` and `after` counts when it's a version on its own, so
        // `glib-networking-2.80.tar.xz` isn't glib.
        if let Some(end) = rest.find(after)
            && is_release(&rest[..end])
        {
            found.push(rest[..end].to_owned());
        }
    }
    found
}

/// A release, not a candidate: numbers separated by dots, the last one maybe with a letter or a
/// portable release such as OpenSSH's `p1`. x.org's `.99.9xx` are its candidates.
fn is_release(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    let (last, numbers) = parts.split_last().expect("split gives at least one part");
    let digits = last.bytes().take_while(u8::is_ascii_digit).count();
    let suffix = &last[digits..];
    let suffix_ok = suffix.is_empty()
        || (suffix.len() == 1 && suffix.bytes().all(|b| b.is_ascii_lowercase()))
        || suffix
            .strip_prefix('p')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let numeric = |part: &&str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    let candidate = parts
        .iter()
        .skip(1)
        .any(|part| *part == "99" || part.parse::<u64>().is_ok_and(|n| n >= 900));
    digits > 0 && suffix_ok && numbers.iter().all(numeric) && !candidate
}

/// GNOME's older projects mark development releases with an odd minor number, such as glib 2.85.
fn gnome_stable(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) if major < 40 => minor % 2 == 0,
        _ => true,
    }
}

/// Orders versions part by part, numbers as numbers.
fn compare(a: &str, b: &str) -> Ordering {
    let key = |v: &str| -> Vec<(u64, String)> {
        v.split('.')
            .map(|part| {
                let digits = &part[..part.bytes().take_while(u8::is_ascii_digit).count()];
                (digits.parse().unwrap_or(0), part[digits.len()..].to_owned())
            })
            .collect()
    };
    key(a).cmp(&key(b))
}

/// One recipe against upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub id: String,
    pub version: String,
    /// Where it was checked, or why it couldn't be.
    pub place: String,
    /// The newest release, when one was found.
    pub latest: Option<String>,
    /// What went wrong, when the check couldn't run.
    pub error: Option<String>,
}

impl Report {
    pub fn is_outdated(&self) -> bool {
        self.latest
            .as_deref()
            .is_some_and(|latest| compare(latest, &self.version) == Ordering::Greater)
    }
}

/// Every recipe in `repository` against upstream, several at a time.
pub fn outdated(repository: &Path, fetch: Fetch<'_>) -> Result<Vec<Report>, String> {
    let mut recipes = Vec::new();
    for file in crate::graph::recipe_files(repository)? {
        recipes.extend(chain::load(repository, &file)?);
    }
    let queue = Mutex::new(recipes.iter().filter(|recipe| recipe.version.is_some()));
    let reports = Mutex::new(Vec::new());
    // A few at a time: servers such as freedesktop.org's reset connections beyond that.
    std::thread::scope(|scope| {
        for _ in 0..6 {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().expect("the queue isn't poisoned").next();
                    let Some(recipe) = next else { break };
                    let report = report(recipe, fetch);
                    reports
                        .lock()
                        .expect("reports aren't poisoned")
                        .push(report);
                }
            });
        }
    });
    let mut reports = reports.into_inner().expect("reports aren't poisoned");
    reports.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(reports)
}

fn report(recipe: &chain::Package, fetch: Fetch<'_>) -> Report {
    let id = recipe.id.clone();
    let version = recipe.version.clone().unwrap_or_default();
    let url = recipe.source_url();
    // Recipes are loaded with their version in their URLs; checks want it back as `{version}`.
    let check = url
        .as_deref()
        .and_then(|url| check_for(&url.replace(&version, VERSION), &version));
    let Some(check) = check else {
        return Report {
            id,
            version,
            place: url.unwrap_or_else(|| "no source".to_owned()),
            latest: None,
            error: Some("nothing to check its source with".to_owned()),
        };
    };
    let (latest, error) = match releases(&check, fetch) {
        Ok(found) => (newest(&found, &version, recipe.track.as_deref()), None),
        Err(error) => (None, Some(error)),
    };
    Report {
        id,
        version,
        place: check.place(),
        latest,
        error,
    }
}

/// Runs `argv` and gives what it printed.
pub fn run(argv: &[&str]) -> Result<String, String> {
    let (program, args) = argv.split_first().ok_or("nothing to run")?;
    let output = Command::new(program)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{}: {}", argv.join(" "), stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repository: &str, prefix: &str) -> Option<Check> {
        Some(Check::Git {
            repository: repository.to_owned(),
            prefix: prefix.to_owned(),
        })
    }

    #[test]
    fn a_git_hosts_tags_are_checked_with_the_prefix_the_url_puts_before_the_version() {
        let check = |url| check_for(url, "1.0");
        assert_eq!(
            check("https://github.com/benhoyt/inih/archive/refs/tags/r{version}.tar.gz"),
            git("https://github.com/benhoyt/inih", "r")
        );
        assert_eq!(
            check("https://github.com/o/tss/releases/download/{version}/tss-{version}.tar.gz"),
            git("https://github.com/o/tss", "")
        );
        assert_eq!(
            check("https://gitlab.freedesktop.org/pipewire/rtkit/-/archive/v{version}/r.tar.gz"),
            git("https://gitlab.freedesktop.org/pipewire/rtkit", "v")
        );
    }

    #[test]
    fn gnome_pypi_and_plain_directories_have_checks_of_their_own() {
        assert_eq!(
            check_for(
                "https://download.gnome.org/sources/glib/2.84/glib-{version}.tar.xz",
                "2"
            ),
            Some(Check::Gnome {
                name: "glib".to_owned()
            })
        );
        assert_eq!(
            check_for(
                "https://files.pythonhosted.org/packages/m/markupsafe/markupsafe-{version}.tar.gz",
                "3"
            ),
            Some(Check::PyPI {
                name: "markupsafe".to_owned()
            })
        );
        assert_eq!(
            check_for("https://ftp.gnu.org/gnu/sed/sed-{version}.tar.xz", "4.9"),
            Some(Check::Listing {
                dir: "https://ftp.gnu.org/gnu/sed/".to_owned(),
                before: "sed-".to_owned(),
                after: ".tar.xz".to_owned(),
            })
        );
        // A directory per release: its parent lists them.
        assert_eq!(
            check_for(
                "https://www.python.org/ftp/python/{version}/Python-{version}.tar.xz",
                "3"
            ),
            Some(Check::Listing {
                dir: "https://www.python.org/ftp/python/".to_owned(),
                before: "\"".to_owned(),
                after: "/".to_owned(),
            })
        );
    }

    #[test]
    fn a_recipe_moves_only_to_a_release_numbered_like_it_on_the_series_it_tracks() {
        let found: Vec<String> = ["4.18.0", "4.19.1", "20001016", "5.0rc1", "4.20.99.901"]
            .map(str::to_owned)
            .to_vec();
        assert_eq!(newest(&found, "4.18.0", None).as_deref(), Some("4.19.1"));
        assert_eq!(
            newest(&found, "4.18.0", Some("4.18.")).as_deref(),
            Some("4.18.0")
        );
        let portable = ["10.0p1", "10.2p1", "10.2"].map(str::to_owned).to_vec();
        assert_eq!(newest(&portable, "10.0p1", None).as_deref(), Some("10.2p1"));
    }

    #[test]
    fn the_newest_release_wins_and_candidates_and_other_projects_dont_count() {
        let listing = "<a href=\"sed-4.8.tar.xz\">sed-4.8.tar.xz</a>\n\
                       <a href=\"sed-4.10.tar.xz\">sed-4.10.tar.xz</a>\n\
                       <a href=\"sed-4.11rc1.tar.xz\">x</a>\n\
                       <a href=\"sed-tools-9.0.tar.xz\">y</a>\n\
                       <a href=\"sed-4.9.tar.xz.sig\">sig</a>";
        let fetch = |argv: &[&str]| -> Result<String, String> {
            assert_eq!(argv[0], "curl");
            Ok(listing.to_owned())
        };
        let check = check_for("https://ftp.gnu.org/gnu/sed/sed-{version}.tar.xz", "4.8").unwrap();
        let found = releases(&check, &fetch).unwrap();
        assert_eq!(newest(&found, "4.8", None).as_deref(), Some("4.10"));
    }

    #[test]
    fn git_tags_without_the_prefix_or_that_arent_releases_are_left_out() {
        let tags = "a\trefs/tags/r61\nb\trefs/tags/r62\nc\trefs/tags/r63-rc1\nd\trefs/tags/v99\n";
        let fetch = |argv: &[&str]| -> Result<String, String> {
            assert_eq!(&argv[..2], ["git", "ls-remote"]);
            Ok(tags.to_owned())
        };
        let check = git("https://github.com/benhoyt/inih", "r").unwrap();
        let found = releases(&check, &fetch).unwrap();
        assert_eq!(newest(&found, "61", None).as_deref(), Some("62"));
    }

    #[test]
    fn gnome_development_releases_of_older_projects_are_left_out() {
        let index = r#"[4, {}, {"glib": ["2.84.4", "2.85.1", "2.86.0"], "gtk": ["4.19.0"]}, []]"#;
        let fetch = |_: &[&str]| -> Result<String, String> { Ok(index.to_owned()) };
        let check = Check::Gnome {
            name: "glib".to_owned(),
        };
        let found = releases(&check, &fetch).unwrap();
        assert_eq!(newest(&found, "2.84.4", None).as_deref(), Some("2.86.0"));
        assert!(!gnome_stable("2.85.1") && gnome_stable("48.1") && gnome_stable("1.56.4"));
    }

    #[test]
    fn versions_compare_by_number_and_a_report_says_when_upstream_is_newer() {
        assert_eq!(compare("4.10", "4.9"), Ordering::Greater);
        assert_eq!(compare("1.0.2u", "1.0.2t"), Ordering::Greater);
        assert_eq!(compare("2.84", "2.84.0"), Ordering::Less);
        let report = |latest: Option<&str>| Report {
            id: "sed".to_owned(),
            version: "4.9".to_owned(),
            place: String::new(),
            latest: latest.map(str::to_owned),
            error: None,
        };
        assert!(report(Some("4.10")).is_outdated());
        assert!(!report(Some("4.9")).is_outdated());
        assert!(!report(None).is_outdated());
    }
}
