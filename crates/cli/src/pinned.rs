//! wrath pinned in the lock like a source, so a configuration is built by the wrath it was
//! locked with: its evaluator, modules and recipes. Running another version of wrath than the
//! locked one needs a builder for it, which only a checkout has so far, so for now a version that
//! isn't the locked one says so and goes on.

use std::path::Path;

use wrath_model::{Lock, Source, WRATH_SOURCE};

/// Says where a packaged wrath was built from, beside its modules: its `rev`, its
/// `hash` and the `url` it lives at, when it has a home.
const SOURCE_FILE: &str = "source.toml";

/// The running wrath as a source, when it was built from a revision. A development build, run
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

/// What the lock pins wrath to, given what it pinned (`current`, the configuration's own source
/// `wrath` when it declares one) and the running wrath. `moving` is `wrath update` taking
/// wrath's newest, which is the running one.
pub fn for_lock(running: Option<Source>, current: Option<&Source>, moving: bool) -> Option<Source> {
    let keep = current.filter(|source| source.is_locked()).cloned();
    if keep.is_some() && !moving {
        return keep;
    }
    let Some(running) = running else {
        if moving && keep.is_some() {
            crate::style::note("this wrath is a development build, so the lock keeps its wrath");
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

/// What to say about the wrath `lock` pins, running as `running`: nothing when it's this one.
fn note(lock: Option<&Lock>, running: Option<&Source>) -> Option<String> {
    let pinned = lock
        .and_then(|lock| lock.sources.get(WRATH_SOURCE))
        .filter(|source| source.is_locked())?;
    match running {
        Some(running) if running.hash == pinned.hash => None,
        Some(running) => Some(format!(
            "the lock pins {}, and this is {}; running another version of wrath needs a builder, \
             so this one goes on, and `wrath update wrath` pins it",
            describe(pinned),
            describe(running)
        )),
        None => Some(format!(
            "this wrath is a development build; the lock pins {}",
            describe(pinned)
        )),
    }
}

/// A wrath version as output names it: its short revision.
fn describe(source: &Source) -> String {
    match &source.rev {
        Some(rev) => format!("wrath {}", &rev[..rev.len().min(7)]),
        None => "another version of wrath".to_owned(),
    }
}

/// Says on stderr when the wrath `lock` pins isn't the one running.
pub fn warn_if_other(root: &Path, lock: Option<&Lock>) {
    if let Some(note) = note(lock, running(root).as_ref()) {
        crate::style::note(&note);
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
        lock.sources.insert(WRATH_SOURCE.to_owned(), source);
        lock
    }

    #[test]
    fn a_packaged_wrath_knows_its_revision_and_a_checkout_doesnt() {
        let source = parse("url = \"git+https://x/wrath\"\nrev = \"abc\"\nhash = \"sha256-h\"\n");
        assert_eq!(
            source,
            Some(Source {
                hash: "sha256-h".into(),
                ..at("abc", "git+https://x/wrath")
            })
        );
        assert_eq!(parse("rev = \"abc\"\nhash = \"h\"\n").unwrap().url, "");
        assert_eq!(parse("hash = \"h\"\n"), None);
    }

    #[test]
    fn the_first_lock_pins_the_running_wrath_and_later_ones_keep_it() {
        let old = at("old", "git+https://x/wrath");
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
    fn updating_pins_the_running_wrath_where_the_configuration_says_it_lives() {
        let old = at("old", "git+https://x/wrath");
        let new = at("new", "");
        assert_eq!(
            for_lock(Some(new.clone()), Some(&old), true),
            Some(at("new", "git+https://x/wrath"))
        );
        assert_eq!(for_lock(None, Some(&old), true), Some(old));
        let declared = Source::follow("git+file:///src/wrath");
        assert_eq!(
            for_lock(Some(new), Some(&declared), false),
            Some(at("new", "git+file:///src/wrath"))
        );
    }

    #[test]
    fn a_wrath_that_isnt_the_locked_one_says_so() {
        let lock = lock_with(at("old", "git+https://x/wrath"));
        assert_eq!(note(None, Some(&at("new", ""))), None);
        assert_eq!(note(Some(&lock), Some(&at("old", ""))), None);
        let other = note(Some(&lock), Some(&at("new", ""))).unwrap();
        assert!(
            other.contains("pins wrath old, and this is wrath new"),
            "{other}"
        );
        assert_eq!(
            note(Some(&lock), None).unwrap(),
            "this wrath is a development build; the lock pins wrath old"
        );
        let unlocked = lock_with(Source::follow("git+https://x/wrath"));
        assert_eq!(note(Some(&unlocked), Some(&at("new", ""))), None);
    }
}
