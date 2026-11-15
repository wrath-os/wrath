//! `[caches]`: binary caches to fetch builds from, each with the keys whose signatures on what it
//! holds are trusted. Part of the configuration's frame, read while loading like `[sources]`.

use std::collections::BTreeMap;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::diag::Diagnostic;
use crate::load::ConfigKind;
use crate::source::{FileId, Span};
use crate::walk::only_keys;

/// The table of caches.
pub(crate) const CACHES: &str = "caches";

/// A cache as a configuration declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredCache {
    /// A directory, or an `http(s)://` URL.
    pub url: String,
    /// The keys trusted to sign what it holds, as `name:hex`. Only a system trusts keys.
    pub keys: Vec<String>,
    pub at: Span,
}

/// Reads `[caches]` from a file of a configuration of `kind` into `declared`.
pub(crate) fn declare(
    value: &Spanned<DeValue<'_>>,
    file: FileId,
    kind: ConfigKind,
    declared: &mut BTreeMap<String, DeclaredCache>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let DeValue::Table(caches) = value.get_ref() else {
        return diagnostics.push(
            Diagnostic::error("`caches` is a table of caches by name")
                .label(Span::new(file, value.span()), "expected `[caches.NAME]`")
                .note("help: for example `[caches.home]` with `url = \"https://...\"`"),
        );
    };
    for (name, cache) in caches.iter() {
        let name = name.get_ref();
        let at = Span::new(file, cache.span());
        let found = match cache.get_ref() {
            DeValue::Table(table) => read(name, table, at, kind),
            _ => Err(
                Diagnostic::error(format!("cache `{name}` is a table with its `url`"))
                    .label(at, "expected `{ url = \"...\" }`"),
            ),
        };
        let cache = match found {
            Ok((url, keys)) => DeclaredCache { url, keys, at },
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                continue;
            }
        };
        match declared.get(name.as_ref()) {
            None => {
                declared.insert(name.to_string(), cache);
            }
            Some(existing) if (&existing.url, &existing.keys) == (&cache.url, &cache.keys) => {}
            Some(existing) => diagnostics.push(
                Diagnostic::error(format!("cache `{name}` is defined twice, differently"))
                    .label(existing.at, "once here")
                    .label(cache.at, "and again here"),
            ),
        }
    }
}

fn read(
    name: &str,
    table: &DeTable<'_>,
    whole: Span,
    kind: ConfigKind,
) -> Result<(String, Vec<String>), Diagnostic> {
    let file = whole.file;
    only_keys(table, &["url", "keys"], "a cache", file)?;
    let url = match table.get("url").map(|url| (url.get_ref(), url.span())) {
        Some((DeValue::String(text), span)) => {
            let scheme = text.split_once("://").map(|(scheme, _)| scheme);
            if scheme.is_some_and(|scheme| !["http", "https", "file"].contains(&scheme)) {
                return Err(Diagnostic::error(format!(
                    "wrath can't fetch from `{text}`: a cache is a directory or an `http(s)://` URL"
                ))
                .label(Span::new(file, span), "not a directory or an HTTP URL"));
            }
            text.to_string()
        }
        Some((other, span)) => {
            return Err(Diagnostic::error(format!(
                "a cache's `url` is a string, found {}",
                other.type_str()
            ))
            .label(
                Span::new(file, span),
                "expected a directory or an `https://` URL",
            ));
        }
        None => {
            return Err(Diagnostic::error(format!("cache `{name}` has no `url`"))
                .label(whole, "expected `url = \"...\"`"));
        }
    };
    let Some(keys) = table.get("keys") else {
        return Ok((url, Vec::new()));
    };
    let at = Span::new(file, keys.span());
    if kind != ConfigKind::System {
        return Err(
            Diagnostic::error("only a system's configuration trusts a cache's keys")
                .label(at, "trusted here")
                .note(
                    "help: list the cache with its `url` alone; what it holds is fetched when \
                     the machine's own configuration trusts a key that signed it",
                ),
        );
    }
    let DeValue::Array(listed) = keys.get_ref() else {
        return Err(Diagnostic::error("a cache's `keys` is a list of keys")
            .label(at, "expected `keys = [\"name:...\"]`"));
    };
    listed
        .iter()
        .map(|key| match key.get_ref() {
            DeValue::String(text) if is_key(text) => Ok(text.to_string()),
            _ => Err(
                Diagnostic::error("a cache key is `name:` and 64 hex digits")
                    .label(Span::new(file, key.span()), "not a key")
                    .note("help: `wrath cache key` prints a key to paste here"),
            ),
        })
        .collect::<Result<_, _>>()
        .map(|keys| (url, keys))
}

/// Whether `text` is a public key as `wrath cache key` prints one: a name, `:`, and 32 bytes in
/// hex.
fn is_key(text: &str) -> bool {
    text.split_once(':').is_some_and(|(name, hex)| {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            && hex.len() == 64
            && hex.chars().all(|c| c.is_ascii_hexdigit())
    })
}
