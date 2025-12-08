use std::time::{Duration, Instant};
use super::Editor;

impl Editor {
    /// Start a mouse selection
    pub fn start_mouse_selection(&mut self, position: usize) {
        let now = Instant::now();
        let double_click_time = Duration::from_millis(500);

        // Clear preferred column on mouse interaction
        self.preferred_column = None;

        // Check for double/triple click
        if let Some(last_time) = self.last_click_time {
            if now.duration_since(last_time) < double_click_time {
                if let Some(last_pos) = self.last_click_position {
                    // Check if clicking near the same position (within 3 characters)
                    let pos_diff = if position > last_pos {
                        position - last_pos
                    } else {
                        last_pos - position
                    };

                    if pos_diff <= 3 {
                        self.click_count += 1;
                        if self.click_count > 3 {
                            self.click_count = 1;
                        }
                    } else {
                        self.click_count = 1;
                    }
                } else {
                    self.click_count = 1;
                }
            } else {
                self.click_count = 1;
            }
        } else {
            self.click_count = 1;
        }

        self.last_click_time = Some(now);
        self.last_click_position = Some(position);

        match self.click_count {
            2 => {
                // Double click - select word
                self.select_word_at(position);
                self.mouse_selecting = false; // Don't continue selecting on drag
                self.update_viewport_for_cursor();
            }
            3 => {
                // Triple click - select line
                self.select_line_at(position);
                self.mouse_selecting = false; // Don't continue selecting on drag
                self.update_viewport_for_cursor();
            }
            _ => {
                // Single click - start normal selection
                self.cursor = position;
                self.selection_start = None;
                self.mouse_selecting = true;
                self.update_viewport_for_cursor();
            }
        }
    }

    /// Skip forward over spaces from a given position to exclude them from selection
    /// Exception: preserve leading indentation spaces at the start of lines
    fn skip_forward_spaces(&self, position: usize) -> usize {
        // Check if we're at the start of a line (including indentation)
        let line = self.buffer.byte_to_line(position);
        let line_start = self.buffer.line_to_byte(line);
        let line_text = self.buffer.line(line);

        // Find where the actual content (non-space) begins on this line
        let mut first_non_space = 0;
        for ch in line_text.chars() {
            if ch != ' ' && ch != '\t' {
                break;
            }
            first_non_space += ch.len_utf8();
        }

        // If position is within the leading indentation, don't skip spaces
        if position >= line_start && position <= line_start + first_non_space {
            return position;
        }

        // Otherwise, skip forward over spaces
        let content = self.buffer.to_string();
        let bytes = content.as_bytes();
        let mut pos = position;

        while pos < bytes.len() && bytes[pos] == b' ' {
            pos += 1;
        }

        pos
    }

    /// Select the word at the given position
    fn select_word_at(&mut self, position: usize) {
        let content = self.buffer.to_string();
        let chars: Vec<char> = content.chars().collect();

        // Convert byte position to char position
        let mut byte_pos = 0;
        let mut char_pos = 0;
        for (i, ch) in chars.iter().enumerate() {
            if byte_pos >= position {
                char_pos = i;
                break;
            }
            byte_pos += ch.len_utf8();
        }

        // Find word boundaries
        let mut start_char = char_pos;
        let mut end_char = char_pos;

        // Move start backward to beginning of word
        while start_char > 0 && (chars[start_char - 1].is_alphanumeric() || chars[start_char - 1] == '_') {
            start_char -= 1;
        }

        // Move end forward to end of word
        while end_char < chars.len() && (chars[end_char].is_alphanumeric() || chars[end_char] == '_') {
            end_char += 1;
        }

        // Convert char positions back to byte positions
        let mut start_byte = 0;
        for i in 0..start_char {
            start_byte += chars[i].len_utf8();
        }

        let mut end_byte = start_byte;
        for i in start_char..end_char {
            end_byte += chars[i].len_utf8();
        }

        // Set selection
        if start_byte < end_byte {
            self.set_selection_start(start_byte);
            self.cursor = end_byte;
        }
    }

    /// Select the line at the given position
    fn select_line_at(&mut self, position: usize) {
        let line = self.buffer.byte_to_line(position);
        let line_start = self.buffer.line_to_byte(line);
        let line_text = self.buffer.line(line);

        // Don't include the newline in the selection
        let line_end = if line_text.ends_with('\n') {
            line_start + line_text.len() - 1
        } else {
            line_start + line_text.len()
        };

        self.set_selection_start(line_start);
        self.cursor = line_end;
    }

    /// Update mouse selection while dragging
    pub fn update_mouse_selection(&mut self, position: usize) {
        if self.mouse_selecting {
            if self.selection_start.is_none() {
                // Start selection from the initial cursor position
                self.set_selection_start(self.cursor);
            }
            // Update cursor to current mouse position
            self.cursor = position;
            self.update_viewport_for_cursor();
        }
    }

    /// Finish mouse selection
    pub fn finish_mouse_selection(&mut self) {
        self.mouse_selecting = false;
        // If selection start equals cursor, clear the selection
        if let Some(start) = self.selection_start {
            if start == self.cursor {
                self.selection_start = None;
            }
        }
    }
}
