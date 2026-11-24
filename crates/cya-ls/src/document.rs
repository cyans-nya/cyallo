//! Open buffers and the UTF-8/UTF-16 boundary. No filesystem reads are needed.
use crate::highlight::Language;
use lsp_types::{Position, Range, TextDocumentContentChangeEvent};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentError {
    TooLarge,
    InvalidPosition(Position),
    InvalidOffset(usize),
    ReversedRange(Range),
    StaleVersion { current: i32, received: i32 },
}
impl fmt::Display for DocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("document exceeds the 4 GiB syntax span limit"),
            Self::InvalidPosition(p) => {
                write!(f, "invalid UTF-16 position {}:{}", p.line, p.character)
            }
            Self::InvalidOffset(offset) => write!(f, "invalid UTF-8 offset {offset}"),
            Self::ReversedRange(r) => write!(f, "range ends before it starts: {r:?}"),
            Self::StaleVersion { current, received } => {
                write!(f, "document version {received} does not follow {current}")
            }
        }
    }
}
impl Error for DocumentError {}

#[derive(Debug, Clone)]
pub struct LineIndex {
    starts: Vec<usize>,
}
impl LineIndex {
    pub fn new(text: &str) -> Result<Self, DocumentError> {
        if text.len() > u32::MAX as usize {
            return Err(DocumentError::TooLarge);
        }
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(offset, _)| offset + 1));
        Ok(Self { starts })
    }

    pub fn offset(&self, text: &str, position: Position) -> Result<usize, DocumentError> {
        let start = *self
            .starts
            .get(position.line as usize)
            .ok_or(DocumentError::InvalidPosition(position))?;
        let end = self
            .starts
            .get(position.line as usize + 1)
            .copied()
            .unwrap_or(text.len());
        let line = text[start..end].trim_end_matches(['\r', '\n']);
        let mut units = 0;
        for (byte, ch) in line.char_indices() {
            if units == position.character {
                return Ok(start + byte);
            }
            units += ch.len_utf16() as u32;
            if units > position.character {
                return Err(DocumentError::InvalidPosition(position));
            }
        }
        // LSP positions beyond the line length mean the end of the line.
        Ok(start + line.len())
    }

    pub fn position(&self, text: &str, offset: usize) -> Result<Position, DocumentError> {
        if offset > text.len() || !text.is_char_boundary(offset) {
            return Err(DocumentError::InvalidOffset(offset));
        }
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        let prefix = text[self.starts[line]..offset].trim_end_matches(['\r', '\n']);
        Ok(Position::new(
            line as u32,
            prefix.encode_utf16().count() as u32,
        ))
    }

    pub fn range(&self, text: &str, range: Range) -> Result<std::ops::Range<usize>, DocumentError> {
        let start = self.offset(text, range.start)?;
        let end = self.offset(text, range.end)?;
        if range.start > range.end || start > end {
            return Err(DocumentError::ReversedRange(range));
        }
        Ok(start..end)
    }
}

pub struct Document {
    pub language: Language,
    pub version: i32,
    pub text: String,
    pub lines: LineIndex,
    pub(crate) highlights: Option<Vec<crate::highlight::Token>>,
}
impl Document {
    pub fn new(language: Language, version: i32, text: String) -> Result<Self, DocumentError> {
        let lines = LineIndex::new(&text)?;
        Ok(Self {
            language,
            version,
            text,
            lines,
            highlights: None,
        })
    }

    /// Apply all edits atomically; each range addresses the preceding edit's text.
    pub fn change(
        &mut self,
        version: i32,
        edits: Vec<TextDocumentContentChangeEvent>,
    ) -> Result<(), DocumentError> {
        if version <= self.version {
            return Err(DocumentError::StaleVersion {
                current: self.version,
                received: version,
            });
        }
        let mut text = self.text.clone();
        let mut lines = self.lines.clone();
        for edit in edits {
            if let Some(range) = edit.range {
                let range = lines.range(&text, range)?;
                let new_len = text.len() - range.len() + edit.text.len();
                if new_len > u32::MAX as usize {
                    return Err(DocumentError::TooLarge);
                }
                text.replace_range(range, &edit.text);
            } else {
                text = edit.text;
            }
            lines = LineIndex::new(&text)?;
        }
        self.text = text;
        self.lines = lines;
        self.version = version;
        self.highlights = None;
        Ok(())
    }
}
