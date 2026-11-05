//! A source check for iteration whose order isn't defined.
//!
//! `for k, v in t do` walks a table in its internal hash order, which depends on how the table
//! was built. `pairs` is replaced with a sorted version, but generalized iteration can't be
//! intercepted for tables a module builds itself, so it is rejected before the module runs.

use std::ops::Range;

/// The expression of the first `for ... in <expression> do` loop that iterates a value directly
/// instead of calling an iterator function such as `pairs` or `ipairs`.
pub(crate) fn direct_iteration(source: &str) -> Option<Range<usize>> {
    let mut state = State::Outside;
    for token in tokens(source) {
        state = match (state, token.text) {
            (_, "for") => State::Header,
            (State::Header, "=") => State::Outside,
            (State::Header, "in") => State::Expression {
                start: None,
                end: token.span.end,
                calls: false,
            },
            (State::Expression { start, end, calls }, "do") => {
                if !calls {
                    return Some(start.unwrap_or(end)..end);
                }
                State::Outside
            }
            (State::Expression { start, calls, .. }, text) => State::Expression {
                start: start.or(Some(token.span.start)),
                end: token.span.end,
                calls: calls || text == "(",
            },
            (state, _) => state,
        };
    }
    None
}

#[derive(Debug, Clone, Copy)]
enum State {
    Outside,
    /// Between `for` and `in` or `=`.
    Header,
    Expression {
        start: Option<usize>,
        end: usize,
        calls: bool,
    },
}

struct Token<'a> {
    text: &'a str,
    span: Range<usize>,
}

/// Identifiers and single punctuation characters, skipping whitespace, comments and strings.
fn tokens(source: &str) -> impl Iterator<Item = Token<'_>> {
    let bytes = source.as_bytes();
    let mut at = 0;
    std::iter::from_fn(move || {
        loop {
            let rest = &source[at..];
            let c = *bytes.get(at)?;
            if c.is_ascii_whitespace() {
                at += 1;
            } else if let Some(comment) = rest.strip_prefix("--") {
                at += match long_bracket(comment) {
                    Some(level) => 2 + skip_long(comment, level),
                    None => rest.find('\n').unwrap_or(rest.len()),
                };
            } else if c == b'"' || c == b'\'' || c == b'`' {
                at += skip_quoted(rest);
            } else if let Some(level) = long_bracket(rest) {
                at += skip_long(rest, level);
            } else {
                let len = if c.is_ascii_alphanumeric() || c == b'_' {
                    rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(rest.len())
                } else {
                    rest.chars().next().map_or(1, char::len_utf8)
                };
                let token = Token {
                    text: &rest[..len],
                    span: at..at + len,
                };
                at += len;
                return Some(token);
            }
        }
    })
}

/// The level of a long bracket such as `[[` (0) or `[==[` (2) at the start of `text`.
fn long_bracket(text: &str) -> Option<usize> {
    let after = text.strip_prefix('[')?;
    let level = after.len() - after.trim_start_matches('=').len();
    after[level..].starts_with('[').then_some(level)
}

fn skip_long(text: &str, level: usize) -> usize {
    let close = format!("]{}]", "=".repeat(level));
    let open = level + 2;
    text[open..]
        .find(&close)
        .map_or(text.len(), |end| open + end + close.len())
}

fn skip_quoted(text: &str) -> usize {
    let quote = text.as_bytes()[0];
    let mut escaped = false;
    for (i, &c) in text.as_bytes().iter().enumerate().skip(1) {
        match c {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            _ if c == quote => return i + 1,
            _ => {}
        }
    }
    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(source: &str) -> Option<&str> {
        direct_iteration(source).map(|span| &source[span])
    }

    #[test]
    fn calls_and_numeric_loops_are_fine() {
        assert_eq!(found("for k, v in pairs(t) do end"), None);
        assert_eq!(found("for _, x in ipairs(ctx.read(\"a\")) do end"), None);
        assert_eq!(found("for i = 1, #t do end"), None);
    }

    #[test]
    fn direct_iteration_is_found() {
        assert_eq!(found("local t = {}\nfor k, v in t do end"), Some("t"));
        assert_eq!(found("for k in cfg.folders do end"), Some("cfg.folders"));
    }

    #[test]
    fn comments_and_strings_are_skipped() {
        let source = r#"
-- for k in t do
--[[ for k in t do ]]
local s = "for k in t do"
local l = [==[ for k in t do ]==]
for k, v in pairs(t) do end
"#;
        assert_eq!(found(source), None);
    }
}
