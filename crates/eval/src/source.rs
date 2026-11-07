//! Source files and byte spans into them.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(u32);

/// A byte range in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub file: FileId,
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(file: FileId, range: Range<usize>) -> Self {
        Self {
            file,
            start: range.start,
            end: range.end,
        }
    }
}

/// A 1-based line and column, counted in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

/// The source files an evaluation read, for turning spans into locations.
#[derive(Debug, Default)]
pub struct Sources {
    files: Vec<SourceFile>,
}

#[derive(Debug)]
struct SourceFile {
    name: String,
    text: String,
    line_starts: Vec<usize>,
}

impl Sources {
    /// Adds a file. `name` is how locations refer to it, normally its path.
    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> FileId {
        let text = text.into();
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        let id = FileId(u32::try_from(self.files.len()).expect("fewer than 2^32 source files"));
        self.files.push(SourceFile {
            name: name.into(),
            text,
            line_starts,
        });
        id
    }

    /// Every file, in the order they were added.
    pub fn files(&self) -> impl Iterator<Item = FileId> + use<> {
        (0..self.files.len())
            .map(|index| FileId(u32::try_from(index).expect("fewer than 2^32 source files")))
    }

    pub fn name(&self, file: FileId) -> &str {
        &self.file(file).name
    }

    pub fn text(&self, file: FileId) -> &str {
        &self.file(file).text
    }

    pub fn position(&self, file: FileId, offset: usize) -> Position {
        let source = self.file(file);
        let offset = offset.min(source.text.len());
        let line = source.line_starts.partition_point(|&start| start <= offset) - 1;
        let start = source.line_starts[line];
        Position {
            line: line + 1,
            column: source.text[start..offset].chars().count() + 1,
        }
    }

    /// The text of a 1-based line, without its line ending.
    pub fn line(&self, file: FileId, line: usize) -> &str {
        let source = self.file(file);
        let start = source.line_starts[line - 1];
        let end = source
            .line_starts
            .get(line)
            .map_or(source.text.len(), |next| next - 1);
        source.text[start..end].trim_end_matches('\r')
    }

    /// `name:line:column` of the start of a span.
    pub fn location(&self, span: Span) -> String {
        let position = self.position(span.file, span.start);
        format!(
            "{}:{}:{}",
            self.name(span.file),
            position.line,
            position.column
        )
    }

    fn file(&self, file: FileId) -> &SourceFile {
        &self.files[file.0 as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_count_lines_and_characters() {
        let mut sources = Sources::default();
        let file = sources.add("a.toml", "a = 1\nnäme = \"x\"\r\nlast");

        assert_eq!(sources.position(file, 0), Position { line: 1, column: 1 });
        let offset = sources.text(file).find("= \"x").unwrap();
        assert_eq!(
            sources.position(file, offset),
            Position { line: 2, column: 6 }
        );
        assert_eq!(sources.line(file, 2), "näme = \"x\"");
        assert_eq!(sources.line(file, 3), "last");
        assert_eq!(
            sources.location(Span::new(file, offset..offset + 1)),
            "a.toml:2:6"
        );
    }
}
