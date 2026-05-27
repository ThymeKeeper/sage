use crossterm::{
    cursor,
    execute,
    terminal::{Clear, ClearType},
    style::{Color, Print, ResetColor, SetForegroundColor},
};
use std::io::{self, Write};
use std::time::{Duration, Instant};

/// Calculate the display width of a string, ignoring ANSI escape sequences.
/// Handles both CSI sequences (\x1b[...m for colors) and OSC sequences (\x1b]...ST for hyperlinks).
/// OSC 8 hyperlinks: \x1b]8;;URL\x1b\\ or \x1b]8;;URL\x07
fn display_width(s: &str) -> usize {
    let mut width = 0;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\x1b' && i + 1 < chars.len() {
            if chars[i + 1] == '[' {
                // CSI sequence: \x1b[...m
                i += 2;
                while i < chars.len() && chars[i] != 'm' {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // Skip the 'm'
                }
            } else if chars[i + 1] == ']' {
                // OSC sequence: \x1b]...ST (ST is \x1b\\ or \x07)
                i += 2;
                while i < chars.len() {
                    if chars[i] == '\x07' {
                        i += 1;
                        break;
                    } else if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else {
                // Unknown escape, skip just the ESC
                i += 1;
            }
        } else {
            width += 1;
            i += 1;
        }
    }
    width
}

/// Strip all ANSI escape sequences from a string, returning only the visible text.
/// Handles both CSI sequences (\x1b[...m) and OSC sequences (\x1b]...ST).
fn strip_ansi(s: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\x1b' && i + 1 < chars.len() {
            if chars[i + 1] == '[' {
                // CSI sequence: \x1b[...m
                i += 2;
                while i < chars.len() && chars[i] != 'm' {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // Skip the 'm'
                }
            } else if chars[i + 1] == ']' {
                // OSC sequence: \x1b]...ST (ST is \x1b\\ or \x07)
                i += 2;
                while i < chars.len() {
                    if chars[i] == '\x07' {
                        i += 1;
                        break;
                    } else if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else {
                // Unknown escape, skip just the ESC
                i += 1;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    result
}

/// Extract a visible substring from a string containing ANSI escape sequences.
/// Returns a substring that displays `visible_width` characters starting from display position `start`.
/// Handles both CSI sequences (\x1b[...m) and OSC sequences (\x1b]...ST).
/// ANSI escape sequences are preserved in the output as they don't consume display space.
fn slice_with_ansi(s: &str, start: usize, visible_width: usize) -> String {
    let mut result = String::new();
    let mut display_pos = 0;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '\x1b' && i + 1 < chars.len() {
            if chars[i + 1] == '[' {
                // CSI sequence: \x1b[...m
                let seq_start = i;
                i += 2;
                while i < chars.len() && chars[i] != 'm' {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1; // Include the 'm'
                }
                // Include escape sequences that are within or before our visible range
                if display_pos < start + visible_width {
                    for j in seq_start..i {
                        result.push(chars[j]);
                    }
                }
            } else if chars[i + 1] == ']' {
                // OSC sequence: \x1b]...ST (ST is \x1b\\ or \x07)
                let seq_start = i;
                i += 2;
                while i < chars.len() {
                    if chars[i] == '\x07' {
                        i += 1;
                        break;
                    } else if chars[i] == '\x1b' && i + 1 < chars.len() && chars[i + 1] == '\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
                // Include OSC sequences that are within or before our visible range
                if display_pos < start + visible_width {
                    for j in seq_start..i {
                        result.push(chars[j]);
                    }
                }
            } else {
                // Unknown escape, skip just the ESC
                i += 1;
            }
        } else {
            // This is a visible character
            if display_pos >= start && display_pos < start + visible_width {
                result.push(chars[i]);
            }
            display_pos += 1;
            i += 1;
        }
    }

    // Force a full SGR reset at the end so styles can't bleed into the next
    // rendered line. The loop above drops ANSI codes whose display position
    // falls outside the visible range, which means a line wider than the
    // viewport can end with an unclosed color / dim / bold and stain
    // subsequent draws. Reset is idempotent — appending one when state is
    // already reset is a no-op.
    result.push_str("\x1b[0m");
    result
}

#[derive(Debug, Clone)]
pub struct OutputEntry {
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
    mouse_selecting: bool, // True while dragging a selection
    last_click_time: Option<Instant>, // Track time of last click for double click
    click_count: usize, // Count consecutive clicks
    last_click_position: Option<(usize, usize)>, // Last click position (line, col)
    output_start_row: u16, // Starting row of output pane on screen
    preferred_column: Option<usize>, // Preferred column for vertical movement
    // Track last render state to avoid redundant redraws on Windows
    last_render_state: Option<OutputRenderState>,
    // Per-line cache to avoid flickering on Windows (similar to renderer.rs)
    #[cfg(target_os = "windows")]
    last_screen: Vec<String>,
}

/// Tracks the state used for the last render to detect if redraw is needed
#[derive(Clone, PartialEq)]
struct OutputRenderState {
    output_count: usize,
    scroll_offset: usize,
    horizontal_offset: usize,
    focused: bool,
    cursor_line: usize,
    cursor_col: usize,
    selection_start: Option<(usize, usize)>,
    start_row: u16,
    height: usize,
    width: u16,
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
            mouse_selecting: false,
            last_click_time: None,
            click_count: 0,
            last_click_position: None,
            output_start_row: 0,
            preferred_column: None,
            last_render_state: None,
            #[cfg(target_os = "windows")]
            last_screen: Vec::new(),
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
                self.horizontal_offset = 0;
                self.auto_scroll = true; // Ensure we're showing the bottom
            }
            // Clear any selection when gaining focus
            self.selection_start = None;
            self.preferred_column = None;
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
                self.horizontal_offset = 0;
                self.auto_scroll = true; // Ensure we're showing the bottom
            }
            // Clear any selection when gaining focus
            self.selection_start = None;
            self.preferred_column = None;
        }
    }

    /// Invalidate the render cache so the next draw will fully redraw.
    /// Call this when something external (like autocomplete popup) has drawn over the output pane.
    pub fn invalidate_cache(&mut self) {
        self.last_render_state = None;
        #[cfg(target_os = "windows")]
        {
            self.last_screen.clear();
        }
    }

    pub fn add_output(&mut self, entry: OutputEntry) {
        self.outputs.push(entry);
        // Auto-scroll to bottom to show newest output
        self.scroll_to_bottom();
        // Invalidate render cache so next draw will update
        self.last_render_state = None;
    }

    pub fn scroll_to_bottom(&mut self) {
        // Enable auto-scroll mode and move cursor to end
        self.auto_scroll = true;
        self.horizontal_offset = 0;
        let total_lines = self.count_total_lines();
        if total_lines > 0 {
            self.cursor_line = total_lines - 1;
            self.cursor_col = 0;
        }
        // Clear any selection
        self.selection_start = None;
    }

    /// Disable auto-scroll mode and sync scroll_offset to the current view position.
    /// This prevents the view from jumping when switching from auto-scroll to manual scroll.
    fn disable_auto_scroll(&mut self) {
        if self.auto_scroll {
            let total_lines = self.count_total_lines();
            // Sync scroll_offset to match what was being displayed in auto-scroll mode
            self.scroll_offset = total_lines.saturating_sub(self.viewport_height);
            self.auto_scroll = false;
        }
    }

    pub fn clear(&mut self) {
        self.outputs.clear();
        self.scroll_offset = 0;
        self.horizontal_offset = 0;
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.selection_start = None;
        self.auto_scroll = true;
        // Invalidate render caches so next draw will update
        self.last_render_state = None;
        #[cfg(target_os = "windows")]
        {
            self.last_screen.clear();
        }
    }

    pub fn scroll_up(&mut self) {
        // Sync scroll_offset to current view position before manual scrolling
        self.disable_auto_scroll();
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        // Sync scroll_offset to current view position before manual scrolling
        self.disable_auto_scroll();
        let total_lines = self.count_total_lines();
        if self.scroll_offset + 1 < total_lines {
            self.scroll_offset += 1;
        } else {
            // Reached bottom, enable auto-scroll and reset horizontal position
            self.auto_scroll = true;
            self.horizontal_offset = 0;
        }
    }

    /// Move cursor up by one page (viewport height)
    pub fn page_up(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        // Set preferred column if not already set
        if self.preferred_column.is_none() {
            self.preferred_column = Some(self.cursor_col);
        }

        // Move cursor up by viewport height
        let page_size = self.viewport_height.max(1);
        self.cursor_line = self.cursor_line.saturating_sub(page_size);

        // Clamp cursor to new line length using preferred column
        let target_col = self.preferred_column.unwrap();
        let line_len = self.get_line_length(self.cursor_line);
        self.cursor_col = target_col.min(line_len);

        self.disable_auto_scroll();
        self.ensure_cursor_visible();
    }

    /// Move cursor down by one page (viewport height)
    pub fn page_down(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        // Set preferred column if not already set
        if self.preferred_column.is_none() {
            self.preferred_column = Some(self.cursor_col);
        }

        let total_lines = self.count_total_lines();
        let page_size = self.viewport_height.max(1);

        // Move cursor down by viewport height, clamping to last line
        self.cursor_line = (self.cursor_line + page_size).min(total_lines.saturating_sub(1));

        // Clamp cursor to new line length using preferred column
        let target_col = self.preferred_column.unwrap();
        let line_len = self.get_line_length(self.cursor_line);
        self.cursor_col = target_col.min(line_len);

        self.disable_auto_scroll();
        self.ensure_cursor_visible();
    }

    /// Scroll horizontally by delta columns (positive = right, negative = left)
    pub fn scroll_horizontal(&mut self, delta: i32) {
        if delta > 0 {
            self.horizontal_offset = self.horizontal_offset.saturating_add(delta as usize);
        } else {
            self.horizontal_offset = self.horizontal_offset.saturating_sub((-delta) as usize);
        }
    }

    /// Move cursor up one line
    pub fn move_cursor_up(&mut self, with_selection: bool) {
        if self.cursor_line == 0 {
            return;
        }

        // Check if we're on a table data row for cell-aware navigation
        if let Some(cur_stripped) = self.get_stripped_line(self.cursor_line) {
            if Self::is_table_data_row(&cur_stripped) {
                let cur_bounds = Self::find_cell_boundaries(&cur_stripped);
                if !cur_bounds.is_empty() {
                    let cell_idx = Self::cursor_to_cell_index(&cur_bounds, self.cursor_col);

                    // Search upward for the next data row, skipping separators
                    let mut target = self.cursor_line - 1;
                    loop {
                        if let Some(s) = self.get_stripped_line(target) {
                            if Self::is_table_data_row(&s) {
                                let new_bounds = Self::find_cell_boundaries(&s);
                                let clamped = cell_idx.min(new_bounds.len().saturating_sub(1));
                                self.cursor_line = target;
                                self.select_table_cell(&s, &new_bounds, clamped);
                                self.preferred_column = None;
                                self.disable_auto_scroll();
                                self.ensure_cursor_visible();
                                return;
                            } else if !Self::is_table_separator_row(&s) {
                                break;
                            }
                        } else {
                            break;
                        }
                        if target == 0 { break; }
                        target -= 1;
                    }
                }
            }
        }

        // Normal movement
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }
        if self.preferred_column.is_none() {
            self.preferred_column = Some(self.cursor_col);
        }
        self.cursor_line -= 1;
        let target_col = self.preferred_column.unwrap();
        let line_len = self.get_line_length(self.cursor_line);
        self.cursor_col = target_col.min(line_len);
        self.disable_auto_scroll();

        // If we just landed on a table data row (entering from outside), snap to cell
        if let Some(new_stripped) = self.get_stripped_line(self.cursor_line) {
            if Self::is_table_data_row(&new_stripped) {
                let bounds = Self::find_cell_boundaries(&new_stripped);
                if !bounds.is_empty() {
                    let cell_idx = Self::cursor_to_cell_index(&bounds, self.cursor_col);
                    self.select_table_cell(&new_stripped, &bounds, cell_idx);
                    self.preferred_column = None;
                }
            }
        }

        self.ensure_cursor_visible();
    }

    /// Move cursor down one line
    pub fn move_cursor_down(&mut self, with_selection: bool) {
        let total_lines = self.count_total_lines();
        if self.cursor_line + 1 >= total_lines {
            return;
        }

        // Check if we're on a table data row for cell-aware navigation
        if let Some(cur_stripped) = self.get_stripped_line(self.cursor_line) {
            if Self::is_table_data_row(&cur_stripped) {
                let cur_bounds = Self::find_cell_boundaries(&cur_stripped);
                if !cur_bounds.is_empty() {
                    let cell_idx = Self::cursor_to_cell_index(&cur_bounds, self.cursor_col);

                    // Search downward for the next data row, skipping separators
                    let mut target = self.cursor_line + 1;
                    while target < total_lines {
                        if let Some(s) = self.get_stripped_line(target) {
                            if Self::is_table_data_row(&s) {
                                let new_bounds = Self::find_cell_boundaries(&s);
                                let clamped = cell_idx.min(new_bounds.len().saturating_sub(1));
                                self.cursor_line = target;
                                self.select_table_cell(&s, &new_bounds, clamped);
                                self.preferred_column = None;
                                self.disable_auto_scroll();
                                self.ensure_cursor_visible();
                                return;
                            } else if !Self::is_table_separator_row(&s) {
                                break;
                            }
                        } else {
                            break;
                        }
                        target += 1;
                    }
                }
            }
        }

        // Normal movement
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }
        if self.preferred_column.is_none() {
            self.preferred_column = Some(self.cursor_col);
        }
        self.cursor_line += 1;
        let target_col = self.preferred_column.unwrap();
        let line_len = self.get_line_length(self.cursor_line);
        self.cursor_col = target_col.min(line_len);
        self.disable_auto_scroll();

        // If we just landed on a table data row (entering from outside), snap to cell
        if let Some(new_stripped) = self.get_stripped_line(self.cursor_line) {
            if Self::is_table_data_row(&new_stripped) {
                let bounds = Self::find_cell_boundaries(&new_stripped);
                if !bounds.is_empty() {
                    let cell_idx = Self::cursor_to_cell_index(&bounds, self.cursor_col);
                    self.select_table_cell(&new_stripped, &bounds, cell_idx);
                    self.preferred_column = None;
                }
            }
        }

        self.ensure_cursor_visible();
    }

    /// Move cursor left one character (or one table cell if on a table row)
    pub fn move_cursor_left(&mut self, with_selection: bool) {
        if let Some(stripped) = self.get_stripped_line(self.cursor_line) {
            if Self::is_table_data_row(&stripped) {
                let bounds = Self::find_cell_boundaries(&stripped);
                if !bounds.is_empty() {
                    let on_separator = stripped.chars().nth(self.cursor_col)
                        .map_or(false, Self::is_cell_separator);
                    let target_cell = if on_separator {
                        // On a separator — enter the cell to the left
                        let mut found = None;
                        for i in (0..bounds.len()).rev() {
                            if bounds[i].1 <= self.cursor_col {
                                found = Some(i);
                                break;
                            }
                        }
                        found
                    } else {
                        let cell = Self::cursor_to_cell_index(&bounds, self.cursor_col);
                        if cell > 0 { Some(cell - 1) } else { None }
                    };
                    if let Some(cell) = target_cell {
                        self.select_table_cell(&stripped, &bounds, cell);
                    }
                    self.preferred_column = None;
                    self.ensure_cursor_visible();
                    return;
                }
            }
        }

        // Normal movement
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.get_line_length(self.cursor_line);
        }
        self.preferred_column = None;
        self.ensure_cursor_visible();
    }

    /// Move cursor right one character (or one table cell if on a table row)
    pub fn move_cursor_right(&mut self, with_selection: bool) {
        if let Some(stripped) = self.get_stripped_line(self.cursor_line) {
            if Self::is_table_data_row(&stripped) {
                let bounds = Self::find_cell_boundaries(&stripped);
                if !bounds.is_empty() {
                    let on_separator = stripped.chars().nth(self.cursor_col)
                        .map_or(false, Self::is_cell_separator);
                    let target_cell = if on_separator {
                        // On a separator — enter the cell to the right
                        let mut found = None;
                        for i in 0..bounds.len() {
                            if bounds[i].0 > self.cursor_col {
                                found = Some(i);
                                break;
                            }
                        }
                        found
                    } else {
                        let cell = Self::cursor_to_cell_index(&bounds, self.cursor_col);
                        if cell + 1 < bounds.len() { Some(cell + 1) } else { None }
                    };
                    if let Some(cell) = target_cell {
                        self.select_table_cell(&stripped, &bounds, cell);
                    }
                    self.preferred_column = None;
                    self.ensure_cursor_visible();
                    return;
                }
            }
        }

        // Normal movement
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }
        let line_len = self.get_line_length(self.cursor_line);
        if self.cursor_col < line_len {
            self.cursor_col += 1;
        } else if self.cursor_col == line_len {
            let total_lines = self.count_total_lines();
            if self.cursor_line + 1 < total_lines {
                self.cursor_line += 1;
                self.cursor_col = 0;
            }
        }
        self.preferred_column = None;
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
        self.preferred_column = None; // Clear preferred column
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
        self.preferred_column = None; // Clear preferred column
        self.ensure_cursor_visible();
    }

    /// Move cursor to previous word boundary (stops at line start, doesn't cross lines)
    pub fn move_cursor_word_left(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        let lines = self.get_all_lines();
        if self.cursor_line >= lines.len() {
            return;
        }

        let line_text = &lines[self.cursor_line].0;
        // Work with visible text only (strip ANSI escape sequences)
        let visible_text = strip_ansi(line_text);

        if self.cursor_col > 0 {
            // Find the previous word boundary within the current line
            let chars: Vec<char> = visible_text.chars().collect();
            let mut new_col = 0;
            let mut in_word = false;

            for (i, ch) in chars.iter().enumerate() {
                if i >= self.cursor_col {
                    break;
                }

                if ch.is_alphanumeric() || *ch == '_' {
                    if !in_word {
                        // Start of a new word
                        new_col = i;
                        in_word = true;
                    }
                } else {
                    in_word = false;
                }
            }

            self.cursor_col = new_col;
        }
        // Already at start of line - stay there (don't cross to previous line)

        self.preferred_column = None;
        self.disable_auto_scroll();
        self.ensure_cursor_visible();
    }

    /// Move cursor to next word boundary (stops at line end, doesn't cross lines)
    pub fn move_cursor_word_right(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        let lines = self.get_all_lines();
        if self.cursor_line >= lines.len() {
            return;
        }

        let line_text = &lines[self.cursor_line].0;
        // Work with visible text only (strip ANSI escape sequences)
        let visible_text = strip_ansi(line_text);
        let chars: Vec<char> = visible_text.chars().collect();
        let line_len = chars.len();

        if self.cursor_col < line_len {
            // Find the next word boundary within the current line
            // Iterate from the start to properly track word state
            let mut in_word = false;
            let mut found_next_word = false;

            for i in 0..chars.len() {
                let ch = chars[i];

                // Check if we found the start of next word (after cursor position)
                if i > self.cursor_col && !in_word && (ch.is_alphanumeric() || ch == '_') {
                    // Found start of next word
                    self.cursor_col = i;
                    found_next_word = true;
                    break;
                }

                // Update word state
                in_word = ch.is_alphanumeric() || ch == '_';
            }

            if !found_next_word {
                // No more words on this line, go to end of line
                self.cursor_col = line_len;
            }
        }
        // Already at end of line - stay there (don't cross to next line)

        self.preferred_column = None;
        self.disable_auto_scroll();
        self.ensure_cursor_visible();
    }

    /// Move cursor to previous paragraph (empty line boundary)
    pub fn move_cursor_paragraph_up(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        let lines = self.get_all_lines();

        // Search backwards for a non-empty line preceded by an empty line
        let mut target_line = None;
        for line_num in (0..self.cursor_line).rev() {
            let line_text = &lines[line_num].0;
            let is_empty = line_text.is_empty();

            if !is_empty && line_num > 0 {
                let prev_line = &lines[line_num - 1].0;
                if prev_line.is_empty() {
                    target_line = Some(line_num);
                    break;
                }
            }
        }

        if let Some(line) = target_line {
            self.cursor_line = line;
            self.cursor_col = 0;
        } else {
            // No paragraph found, go to start
            self.cursor_line = 0;
            self.cursor_col = 0;
        }

        self.preferred_column = None;
        self.disable_auto_scroll();
        self.ensure_cursor_visible();
    }

    /// Move cursor to next paragraph (empty line boundary)
    pub fn move_cursor_paragraph_down(&mut self, with_selection: bool) {
        if with_selection && self.selection_start.is_none() {
            self.selection_start = Some((self.cursor_line, self.cursor_col));
        } else if !with_selection {
            self.selection_start = None;
        }

        let lines = self.get_all_lines();
        let total_lines = lines.len();

        // Search forward for a non-empty line preceded by an empty line
        let mut found_empty = false;
        let mut target_line = None;

        for line_num in (self.cursor_line + 1)..total_lines {
            let line_text = &lines[line_num].0;
            let is_empty = line_text.is_empty();

            if is_empty {
                found_empty = true;
            } else if found_empty {
                // Found a non-empty line after an empty line
                target_line = Some(line_num);
                break;
            }
        }

        if let Some(line) = target_line {
            self.cursor_line = line;
            self.cursor_col = 0;
        } else {
            // No paragraph found, go to end
            let total_lines = self.count_total_lines();
            if total_lines > 0 {
                self.cursor_line = total_lines - 1;
                self.cursor_col = self.get_line_length(self.cursor_line);
            }
        }

        self.preferred_column = None;
        self.disable_auto_scroll();
        self.ensure_cursor_visible();
    }

    /// Get the indent size for a specific line
    fn get_line_indent(&self, line_idx: usize) -> usize {
        let lines = self.get_all_lines();
        if line_idx < lines.len() {
            if lines[line_idx].1 { 2 } else { 4 } // is_header -> 2, else 4
        } else {
            4 // Default to content indent
        }
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
        // cursor_col is in raw line coordinates (without indent)
        // We need to account for indent when calculating screen position
        let indent = self.get_line_indent(self.cursor_line);
        let cursor_screen_col = indent + self.cursor_col; // Position including indent
        let visible_width = self.viewport_width;

        if visible_width > 0 {
            // When on a table row, scroll to show the full cell (including pipe delimiters)
            // so center-aligned headers in wide columns are fully visible
            let (cell_left, cell_right) = if let Some(stripped) = self.get_stripped_line(self.cursor_line) {
                if Self::is_table_data_row(&stripped) {
                    let bounds = Self::find_cell_boundaries(&stripped);
                    let cell = Self::cursor_to_cell_index(&bounds, self.cursor_col);
                    if cell < bounds.len() {
                        // Include the pipe characters on either side of the cell
                        let left = indent + bounds[cell].0.saturating_sub(1);
                        let right = indent + bounds[cell].1; // the right pipe position
                        (Some(left), Some(right))
                    } else {
                        (None, None)
                    }
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };

            // Jump all the way left when cursor is at start of line (factoring virtual indent)
            if self.cursor_col == 0 {
                self.horizontal_offset = 0;
            } else if let (Some(cl), Some(cr)) = (cell_left, cell_right) {
                // For table cells: ensure the entire cell is visible
                if cl < self.horizontal_offset {
                    // Cell's left edge is off-screen — scroll left to show it
                    self.horizontal_offset = cl;
                } else if cr >= self.horizontal_offset + visible_width {
                    // Cell's right edge is off-screen — scroll right to show it
                    self.horizontal_offset = cr.saturating_sub(visible_width - 1);
                }
            } else {
                // Normal (non-table) scrolling
                if cursor_screen_col < self.horizontal_offset {
                    self.horizontal_offset = cursor_screen_col;
                } else if cursor_screen_col >= self.horizontal_offset + visible_width {
                    self.horizontal_offset = cursor_screen_col.saturating_sub(visible_width - 1);
                }
            }
        }
    }

    /// Ensure cursor is visible with scrolloff zones for drag selection
    fn ensure_cursor_visible_with_scrolloff(&mut self) {
        let scrolloff = 2; // Number of lines to keep as margin (vertical)
        let horizontal_scrolloff = 8; // Number of cols to keep as margin (horizontal)
        let total_lines = self.count_total_lines();

        // Vertical scrolling with scrolloff
        let cursor_relative_line = self.cursor_line.saturating_sub(self.scroll_offset);

        // Scroll up if cursor is in the top scrolloff zone
        if cursor_relative_line < scrolloff && self.scroll_offset > 0 {
            self.scroll_offset = self.cursor_line.saturating_sub(scrolloff);
        }
        // Scroll down if cursor is in the bottom scrolloff zone
        else if cursor_relative_line >= self.viewport_height.saturating_sub(scrolloff) {
            let target_offset = self.cursor_line + scrolloff;
            if target_offset >= self.viewport_height {
                self.scroll_offset = target_offset - self.viewport_height;
            }
            // Don't scroll past the end
            let max_offset = total_lines.saturating_sub(self.viewport_height);
            if self.scroll_offset > max_offset {
                self.scroll_offset = max_offset;
            }
        }

        // Horizontal scrolling with scrolloff
        // cursor_col is in raw line coordinates, add indent for screen position
        let indent = self.get_line_indent(self.cursor_line);
        let cursor_screen_col = indent + self.cursor_col;
        let visible_width = self.viewport_width;

        if visible_width > 0 {
            // Scroll left if cursor is in the left scrolloff zone
            if cursor_screen_col < self.horizontal_offset + horizontal_scrolloff {
                self.horizontal_offset = cursor_screen_col.saturating_sub(horizontal_scrolloff);
            }
            // Scroll right if cursor is in the right scrolloff zone
            else if cursor_screen_col >= self.horizontal_offset + visible_width.saturating_sub(horizontal_scrolloff) {
                self.horizontal_offset = cursor_screen_col + horizontal_scrolloff - visible_width + 1;
            }
        }

        // Reset horizontal scroll when on an empty line
        if self.get_line_length(self.cursor_line) == 0 {
            self.horizontal_offset = 0;
        }
    }

    /// Check if a character is a vertical box-drawing separator (│ or ┆)
    fn is_cell_separator(ch: char) -> bool {
        matches!(ch, '│' | '┆')
    }

    /// Check if a line (ANSI-stripped) is a table data row (starts and ends with │ or ┆)
    fn is_table_data_row(stripped: &str) -> bool {
        let trimmed = stripped.trim_end();
        if trimmed.len() <= 1 { return false; }
        let first = trimmed.chars().next().unwrap();
        let last = trimmed.chars().last().unwrap();
        Self::is_cell_separator(first) && Self::is_cell_separator(last)
    }

    /// Check if a line is a table separator row (┌├└╞ lines)
    fn is_table_separator_row(stripped: &str) -> bool {
        let trimmed = stripped.trim_end();
        !trimmed.is_empty() && matches!(trimmed.chars().next(), Some('┌' | '├' | '└' | '╞'))
    }

    /// Find cell boundaries in a table data row. Returns vec of (start_col, end_col)
    /// pairs for each cell's content region (between separator characters).
    fn find_cell_boundaries(stripped: &str) -> Vec<(usize, usize)> {
        let mut pipes: Vec<usize> = Vec::new();
        for (i, ch) in stripped.chars().enumerate() {
            if Self::is_cell_separator(ch) {
                pipes.push(i);
            }
        }
        let mut cells = Vec::new();
        for w in pipes.windows(2) {
            cells.push((w[0] + 1, w[1]));
        }
        cells
    }

    /// Map a cursor column to a cell index within the given boundaries.
    /// If the cursor is on a separator or before the first cell, returns the
    /// nearest cell to the right; past the last cell returns the last cell.
    fn cursor_to_cell_index(boundaries: &[(usize, usize)], col: usize) -> usize {
        for (i, &(_start, end)) in boundaries.iter().enumerate() {
            if col < end {
                return i;
            }
        }
        boundaries.len().saturating_sub(1)
    }

    /// Find the content range (first non-space to last non-space) within a cell.
    fn cell_content_range(stripped: &str, boundaries: &[(usize, usize)], cell_idx: usize) -> (usize, usize) {
        if cell_idx >= boundaries.len() {
            return (0, 0);
        }
        let (start, end) = boundaries[cell_idx];
        let chars: Vec<char> = stripped.chars().collect();

        let mut first = start;
        while first < end && first < chars.len() && chars[first] == ' ' {
            first += 1;
        }
        let mut last = end;
        while last > first && last > 0 && chars[last - 1] == ' ' {
            last -= 1;
        }
        if first >= last {
            // All spaces — place cursor in the middle
            (start + 1, start + 1)
        } else {
            (first, last)
        }
    }

    /// Select a table cell's content and position cursor at end of it.
    fn select_table_cell(&mut self, stripped: &str, bounds: &[(usize, usize)], cell_idx: usize) {
        let (content_start, content_end) = Self::cell_content_range(stripped, bounds, cell_idx);
        self.selection_start = Some((self.cursor_line, content_start));
        self.cursor_col = content_end;
    }

    /// Get the stripped text for a given line index, or None if out of range.
    fn get_stripped_line(&self, line_idx: usize) -> Option<String> {
        let lines = self.get_all_lines();
        if line_idx < lines.len() {
            Some(strip_ansi(&lines[line_idx].0))
        } else {
            None
        }
    }

    /// Get the length of a specific line (in display columns, accounting for ANSI escapes)
    fn get_line_length(&self, line_idx: usize) -> usize {
        let lines = self.get_all_lines();
        if line_idx < lines.len() {
            display_width(&lines[line_idx].0)  // Use display width, not char count
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

    /// Get selected text (returns only visible text, stripping ANSI escape sequences)
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
                // Work with visible text only (strip ANSI escape sequences)
                let visible_text = strip_ansi(line_text);
                let visible_chars: Vec<char> = visible_text.chars().collect();
                let visible_len = visible_chars.len();

                if line_idx == sel_start_line && line_idx == sel_end_line {
                    // Selection within single line - use display position indexing
                    let start = sel_start_col.min(visible_len);
                    let end = sel_end_col.min(visible_len);
                    let substring: String = visible_chars[start..end].iter().collect();
                    selected.push_str(&substring);
                } else if line_idx == sel_start_line {
                    // First line of multi-line selection
                    let start = sel_start_col.min(visible_len);
                    let substring: String = visible_chars[start..].iter().collect();
                    selected.push_str(&substring);
                    selected.push('\n');
                } else if line_idx == sel_end_line {
                    // Last line of multi-line selection
                    let end = sel_end_col.min(visible_len);
                    let substring: String = visible_chars[..end].iter().collect();
                    selected.push_str(&substring);
                } else {
                    // Middle lines - use full visible text
                    selected.push_str(&visible_text);
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

    pub fn draw<W: Write>(&mut self, writer: &mut W, start_row: u16, height: usize, width: u16) -> io::Result<()> {
        // Save start row for mouse position calculations
        self.output_start_row = start_row;

        // Calculate the effective scroll offset for rendering
        let total_lines = self.count_total_lines();
        let display_lines = height;
        let effective_scroll = if self.auto_scroll {
            total_lines.saturating_sub(display_lines)
        } else {
            self.scroll_offset.min(total_lines.saturating_sub(1))
        };

        // On Windows, check if we actually need to redraw to avoid flicker
        #[cfg(target_os = "windows")]
        {
            let current_state = OutputRenderState {
                output_count: self.outputs.len(),
                scroll_offset: effective_scroll,
                horizontal_offset: self.horizontal_offset,
                focused: self.focused,
                cursor_line: self.cursor_line,
                cursor_col: self.cursor_col,
                selection_start: self.selection_start,
                start_row,
                height,
                width,
            };

            if self.last_render_state.as_ref() == Some(&current_state) {
                // Nothing changed, skip redraw - but still show cursor if focused
                // (because the renderer hides cursor at start of each draw cycle)
                if self.focused {
                    let line_offset = effective_scroll;
                    if self.cursor_line >= line_offset && self.cursor_line < line_offset + display_lines {
                        let cursor_row = start_row + (self.cursor_line - line_offset) as u16;
                        let indent = self.get_line_indent(self.cursor_line);
                        let cursor_screen_col = (indent + self.cursor_col).saturating_sub(self.horizontal_offset);
                        execute!(writer, cursor::MoveTo(cursor_screen_col as u16, cursor_row), crossterm::cursor::Show)?;
                    }
                }
                return Ok(());
            }

            // Save current state for next comparison
            self.last_render_state = Some(current_state);
        }

        // On Windows, use per-line caching to avoid flickering (similar to renderer.rs)
        // Instead of clearing all rows first, we build each line with padding and only
        // write lines that have changed from the cache.
        #[cfg(target_os = "windows")]
        {
            // Ensure cache is sized correctly
            let total_rows = height;
            if self.last_screen.len() != total_rows {
                self.last_screen = vec![String::new(); total_rows];
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            // Clear all rows in the output pane area with output bg (one shade brighter than editor)
            let output_bg = Color::Rgb { r: 33, g: 33, b: 33 };
            for row in start_row..(start_row + height as u16) {
                execute!(
                    writer,
                    cursor::MoveTo(0, row),
                    crossterm::style::SetBackgroundColor(output_bg),
                    Clear(ClearType::CurrentLine)
                )?;
            }
        }

        if self.outputs.is_empty() {
            // On Windows: Clear all content rows when empty (to handle clear() properly)
            #[cfg(target_os = "windows")]
            {
                let empty_line = format!("\x1b[48;2;33;33;33m{}", " ".repeat(width as usize));
                for row_idx in 0..height {
                    let screen_row = start_row + row_idx as u16;
                    // Check cache to avoid unnecessary writes
                    let cache_idx = row_idx;
                    if cache_idx >= self.last_screen.len() || self.last_screen[cache_idx] != empty_line {
                        write!(writer, "\x1b[{};1H{}", screen_row + 1, empty_line)?;
                        if cache_idx < self.last_screen.len() {
                            self.last_screen[cache_idx] = empty_line.clone();
                        }
                    }
                }
            }

            // Show hint
            let hint = "No output yet. Execute a cell with Ctrl+E or Ctrl+Enter";
            #[cfg(target_os = "windows")]
            {
                // Build hint with padding
                let hint_line = format!("\x1b[48;2;33;33;33m\x1b[90m  {}\x1b[39m{}", hint, " ".repeat(width as usize - 2 - hint.len()));
                if self.last_screen.get(0) != Some(&hint_line) {
                    write!(writer, "\x1b[{};1H{}", start_row + 1, hint_line)?;
                    if !self.last_screen.is_empty() {
                        self.last_screen[0] = hint_line;
                    }
                }
            }
            #[cfg(not(target_os = "windows"))]
            {
                execute!(
                    writer,
                    cursor::MoveTo(2, start_row),
                    SetForegroundColor(Color::DarkGrey),
                    Print(hint),
                    ResetColor
                )?;
            }
            return Ok(());
        }

        // Draw outputs with line-by-line scrolling
        let mut current_row = start_row;
        let max_row = start_row + height as u16;

        // Update viewport dimensions
        let display_lines = height; // Lines available for display
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
        // On Windows, we build complete ANSI strings and use per-line caching to avoid flicker
        #[cfg(target_os = "windows")]
        let mut screen_row_idx: usize = 1; // Start at 1 (row 0 is the separator/title)

        for (absolute_line_idx, (line_text, is_header, is_error)) in all_lines.iter().enumerate() {
            // Skip lines before line_offset
            if absolute_line_idx < line_offset {
                continue;
            }

            if current_row >= max_row {
                break;
            }

            // Determine indent and create the full line with indent prepended
            // This makes indentation part of the scrollable content
            let indent_str = if *is_header { "  " } else { "    " };
            let indent_len = indent_str.len();

            // Create the full line with indent prepended (for scrolling purposes)
            // The full_line is: indent + line_text
            // We'll slice this for display, so indent scrolls with content
            let full_line = format!("{}{}", indent_str, line_text);
            let full_line_display_width = display_width(&full_line);

            // Apply horizontal scrolling - get visible portion of the full line
            let visible_width = width as usize;
            let h_offset = self.horizontal_offset;

            // Use ANSI-aware slicing to extract the visible portion
            let visible_line_owned: String = if h_offset < full_line_display_width {
                slice_with_ansi(&full_line, h_offset, visible_width)
            } else {
                String::new()
            };
            let visible_line = visible_line_owned.as_str();
            let visible_line_display_len = display_width(visible_line);

            // Check if cursor is on this line
            let is_cursor_line = self.focused && absolute_line_idx == self.cursor_line;
            if is_cursor_line {
                cursor_screen_row = Some(current_row);
                // Cursor column in screen space:
                // cursor_col is in raw line coordinates (without indent)
                // screen position = indent_len + cursor_col - horizontal_offset
                let cursor_full_col = indent_len + self.cursor_col;
                let screen_col = if cursor_full_col >= h_offset {
                    (cursor_full_col - h_offset) as u16
                } else {
                    0
                };
                cursor_screen_col = Some(screen_col.min(width - 1));
            }

            // On Windows: Build complete line string with ANSI codes and use caching
            #[cfg(target_os = "windows")]
            {
                let mut line_content = String::from("\x1b[48;2;33;33;33m"); // Output pane bg (one shade brighter than editor 234)

                // Calculate selection info for this line
                let selection_info = if let Some(((sel_start_line, sel_start_col), (sel_end_line, sel_end_col))) = selection_range {
                    if absolute_line_idx >= sel_start_line && absolute_line_idx <= sel_end_line {
                        let raw_line_len = display_width(line_text);
                        let (raw_sel_from, raw_sel_to) = if absolute_line_idx == sel_start_line && absolute_line_idx == sel_end_line {
                            (sel_start_col, sel_end_col)
                        } else if absolute_line_idx == sel_start_line {
                            (sel_start_col, raw_line_len)
                        } else if absolute_line_idx == sel_end_line {
                            (0, sel_end_col)
                        } else {
                            (0, raw_line_len)
                        };
                        let full_sel_from = indent_len + raw_sel_from;
                        let full_sel_to = indent_len + raw_sel_to;
                        let vis_sel_from = if full_sel_from > h_offset { full_sel_from - h_offset } else { 0 };
                        let vis_sel_to = if full_sel_to > h_offset { (full_sel_to - h_offset).min(visible_line_display_len) } else { 0 };
                        Some((vis_sel_from.min(visible_line_display_len), vis_sel_to.min(visible_line_display_len)))
                    } else {
                        None
                    }
                } else {
                    None
                };

                // Build line content with colors
                // Use \x1b[39m (reset fg only) instead of \x1b[0m to preserve output pane bg
                if let Some((vis_sel_from, vis_sel_to)) = selection_info {
                    // Line has selection - build in parts
                    if vis_sel_from > 0 {
                        let before = slice_with_ansi(visible_line, 0, vis_sel_from);
                        let before_plain = strip_ansi(&before);
                        if *is_header {
                            line_content.push_str("\x1b[32m"); // Green
                            line_content.push_str(&before_plain);
                            line_content.push_str("\x1b[39m");
                        } else if *is_error {
                            line_content.push_str("\x1b[31m"); // Red
                            line_content.push_str(&before_plain);
                            line_content.push_str("\x1b[39m");
                        } else {
                            line_content.push_str(&before_plain);
                        }
                    }
                    if vis_sel_to > vis_sel_from {
                        let selected = slice_with_ansi(visible_line, vis_sel_from, vis_sel_to - vis_sel_from);
                        let selected_plain = strip_ansi(&selected);
                        // Selection: ANSI 256 teal background with black text
                        line_content.push_str("\x1b[48;5;30m\x1b[30m");
                        line_content.push_str(&selected_plain);
                        line_content.push_str("\x1b[39m\x1b[48;2;33;33;33m"); // Restore fg default + output bg
                    }
                    if vis_sel_to < visible_line_display_len {
                        let after = slice_with_ansi(visible_line, vis_sel_to, visible_line_display_len - vis_sel_to);
                        let after_plain = strip_ansi(&after);
                        if *is_header {
                            line_content.push_str("\x1b[32m");
                            line_content.push_str(&after_plain);
                            line_content.push_str("\x1b[39m");
                        } else if *is_error {
                            line_content.push_str("\x1b[31m");
                            line_content.push_str(&after_plain);
                            line_content.push_str("\x1b[39m");
                        } else {
                            line_content.push_str(&after_plain);
                        }
                    }
                } else {
                    // No selection - simple colored line
                    if *is_header {
                        line_content.push_str("\x1b[32m"); // Green
                        line_content.push_str(visible_line);
                        line_content.push_str("\x1b[39m");
                    } else if *is_error {
                        line_content.push_str("\x1b[31m"); // Red
                        line_content.push_str(visible_line);
                        line_content.push_str("\x1b[39m");
                    } else {
                        line_content.push_str(visible_line);
                    }
                }

                // Pad to full width to overwrite old content (this avoids needing to clear)
                let current_display_len = display_width(&strip_ansi(&line_content));
                if current_display_len < visible_width {
                    for _ in current_display_len..visible_width {
                        line_content.push(' ');
                    }
                }

                // Only write if changed from cache
                if screen_row_idx < self.last_screen.len() {
                    if self.last_screen[screen_row_idx] != line_content {
                        write!(writer, "\x1b[{};1H{}", current_row + 1, line_content)?;
                        self.last_screen[screen_row_idx] = line_content;
                    }
                } else {
                    write!(writer, "\x1b[{};1H{}", current_row + 1, line_content)?;
                }
                screen_row_idx += 1;
            }

            // On non-Windows: Use execute! macros as before
            #[cfg(not(target_os = "windows"))]
            {
                // Draw line with selection highlighting
                // Selection coordinates are in raw line coordinates (without indent)
                // We need to translate them to full_line coordinates (with indent)
                if let Some(((sel_start_line, sel_start_col), (sel_end_line, sel_end_col))) = selection_range {
                    if absolute_line_idx >= sel_start_line && absolute_line_idx <= sel_end_line {
                        // This line has selection
                        // Convert raw selection coordinates to full_line coordinates
                        let raw_line_len = display_width(line_text);
                        let (raw_sel_from, raw_sel_to) = if absolute_line_idx == sel_start_line && absolute_line_idx == sel_end_line {
                            (sel_start_col, sel_end_col)
                        } else if absolute_line_idx == sel_start_line {
                            (sel_start_col, raw_line_len)
                        } else if absolute_line_idx == sel_end_line {
                            (0, sel_end_col)
                        } else {
                            (0, raw_line_len)
                        };

                        // Convert to full_line coordinates (add indent_len)
                        let full_sel_from = indent_len + raw_sel_from;
                        let full_sel_to = indent_len + raw_sel_to;

                        // Translate selection to visible range (accounting for horizontal scroll)
                        let vis_sel_from = if full_sel_from > h_offset {
                            full_sel_from - h_offset
                        } else {
                            0
                        };
                        let vis_sel_to = if full_sel_to > h_offset {
                            (full_sel_to - h_offset).min(visible_line_display_len)
                        } else {
                            0
                        };

                        execute!(writer, cursor::MoveTo(0, current_row))?;

                        // Use ANSI-aware slicing to properly handle colored text
                        let vis_line_display_len = visible_line_display_len;
                        let vis_sel_from = vis_sel_from.min(vis_line_display_len);
                        let vis_sel_to = vis_sel_to.min(vis_line_display_len);

                        // Draw before selection (using display-position-aware slicing)
                        if vis_sel_from > 0 {
                            let before = slice_with_ansi(visible_line, 0, vis_sel_from);
                            // Strip ANSI codes for selection rendering - we apply our own colors
                            let before_plain = strip_ansi(&before);
                            if *is_header {
                                execute!(writer, SetForegroundColor(Color::Green), Print(before_plain), ResetColor)?;
                            } else if *is_error {
                                execute!(writer, SetForegroundColor(Color::Red), Print(before_plain), ResetColor)?;
                            } else {
                                execute!(writer, Print(before_plain))?;
                            }
                        }

                        // Draw selection (teal background matching editor selection color)
                        if vis_sel_to > vis_sel_from {
                            let selected = slice_with_ansi(visible_line, vis_sel_from, vis_sel_to - vis_sel_from);
                            // Strip ANSI codes - selection has its own styling
                            let selected_plain = strip_ansi(&selected);
                            // Use ANSI 256 teal background (30) with black foreground
                            execute!(writer, crossterm::style::SetBackgroundColor(crossterm::style::Color::AnsiValue(30)),
                                     crossterm::style::SetForegroundColor(crossterm::style::Color::Black),
                                     Print(selected_plain),
                                     crossterm::style::ResetColor)?;
                        }

                        // Draw after selection
                        if vis_sel_to < vis_line_display_len {
                            let after = slice_with_ansi(visible_line, vis_sel_to, vis_line_display_len - vis_sel_to);
                            // Strip ANSI codes for selection rendering - we apply our own colors
                            let after_plain = strip_ansi(&after);
                            if *is_header {
                                execute!(writer, SetForegroundColor(Color::Green), Print(after_plain), ResetColor)?;
                            } else if *is_error {
                                execute!(writer, SetForegroundColor(Color::Red), Print(after_plain), ResetColor)?;
                            } else {
                                execute!(writer, Print(after_plain))?;
                            }
                        }
                    } else {
                        // No selection on this line
                        execute!(writer, cursor::MoveTo(0, current_row))?;
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
                    execute!(writer, cursor::MoveTo(0, current_row))?;
                    if *is_header {
                        execute!(writer, SetForegroundColor(Color::Green), Print(visible_line), ResetColor)?;
                    } else if *is_error {
                        execute!(writer, SetForegroundColor(Color::Red), Print(visible_line), ResetColor)?;
                    } else {
                        execute!(writer, Print(visible_line))?;
                    }
                }
            }

            current_row += 1;
        }

        // On Windows: Clear remaining rows if we have fewer lines than before
        #[cfg(target_os = "windows")]
        {
            while screen_row_idx < self.last_screen.len() && current_row < max_row {
                let empty_line = format!("\x1b[48;2;33;33;33m{}", " ".repeat(width as usize));
                if self.last_screen.get(screen_row_idx) != Some(&empty_line) {
                    write!(writer, "\x1b[{};1H{}", current_row + 1, empty_line)?;
                    if screen_row_idx < self.last_screen.len() {
                        self.last_screen[screen_row_idx] = empty_line;
                    }
                }
                screen_row_idx += 1;
                current_row += 1;
            }
        }

        // Show cursor if focused and visible
        if self.focused {
            if let (Some(row), Some(col)) = (cursor_screen_row, cursor_screen_col) {
                // Set cursor style based on whether we have a selection
                if self.selection_start.is_some() {
                    // Underline cursor when selecting
                    write!(writer, "\x1b[4 q")?;
                } else {
                    // Block cursor when not selecting
                    write!(writer, "\x1b[2 q")?;
                }
                execute!(writer, cursor::MoveTo(col, row), crossterm::cursor::Show)?;
            }
        }

        Ok(())
    }

    /// Convert screen coordinates to (line, col) position in output
    pub fn screen_to_position(&self, screen_col: usize, screen_row: usize) -> Option<(usize, usize)> {
        // Check if screen row is within the output pane
        if screen_row < self.output_start_row as usize {
            return None;
        }

        let relative_row = screen_row - self.output_start_row as usize;

        // Calculate which line this corresponds to
        let display_lines = self.viewport_height;
        let total_lines = self.count_total_lines();
        let line_offset = if self.auto_scroll {
            total_lines.saturating_sub(display_lines)
        } else {
            self.scroll_offset.min(total_lines.saturating_sub(1))
        };

        let line_idx = line_offset + relative_row;

        if line_idx >= total_lines {
            return None;
        }

        // Get the line to determine indent
        let lines = self.get_all_lines();
        if line_idx >= lines.len() {
            return None;
        }

        let (line_text, is_header, _) = &lines[line_idx];
        let indent = if *is_header { 2 } else { 4 };

        // Calculate column position in raw line coordinates (without indent)
        // screen_col + horizontal_offset gives us the position in the "indented line"
        // We need to subtract indent to get the raw line position
        let full_col = screen_col + self.horizontal_offset;
        let col = if full_col < indent {
            0 // Click is in the indent area, map to column 0
        } else {
            full_col - indent // Convert to raw line coordinates
        };

        // Clamp to line length (in display columns)
        let line_len = display_width(line_text);
        let col = col.min(line_len);

        Some((line_idx, col))
    }

    /// Start a mouse selection
    pub fn start_mouse_selection(&mut self, screen_col: usize, screen_row: usize, output_start_row: u16, pane_height: usize) {
        // Set focus directly without resetting scroll position
        // (unlike set_focused which jumps to bottom)
        self.focused = true;

        // Update layout info so screen_to_position uses current values
        self.output_start_row = output_start_row;
        self.viewport_height = pane_height;

        let now = Instant::now();
        let double_click_time = Duration::from_millis(500);

        if let Some((line, col)) = self.screen_to_position(screen_col, screen_row) {
            // Check for double click
            if let Some(last_time) = self.last_click_time {
                if now.duration_since(last_time) < double_click_time {
                    if let Some(last_pos) = self.last_click_position {
                        // Check if clicking near the same position
                        let line_diff = if line > last_pos.0 {
                            line - last_pos.0
                        } else {
                            last_pos.0 - line
                        };
                        let col_diff = if col > last_pos.1 {
                            col - last_pos.1
                        } else {
                            last_pos.1 - col
                        };

                        if line_diff == 0 && col_diff <= 3 {
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
            self.last_click_position = Some((line, col));

            match self.click_count {
                2 => {
                    // Double click - select word
                    self.select_word_at(line, col);
                    self.mouse_selecting = false;
                }
                3 => {
                    // Triple click - select line
                    self.select_line_at(line);
                    self.mouse_selecting = false;
                }
                _ => {
                    // Single click - start normal selection
                    // Keep scroll position where it is, just move cursor to click position
                    self.cursor_line = line;
                    self.cursor_col = col;
                    self.selection_start = None;
                    self.mouse_selecting = true;
                    self.disable_auto_scroll();
                    self.preferred_column = None;

                    // Adjust horizontal scroll if cursor is left of viewport
                    let indent = self.get_line_indent(line);
                    let cursor_display_col = indent + col;
                    if cursor_display_col < self.horizontal_offset {
                        self.horizontal_offset = cursor_display_col;
                    }
                }
            }
        }
    }

    /// Update mouse selection while dragging
    pub fn update_mouse_selection(&mut self, screen_col: usize, screen_row: usize, output_start_row: u16, pane_height: usize) {
        // Update layout info in case terminal was resized during drag
        self.output_start_row = output_start_row;
        self.viewport_height = pane_height;

        if self.mouse_selecting {
            if self.selection_start.is_none() {
                // Start selection from the initial cursor position
                self.selection_start = Some((self.cursor_line, self.cursor_col));
            }

            // Handle dragging above the output pane (scroll up)
            if screen_row < self.output_start_row as usize + 1 {
                // Sync scroll_offset before manual adjustment
                self.disable_auto_scroll();
                // Move cursor to first visible line
                if self.scroll_offset > 0 {
                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                }
                self.cursor_line = self.scroll_offset;
                self.cursor_col = 0;
            }
            // Handle dragging below the output pane (scroll down)
            else if screen_row >= self.output_start_row as usize + self.viewport_height {
                // Sync scroll_offset before manual adjustment
                self.disable_auto_scroll();
                // Move cursor to last visible line and scroll down
                let total_lines = self.count_total_lines();
                if self.scroll_offset + self.viewport_height < total_lines {
                    self.scroll_offset += 1;
                }
                self.cursor_line = (self.scroll_offset + self.viewport_height - 1).min(total_lines.saturating_sub(1));
                let lines = self.get_all_lines();
                if self.cursor_line < lines.len() {
                    self.cursor_col = display_width(&lines[self.cursor_line].0);
                }
            }
            // Normal case: mouse is within the output pane
            else if let Some((line, col)) = self.screen_to_position(screen_col, screen_row) {
                // Update cursor to current mouse position
                self.cursor_line = line;
                self.cursor_col = col;
                self.disable_auto_scroll();
                self.ensure_cursor_visible_with_scrolloff();
            }
        }
    }

    /// Finish mouse selection
    pub fn finish_mouse_selection(&mut self) {
        self.mouse_selecting = false;
        // If selection start equals cursor, clear the selection
        if let Some(start) = self.selection_start {
            if start == (self.cursor_line, self.cursor_col) {
                self.selection_start = None;
            }
        }
    }

    /// Select the word at the given position (col is a display position)
    fn select_word_at(&mut self, line: usize, col: usize) {
        let lines = self.get_all_lines();
        if line >= lines.len() {
            return;
        }

        let line_text = &lines[line].0;
        // Work with visible text only (strip ANSI escape sequences)
        let visible_text = strip_ansi(line_text);
        let chars: Vec<char> = visible_text.chars().collect();

        if col >= chars.len() {
            return;
        }

        // Find word boundaries on visible text
        let mut start_col = col;
        let mut end_col = col;

        // Move start backward to beginning of word
        while start_col > 0 && (chars[start_col - 1].is_alphanumeric() || chars[start_col - 1] == '_') {
            start_col -= 1;
        }

        // Move end forward to end of word
        while end_col < chars.len() && (chars[end_col].is_alphanumeric() || chars[end_col] == '_') {
            end_col += 1;
        }

        // Set selection (coordinates are display positions)
        if start_col < end_col {
            self.selection_start = Some((line, start_col));
            self.cursor_line = line;
            self.cursor_col = end_col;
            self.disable_auto_scroll();
            self.ensure_cursor_visible();
        }
    }

    /// Select the entire line at the given position
    fn select_line_at(&mut self, line: usize) {
        let lines = self.get_all_lines();
        if line >= lines.len() {
            return;
        }

        let line_text = &lines[line].0;
        let line_len = display_width(line_text);

        // Select from start to end of line
        self.selection_start = Some((line, 0));
        self.cursor_line = line;
        self.cursor_col = line_len;
        self.disable_auto_scroll();
        self.ensure_cursor_visible();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_width_plain_text() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
        assert_eq!(display_width("hello world"), 11);
    }

    #[test]
    fn test_display_width_with_ansi() {
        // Red text: \x1b[31m ... \x1b[0m
        assert_eq!(display_width("\x1b[31mhello\x1b[0m"), 5);
        // Multiple escape sequences
        assert_eq!(display_width("\x1b[31mred\x1b[0m \x1b[32mgreen\x1b[0m"), 9);
        // Only escape sequences
        assert_eq!(display_width("\x1b[31m\x1b[0m"), 0);
    }

    #[test]
    fn test_slice_with_ansi_plain_text() {
        assert_eq!(slice_with_ansi("hello world", 0, 5), "hello");
        assert_eq!(slice_with_ansi("hello world", 6, 5), "world");
        assert_eq!(slice_with_ansi("hello world", 0, 100), "hello world");
        assert_eq!(slice_with_ansi("hello world", 20, 5), "");
    }

    #[test]
    fn test_slice_with_ansi_preserves_escapes() {
        // Slice from start should include the color code
        let s = "\x1b[31mhello\x1b[0m world";
        let result = slice_with_ansi(s, 0, 5);
        assert!(result.contains("\x1b[31m")); // Color code preserved
        assert!(result.contains("hello"));

        // Slice in the middle should work correctly
        let result = slice_with_ansi(s, 0, 11);
        assert_eq!(display_width(&result), 11);
    }

    #[test]
    fn test_slice_with_ansi_offset() {
        // Start after the colored portion
        let s = "\x1b[31mred\x1b[0m green";
        let result = slice_with_ansi(s, 4, 5);
        // Should get "green" (starting at display position 4, which is 'g')
        assert!(result.contains("green"));
    }

    #[test]
    fn test_strip_ansi() {
        assert_eq!(strip_ansi("hello"), "hello");
        assert_eq!(strip_ansi("\x1b[31mhello\x1b[0m"), "hello");
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m \x1b[32mgreen\x1b[0m"), "red green");
        assert_eq!(strip_ansi("\x1b[31m\x1b[0m"), "");
    }

    #[test]
    fn test_selection_with_ansi() {
        // Simulate selecting text that contains ANSI codes
        let text = "\x1b[31mCatalogException\x1b[0m: Error message";

        // Selecting first 16 chars ("CatalogException")
        let selected = slice_with_ansi(text, 0, 16);
        let selected_plain = strip_ansi(&selected);
        assert_eq!(selected_plain, "CatalogException");

        // Selecting from position 18 onwards (": Error message" minus the colon space)
        let after = slice_with_ansi(text, 18, 100);
        let after_plain = strip_ansi(&after);
        assert_eq!(after_plain, "Error message");
    }

    #[test]
    fn test_display_width_with_osc8_hyperlink() {
        // OSC 8 hyperlink with BEL terminator: \x1b]8;;URL\x07text\x1b]8;;\x07
        let link_bel = "\x1b]8;;https://example.com\x07click here\x1b]8;;\x07";
        assert_eq!(display_width(link_bel), 10); // "click here" = 10 chars

        // OSC 8 hyperlink with ST terminator: \x1b]8;;URL\x1b\\text\x1b]8;;\x1b\\
        let link_st = "\x1b]8;;https://example.com\x1b\\click here\x1b]8;;\x1b\\";
        assert_eq!(display_width(link_st), 10); // "click here" = 10 chars

        // Mixed content: colored hyperlink
        let colored_link = "\x1b[34m\x1b]8;;https://example.com\x07link\x1b]8;;\x07\x1b[0m";
        assert_eq!(display_width(colored_link), 4); // "link" = 4 chars
    }

    #[test]
    fn test_strip_ansi_with_osc8_hyperlink() {
        // OSC 8 hyperlink with BEL terminator
        let link_bel = "\x1b]8;;https://example.com\x07click here\x1b]8;;\x07";
        assert_eq!(strip_ansi(link_bel), "click here");

        // OSC 8 hyperlink with ST terminator
        let link_st = "\x1b]8;;https://example.com\x1b\\click here\x1b]8;;\x1b\\";
        assert_eq!(strip_ansi(link_st), "click here");

        // Mixed: CSI color + OSC hyperlink
        let mixed = "\x1b[31mError: \x1b]8;;file://path\x07file.txt\x1b]8;;\x07\x1b[0m";
        assert_eq!(strip_ansi(mixed), "Error: file.txt");
    }

    #[test]
    fn test_slice_with_ansi_osc8_hyperlink() {
        // OSC 8 hyperlink
        let link = "\x1b]8;;https://example.com\x07click here\x1b]8;;\x07 more text";

        // Slice just the link text
        let result = slice_with_ansi(link, 0, 10);
        assert!(result.contains("\x1b]8;;https://example.com\x07")); // OSC start preserved
        assert!(result.contains("click here"));
        assert_eq!(display_width(&result), 10);

        // Slice after the link
        let after = slice_with_ansi(link, 11, 9);
        assert_eq!(strip_ansi(&after), "more text");
    }

    #[test]
    fn test_slice_with_ansi_preserves_hyperlink_across_slice() {
        // When slicing in the middle of hyperlink text, OSC sequences should be preserved
        let link = "prefix \x1b]8;;https://example.com\x07click here\x1b]8;;\x07 suffix";

        // Slice starting from "click"
        let result = slice_with_ansi(link, 7, 10);
        // Should contain the OSC start sequence since it's before/at our start position
        assert!(result.contains("\x1b]8;;"));
        assert_eq!(strip_ansi(&result), "click here");
    }
}
