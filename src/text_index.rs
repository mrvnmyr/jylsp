use tower_lsp::lsp_types::{Position, Range};

/// Fast-ish byte-index -> LSP Position conversion.
///
/// LSP positions are UTF-16 code units, 0-based.
#[derive(Debug, Clone)]
pub struct TextIndex {
    text: String,
    line_starts: Vec<usize>,
}

impl TextIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0usize];
        for (i, b) in text.as_bytes().iter().enumerate() {
            if *b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            text: text.to_string(),
            line_starts,
        }
    }

    pub fn pos_from_byte(&self, mut byte: usize) -> Position {
        if byte > self.text.len() {
            byte = self.text.len();
        }
        while byte > 0 && !self.text.is_char_boundary(byte) {
            byte -= 1;
        }

        let line = match self.line_starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };

        let line_start = self.line_starts.get(line).copied().unwrap_or(0);
        let slice = &self.text[line_start..byte];

        let mut utf16: u32 = 0;
        for ch in slice.chars() {
            utf16 += ch.len_utf16() as u32;
        }

        Position {
            line: line as u32,
            character: utf16,
        }
    }

    pub fn range_from_bytes(&self, start: usize, end: usize) -> Range {
        let s = self.pos_from_byte(start);
        let e = self.pos_from_byte(end);
        Range { start: s, end: e }
    }
}
