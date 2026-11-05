//! Strings with `${path}` references to other options and machine facts.

use crate::path::OptionPath;
use crate::source::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    Ref { path: OptionPath, span: Span },
}

/// Splits a string into text and references. `$${` is a literal `${`.
///
/// `base` is the file offset of the string's first character when the source text matches the
/// string exactly, so references get precise spans. Otherwise, as with escapes, every reference
/// gets `fallback`, the span of the whole string.
pub fn parse(text: &str, base: Option<usize>, fallback: Span) -> Result<Vec<Part>, (String, Span)> {
    let span_of = |start: usize, end: usize| match base {
        Some(base) => Span {
            file: fallback.file,
            start: base + start,
            end: base + end,
        },
        None => fallback,
    };
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut rest = 0;
    while let Some(found) = text[rest..].find('$') {
        let at = rest + found;
        let after = &text[at..];
        if after.starts_with("$${") {
            literal.push_str(&text[rest..at]);
            literal.push_str("${");
            rest = at + 3;
        } else if let Some(inner) = after.strip_prefix("${") {
            literal.push_str(&text[rest..at]);
            let Some(len) = inner.find('}') else {
                return Err((
                    "unterminated reference, expected `}`".into(),
                    span_of(at, text.len()),
                ));
            };
            let end = at + 2 + len + 1;
            let span = span_of(at, end);
            let path = OptionPath::parse(inner[..len].trim())
                .map_err(|e| (format!("invalid reference: {e}"), span))?;
            if !literal.is_empty() {
                parts.push(Part::Text(std::mem::take(&mut literal)));
            }
            parts.push(Part::Ref { path, span });
            rest = end;
        } else {
            literal.push_str(&text[rest..=at]);
            rest = at + 1;
        }
    }
    literal.push_str(&text[rest..]);
    if !literal.is_empty() || parts.is_empty() {
        parts.push(Part::Text(literal));
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Sources;

    fn span(start: usize, end: usize) -> Span {
        let file = Sources::default().add("a", "");
        Span { file, start, end }
    }

    fn path(text: &str) -> OptionPath {
        OptionPath::parse(text).unwrap()
    }

    #[test]
    fn plain_text_is_one_part() {
        assert_eq!(
            parse("cost: $5", Some(0), span(0, 0)).unwrap(),
            [Part::Text("cost: $5".into())]
        );
        assert_eq!(
            parse("", Some(0), span(0, 0)).unwrap(),
            [Part::Text(String::new())]
        );
    }

    #[test]
    fn splits_references_with_precise_spans() {
        let parts = parse("${system.hostname}.lan", Some(10), span(9, 33)).unwrap();

        assert_eq!(
            parts,
            [
                Part::Ref {
                    path: path("system.hostname"),
                    span: span(10, 28)
                },
                Part::Text(".lan".into()),
            ]
        );
    }

    #[test]
    fn falls_back_to_the_whole_string_span() {
        let parts = parse("a${b}", None, span(3, 12)).unwrap();

        assert_eq!(
            parts,
            [
                Part::Text("a".into()),
                Part::Ref {
                    path: path("b"),
                    span: span(3, 12)
                },
            ]
        );
    }

    #[test]
    fn double_dollar_escapes() {
        assert_eq!(
            parse("$${HOME} and $$ ${x}", Some(0), span(0, 0)).unwrap(),
            [
                Part::Text("${HOME} and $$ ".into()),
                Part::Ref {
                    path: path("x"),
                    span: span(16, 20)
                },
            ]
        );
    }

    #[test]
    fn reports_malformed_references() {
        assert_eq!(
            parse("a ${b", Some(0), span(0, 0)).unwrap_err(),
            ("unterminated reference, expected `}`".into(), span(2, 5))
        );
        assert_eq!(
            parse("${a..b}", Some(0), span(0, 0)).unwrap_err(),
            (
                "invalid reference: empty segment in `a..b`".into(),
                span(0, 7)
            )
        );
    }
}
