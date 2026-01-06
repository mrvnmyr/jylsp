use tower_lsp::lsp_types::{Position, Range};

fn debug_enabled() -> bool {
    std::env::var_os("DEBUG").is_some()
}

fn dprintln(msg: &str) {
    if debug_enabled() {
        eprintln!("[DEBUG] {msg}");
    }
}

/// Text index for converting byte offsets into LSP Positions/Ranges.
///
/// LSP character offsets are UTF-16 code units.
#[derive(Debug, Clone)]
pub struct TextIndex {
    text: String,
    line_starts: Vec<usize>,
}

impl TextIndex {
    pub fn new(text: &str) -> Self {
        let mut line_starts = Vec::with_capacity(128);
        line_starts.push(0);
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i + 1);
            }
        }
        if debug_enabled() {
            dprintln(&format!(
                "TextIndex::new: bytes={} lines={}",
                text.len(),
                line_starts.len()
            ));
        }
        Self {
            text: text.to_string(),
            line_starts,
        }
    }

    pub fn position_from_byte(&self, mut byte: usize) -> Position {
        if byte > self.text.len() {
            byte = self.text.len();
        }
        // Clamp to a valid UTF-8 boundary.
        while byte > 0 && !self.text.is_char_boundary(byte) {
            byte -= 1;
        }

        let line = match self.line_starts.binary_search(&byte) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let line_start = self.line_starts.get(line).copied().unwrap_or(0);

        // Compute UTF-16 code units from line_start..byte.
        let slice = &self.text[line_start..byte];
        let character: u32 = slice.encode_utf16().count() as u32;

        Position {
            line: line as u32,
            character,
        }
    }

    pub fn range_from_bytes(&self, start: usize, end: usize) -> Range {
        let s = self.position_from_byte(start);
        let e = self.position_from_byte(end);
        if debug_enabled() {
            dprintln(&format!(
                "range_from_bytes: {start}..{end} -> {}:{}..{}:{}",
                s.line, s.character, e.line, e.character
            ));
        }
        Range { start: s, end: e }
    }
}
