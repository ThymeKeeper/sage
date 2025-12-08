use unicode_width::UnicodeWidthChar;
use super::Editor;

impl Editor {
    pub fn cursor_position(&self) -> (usize, usize) {
        let line = self.buffer.byte_to_line(self.cursor);
        let line_start = self.buffer.line_to_byte(line);

        // Calculate display column by summing character widths
        let line_text = self.buffer.line(line);
        let mut byte_pos = 0;
        let mut display_col = 0;

        for ch in line_text.chars() {
            if line_start + byte_pos >= self.cursor {
                break;
            }
            display_col += ch.width().unwrap_or(1);
            byte_pos += ch.len_utf8();
        }

        (line, display_col)
    }

    /// Get position of a byte offset as (line, display_column)
    pub(super) fn byte_position_to_display(&self, byte_pos: usize) -> (usize, usize) {
        let line = self.buffer.byte_to_line(byte_pos);
        let line_start = self.buffer.line_to_byte(line);

        // Calculate display column by summing character widths
        let line_text = self.buffer.line(line);
        let mut current_byte_pos = 0;
        let mut display_col = 0;

        for ch in line_text.chars() {
            if line_start + current_byte_pos >= byte_pos {
                break;
            }
            display_col += ch.width().unwrap_or(1);
            current_byte_pos += ch.len_utf8();
        }

        (line, display_col)
    }

    /// Update viewport to follow cursor - call this when cursor moves
    /// bottom_window_height: height of any bottom window (output pane, find/replace, etc.)
    pub fn update_viewport_for_cursor(&mut self) {
        self.update_viewport_for_cursor_with_bottom(0);
    }

    /// Update viewport to follow cursor with bottom window
    pub fn update_viewport_for_cursor_with_bottom(&mut self, bottom_window_height: usize) {
        // Get terminal size
        if let Ok((width, height)) = crossterm::terminal::size() {
            // Account for status bar and bottom window (output pane, find/replace, etc.)
            let viewport_height = (height as usize).saturating_sub(1 + bottom_window_height);
            let viewport_width = width as usize;
            self.update_viewport(viewport_height, viewport_width);
        }
    }

    /// Update viewport to follow cursor with scrolloff
    pub(super) fn update_viewport(&mut self, viewport_height: usize, viewport_width: usize) {
        let scrolloff = 3;
        let (cursor_line, cursor_col) = self.cursor_position();

        // Logical line includes the 2 virtual lines before the buffer
        let logical_cursor_line = cursor_line + 3;

        // Vertical scrolling
        let cursor_screen_row = logical_cursor_line.saturating_sub(self.viewport_offset.0);

        // Upward scrolling: but respect that we can't go above 0
        if cursor_screen_row < scrolloff && self.viewport_offset.0 > 0 {
            let desired_offset = logical_cursor_line.saturating_sub(scrolloff);
            self.viewport_offset.0 = desired_offset.max(0);
        }
        // Downward scrolling
        else if cursor_screen_row >= viewport_height - scrolloff {
            self.viewport_offset.0 = logical_cursor_line + scrolloff - viewport_height;
        }

        // Horizontal scrolling - consider both cursor and selection start
        let mut left_col = cursor_col;
        let mut right_col = cursor_col;

        // If there's a selection, we need to consider both ends
        if let Some(sel_start) = self.selection_start {
            let (_, sel_col) = self.byte_position_to_display(sel_start);
            left_col = left_col.min(sel_col);
            right_col = right_col.max(sel_col);
        }

        // Ensure the leftmost position is visible with scrolloff
        if left_col < self.viewport_offset.1 + scrolloff {
            self.viewport_offset.1 = left_col.saturating_sub(scrolloff);
        }
        // Always check if we need to scroll right for the cursor
        if cursor_col >= self.viewport_offset.1 + viewport_width - scrolloff {
            self.viewport_offset.1 = cursor_col + scrolloff + 1 - viewport_width;
        }
    }

    /// Convert screen coordinates to buffer position
    pub fn screen_to_buffer_position(&self, screen_col: usize, screen_row: usize) -> Option<usize> {
        // Get terminal height to check if click is on status bar
        if let Ok((_, height)) = crossterm::terminal::size() {
            // Ignore clicks on the status bar (last row)
            if screen_row >= (height - 1) as usize {
                return None;
            }
        }

        // Calculate logical line from screen row
        let logical_line = self.viewport_offset.0 + screen_row;

        // Virtual lines before the buffer (lines 0 and 1)
        if logical_line < 2 {
            return Some(0); // Click on virtual lines maps to start of buffer
        }

        // Map logical line to buffer line
        let buffer_line = logical_line - 2;

        // Check if the line exists in the buffer
        if buffer_line >= self.buffer.len_lines() {
            // Click beyond the buffer content
            return Some(self.buffer.len_bytes());
        }

        // Get the line content
        let line = self.buffer.line(buffer_line);
        let line_start = self.buffer.line_to_byte(buffer_line);

        // Calculate the display column accounting for horizontal scroll
        let target_display_col = screen_col + self.viewport_offset.1;

        // Find the byte position for the target display column
        let mut byte_pos = 0;
        let mut display_col = 0;

        for ch in line.chars() {
            if display_col >= target_display_col {
                break;
            }

            let char_width = ch.width().unwrap_or(1);

            // Check if clicking in the middle of a wide character
            if display_col + char_width > target_display_col {
                // Click is within this character, decide based on position
                if target_display_col - display_col < char_width / 2 {
                    // Closer to start of character
                    break;
                } else {
                    // Closer to end of character
                    byte_pos += ch.len_utf8();
                    break;
                }
            }

            display_col += char_width;
            byte_pos += ch.len_utf8();
        }

        // Don't include the newline in selection
        if line.ends_with('\n') && byte_pos >= line.len() - 1 {
            byte_pos = line.len().saturating_sub(1);
        }

        Some(line_start + byte_pos)
    }

    /// Scroll viewport vertically without moving cursor
    pub fn scroll_viewport_vertical(&mut self, lines: i32) {
        if lines > 0 {
            // Scrolling down (viewport moves down, content moves up)
            self.viewport_offset.0 = self.viewport_offset.0.saturating_add(lines as usize);
            // Don't scroll past the end of the buffer
            // We have 2 virtual lines before the buffer and allow some after
            let max_offset = self.buffer.len_lines().saturating_add(2).saturating_add(10);
            if self.viewport_offset.0 > max_offset {
                self.viewport_offset.0 = max_offset;
            }
        } else {
            // Scrolling up (viewport moves up, content moves down)
            self.viewport_offset.0 = self.viewport_offset.0.saturating_sub((-lines) as usize);
        }
    }

    /// Scroll viewport horizontally without moving cursor
    pub fn scroll_viewport_horizontal(&mut self, cols: i32) {
        if cols > 0 {
            // Scrolling right (viewport moves right, content moves left)
            self.viewport_offset.1 = self.viewport_offset.1.saturating_add(cols as usize);

            // Find the longest line to limit scrolling
            let mut max_line_width = 0;
            for i in 0..self.buffer.len_lines() {
                let line = self.buffer.line(i);
                let mut line_width = 0;
                for ch in line.chars() {
                    if ch == '\n' {
                        break;
                    }
                    line_width += ch.width().unwrap_or(1);
                }
                max_line_width = max_line_width.max(line_width);
            }

            // Limit horizontal scrolling to the longest line + some padding
            let max_offset = max_line_width.saturating_add(20);
            if self.viewport_offset.1 > max_offset {
                self.viewport_offset.1 = max_offset;
            }
        } else {
            // Scrolling left (viewport moves left, content moves right)
            self.viewport_offset.1 = self.viewport_offset.1.saturating_sub((-cols) as usize);
        }
    }

    pub fn viewport_offset(&self) -> (usize, usize) {
        self.viewport_offset
    }

    pub fn set_viewport_offset(&mut self, offset: (usize, usize)) {
        self.viewport_offset = offset;
    }

    /// Get cursor screen position (for drawing overlays like autocomplete)
    pub fn cursor_screen_position(&self) -> (usize, usize) {
        let (cursor_line, cursor_col) = self.cursor_position();
        let (viewport_row, viewport_col) = self.viewport_offset();

        // Calculate screen position (add 2 for virtual lines before buffer)
        let logical_cursor_line = cursor_line + 2;
        let screen_row = logical_cursor_line.saturating_sub(viewport_row);
        let screen_col = cursor_col.saturating_sub(viewport_col);

        (screen_col, screen_row)
    }
}
