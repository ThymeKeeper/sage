use crossterm::{
    cursor,
    execute,
    terminal::{Clear, ClearType},
    style::{Color, Print, ResetColor, SetForegroundColor},
};
use std::io::{self, Write};

#[derive(Debug, Clone)]
pub struct OutputEntry {
    pub execution_count: usize,
    pub cell_line: usize,
    pub output: String,
    pub is_error: bool,
    pub elapsed_secs: f64,
}

pub struct OutputPane {
    outputs: Vec<OutputEntry>,
    scroll_offset: usize, // Line offset for scrolling
    horizontal_offset: usize, // Horizontal scroll offset
    focused: bool,
    auto_scroll: bool, // When true, always show the most recent output
    cursor_line: usize, // Cursor position (line number in flattened output)
    cursor_col: usize, // Cursor column
    selection_start: Option<(usize, usize)>, // Selection start (line, col)
    viewport_height: usize, // Height of visible area for scrolling
    viewport_width: usize, // Width of visible area for horizontal scrolling
}

impl OutputPane {
    pub fn new() -> Self {
        OutputPane {
            outputs: Vec::new(),
            scroll_offset: 0,
            horizontal_offset: 0,
            focused: false,
            auto_scroll: true,
            cursor_line: 0,
            cursor_col: 0,
            selection_start: None,
            viewport_height: 10, // Default, will be updated in draw
            viewport_width: 80, // Default, will be updated in draw
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        let was_focused = self.focused;
        self.focused = focused;

        // When gaining focus, position cursor at a visible location and clear selection
        if self.focused && !was_focused {
            let total_lines = self.count_total_lines();
            if total_lines > 0 {
                // Position cursor at the last line (same as auto-scroll position)
                self.cursor_line = total_lines.saturating_sub(1);
                self.cursor_col = 0;
                self.auto_scroll = true; // Ensure we're showing the bottom
            }
            // Clear any selection when gaining focus
            self.selection_start = None;
        }
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn toggle_focus(&mut self) {
        self.focused = !self.focused;

        // When gaining focus, position cursor at a visible location and clear selection
        if self.focused {
            let total_lines = self.count_total_lines();
            if total_lines > 0 {
                // Position cursor at the last line (same as auto-scroll position)
                self.cursor_line = total_lines.saturating_sub(1);
                self.cursor_col = 0;
                self.auto_scroll = true; // Ensure we're showing the bottom
            }
            // Clear any selection when gaining focus
            self.selection_start = None;
        }
    }

    pub fn add_output(&mut self, entry: OutputEntry) {
        self.outputs.push(entry);
        // Auto-scroll to bottom to show newest output
        self.scroll_to_bottom();
    }

    pub fn scroll_to_bottom(&mut self) {
        // Enable auto-scroll mode and move cursor to end
        self.auto_scroll = true;
        let total_lines = self.count_total_lines();
        if total_lines > 0 {
            self.cursor_line = total_lines - 1;
            self.cursor_col = 0;
        }
        // Clear any selection
        self.selection_start = None;
    }

    pub fn clear(&mut self) {
        self.outputs.clear();
        self.scroll_offset = 0;
    }

    pub fn scroll_up(&mut self) {
        self.auto_scroll = false;
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        self.auto_scroll = false;
        let total_lines = self.count_total_lines();
        if self.scroll_offset + 1 < total_lines {
            self.scroll_offset += 1;
        } else {
            self.auto_scroll = true;
        }
    }

    /// Move cursor up one line
    pub fn move_cursor_up(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.auto_scroll = false;
            self.ensure_cursor_visible();
        }
    }

    /// Move cursor down one line
    pub fn move_cursor_down(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        let total_lines = self.count_total_lines();
        if self.cursor_line + 1 < total_lines {
            self.cursor_line += 1;
            self.auto_scroll = false;
            self.ensure_cursor_visible();
        }
    }

    /// Move cursor left one character
    pub fn move_cursor_left(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            // Move to end of previous line
            self.cursor_line -= 1;
            self.cursor_col = self.get_line_length(self.cursor_line);
        }
        self.ensure_cursor_visible();
    }

    /// Move cursor right one character
    pub fn move_cursor_right(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        let line_len = self.get_line_length(self.cursor_line);
        // Allow cursor up to line_len (one past last char) for full scrolling
        if self.cursor_col < line_len {
            self.cursor_col += 1;
        } else if self.cursor_col == line_len {
            // At the virtual position after last char - move to next line
            let total_lines = self.count_total_lines();
            if self.cursor_line + 1 < total_lines {
                self.cursor_line += 1;
                self.cursor_col = 0;
            }
        }
        self.ensure_cursor_visible();
    }

    /// Move cursor to start of line
    pub fn move_cursor_home(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        self.cursor_col = 0;
        self.ensure_cursor_visible();
    }

    /// Move cursor to end of line
    pub fn move_cursor_end(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        self.cursor_col = self.get_line_length(self.cursor_line);
        self.ensure_cursor_visible();
    }

    /// Ensure cursor is visible in viewport
    fn ensure_cursor_visible(&mut self) {
        // Vertical scrolling
        if self.cursor_line < self.scroll_offset {
            self.scroll_offset = self.cursor_line;
        }
        else if self.cursor_line >= self.scroll_offset + self.viewport_height {
            self.scroll_offset = self.cursor_line.saturating_sub(self.viewport_height - 1);
        }

        // Horizontal scrolling
        // Account for indent (2 for headers, 4 for content) + 2 margin (must match rendering!)
        let indent = 4; // Use content indent as reference
        let visible_width = self.viewport_width.saturating_sub(indent + 2);

        if visible_width > 0 {
            // Scroll left if cursor is before viewport
            if self.cursor_col < self.horizontal_offset {
                self.horizontal_offset = self.cursor_col;
            }
            // Scroll right if cursor is past viewport
            else if self.cursor_col >= self.horizontal_offset + visible_width {
                // Position offset to show maximum content
                // When cursor is at line_len, we want to show from (line_len - visible_width) to (line_len - 1)
                if self.cursor_col >= visible_width {
                    self.horizontal_offset = self.cursor_col.saturating_sub(visible_width);
                } else {
                    self.horizontal_offset = 0;
                }
            }
        }
    }

    /// Get the length of a specific line (in characters, not bytes)
    fn get_line_length(&self, line_idx: usize) -> usize {
        let lines = self.get_all_lines();
        if line_idx < lines.len() {
            lines[line_idx].0.chars().count()  // Use char count, not byte length
        } else {
            0
        }
    }

    /// Get all lines with metadata (for cursor operations)
    fn get_all_lines(&self) -> Vec<(String, bool, bool)> {
        let mut all_lines = Vec::new();
        for entry in &self.outputs {
            all_lines.push((format!("Cell {} ({:.3}s):", entry.cell_line, entry.elapsed_secs), true, false));
            for line in entry.output.lines() {
                all_lines.push((line.to_string(), false, entry.is_error));
            }
            all_lines.push((String::new(), false, false));
        }
        all_lines
    }

    /// Get selected text
    pub fn get_selected_text(&self) -> Option<String> {
        if let Some((start_line, start_col)) = self.selection_start {
            let lines = self.get_all_lines();
            let (end_line, end_col) = (self.cursor_line, self.cursor_col);

            // Normalize selection (start should be before end)
            let ((sel_start_line, sel_start_col), (sel_end_line, sel_end_col)) =
                if start_line < end_line || (start_line == end_line && start_col < end_col) {
                    ((start_line, start_col), (end_line, end_col))
                } else {
                    ((end_line, end_col), (start_line, start_col))
                };

            let mut selected = String::new();
            for line_idx in sel_start_line..=sel_end_line.min(lines.len().saturating_sub(1)) {
                let line_text = &lines[line_idx].0;
                let line_chars: Vec<char> = line_text.chars().collect();
                let line_char_count = line_chars.len();

                if line_idx == sel_start_line && line_idx == sel_end_line {
                    // Selection within single line - use char-based indexing
                    let start = sel_start_col.min(line_char_count);
                    let end = sel_end_col.min(line_char_count);
                    let substring: String = line_chars[start..end].iter().collect();
                    selected.push_str(&substring);
                } else if line_idx == sel_start_line {
                    // First line of multi-line selection
                    let start = sel_start_col.min(line_char_count);
                    let substring: String = line_chars[start..].iter().collect();
                    selected.push_str(&substring);
                    selected.push('\n');
                } else if line_idx == sel_end_line {
                    // Last line of multi-line selection
                    let end = sel_end_col.min(line_char_count);
                    let substring: String = line_chars[..end].iter().collect();
                    selected.push_str(&substring);
                } else {
                    // Middle lines
                    selected.push_str(line_text);
                    selected.push('\n');
                }
            }

            if !selected.is_empty() {
                Some(selected)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// Count total lines across all output entries
    fn count_total_lines(&self) -> usize {
        let mut total = 0;
        for entry in &self.outputs {
            // Header line
            total += 1;
            // Output lines
            total += entry.output.lines().count();
            // Blank line between entries
            total += 1;
        }
        total
    }

    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    pub fn draw<W: Write>(&mut self, writer: &mut W, start_row: u16, height: usize, width: u16) -> io::Result<()> {
        // Clear all rows in the output pane area first (to handle resizing)
        for row in start_row..=(start_row + height as u16) {
            execute!(
                writer,
                cursor::MoveTo(0, row),
                Clear(ClearType::CurrentLine)
            )?;
        }

        // Draw separator line
        execute!(
            writer,
            cursor::MoveTo(0, start_row),
            SetForegroundColor(Color::DarkGrey),
            Print("─".repeat(width as usize)),
            ResetColor
        )?;

        // Draw title
        let title = if self.outputs.is_empty() {
            " Output (Esc to focus, Ctrl+O to toggle, Ctrl+L to clear) "
        } else {
            " Output (Esc to focus, arrows/mouse to scroll, Ctrl+O/L to toggle/clear) "
        };
        execute!(
            writer,
            cursor::MoveTo(2, start_row),
            SetForegroundColor(Color::Cyan),
            Print(title),
            ResetColor
        )?;

        if self.outputs.is_empty() {
            // Show hint
            execute!(
                writer,
                cursor::MoveTo(2, start_row + 1),
                SetForegroundColor(Color::DarkGrey),
                Print("No output yet. Execute a cell with Ctrl+E or Ctrl+Enter"),
                ResetColor
            )?;
            return Ok(());
        }

        // Draw outputs with line-by-line scrolling
        let mut current_row = start_row + 1;
        let max_row = start_row + height as u16;

        // Update viewport dimensions
        let display_lines = (height - 1) as usize; // Lines available for display
        self.viewport_height = display_lines;
        self.viewport_width = width as usize;

        // Calculate scroll offset - if auto_scroll, show the last lines
        let total_lines = self.count_total_lines();
        let line_offset = if self.auto_scroll {
            // Show the last N lines
            total_lines.saturating_sub(display_lines)
        } else {
            self.scroll_offset.min(total_lines.saturating_sub(1))
        };

        // Build a flat list of all lines with their metadata
        let mut all_lines: Vec<(String, bool, bool)> = Vec::new(); // (line_text, is_header, is_error)
        for entry in &self.outputs {
            // Add header line with elapsed time
            all_lines.push((format!("Cell {} ({:.3}s):", entry.cell_line, entry.elapsed_secs), true, false));

            // Add output lines (no truncation - horizontal scrolling will handle this)
            for line in entry.output.lines() {
                all_lines.push((line.to_string(), false, entry.is_error));
            }

            // Add blank line
            all_lines.push((String::new(), false, false));
        }

        // Calculate selection range if exists
        let selection_range = self.selection_start.map(|(start_line, start_col)| {
            let (end_line, end_col) = (self.cursor_line, self.cursor_col);
            if start_line < end_line || (start_line == end_line && start_col < end_col) {
                ((start_line, start_col), (end_line, end_col))
            } else {
                ((end_line, end_col), (start_line, start_col))
            }
        });

        // Track cursor screen position for rendering later
        let mut cursor_screen_row = None;
        let mut cursor_screen_col = None;

        // Draw lines starting from line_offset
        for (absolute_line_idx, (line_text, is_header, is_error)) in all_lines.iter().enumerate() {
            // Skip lines before line_offset
            if absolute_line_idx < line_offset {
                continue;
            }

            if current_row >= max_row {
                break;
            }

            let indent = if *is_header { 2 } else { 4 };

            // Apply horizontal scrolling - get visible portion of line
            // Use char-based indexing to avoid UTF-8 boundary panics
            let visible_width = (width as usize).saturating_sub(indent + 2);
            let h_offset = self.horizontal_offset;
            let char_count = line_text.chars().count();
            let visible_line_owned: String = if h_offset < char_count {
                let end = (h_offset + visible_width).min(char_count);
                line_text.chars().skip(h_offset).take(end - h_offset).collect()
            } else {
                String::new()
            };
            let visible_line = visible_line_owned.as_str();

            // Check if cursor is on this line
            let is_cursor_line = self.focused && absolute_line_idx == self.cursor_line;
            if is_cursor_line {
                cursor_screen_row = Some(current_row);
                // Cursor column in screen space = indent + (cursor_col - horizontal_offset)
                let screen_col = if self.cursor_col >= h_offset {
                    indent as u16 + (self.cursor_col - h_offset) as u16
                } else {
                    indent as u16
                };
                cursor_screen_col = Some(screen_col.min(width - 1));
            }

            // Draw line with selection highlighting
            if let Some(((sel_start_line, sel_start_col), (sel_end_line, sel_end_col))) = selection_range {
                if absolute_line_idx >= sel_start_line && absolute_line_idx <= sel_end_line {
                    // This line has selection - adjust for horizontal offset
                    let (sel_from, sel_to) = if absolute_line_idx == sel_start_line && absolute_line_idx == sel_end_line {
                        (sel_start_col, sel_end_col)
                    } else if absolute_line_idx == sel_start_line {
                        (sel_start_col, line_text.len())
                    } else if absolute_line_idx == sel_end_line {
                        (0, sel_end_col)
                    } else {
                        (0, line_text.len())
                    };

                    // Translate selection to visible range
                    let vis_sel_from = if sel_from > h_offset {
                        sel_from - h_offset
                    } else if sel_from == h_offset {
                        0
                    } else {
                        // Selection starts before visible area
                        0
                    };
                    let vis_sel_to = if sel_to > h_offset {
                        (sel_to - h_offset).min(visible_line.len())
                    } else {
                        0
                    };

                    execute!(writer, cursor::MoveTo(indent as u16, current_row))?;

                    // Use char-based slicing to avoid UTF-8 boundary issues
                    let vis_line_chars: Vec<char> = visible_line.chars().collect();
                    let vis_line_len = vis_line_chars.len();
                    let vis_sel_from = vis_sel_from.min(vis_line_len);
                    let vis_sel_to = vis_sel_to.min(vis_line_len);

                    // Draw before selection
                    if vis_sel_from > 0 {
                        let before: String = vis_line_chars[..vis_sel_from].iter().collect();
                        if *is_header {
                            execute!(writer, SetForegroundColor(Color::Green), Print(before), ResetColor)?;
                        } else if *is_error {
                            execute!(writer, SetForegroundColor(Color::Red), Print(before), ResetColor)?;
                        } else {
                            execute!(writer, Print(before))?;
                        }
                    }

                    // Draw selection (inverted colors)
                    if vis_sel_to > vis_sel_from {
                        let selected: String = vis_line_chars[vis_sel_from..vis_sel_to].iter().collect();
                        execute!(writer, crossterm::style::SetBackgroundColor(crossterm::style::Color::White),
                                 crossterm::style::SetForegroundColor(crossterm::style::Color::Black),
                                 Print(selected),
                                 crossterm::style::ResetColor)?;
                    }

                    // Draw after selection
                    if vis_sel_to < vis_line_len {
                        let after: String = vis_line_chars[vis_sel_to..].iter().collect();
                        if *is_header {
                            execute!(writer, SetForegroundColor(Color::Green), Print(after), ResetColor)?;
                        } else if *is_error {
                            execute!(writer, SetForegroundColor(Color::Red), Print(after), ResetColor)?;
                        } else {
                            execute!(writer, Print(after))?;
                        }
                    }
                } else {
                    // No selection on this line
                    execute!(writer, cursor::MoveTo(indent as u16, current_row))?;
                    if *is_header {
                        execute!(writer, SetForegroundColor(Color::Green), Print(visible_line), ResetColor)?;
                    } else if *is_error {
                        execute!(writer, SetForegroundColor(Color::Red), Print(visible_line), ResetColor)?;
                    } else {
                        execute!(writer, Print(visible_line))?;
                    }
                }
            } else {
                // No selection anywhere
                execute!(writer, cursor::MoveTo(indent as u16, current_row))?;
                if *is_header {
                    execute!(writer, SetForegroundColor(Color::Green), Print(visible_line), ResetColor)?;
                } else if *is_error {
                    execute!(writer, SetForegroundColor(Color::Red), Print(visible_line), ResetColor)?;
                } else {
                    execute!(writer, Print(visible_line))?;
                }
            }

            current_row += 1;
        }

        // Show scroll indicator if not showing all lines
        if line_offset > 0 || line_offset + display_lines < all_lines.len() {
            let scroll_info = format!(" {}-{}/{} ",
                line_offset + 1,
                (line_offset + display_lines).min(all_lines.len()),
                all_lines.len()
            );
            execute!(
                writer,
                cursor::MoveTo(width.saturating_sub(scroll_info.len() as u16 + 2), start_row),
                SetForegroundColor(Color::DarkGrey),
                Print(scroll_info),
                ResetColor
            )?;
        }

        // Show cursor if focused and visible (AFTER drawing scroll indicator)
        if self.focused {
            if let (Some(row), Some(col)) = (cursor_screen_row, cursor_screen_col) {
                execute!(writer, cursor::MoveTo(col, row), crossterm::cursor::Show)?;
            }
        }

        writer.flush()?;
        Ok(())
    }
}
