//! distro pinned in the lock like a source, so a configuration is always built by the distro it
//! was locked with: its evaluator, modules and factory. A distro that isn't the locked one runs
//! the locked one instead.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use distro_backend_nixos as backend;
use distro_model::{DISTRO_SOURCE, Lock, Source};

/// Set on the distro run in place of another, to the hash it was run for.
const GUARD: &str = "DISTRO_PINNED";

/// Says where a packaged distro was built from, beside its modules and factory: its `rev`, its
/// `hash` and the `url` it lives at, when it has a home.
const SOURCE_FILE: &str = "source.toml";

/// The running distro as a source, when it was built from a revision. A development build, run
/// from a checkout or built from changes not yet committed, has none and can't be pinned.
pub fn running(root: &Path) -> Option<Source> {
    let text = std::fs::read_to_string(root.join(SOURCE_FILE)).ok()?;
    parse(&text)
}

fn parse(text: &str) -> Option<Source> {
    let file: toml_edit::DocumentMut = text.parse().ok()?;
    let field = |name: &str| file.get(name)?.as_str().map(str::to_owned);
    Some(Source {
        url: field("url").unwrap_or_default(),
        rev: Some(field("rev")?),
        hash: field("hash")?,
        date: None,
    })
}

/// What the lock pins distro to, given what it pinned (`current`, the configuration's own source
/// `distro` when it declares one) and the running distro. `moving` is `distro update` taking
/// distro's newest, which is the running one.
pub fn for_lock(running: Option<Source>, current: Option<&Source>, moving: bool) -> Option<Source> {
    let keep = current.filter(|source| source.is_locked()).cloned();
    if keep.is_some() && !moving {
        return keep;
    }
    let Some(running) = running else {
        if moving && keep.is_some() {
            eprintln!("note: this distro is a development build, so the lock keeps its distro");
        }
        return keep;
    };
    Some(match current {
        Some(current) if !current.url.is_empty() => Source {
            url: current.url.clone(),
            ..running
        },
        _ => running,
    })
}

/// What to do about the distro `lock` pins, running as `running`.
#[derive(Debug, PartialEq)]
enum Pick {
    Run,
    /// A development build runs whatever the lock says, and says so.
    Development(String),
    Pinned(Source),
}

fn pick(lock: Option<&Lock>, running: Option<&Source>, guard: Option<&str>) -> Pick {
    let Some(pinned) = lock
        .and_then(|lock| lock.sources.get(DISTRO_SOURCE))
        .filter(|source| source.is_locked())
    else {
        return Pick::Run;
    };
    if guard == Some(pinned.hash.as_str()) {
        return Pick::Run;
    }
    match running {
        Some(running) if running.hash == pinned.hash => Pick::Run,
        Some(_) => Pick::Pinned(pinned.clone()),
        None => Pick::Development(describe(pinned)),
    }
}

/// A distro version as output names it: its short revision.
fn describe(source: &Source) -> String {
    match &source.rev {
        Some(rev) => format!("distro {}", &rev[..rev.len().min(7)]),
        None => "another distro".to_owned(),
    }
}

/// Runs the distro `lock` pins in place of this one, when it's another. Returns only when this
/// one is the one to run.
pub fn defer(root: &Path, lock: Option<&Lock>) -> Result<(), String> {
    let guard = std::env::var(GUARD).ok();
    match pick(lock, running(root).as_ref(), guard.as_deref()) {
        Pick::Run => Ok(()),
        Pick::Development(pinned) => {
            eprintln!("note: this distro is a development build; the lock pins {pinned}");
            Ok(())
        }
        Pick::Pinned(pinned) => {
            if pinned.url.is_empty() {
                return Err(format!(
                    "the lock pins {}, but not where to get it; declare `[sources.distro]` \
                     with its url, or `distro update distro` pins this one",
                    describe(&pinned)
                ));
            }
            eprintln!("using {} as the lock pins it", describe(&pinned));
            let program = backend::distro_at(&pinned)?;
            let error = Command::new(program)
                .args(std::env::args_os().skip(1))
                .env(GUARD, &pinned.hash)
                .exec();
            Err(format!("cannot run {}: {error}", describe(&pinned)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(rev: &str, url: &str) -> Source {
        Source {
            url: url.to_owned(),
            rev: Some(rev.to_owned()),
            hash: format!("sha256-{rev}"),
            date: None,
        }
    }

    fn lock_with(source: Source) -> Lock {
        let mut lock = Lock::default();
        lock.sources.insert(DISTRO_SOURCE.to_owned(), source);
        lock
    }

    #[test]
    fn a_packaged_distro_knows_its_revision_and_a_checkout_doesnt() {
        let source = parse("url = \"git+https://x/distro\"\nrev = \"abc\"\nhash = \"sha256-h\"\n");
        assert_eq!(
            source,
            Some(Source {
                hash: "sha256-h".into(),
                ..at("abc", "git+https://x/distro")
            })
        );
        assert_eq!(parse("rev = \"abc\"\nhash = \"h\"\n").unwrap().url, "");
        assert_eq!(parse("hash = \"h\"\n"), None);
    }

    #[test]
    fn the_first_lock_pins_the_running_distro_and_later_ones_keep_it() {
        let old = at("old", "git+https://x/distro");
        let new = at("new", "");
        assert_eq!(for_lock(Some(new.clone()), None, false), Some(new.clone()));
        assert_eq!(
            for_lock(Some(new.clone()), Some(&old), false),
            Some(old.clone())
        );
        assert_eq!(for_lock(None, None, false), None);
        assert_eq!(for_lock(None, Some(&old), false), Some(old.clone()));
    }

    #[test]
    fn updating_pins_the_running_distro_where_the_configuration_says_it_lives() {
        let old = at("old", "git+https://x/distro");
        let new = at("new", "");
        assert_eq!(
            for_lock(Some(new.clone()), Some(&old), true),
            Some(at("new", "git+https://x/distro"))
        );
        assert_eq!(for_lock(None, Some(&old), true), Some(old));
        let declared = Source::follow("git+file:///src/distro");
        assert_eq!(
            for_lock(Some(new), Some(&declared), false),
            Some(at("new", "git+file:///src/distro"))
        );
    }

    #[test]
    fn a_distro_that_isnt_the_locked_one_runs_it() {
        let pinned = at("old", "git+https://x/distro");
        let lock = lock_with(pinned.clone());
        assert_eq!(pick(None, Some(&at("new", "")), None), Pick::Run);
        assert_eq!(pick(Some(&lock), Some(&at("old", "")), None), Pick::Run);
        assert_eq!(
            pick(Some(&lock), Some(&at("new", "")), None),
            Pick::Pinned(pinned.clone())
        );
        assert_eq!(
            pick(Some(&lock), None, None),
            Pick::Development("distro old".into())
        );
        // Once run for that pin, it doesn't run another, whatever it says it is.
        assert_eq!(
            pick(Some(&lock), Some(&at("new", "")), Some("sha256-old")),
            Pick::Run
        );
        let unlocked = lock_with(Source::follow("git+https://x/distro"));
        assert_eq!(pick(Some(&unlocked), Some(&at("new", "")), None), Pick::Run);
    }
}
