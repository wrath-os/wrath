//! Color for a terminal, used where it carries meaning: what a plan adds, removes and changes,
//! and what's an error, a warning or a note. Nothing is colored when the output isn't a terminal
//! or `NO_COLOR` is set.

use std::io::IsTerminal;

/// Whether what goes to stdout gets color.
pub fn stdout() -> bool {
    wanted(std::io::stdout().is_terminal())
}

/// Whether what goes to stderr gets color.
pub fn stderr() -> bool {
    wanted(std::io::stderr().is_terminal())
}

fn wanted(terminal: bool) -> bool {
    terminal && std::env::var_os("NO_COLOR").is_none()
}

/// The labels a report's lines start with, and their color.
const LABELS: [(&str, &str); 4] = [
    ("error:", "1;31"),
    ("warning:", "1;33"),
    ("note:", "1;36"),
    ("help:", "1;36"),
];

/// `text`, an error or another report, with its labels colored and the lines that point at a
/// file dimmed, when `color` says so.
pub fn report(text: &str, color: bool) -> String {
    if !color {
        return text.to_owned();
    }
    text.split_inclusive('\n')
        .map(|line| {
            let body = line.trim_start();
            let indent = &line[..line.len() - body.len()];
            let body = body.strip_prefix("= ").unwrap_or(body);
            let marker = &line[indent.len()..line.len() - body.len()];
            if let Some((label, codes)) = LABELS.iter().find(|(label, _)| body.starts_with(label)) {
                format!(
                    "{indent}{marker}\x1b[{codes}m{label}\x1b[0m{}",
                    &body[label.len()..]
                )
            } else if body.starts_with("--> ") {
                let path = body.trim_end();
                format!("{indent}\x1b[2m{path}\x1b[0m{}", &body[path.len()..])
            } else {
                line.to_owned()
            }
        })
        .collect()
}

/// Prints `text`, an error or another report, to stderr.
pub fn print_report(text: &str) {
    eprint!("{}", report(text, stderr()));
}

/// Prints an error to stderr.
pub fn error(message: &str) {
    print_report(&format!("error: {message}\n"));
}

/// Prints a warning to stderr.
pub fn warning(message: &str) {
    print_report(&format!("warning: {message}\n"));
}

/// Prints a note to stderr.
pub fn note(message: &str) {
    print_report(&format!("note: {message}\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_colored_and_the_rest_is_left_as_it_is() {
        let text = "error: unknown option `x`\n --> system.toml:2:1\n  = help: did you mean `y`?\n";
        assert_eq!(report(text, false), text);
        let colored = report(text, true);
        assert!(colored.starts_with("\x1b[1;31merror:\x1b[0m unknown option `x`\n"));
        assert!(
            colored.contains(" \x1b[2m--> system.toml:2:1\x1b[0m\n"),
            "{colored:?}"
        );
        assert!(
            colored.contains("  = \x1b[1;36mhelp:\x1b[0m did you mean"),
            "{colored:?}"
        );
        assert_eq!(report("plain\n", true), "plain\n");
    }
}
