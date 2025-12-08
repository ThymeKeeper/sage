use super::Editor;

impl Editor {
    /// Ensure a byte position is on a valid character boundary
    pub(super) fn ensure_char_boundary(&self, pos: usize) -> usize {
        let char_pos = self.buffer.byte_to_char(pos);
        self.buffer.char_to_byte(char_pos)
    }

    /// Get the current selection as (start, end) byte positions
    pub(super) fn get_selection(&self) -> Option<(usize, usize)> {
        self.selection_start.map(|start| {
            // Ensure both positions are on character boundaries
            let start = self.ensure_char_boundary(start);
            let cursor = self.ensure_char_boundary(self.cursor);

            if start < cursor {
                (start, cursor)
            } else {
                (cursor, start)
            }
        })
    }

    /// Get selected text
    pub(super) fn get_selected_text(&self) -> Option<String> {
        self.get_selection().and_then(|(start, end)| {
            // Validate range before attempting byte_slice
            if start >= end || end > self.buffer.len_bytes() {
                return None;
            }

            // Try to get the text, catching any panics
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.buffer.rope().byte_slice(start..end).to_string()
            })) {
                Ok(text) => Some(text),
                Err(_) => {
                    eprintln!("Warning: get_selected_text failed with start={}, end={}, len={}",
                             start, end, self.buffer.len_bytes());
                    None
                }
            }
        })
    }

    /// Delete the current selection
    pub(super) fn delete_selection(&mut self) -> bool {
        if let Some((start, end)) = self.get_selection() {
            let cursor_before = self.cursor;

            // Track line changes for syntax highlighting
            let start_line = self.buffer.byte_to_line(start);
            let end_line = self.buffer.byte_to_line(end);
            let lines_affected = end_line - start_line;

            self.buffer.delete(start, end, cursor_before, start);
            self.cursor = start;
            self.selection_start = None;
            self.modified = true;

            // Update syntax highlighting
            if lines_affected > 0 {
                // Multiple lines were deleted
                self.syntax.lines_deleted(start_line, lines_affected);
            } else {
                // Single line modification
                self.syntax.line_modified(start_line);
            }

            true
        } else {
            false
        }
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        self.get_selection()
    }

    pub fn select_range(&mut self, start: usize, end: usize) {
        self.selection_start = Some(start);
        self.cursor = end;
    }

    pub fn replace_selection(&mut self, replacement: &str) -> bool {
        if let Some((start, end)) = self.get_selection() {
            self.replace_at(start, end, replacement);
            self.selection_start = None;
            true
        } else {
            false
        }
    }

    pub fn replace_at(&mut self, start: usize, end: usize, replacement: &str) {
        let cursor_before = self.cursor;
        let start_line = self.buffer.byte_to_line(start);
        let end_line = self.buffer.byte_to_line(end);

        self.buffer.delete(start, end, cursor_before, start);
        self.buffer.insert(start, replacement, cursor_before, start + replacement.len());
        self.cursor = start + replacement.len();
        self.modified = true;

        // Update syntax highlighting
        let lines_affected = end_line - start_line;
        if lines_affected > 0 {
            self.syntax.lines_deleted(start_line, lines_affected);
        }
        self.syntax.line_modified(start_line);
    }

    pub(super) fn set_selection_start(&mut self, position: usize) {
        self.selection_start = Some(position);
    }
}
