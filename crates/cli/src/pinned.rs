//! wrath pinned in the lock like a source. A lock that pins wrath has the systems it builds carry
//! that version, built from its tree like any package, so machines following a configuration
//! upgrade wrath when the lock moves it. A lock that doesn't pin it builds the running wrath.

use wrath_model::Source;

/// What the lock pins wrath to, given what it pinned (`current`, or the configuration's own
/// source `wrath` when it declares one). `moving` is `wrath update` moving wrath: it pins the
/// newest commit where wrath lives, as `follow` finds it, at the URL it was pinned from or
/// otherwise wrath's own. Anything else keeps the pin, or pins nothing.
pub fn for_lock(
    current: Option<&Source>,
    moving: bool,
    follow: impl FnOnce(&str) -> Result<Source, String>,
) -> Result<Option<Source>, String> {
    if !moving {
        return Ok(current.filter(|source| source.is_locked()).cloned());
    }
    let url = current
        .map(|source| source.url.as_str())
        .filter(|url| !url.is_empty())
        .unwrap_or(crate::places::WRATH_URL);
    follow(url)
        .map(Some)
        .map_err(|message| format!("source `wrath`: {message}"))
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

    /// Follows any URL to its newest commit, "new".
    fn newest(url: &str) -> Result<Source, String> {
        Ok(at("new", url))
    }

    #[test]
    fn a_lock_keeps_its_wrath_and_pins_none_until_wrath_is_updated() {
        let old = at("old", "github:wrath-os/wrath");
        assert_eq!(for_lock(Some(&old), false, newest), Ok(Some(old.clone())));
        assert_eq!(for_lock(None, false, newest), Ok(None));
        let unlocked = Source::follow("github:acme/wrath");
        assert_eq!(for_lock(Some(&unlocked), false, newest), Ok(None));
    }

    #[test]
    fn updating_pins_the_newest_wrath_where_it_was_pinned_from_or_wraths_own() {
        let old = at("old", "github:acme/wrath");
        assert_eq!(
            for_lock(Some(&old), true, newest),
            Ok(Some(at("new", "github:acme/wrath")))
        );
        assert_eq!(
            for_lock(None, true, newest),
            Ok(Some(at("new", "github:wrath-os/wrath")))
        );
        let declared = Source::follow("github:acme/fork");
        assert_eq!(
            for_lock(Some(&declared), true, newest),
            Ok(Some(at("new", "github:acme/fork")))
        );
        let offline = for_lock(None, true, |_| Err("no network".to_owned()));
        assert_eq!(offline, Err("source `wrath`: no network".to_owned()));
    }
}
