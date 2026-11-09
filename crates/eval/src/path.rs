//! Dotted option paths such as `services.nginx.main.port`.

use std::fmt;

/// A path to an option. Segments that aren't bare TOML keys are written quoted, as in
/// `files.".config/helix/config.toml"`.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OptionPath(Vec<String>);

impl OptionPath {
    pub fn root() -> Self {
        Self::default()
    }

    pub fn segments(&self) -> &[String] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn child(&self, segment: impl Into<String>) -> Self {
        let mut segments = self.0.clone();
        segments.push(segment.into());
        Self(segments)
    }

    pub fn starts_with(&self, prefix: &OptionPath) -> bool {
        self.0.starts_with(&prefix.0)
    }

    /// Every path this one starts with, from the root to the path itself.
    pub(crate) fn prefixes(&self) -> impl Iterator<Item = OptionPath> {
        (0..=self.0.len()).map(|length| Self(self.0[..length].to_vec()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut segments = Vec::new();
        let mut rest = text;
        loop {
            let (segment, after) = if let Some(quoted) = rest.strip_prefix('"') {
                let end = quoted
                    .find('"')
                    .ok_or_else(|| format!("unterminated quote in `{text}`"))?;
                (&quoted[..end], &quoted[end + 1..])
            } else {
                let end = rest.find('.').unwrap_or(rest.len());
                let segment = &rest[..end];
                if !is_bare(segment) {
                    return Err(if segment.is_empty() {
                        format!("empty segment in `{text}`")
                    } else {
                        format!("`{segment}` in `{text}` needs quotes")
                    });
                }
                (segment, &rest[end..])
            };
            segments.push(segment.to_owned());
            match after.strip_prefix('.') {
                Some(next) => rest = next,
                None if after.is_empty() => return Ok(Self(segments)),
                None => return Err(format!("expected `.` after `{segment}` in `{text}`")),
            }
        }
    }
}

impl fmt::Display for OptionPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, segment) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            write_key(f, segment)?;
        }
        Ok(())
    }
}

/// Writes a TOML key, quoted if it isn't a bare key.
pub(crate) fn write_key(f: &mut impl fmt::Write, key: &str) -> fmt::Result {
    if is_bare(key) {
        f.write_str(key)
    } else {
        write!(f, "\"{key}\"")
    }
}

fn is_bare(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_bare_and_quoted_segments() {
        let path = OptionPath::parse("files.\".config/helix/config.toml\".mode").unwrap();

        assert_eq!(
            path.segments(),
            ["files", ".config/helix/config.toml", "mode"]
        );
        assert_eq!(path.to_string(), "files.\".config/helix/config.toml\".mode");
    }

    #[test]
    fn rejects_malformed_paths() {
        assert_eq!(
            OptionPath::parse("a..b").unwrap_err(),
            "empty segment in `a..b`"
        );
        assert_eq!(
            OptionPath::parse("a.b c").unwrap_err(),
            "`b c` in `a.b c` needs quotes"
        );
        assert_eq!(
            OptionPath::parse("a.\"b").unwrap_err(),
            "unterminated quote in `a.\"b`"
        );
        assert_eq!(
            OptionPath::parse("a.\"b\"c").unwrap_err(),
            "expected `.` after `b` in `a.\"b\"c`"
        );
        assert_eq!(OptionPath::parse("").unwrap_err(), "empty segment in ``");
    }

    #[test]
    fn prefixes() {
        let path = OptionPath::parse("a.b.c").unwrap();

        assert!(path.starts_with(&OptionPath::parse("a.b").unwrap()));
        assert!(path.starts_with(&OptionPath::root()));
        assert!(!path.starts_with(&OptionPath::parse("a.c").unwrap()));
        assert_eq!(OptionPath::root().child("a").child("b").len(), 2);
    }
}
