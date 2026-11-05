//! Error messages that point at the configuration that caused them.

use std::fmt::Write;

use crate::source::{Sources, Span};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: String,
    /// Places in the source the message refers to. The first one is the primary location.
    pub labels: Vec<Label>,
    /// Lines shown after the source, such as `help: did you mean ...`.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

impl Diagnostic {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    /// Adds a label if there is a span to put it on.
    pub fn label_opt(self, span: Option<Span>, message: impl Into<String>) -> Self {
        match span {
            Some(span) => self.label(span, message),
            None => self,
        }
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

/// Renders diagnostics in the style of compiler errors, without colour.
pub fn render(diagnostics: &[Diagnostic], sources: &Sources) -> String {
    let mut out = String::new();
    for (i, diagnostic) in diagnostics.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        render_one(diagnostic, sources, &mut out);
    }
    out
}

fn render_one(diagnostic: &Diagnostic, sources: &Sources, out: &mut String) {
    // Writing to a String cannot fail.
    let _ = writeln!(out, "error: {}", diagnostic.message);
    let width = diagnostic
        .labels
        .iter()
        .map(|label| {
            let line = sources.position(label.span.file, label.span.start).line;
            line.to_string().len()
        })
        .max()
        .unwrap_or(0);
    let pad = " ".repeat(width);

    for label in &diagnostic.labels {
        let file = label.span.file;
        let start = sources.position(file, label.span.start);
        let end = sources.position(file, label.span.end);
        let text = sources.line(file, start.line);
        let underline = if end.line == start.line {
            end.column.saturating_sub(start.column).max(1)
        } else {
            (text.chars().count() + 1)
                .saturating_sub(start.column)
                .max(1)
        };
        let _ = writeln!(
            out,
            "{pad}--> {}:{}:{}",
            sources.name(file),
            start.line,
            start.column
        );
        let _ = writeln!(out, "{pad} |");
        let _ = writeln!(out, "{:>width$} | {text}", start.line);
        let marker = format!(
            "{}{} {}",
            " ".repeat(start.column - 1),
            "^".repeat(underline),
            label.message
        );
        let _ = writeln!(out, "{pad} | {}", marker.trim_end());
    }
    for note in &diagnostic.notes {
        let _ = writeln!(out, "{pad} = {note}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_labels_and_notes() {
        let mut sources = Sources::default();
        let file = sources.add("system.toml", "[system]\nhostnme = \"box\"\n");
        let start = sources.text(file).find("hostnme").unwrap();
        let diagnostic = Diagnostic::error("unknown option `system.hostnme`")
            .label(
                Span::new(file, start..start + 7),
                "no option with this name",
            )
            .note("help: did you mean `hostname`?");

        assert_eq!(
            render(&[diagnostic], &sources),
            "\
error: unknown option `system.hostnme`
 --> system.toml:2:1
  |
2 | hostnme = \"box\"
  | ^^^^^^^ no option with this name
  = help: did you mean `hostname`?
"
        );
    }

    #[test]
    fn renders_a_diagnostic_without_labels() {
        let diagnostic = Diagnostic::error("no system.toml in `config`").note("help: create it");

        assert_eq!(
            render(&[diagnostic], &Sources::default()),
            "error: no system.toml in `config`\n = help: create it\n"
        );
    }
}
