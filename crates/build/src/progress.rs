//! How builds show their progress on stderr: a line as each starts, a status line kept up to date
//! on a terminal, or everything each build prints with `--verbose`.

use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A line as each build starts, for logs and pipes.
    Lines,
    /// A line per build that shows its time and latest output while it runs.
    Live,
    /// Everything each build prints, as it prints it.
    Verbose,
}

static MODE: OnceLock<Mode> = OnceLock::new();

/// Sets how builds show progress, once, before the first build.
pub fn set(mode: Mode) {
    let _ = MODE.set(mode);
}

pub(crate) fn mode() -> Mode {
    MODE.get().copied().unwrap_or(Mode::Lines)
}

/// Whether `--verbose` was given.
pub fn verbose() -> bool {
    mode() == Mode::Verbose
}

/// The mode for `--verbose` and for whether stderr is a terminal.
pub fn chosen(verbose: bool, terminal: bool) -> Mode {
    match (verbose, terminal) {
        (true, _) => Mode::Verbose,
        (false, true) => Mode::Live,
        (false, false) => Mode::Lines,
    }
}

/// `elapsed` as people read a build's time: `42s`, `3m 05s`, `1h 02m`.
pub(crate) fn clock(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// The status line of a running build, cut to `width` columns.
pub(crate) fn status(
    verb: &str,
    name: &str,
    elapsed: Duration,
    last: &str,
    width: usize,
) -> String {
    let head = format!("  {verb}  {name}  {}", clock(elapsed));
    let room = width.saturating_sub(head.chars().count() + 3);
    let tail: String = last.chars().take(room).collect();
    if tail.is_empty() {
        head.chars().take(width.saturating_sub(1)).collect()
    } else {
        format!("{head}  {tail}")
    }
}

/// The last whole line a build printed, without colors or other control characters.
#[derive(Debug, Default)]
pub(crate) struct Tail {
    partial: Vec<u8>,
    line: String,
}

impl Tail {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' || byte == b'\r' {
                let text = plain(&self.partial);
                if !text.is_empty() {
                    self.line = text;
                }
                self.partial.clear();
            } else {
                self.partial.push(byte);
            }
        }
    }

    pub(crate) fn line(&self) -> &str {
        &self.line
    }
}

/// `bytes` as text without terminal escape sequences or control characters, trimmed.
fn plain(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            // An escape sequence such as `ESC[1;31m` ends at its first letter.
            '\u{1b}' => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out.trim().to_owned()
}

/// How wide the terminal on stderr is, or 80 columns.
pub(crate) fn width() -> usize {
    rustix::termios::tcgetwinsize(std::io::stderr())
        .ok()
        .map(|size| usize::from(size.ws_col))
        .filter(|&columns| columns > 0)
        .unwrap_or(80)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbose_wins_and_a_terminal_gets_a_live_line() {
        assert_eq!(chosen(true, true), Mode::Verbose);
        assert_eq!(chosen(true, false), Mode::Verbose);
        assert_eq!(chosen(false, true), Mode::Live);
        assert_eq!(chosen(false, false), Mode::Lines);
    }

    #[test]
    fn times_read_in_the_largest_units_that_matter() {
        let s = Duration::from_secs;
        assert_eq!(clock(s(0)), "0s");
        assert_eq!(clock(s(59)), "59s");
        assert_eq!(clock(s(185)), "3m 05s");
        assert_eq!(clock(s(3720)), "1h 02m");
    }

    #[test]
    fn the_status_line_shows_the_latest_output_cut_to_the_width() {
        let line = status(
            "build",
            "foot",
            Duration::from_secs(65),
            "[12/80] cc vt.c",
            80,
        );
        assert_eq!(line, "  build  foot  1m 05s  [12/80] cc vt.c");
        let narrow = status(
            "build",
            "foot",
            Duration::from_secs(5),
            "[12/80] cc vt.c",
            25,
        );
        assert_eq!(narrow, "  build  foot  5s  [12/8");
        assert!(narrow.chars().count() < 25);
        assert_eq!(
            status("build", "foot", Duration::ZERO, "", 80),
            "  build  foot  0s"
        );
    }

    #[test]
    fn the_tail_keeps_the_last_whole_line_without_colors() {
        let mut tail = Tail::default();
        tail.push(b"checking for cc... yes\n\x1b[1;32m[3/9] Compiling\x1b[0m vt.c\r\n[4/");
        assert_eq!(tail.line(), "[3/9] Compiling vt.c");
        tail.push(b"9] Linking foot\n\n   \n");
        assert_eq!(tail.line(), "[4/9] Linking foot");
    }
}
