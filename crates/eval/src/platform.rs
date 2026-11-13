//! Which platform a system builds on: wrath's own modules, or a platform from a source, chosen
//! with `platform` in the starting file.

use std::collections::BTreeMap;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::diag::{Diagnostic, render};
use crate::fs::ConfigFs;
use crate::load::{self, ConfigKind, DeclaredSource, SOURCES};
use crate::source::{FileId, Sources, Span};
use crate::walk::only_keys;

/// The key in the starting file that chooses the platform.
pub(crate) const PLATFORM: &str = "platform";

/// The platform's file in a source, when `platform` doesn't name another.
const PLATFORM_FILE: &str = "platform.toml";

/// The platform a configuration builds on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Platform {
    /// Wrath's own modules.
    #[default]
    Wrath,
    /// The platform in a source: its name, the URL it follows, and the platform's file in it.
    Source {
        name: String,
        url: String,
        path: String,
    },
}

impl Platform {
    /// How the plan and the model name it: `wrath`, or the source's name.
    pub fn name(&self) -> &str {
        match self {
            Self::Wrath => "wrath",
            Self::Source { name, .. } => name,
        }
    }
}

/// The platform `fs` builds on, as the starting file for `role` chooses it, read before the
/// modules are loaded. Only a system chooses one; projects and homes use wrath's.
pub fn platform(fs: &dyn ConfigFs, role: Option<&str>) -> Result<Platform, String> {
    if load::kind_of(fs) != ConfigKind::System {
        return Ok(Platform::Wrath);
    }
    let mut sources = Sources::default();
    let path = load::starting_file(fs, role).map_err(|d| render(&[d], &sources))?;
    let Ok(text) = fs.read(&path) else {
        // Loading the configuration says what's missing.
        return Ok(Platform::Wrath);
    };
    let file = sources.add(fs.display(&path), text);
    let Ok(table) = DeTable::parse(sources.text(file)) else {
        return Ok(Platform::Wrath);
    };
    let Some(value) = table.get_ref().get(PLATFORM) else {
        return Ok(Platform::Wrath);
    };
    let mut declared = BTreeMap::new();
    if let Some(DeValue::Table(listed)) = table.get_ref().get(SOURCES).map(Spanned::get_ref) {
        for (name, source) in listed {
            if let Ok((url, at)) = load::source_url(name.get_ref(), source, file) {
                declared.insert(name.get_ref().to_string(), DeclaredSource { url, at });
            }
        }
    }
    parse(value, file, &declared).map_err(|d| render(&[d], &sources))
}

/// `platform`'s value in the starting file `file`, whose `[sources]` are `declared`.
pub(crate) fn parse(
    value: &Spanned<DeValue<'_>>,
    file: FileId,
    declared: &BTreeMap<String, DeclaredSource>,
) -> Result<Platform, Diagnostic> {
    let span = Span::new(file, value.span());
    match value.get_ref() {
        DeValue::String(name) if name == "wrath" => Ok(Platform::Wrath),
        DeValue::String(name) => Err(Diagnostic::error(format!("there's no platform `{name}`"))
            .label(span, "only `wrath` is built in")
            .note(format!(
                "help: one from a source is `platform = {{ source = \"{name}\" }}`"
            ))),
        DeValue::Table(table) => {
            only_keys(table, &["source", "path"], "a platform", file)?;
            let text = |key: &str| match table.get(key).map(|v| (v.get_ref(), v.span())) {
                None => Ok(None),
                Some((DeValue::String(text), _)) => Ok(Some(text.to_string())),
                Some((other, at)) => Err(Diagnostic::error(format!(
                    "a platform's `{key}` is a string, found {}",
                    other.type_str()
                ))
                .label(Span::new(file, at), "expected a string")),
            };
            let name = text("source")?.ok_or_else(|| {
                Diagnostic::error("a platform names its source")
                    .label(span, "expected `{ source = \"NAME\" }`")
            })?;
            let source = declared.get(&name).ok_or_else(|| {
                Diagnostic::error(format!("the platform's source `{name}` isn't declared"))
                    .label(span, "used here")
                    .note(format!(
                        "help: declare it in this file, as `[sources.{name}]` with its `url`"
                    ))
            })?;
            Ok(Platform::Source {
                name,
                url: source.url.clone(),
                path: text("path")?.unwrap_or_else(|| PLATFORM_FILE.to_owned()),
            })
        }
        other => Err(Diagnostic::error(format!(
            "`platform` is `\"wrath\"` or a table naming a source, found {}",
            other.type_str()
        ))
        .label(span, "expected `\"wrath\"` or `{ source = \"NAME\" }`")),
    }
}
