use crate::buffer::Buffer;
use crate::commands::Command;
use crate::syntax::SyntaxHighlighter;
use crate::cell::Cell;
use crate::kernel::Kernel;
use crate::clipboard::ClipboardProvider;
use crate::spreadsheet::Spreadsheet;
use std::io;
use std::path::PathBuf;
use std::time::Instant;
use unicode_width::UnicodeWidthChar;

// Submodules
mod file_ops;
mod selection;
mod mouse;
mod viewport;
mod matching;
mod notebook;
mod wrap;

pub struct Editor {
    buffer: Buffer,
    cursor: usize,           // Byte position in the buffer
    pub selection_start: Option<usize>,  // Start of selection (if any)
    file_path: Option<PathBuf>,
    modified: bool,
    viewport_offset: (usize, usize),  // (row, col) offset for scrolling
    viewport_top_seg: usize,          // Wrap mode: segment index of the top buffer line
    word_wrap: bool,                  // Word-wrap preference (active only in plain text/Markdown)
    last_saved_undo_len: usize,       // Track save point for modified flag
    clipboard: ClipboardProvider,      // System clipboard (supports native + OSC 52 for SSH)
    mouse_selecting: bool,            // Track if we're actively selecting with mouse
    last_click_time: Option<Instant>, // Track time of last click for double/triple click
    last_click_position: Option<usize>, // Track position of last click
    click_count: usize,               // Track consecutive clicks (1=single, 2=double, 3=triple)
    word_select_mode: bool,           // Track if we're in word selection mode (double-click + drag)
    line_select_mode: bool,           // Track if we're in line selection mode (triple-click + drag)
    word_select_anchor: Option<(usize, usize)>, // Original word boundaries (start, end) for word selection
    line_select_anchor: Option<(usize, usize)>, // Original line boundaries (start, end) for line selection
    preferred_column: Option<usize>,  // Preferred column for vertical movement
    syntax: SyntaxHighlighter,       // Syntax highlighting state
    read_only: bool,                  // Whether the file is read-only
    pub status_message: Option<(String, bool)>, // Status bar message (text, is_error)
    status_message_persistent: bool, // Whether status message should persist until cleared
    matching_brackets: Option<(usize, usize)>, // Positions of matching brackets
    matching_text_positions: Vec<(usize, usize)>, // Positions of text matching the selection
    find_matches: Vec<(usize, usize)>, // Positions of find/replace matches
    current_find_match: Option<usize>, // Index of the current find match
    // REPL/Notebook fields
    cells: Vec<Cell>,                  // Parsed cells for notebook mode
    kernel: Option<Box<dyn Kernel>>,   // Active Python kernel
    repl_mode: bool,                   // Whether we're in REPL mode
    executing_kernel_name: Option<String>, // Kernel name while executing (kernel is temporarily taken)
    spreadsheet: Option<Spreadsheet>,   // Active spreadsheet (CSV/TSV) grid, replaces buffer editing
    // CSV/TSV data shown as editable text (Ctrl+Y, a text language). The text
    // is raw: typing and pasting insert exactly what was typed (tabs and curly
    // quotes are data), Tab types a tab, and Save writes the text. The grid is
    // parked in `spreadsheet` to come back to if the text isn't changed.
    grid_text_view: bool,
    // The text as the view opened (or last saved), to tell whether Spreadsheet
    // can bring back the parked grid or must read the text again.
    text_view_origin: Option<String>,
    // Fingerprint of the text as it is on disk (as loaded or last saved), so
    // undo and redo can tell whether the text is back to the saved state.
    // None when the text never matched the disk (built from unsaved edits).
    clean_text_hash: Option<u64>,
}

impl Editor {
    /// Normalize text by removing invisible characters and converting line endings/tabs
    pub fn new() -> Self {
        Self {
            buffer: Buffer::new(),
            cursor: 0,
            selection_start: None,
            file_path: None,
            modified: false,
            viewport_offset: (0, 0),
            viewport_top_seg: 0,
            word_wrap: true, // Wrap by default in plain text / Markdown; Ctrl+W toggles
            last_saved_undo_len: 0,
            clipboard: ClipboardProvider::new(),
            mouse_selecting: false,
            last_click_time: None,
            last_click_position: None,
            click_count: 0,
            word_select_mode: false,
            line_select_mode: false,
            word_select_anchor: None,
            line_select_anchor: None,
            preferred_column: None,
            syntax: SyntaxHighlighter::new(),
            read_only: false,
            status_message: None,
            matching_brackets: None,
            matching_text_positions: Vec::new(),
            find_matches: Vec::new(),
            current_find_match: None,
            cells: Vec::new(),
            kernel: None,
            repl_mode: false,
            status_message_persistent: false,
            executing_kernel_name: None,
            spreadsheet: None,
            grid_text_view: false,
            text_view_origin: None,
            clean_text_hash: Some(Buffer::new().content_hash()),
        }
    }

    /// Record that the buffer's text now matches the file on disk.
    pub(super) fn mark_text_clean(&mut self) {
        self.modified = false;
        self.clean_text_hash = Some(self.buffer.content_hash());
    }

    /// Whether the text differs from the file on disk (after undo or redo).
    fn text_differs_from_saved(&self) -> bool {
        self.clean_text_hash != Some(self.buffer.content_hash())
    }

    pub fn spreadsheet(&self) -> Option<&Spreadsheet> {
        self.spreadsheet.as_ref()
    }

    pub fn spreadsheet_mut(&mut self) -> Option<&mut Spreadsheet> {
        self.spreadsheet.as_mut()
    }

    /// True while a CSV/TSV is drawn and edited as a grid. False in its text
    /// view, where the text editor shows the data instead.
    pub fn is_spreadsheet_mode(&self) -> bool {
        self.spreadsheet.is_some() && !self.grid_text_view
    }

    /// True while CSV/TSV data is shown as (raw, editable) text.
    pub fn is_grid_text_view(&self) -> bool {
        self.grid_text_view
    }

    /// Text on its way into the buffer from typing or pasting. Normally it
    /// goes through the input policy (tabs to spaces, curly quotes folded,
    /// invisibles dropped); CSV/TSV data shown as text is kept exactly, since
    /// its tabs and quotes are data, with only line ends made LF. There a
    /// character the editor would break the line on but the data reads as
    /// part of a field (U+2028, say) is refused, since the lines shown would
    /// no longer be the records.
    fn input_text(&self, text: String) -> Result<String, String> {
        if !self.grid_text_view {
            return Ok(Self::normalize_text(text));
        }
        if let Some(ch) = text.chars().find(|&c| crate::dsv::is_foreign_line_break(c)) {
            return Err(format!(
                "Not pasted: the text holds U+{:04X}, a line break the data would read as part of a value",
                ch as u32
            ));
        }
        Ok(text.replace("\r\n", "\n").replace('\r', "\n"))
    }

    pub fn execute(&mut self, cmd: Command) -> io::Result<()> {
        // Delimited data shown as text: Tab types a tab (a TSV delimiter), and
        // indenting lines of data means nothing, so Shift+Tab does nothing.
        let cmd = match cmd {
            Command::Indent if self.grid_text_view => Command::InsertTab,
            Command::Dedent if self.grid_text_view => Command::None,
            other => other,
        };

        // Clear non-persistent status messages on user action
        if !self.status_message_persistent {
            if matches!(cmd,
                Command::InsertChar(_) | Command::InsertNewline |
                Command::Backspace | Command::Delete |
                Command::MoveUp | Command::MoveDown | Command::MoveLeft | Command::MoveRight
            ) {
                self.status_message = None;
            }
        }

        // Clear mouse selection mode on any keyboard input
        self.mouse_selecting = false;
        self.word_select_mode = false;
        self.line_select_mode = false;
        self.word_select_anchor = None;
        self.line_select_anchor = None;
        
        // Clear error messages on any input (except for save commands)
        if !matches!(cmd, Command::Save | Command::SaveAs) && self.status_message.is_some() {
            if let Some((_, is_error)) = self.status_message {
                if is_error {
                    self.status_message = None;
                }
            }
        }
        
        // Track if cursor moved to update viewport
        let mut cursor_moved = false;
        
        // For non-selection movement commands, clear selection
        // Note: MoveLeft, MoveRight, MoveUp, and MoveDown handle their own selection clearing
        match cmd {
            Command::MoveHome | Command::MoveEnd | Command::PageUp | Command::PageDown |
            Command::MoveWordLeft | Command::MoveWordRight | 
            Command::MoveParagraphUp | Command::MoveParagraphDown => {
                self.selection_start = None;
                cursor_moved = true;
            }
            _ => {}
        }
        
        match cmd {
            Command::InsertChar(c) => {
                // Delete selection first if any
                self.delete_selection();
                
                let cursor_before = self.cursor;

                // Apply the WYSIWYG input policy: invisibles are dropped, visible
                // confusables (NBSP, curly quotes, Unicode dashes) fold to ASCII,
                // tabs become spaces. Shared with paste and file load via
                // crate::normalize so all input paths behave identically.
                // (Not for CSV/TSV data shown as text: that is typed as is.)
                let text = if self.grid_text_view {
                    c.to_string()
                } else {
                    crate::normalize::fold_char_to_str(c)
                };
                if text.is_empty() {
                    return Ok(()); // Dropped character (invisible / zero-width)
                }

                self.buffer.insert(self.cursor, &text, cursor_before, self.cursor + text.len());
                self.cursor += text.len();
                self.modified = true;
                self.preferred_column = None; // Clear preferred column
                
                // Update syntax highlighting for the modified line
                let line = self.buffer.byte_to_line(self.cursor);
                self.syntax.line_modified(line);
            }
            
            Command::InsertNewline => {
                // Delete selection first if any
                self.delete_selection();
                
                // Get the current line to check for indentation
                let current_line = self.buffer.byte_to_line(self.cursor);
                let line_start = self.buffer.line_to_byte(current_line);
                let is_at_line_start = self.cursor == line_start;
                
                let mut new_text = String::from("\n");
                
                // Only add indentation if cursor is NOT at the start of the line
                // (and never in CSV/TSV data, where leading spaces are data)
                if !is_at_line_start && !self.grid_text_view {
                    let line_text = self.buffer.line(current_line);
                    
                    // Count leading spaces
                    let indent_count = line_text.chars()
                        .take_while(|&c| c == ' ')
                        .count();
                    
                    // Add indentation spaces
                    for _ in 0..indent_count {
                        new_text.push(' ');
                    }
                }
                
                let cursor_before = self.cursor;
                self.buffer.insert(self.cursor, &new_text, cursor_before, self.cursor + new_text.len());
                self.cursor += new_text.len();
                self.modified = true;
                self.preferred_column = None; // Clear preferred column
                
                // Update syntax highlighting - line was inserted
                self.syntax.line_modified(current_line); // Mark current line as dirty since its content changed
                self.syntax.lines_inserted(current_line + 1, 1);
            }
            
            Command::InsertTab => {
                // Delete selection first if any
                self.delete_selection();

                let cursor_before = self.cursor;
                let line = self.buffer.byte_to_line(self.cursor);
                // A real tab in CSV/TSV data shown as text; spaces elsewhere.
                let tab = if self.grid_text_view { "\t" } else { "    " };
                self.buffer.insert(self.cursor, tab, cursor_before, self.cursor + tab.len());
                self.cursor += tab.len();
                self.modified = true;
                self.preferred_column = None; // Clear preferred column

                // Mark line as dirty for syntax highlighting
                self.syntax.mark_dirty(line);
            }
            
            Command::Backspace => {
                // If there's a selection, delete it
                if !self.delete_selection() {
                    // Otherwise delete character before cursor
                    if self.cursor > 0 {
                        let cursor_before = self.cursor;
                        let line_before = self.buffer.byte_to_line(self.cursor);
                        
                        // Find the previous character boundary
                        let char_pos = self.buffer.byte_to_char(self.cursor);
                        if char_pos > 0 {
                            let prev_char_pos = char_pos - 1;
                            let prev_byte = self.buffer.char_to_byte(prev_char_pos);
                            
                            self.buffer.delete(prev_byte, self.cursor, cursor_before, prev_byte);
                            self.cursor = prev_byte;
                            self.modified = true;
                            
                            // Update syntax - check if we deleted a newline (merged lines)
                            let line_after = self.buffer.byte_to_line(self.cursor);
                            if line_before != line_after {
                                // Lines were merged
                                self.syntax.lines_deleted(line_after, 1);
                            }
                            self.syntax.line_modified(line_after);
                        }
                    }
                }
                self.preferred_column = None; // Clear preferred column
            }
            
            Command::Delete => {
                // If there's a selection, delete it
                if !self.delete_selection() {
                    // Otherwise delete character after cursor
                    if self.cursor < self.buffer.len_bytes() {
                        let cursor_before = self.cursor;
                        let line_before = self.buffer.byte_to_line(self.cursor);
                        let lines_before = self.buffer.len_lines();

                        // Find the next character boundary
                        let char_pos = self.buffer.byte_to_char(self.cursor);
                        let next_char_pos = char_pos + 1;
                        let next_byte = self.buffer.char_to_byte(next_char_pos);

                        self.buffer.delete(self.cursor, next_byte, cursor_before, self.cursor);
                        self.modified = true;

                        // Update syntax - check if we deleted a newline (merged lines)
                        let lines_after = self.buffer.len_lines();
                        if lines_after < lines_before {
                            // A newline was deleted, lines were merged
                            // The line after line_before was merged into line_before
                            self.syntax.lines_deleted(line_before + 1, lines_before - lines_after);
                        }
                        self.syntax.line_modified(line_before);
                    }
                }
                self.preferred_column = None; // Clear preferred column
            }
            
            Command::MoveLeft => {
                // If there's a selection, just move to the start of it
                if let Some((start, _end)) = self.get_selection() {
                    self.cursor = start;
                    self.selection_start = None;
                } else {
                    // Otherwise perform normal left movement
                    if self.cursor > 0 {
                        let char_pos = self.buffer.byte_to_char(self.cursor);
                        if char_pos > 0 {
                            self.cursor = self.buffer.char_to_byte(char_pos - 1);
                        }
                    }
                }
                self.preferred_column = None; // Clear preferred column on horizontal movement
                cursor_moved = true;
            }
            
            Command::MoveRight => {
                // If there's a selection, just move to the end of it
                if let Some((_start, end)) = self.get_selection() {
                    self.cursor = end;
                    self.selection_start = None;
                } else {
                    // Otherwise perform normal right movement
                    if self.cursor < self.buffer.len_bytes() {
                        let char_pos = self.buffer.byte_to_char(self.cursor);
                        self.cursor = self.buffer.char_to_byte(char_pos + 1);
                    }
                }
                self.preferred_column = None; // Clear preferred column on horizontal movement
                cursor_moved = true;
            }
            
            Command::MoveUp => {
                // If there's a selection, move to the start of it
                if let Some((start, _end)) = self.get_selection() {
                    self.cursor = start;
                    self.selection_start = None;
                    self.preferred_column = None; // Clear preferred column when collapsing selection
                    cursor_moved = true;
                } else if self.is_wrap_active() {
                    self.visual_move_vertical(false);
                    cursor_moved = true;
                } else {
                    let current_line = self.buffer.byte_to_line(self.cursor);
                    if current_line > 0 {
                    // Set preferred column if not already set
                    if self.preferred_column.is_none() {
                        let (_, col) = self.cursor_position();
                        self.preferred_column = Some(col);
                    }
                    
                    // Use preferred column as target
                    let target_display_col = self.preferred_column.unwrap();
                    
                    let new_line = current_line - 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_text = self.buffer.line(new_line);
                    
                    // Find the best position on the new line
                    let mut best_byte_pos = 0;
                    let mut current_byte_pos = 0;
                    let mut display_col = 0;
                    
                    for ch in new_line_text.chars() {
                        if ch == '\n' {
                            break; // Stop at newline
                        }
                        
                        let char_width = ch.width().unwrap_or(1);
                        
                        // If adding this character would overshoot, decide whether to include it
                        if display_col + char_width > target_display_col {
                            // Check if we're closer to target by including or excluding this char
                            let without_char_distance = target_display_col - display_col;
                            let with_char_distance = (display_col + char_width) - target_display_col;
                            
                            if with_char_distance < without_char_distance {
                                // Include this character
                                best_byte_pos = current_byte_pos + ch.len_utf8();
                            } else {
                                // Exclude this character
                                best_byte_pos = current_byte_pos;
                            }
                            break;
                        }
                        
                        // Move past this character
                        current_byte_pos += ch.len_utf8();
                        display_col += char_width;
                        best_byte_pos = current_byte_pos;
                        
                        // If we've reached exactly the target column, stop
                        if display_col == target_display_col {
                            break;
                        }
                    }
                    
                    self.cursor = new_line_start + best_byte_pos;
                } else {
                    // Already on first line, move to start of buffer
                    self.cursor = 0;
                    self.preferred_column = Some(0); // Reset preferred column at buffer start
                }
                cursor_moved = true;
                }
            }
            
            Command::MoveDown => {
                // If there's a selection, move to the end of it
                if let Some((_start, end)) = self.get_selection() {
                    self.cursor = end;
                    self.selection_start = None;
                    self.preferred_column = None; // Clear preferred column when collapsing selection
                    cursor_moved = true;
                } else if self.is_wrap_active() {
                    self.visual_move_vertical(true);
                    cursor_moved = true;
                } else {
                    let current_line = self.buffer.byte_to_line(self.cursor);
                    if current_line < self.buffer.len_lines() - 1 {
                    // Set preferred column if not already set
                    if self.preferred_column.is_none() {
                        let (_, col) = self.cursor_position();
                        self.preferred_column = Some(col);
                    }
                    
                    // Use preferred column as target
                    let target_display_col = self.preferred_column.unwrap();
                    
                    let new_line = current_line + 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_text = self.buffer.line(new_line);
                    
                    // Find the best position on the new line
                    let mut best_byte_pos = 0;
                    let mut current_byte_pos = 0;
                    let mut display_col = 0;
                    
                    for ch in new_line_text.chars() {
                        if ch == '\n' {
                            break; // Stop at newline
                        }
                        
                        let char_width = ch.width().unwrap_or(1);
                        
                        // If adding this character would overshoot, decide whether to include it
                        if display_col + char_width > target_display_col {
                            // Check if we're closer to target by including or excluding this char
                            let without_char_distance = target_display_col - display_col;
                            let with_char_distance = (display_col + char_width) - target_display_col;
                            
                            if with_char_distance < without_char_distance {
                                // Include this character
                                best_byte_pos = current_byte_pos + ch.len_utf8();
                            } else {
                                // Exclude this character
                                best_byte_pos = current_byte_pos;
                            }
                            break;
                        }
                        
                        // Move past this character
                        current_byte_pos += ch.len_utf8();
                        display_col += char_width;
                        best_byte_pos = current_byte_pos;
                        
                        // If we've reached exactly the target column, stop
                        if display_col == target_display_col {
                            break;
                        }
                    }
                    
                    self.cursor = new_line_start + best_byte_pos;
                } else {
                    // Already on last line, move to end of buffer
                    self.cursor = self.buffer.len_bytes();
                    // Reset preferred column to end of last line for consistency
                    let (_, col) = self.cursor_position();
                    self.preferred_column = Some(col);
                }
                cursor_moved = true;
                }
            }
            
            Command::MoveHome => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                self.cursor = self.buffer.line_to_byte(current_line);
                self.preferred_column = None; // Clear preferred column
            }
            
            Command::MoveEnd => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                let line_start = self.buffer.line_to_byte(current_line);
                let line = self.buffer.line(current_line);
                let line_len = if line.ends_with('\n') {
                    line.len().saturating_sub(1)
                } else {
                    line.len()
                };
                self.cursor = line_start + line_len;
                self.preferred_column = None; // Clear preferred column
            }
            
            Command::PageUp => {
                // Move up ~20 lines
                for _ in 0..20 {
                    self.execute(Command::MoveUp)?;
                }
            }
            
            Command::PageDown => {
                // Move down ~20 lines
                for _ in 0..20 {
                    self.execute(Command::MoveDown)?;
                }
            }
            
            // Selection movement commands
            Command::SelectLeft => {
                if self.selection_start.is_none() {
                    // Set anchor at exact cursor position (don't skip spaces)
                    // Ensure it's on a character boundary
                    self.selection_start = Some(self.ensure_char_boundary(self.cursor));
                }
                if self.cursor > 0 {
                    let char_pos = self.buffer.byte_to_char(self.cursor);
                    if char_pos > 0 {
                        let new_cursor = self.buffer.char_to_byte(char_pos - 1);
                        // Ensure the new cursor position is on a character boundary
                        self.cursor = self.ensure_char_boundary(new_cursor);
                        cursor_moved = true;
                    }
                }
                self.preferred_column = None; // Clear on horizontal movement
            }

            Command::SelectRight => {
                if self.selection_start.is_none() {
                    // Set anchor at exact cursor position (don't skip spaces)
                    // Ensure it's on a character boundary
                    self.selection_start = Some(self.ensure_char_boundary(self.cursor));
                }
                if self.cursor < self.buffer.len_bytes() {
                    let char_pos = self.buffer.byte_to_char(self.cursor);
                    let new_cursor = self.buffer.char_to_byte(char_pos + 1);
                    // Ensure the new cursor position is on a character boundary
                    self.cursor = self.ensure_char_boundary(new_cursor);
                    cursor_moved = true;
                }
                self.preferred_column = None; // Clear on horizontal movement
            }
            
            Command::SelectUp => {
                if self.selection_start.is_none() {
                    // Set anchor at exact cursor position (don't skip spaces)
                    self.selection_start = Some(self.cursor);
                }
                if self.is_wrap_active() {
                    self.visual_move_vertical(false);
                    cursor_moved = true;
                } else if self.buffer.byte_to_line(self.cursor) > 0 {
                    let current_line = self.buffer.byte_to_line(self.cursor);
                    // Set preferred column if not already set
                    if self.preferred_column.is_none() {
                        let (_, col) = self.cursor_position();
                        self.preferred_column = Some(col);
                    }
                    
                    // Use preferred column as target
                    let target_display_col = self.preferred_column.unwrap();
                    
                    let new_line = current_line - 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_text = self.buffer.line(new_line);
                    
                    // Find the best position on the new line
                    let mut best_byte_pos = 0;
                    let mut current_byte_pos = 0;
                    let mut display_col = 0;
                    
                    for ch in new_line_text.chars() {
                        if ch == '\n' {
                            break; // Stop at newline
                        }
                        
                        let char_width = ch.width().unwrap_or(1);
                        
                        // If adding this character would overshoot, decide whether to include it
                        if display_col + char_width > target_display_col {
                            // Check if we're closer to target by including or excluding this char
                            let without_char_distance = target_display_col - display_col;
                            let with_char_distance = (display_col + char_width) - target_display_col;
                            
                            if with_char_distance < without_char_distance {
                                // Include this character
                                best_byte_pos = current_byte_pos + ch.len_utf8();
                            } else {
                                // Exclude this character
                                best_byte_pos = current_byte_pos;
                            }
                            break;
                        }
                        
                        // Move past this character
                        current_byte_pos += ch.len_utf8();
                        display_col += char_width;
                        best_byte_pos = current_byte_pos;
                        
                        // If we've reached exactly the target column, stop
                        if display_col == target_display_col {
                            break;
                        }
                    }
                    
                    self.cursor = new_line_start + best_byte_pos;
                    cursor_moved = true;
                } else {
                    // Already on first line, move to start of buffer
                    self.cursor = 0;
                    self.preferred_column = Some(0); // Reset preferred column at buffer start
                    cursor_moved = true;
                }
            }
            
            Command::SelectDown => {
                if self.selection_start.is_none() {
                    // Set anchor at exact cursor position (don't skip spaces)
                    self.selection_start = Some(self.cursor);
                }
                if self.is_wrap_active() {
                    self.visual_move_vertical(true);
                    cursor_moved = true;
                } else if self.buffer.byte_to_line(self.cursor) < self.buffer.len_lines() - 1 {
                    let current_line = self.buffer.byte_to_line(self.cursor);
                    // Set preferred column if not already set
                    if self.preferred_column.is_none() {
                        let (_, col) = self.cursor_position();
                        self.preferred_column = Some(col);
                    }
                    
                    // Use preferred column as target
                    let target_display_col = self.preferred_column.unwrap();
                    
                    let new_line = current_line + 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_text = self.buffer.line(new_line);
                    
                    // Find the best position on the new line
                    let mut best_byte_pos = 0;
                    let mut current_byte_pos = 0;
                    let mut display_col = 0;
                    
                    for ch in new_line_text.chars() {
                        if ch == '\n' {
                            break; // Stop at newline
                        }
                        
                        let char_width = ch.width().unwrap_or(1);
                        
                        // If adding this character would overshoot, decide whether to include it
                        if display_col + char_width > target_display_col {
                            // Check if we're closer to target by including or excluding this char
                            let without_char_distance = target_display_col - display_col;
                            let with_char_distance = (display_col + char_width) - target_display_col;
                            
                            if with_char_distance < without_char_distance {
                                // Include this character
                                best_byte_pos = current_byte_pos + ch.len_utf8();
                            } else {
                                // Exclude this character
                                best_byte_pos = current_byte_pos;
                            }
                            break;
                        }
                        
                        // Move past this character
                        current_byte_pos += ch.len_utf8();
                        display_col += char_width;
                        best_byte_pos = current_byte_pos;
                        
                        // If we've reached exactly the target column, stop
                        if display_col == target_display_col {
                            break;
                        }
                    }
                    
                    self.cursor = new_line_start + best_byte_pos;
                    cursor_moved = true;
                } else {
                    // Already on last line, move to end of buffer
                    self.cursor = self.buffer.len_bytes();
                    // Reset preferred column to end of last line for consistency
                    let (_, col) = self.cursor_position();
                    self.preferred_column = Some(col);
                    cursor_moved = true;
                }
            }
            
            Command::SelectHome => {
                if self.selection_start.is_none() {
                    self.set_selection_start(self.cursor);
                }
                let current_line = self.buffer.byte_to_line(self.cursor);
                self.cursor = self.buffer.line_to_byte(current_line);
                self.preferred_column = None; // Clear preferred column
                cursor_moved = true;
            }
            
            Command::SelectEnd => {
                if self.selection_start.is_none() {
                    self.set_selection_start(self.cursor);
                }
                let current_line = self.buffer.byte_to_line(self.cursor);
                let line_start = self.buffer.line_to_byte(current_line);
                let line = self.buffer.line(current_line);
                let line_len = if line.ends_with('\n') {
                    line.len().saturating_sub(1)
                } else {
                    line.len()
                };
                self.cursor = line_start + line_len;
                self.preferred_column = None; // Clear preferred column
                cursor_moved = true;
            }
            
            Command::SelectAll => {
                self.set_selection_start(0);
                self.cursor = self.buffer.len_bytes();
                self.preferred_column = None; // Clear preferred column
            }
            
            Command::SelectWordLeft => {
                // Set selection anchor if not already set
                if self.selection_start.is_none() {
                    self.selection_start = Some(self.cursor);
                }

                // Use same word boundary logic as MoveWordLeft
                let current_line = self.buffer.byte_to_line(self.cursor);
                let line_start = self.buffer.line_to_byte(current_line);
                let line_text = self.buffer.line(current_line);
                let cursor_in_line = self.cursor - line_start;

                if cursor_in_line > 0 {
                    // Find the previous word boundary within the current line
                    let mut new_pos = 0;
                    let mut in_word = false;
                    let mut byte_pos = 0;

                    for ch in line_text.chars() {
                        if byte_pos >= cursor_in_line {
                            break;
                        }

                        if ch.is_alphanumeric() || ch == '_' {
                            if !in_word {
                                // Start of a new word
                                new_pos = byte_pos;
                                in_word = true;
                            }
                        } else {
                            in_word = false;
                        }

                        byte_pos += ch.len_utf8();
                    }

                    self.cursor = line_start + new_pos;
                } else {
                    // Already at start of line, stay there
                    self.cursor = line_start;
                }
                self.preferred_column = None;
                cursor_moved = true;
            }
            
            Command::SelectWordRight => {
                // Set selection anchor if not already set
                if self.selection_start.is_none() {
                    self.selection_start = Some(self.cursor);
                }

                // Use same word boundary logic as MoveWordRight
                let current_line = self.buffer.byte_to_line(self.cursor);
                let line_start = self.buffer.line_to_byte(current_line);
                let line_text = self.buffer.line(current_line);
                let cursor_in_line = self.cursor - line_start;

                // Remove trailing newline from line text for processing
                let line_without_newline = if line_text.ends_with('\n') {
                    &line_text[..line_text.len() - 1]
                } else {
                    &line_text
                };

                if cursor_in_line < line_without_newline.len() {
                    // Find the next word boundary within the current line
                    let mut in_word = false;
                    let mut found_next_word = false;
                    let mut byte_pos = 0;

                    for ch in line_without_newline.chars() {
                        if byte_pos > cursor_in_line && !in_word && (ch.is_alphanumeric() || ch == '_') {
                            // Found start of next word
                            self.cursor = line_start + byte_pos;
                            found_next_word = true;
                            break;
                        }

                        in_word = ch.is_alphanumeric() || ch == '_';
                        byte_pos += ch.len_utf8();
                    }

                    if !found_next_word {
                        // No more words on this line, go to end of line
                        self.cursor = line_start + line_without_newline.len();
                    }
                } else {
                    // Already at end of line, stay there
                    self.cursor = line_start + line_without_newline.len();
                }
                self.preferred_column = None;
                cursor_moved = true;
            }
            
            Command::SelectParagraphUp => {
                if self.selection_start.is_none() {
                    self.set_selection_start(self.cursor);
                }
                let current_line = self.buffer.byte_to_line(self.cursor);
                
                // Search backwards for a non-empty line preceded by an empty line
                let mut target_line = None;
                for line_num in (0..current_line).rev() {
                    let line_text = self.buffer.line(line_num);
                    let is_empty = line_text.is_empty() || line_text == "\n";
                    
                    if !is_empty && line_num > 0 {
                        let prev_line = self.buffer.line(line_num - 1);
                        if prev_line.is_empty() || prev_line == "\n" {
                            target_line = Some(line_num);
                            break;
                        }
                    }
                }
                
                if let Some(line) = target_line {
                    self.cursor = self.buffer.line_to_byte(line);
                } else {
                    // No paragraph found, go to start of file
                    self.cursor = 0;
                }
                self.preferred_column = None;
                cursor_moved = true;
            }
            
            Command::SelectParagraphDown => {
                if self.selection_start.is_none() {
                    self.set_selection_start(self.cursor);
                }
                let current_line = self.buffer.byte_to_line(self.cursor);
                let total_lines = self.buffer.len_lines();
                
                // Search forward for a non-empty line preceded by an empty line
                let mut found_empty = false;
                let mut target_line = None;
                
                for line_num in (current_line + 1)..total_lines {
                    let line_text = self.buffer.line(line_num);
                    let is_empty = line_text.is_empty() || line_text == "\n";
                    
                    if is_empty {
                        found_empty = true;
                    } else if found_empty {
                        // Found a non-empty line after an empty line
                        target_line = Some(line_num);
                        break;
                    }
                }
                
                if let Some(line) = target_line {
                    self.cursor = self.buffer.line_to_byte(line);
                } else {
                    // No paragraph found, go to end of file
                    self.cursor = self.buffer.len_bytes();
                }
                self.preferred_column = None;
                cursor_moved = true;
            }
            
            // Clipboard operations
            Command::Copy => {
                if let Some(text) = self.get_selected_text() {
                    if let Err(e) = self.clipboard.set_text(text) {
                        self.status_message = Some((format!("Copy failed: {}", e), true));
                    }
                }
            }
            
            Command::Cut => {
                if let Some(text) = self.get_selected_text() {
                    if let Err(e) = self.clipboard.set_text(text) {
                        self.status_message = Some((format!("Cut failed: {}", e), true));
                    } else {
                        self.delete_selection();
                    }
                }
            }
            
            Command::Paste => {
                match self.clipboard.get_text() {
                    Ok(text) => self.paste_text(text),
                    Err(e) => {
                        self.status_message = Some((format!("Paste failed: {}", e), true));
                    }
                }
            }
            
            Command::Undo => {
                if let Some(cursor) = self.buffer.undo() {
                    let cursor = cursor.min(self.buffer.len_bytes());
                    self.cursor = self.ensure_char_boundary(cursor);
                    // Unsaved unless the text is back to what's on disk (undo
                    // can pass the last save, and a view built from unsaved
                    // edits never matched the disk).
                    self.modified = self.text_differs_from_saved();
                    cursor_moved = true;

                    // Reinitialize syntax highlighting after undo
                    self.reinit_syntax_highlighting();
                }
            }

            Command::Redo => {
                if let Some(cursor) = self.buffer.redo() {
                    let cursor = cursor.min(self.buffer.len_bytes());
                    self.cursor = self.ensure_char_boundary(cursor);
                    self.modified = self.text_differs_from_saved();
                    cursor_moved = true;

                    // Reinitialize syntax highlighting after redo
                    self.reinit_syntax_highlighting();
                }
            }
            
            Command::Save => {
                self.save()?;
            }
            
            Command::SaveAs => {
                // This is handled in main.rs as it needs UI interaction
                return Ok(());
            }
            
            Command::FindReplace | Command::FindNext | Command::FindPrev | 
            Command::Replace | Command::ReplaceAll => {
                // These are handled in main.rs with the find/replace window
                return Ok(());
            }
            
            Command::MoveWordLeft => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                let line_start = self.buffer.line_to_byte(current_line);
                let line_text = self.buffer.line(current_line);
                let cursor_in_line = self.cursor - line_start;
                
                if cursor_in_line > 0 {
                    // Find the previous word boundary within the current line
                    let mut new_pos = 0;
                    let mut in_word = false;
                    let mut byte_pos = 0;
                    
                    for ch in line_text.chars() {
                        if byte_pos >= cursor_in_line {
                            break;
                        }
                        
                        if ch.is_alphanumeric() || ch == '_' {
                            if !in_word {
                                // Start of a new word
                                new_pos = byte_pos;
                                in_word = true;
                            }
                        } else {
                            in_word = false;
                        }
                        
                        byte_pos += ch.len_utf8();
                    }
                    
                    self.cursor = line_start + new_pos;
                } else {
                    // Already at start of line, stay there
                    self.cursor = line_start;
                }
                self.preferred_column = None;
                cursor_moved = true;
            }
            
            Command::MoveWordRight => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                let line_start = self.buffer.line_to_byte(current_line);
                let line_text = self.buffer.line(current_line);
                let cursor_in_line = self.cursor - line_start;
                
                // Remove trailing newline from line text for processing
                let line_without_newline = if line_text.ends_with('\n') {
                    &line_text[..line_text.len() - 1]
                } else {
                    &line_text
                };
                
                if cursor_in_line < line_without_newline.len() {
                    // Find the next word boundary within the current line
                    let mut in_word = false;
                    let mut found_next_word = false;
                    let mut byte_pos = 0;
                    
                    for ch in line_without_newline.chars() {
                        if byte_pos > cursor_in_line && !in_word && (ch.is_alphanumeric() || ch == '_') {
                            // Found start of next word
                            self.cursor = line_start + byte_pos;
                            found_next_word = true;
                            break;
                        }
                        
                        in_word = ch.is_alphanumeric() || ch == '_';
                        byte_pos += ch.len_utf8();
                    }
                    
                    if !found_next_word {
                        // No more words on this line, go to end of line
                        self.cursor = line_start + line_without_newline.len();
                    }
                } else {
                    // Already at end of line, stay there
                    self.cursor = line_start + line_without_newline.len();
                }
                self.preferred_column = None;
                cursor_moved = true;
            }
            
            Command::MoveParagraphUp => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                
                // Search backwards for a non-empty line preceded by an empty line
                let mut target_line = None;
                for line_num in (0..current_line).rev() {
                    let line_text = self.buffer.line(line_num);
                    let is_empty = line_text.is_empty() || line_text == "\n";
                    
                    if !is_empty && line_num > 0 {
                        let prev_line = self.buffer.line(line_num - 1);
                        if prev_line.is_empty() || prev_line == "\n" {
                            target_line = Some(line_num);
                            break;
                        }
                    }
                }
                
                if let Some(line) = target_line {
                    self.cursor = self.buffer.line_to_byte(line);
                } else {
                    // No paragraph found, go to start of file
                    self.cursor = 0;
                }
                self.preferred_column = None;
                cursor_moved = true;
            }
            
            Command::MoveParagraphDown => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                let total_lines = self.buffer.len_lines();
                
                // Search forward for a non-empty line preceded by an empty line
                let mut found_empty = false;
                let mut target_line = None;
                
                for line_num in (current_line + 1)..total_lines {
                    let line_text = self.buffer.line(line_num);
                    let is_empty = line_text.is_empty() || line_text == "\n";
                    
                    if is_empty {
                        found_empty = true;
                    } else if found_empty {
                        // Found a non-empty line after an empty line
                        target_line = Some(line_num);
                        break;
                    }
                }
                
                if let Some(line) = target_line {
                    self.cursor = self.buffer.line_to_byte(line);
                } else {
                    // No paragraph found, go to end of file
                    self.cursor = self.buffer.len_bytes();
                }
                self.preferred_column = None;
                cursor_moved = true;
            }
            
            Command::Indent => {
                // Get the lines to indent
                let (start_line, end_line) = if let Some((sel_start, sel_end)) = self.get_selection() {
                    // Indent all lines in selection
                    let start = self.buffer.byte_to_line(sel_start);
                    let end = self.buffer.byte_to_line(sel_end);
                    (start, end)
                } else {
                    // Indent current line only
                    let line = self.buffer.byte_to_line(self.cursor);
                    (line, line)
                };
                
                // Track cursor adjustment
                let mut cursor_adjustment = 0;
                let mut selection_start_adjustment = 0;
                
                // Process each line from last to first to maintain positions
                for line_num in (start_line..=end_line).rev() {
                    let line_start = self.buffer.line_to_byte(line_num);
                    
                    // Insert 4 spaces at the start of the line
                    let cursor_before = self.cursor;
                    self.buffer.insert(line_start, "    ", cursor_before, cursor_before);
                    
                    // Track adjustments for cursor and selection
                    if self.cursor >= line_start {
                        cursor_adjustment += 4;
                    }
                    if let Some(sel_start) = self.selection_start {
                        if sel_start >= line_start {
                            selection_start_adjustment += 4;
                        }
                    }
                }
                
                // Apply cursor adjustment
                self.cursor += cursor_adjustment;
                if let Some(ref mut sel_start) = self.selection_start {
                    *sel_start += selection_start_adjustment;
                }

                // Mark affected lines as dirty for syntax highlighting
                for line in start_line..=end_line {
                    self.syntax.mark_dirty(line);
                }

                self.modified = true;
            }
            
            Command::Dedent => {
                // Get the lines to dedent
                let (start_line, end_line) = if let Some((sel_start, sel_end)) = self.get_selection() {
                    // Dedent all lines in selection
                    let start = self.buffer.byte_to_line(sel_start);
                    let end = self.buffer.byte_to_line(sel_end);
                    (start, end)
                } else {
                    // Dedent current line only
                    let line = self.buffer.byte_to_line(self.cursor);
                    (line, line)
                };
                
                // Store original positions
                let original_cursor = self.cursor;
                let original_selection_start = self.selection_start;
                
                // Track total adjustment needed
                let mut cursor_adjustment = 0;
                let mut selection_adjustment = 0;
                
                // Process lines from last to first so deletions don't affect line positions
                for line_num in (start_line..=end_line).rev() {
                    let line_start = self.buffer.line_to_byte(line_num);
                    let line_text = self.buffer.line(line_num);
                    
                    // Count leading spaces (up to 4)
                    let mut spaces_count = 0;
                    for ch in line_text.chars().take(4) {
                        if ch == ' ' {
                            spaces_count += 1;
                        } else {
                            break;
                        }
                    }
                    
                    if spaces_count > 0 {
                        // Delete the spaces
                        self.buffer.delete(line_start, line_start + spaces_count, 
                                         original_cursor, line_start);
                        
                        // Update adjustments if this deletion affects cursor/selection
                        if line_start < original_cursor {
                            cursor_adjustment += spaces_count;
                        }
                        if let Some(sel) = original_selection_start {
                            if line_start < sel {
                                selection_adjustment += spaces_count;
                            }
                        }
                    }
                }
                
                // Apply adjustments
                self.cursor = original_cursor.saturating_sub(cursor_adjustment);
                if let Some(sel) = original_selection_start {
                    self.set_selection_start(sel.saturating_sub(selection_adjustment));
                }

                // Mark affected lines as dirty for syntax highlighting
                for line in start_line..=end_line {
                    self.syntax.mark_dirty(line);
                }

                self.modified = true;
            }
            
            Command::ToggleCase => {
                if let Some(text) = self.get_selected_text() {
                    // Count uppercase letters to decide which way to toggle
                    let upper_count = text.chars().filter(|c| c.is_uppercase()).count();
                    let lower_count = text.chars().filter(|c| c.is_lowercase()).count();

                    let toggled = if upper_count > lower_count {
                        // More uppercase, convert to lowercase
                        text.to_lowercase()
                    } else {
                        // More lowercase (or equal), convert to uppercase
                        text.to_uppercase()
                    };

                    if let Some((start, end)) = self.get_selection() {
                        // Preserve selection after replacement
                        let new_end = start + toggled.len();
                        self.replace_at(start, end, &toggled);
                        self.select_range(start, new_end);

                        // Mark affected lines as dirty for syntax highlighting
                        let start_line = self.buffer.byte_to_line(start);
                        let end_line = self.buffer.byte_to_line(new_end.saturating_sub(1).max(start));
                        for line in start_line..=end_line {
                            self.syntax.mark_dirty(line);
                        }
                    }
                }
            }

            Command::MoveLineUp => {
                self.move_lines_up();
                cursor_moved = true;
            }
            Command::MoveLineDown => {
                self.move_lines_down();
                cursor_moved = true;
            }

            Command::None => {}
        }
        
        // Update viewport if cursor moved (but not for pure viewport scrolling)
        if cursor_moved || matches!(cmd,
            Command::InsertChar(_) | Command::InsertNewline | Command::InsertTab |
            Command::Indent | Command::Dedent |
            Command::Backspace | Command::Delete | Command::Paste |
            Command::SelectUp | Command::SelectDown | Command::SelectLeft | Command::SelectRight |
            Command::SelectHome | Command::SelectEnd | Command::SelectAll |
            Command::MoveWordLeft | Command::MoveWordRight |
            Command::MoveParagraphUp | Command::MoveParagraphDown |
            Command::SelectWordLeft | Command::SelectWordRight |
            Command::SelectParagraphUp | Command::SelectParagraphDown |
            Command::MoveLineUp | Command::MoveLineDown
        ) {
            self.update_viewport_for_cursor();
            // Update bracket and text matching after cursor/selection changes
            self.update_matching();
        }

        Ok(())
    }
    
    // Getters for the renderer
    

    /// Rebuild a region of rearranged lines with correct newline structure.
    /// Every line gets a \n except the last line when the region is at the buffer end.
    fn rebuild_region(lines: &[String], at_buffer_end: bool) -> String {
        let mut result = String::new();
        for (i, line) in lines.iter().enumerate() {
            let content = line.trim_end_matches('\n');
            result.push_str(content);
            if i < lines.len() - 1 || !at_buffer_end {
                result.push('\n');
            }
        }
        result
    }

    /// Move the current line (or all selected lines) up by one line.
    fn move_lines_up(&mut self) {
        let (first_line, last_line) = self.affected_line_range();
        if first_line == 0 {
            return;
        }

        let swap_line = first_line - 1;
        let region_start = self.buffer.line_to_byte(swap_line);
        let selected_start = self.buffer.line_to_byte(first_line);
        let region_end = if last_line + 1 < self.buffer.len_lines() {
            self.buffer.line_to_byte(last_line + 1)
        } else {
            self.buffer.len_bytes()
        };
        let at_buffer_end = region_end >= self.buffer.len_bytes();

        // Collect lines individually so phantom trailing lines are captured
        let mut lines: Vec<String> = (swap_line..=last_line)
            .map(|i| self.buffer.line(i).to_string())
            .collect();
        // Rearrange: [swap, selected...] → [selected..., swap]
        let swap = lines.remove(0);
        lines.push(swap);

        let new_text = Self::rebuild_region(&lines, at_buffer_end);

        let cursor_before = self.cursor;
        self.buffer.delete(region_start, region_end, cursor_before, cursor_before);
        self.buffer.insert(region_start, &new_text, cursor_before, cursor_before);
        self.buffer.finalize_undo_group();

        let offset = selected_start - region_start;
        self.cursor = self.cursor.saturating_sub(offset);
        if let Some(ref mut sel) = self.selection_start {
            *sel = sel.saturating_sub(offset);
        }

        self.modified = true;
        self.reinit_syntax_highlighting();
    }

    /// Move the current line (or all selected lines) down by one line.
    fn move_lines_down(&mut self) {
        let (first_line, last_line) = self.affected_line_range();
        let swap_line = last_line + 1;
        if swap_line >= self.buffer.len_lines() {
            return;
        }
        if self.buffer.line_to_byte(swap_line) > self.buffer.len_bytes() {
            return;
        }

        let region_start = self.buffer.line_to_byte(first_line);
        let region_end = if swap_line + 1 < self.buffer.len_lines() {
            self.buffer.line_to_byte(swap_line + 1)
        } else {
            self.buffer.len_bytes()
        };
        let at_buffer_end = region_end >= self.buffer.len_bytes();

        // Collect lines individually so phantom trailing lines are captured
        let mut lines: Vec<String> = (first_line..=swap_line)
            .map(|i| self.buffer.line(i).to_string())
            .collect();
        // Rearrange: [selected..., swap] → [swap, selected...]
        let swap = lines.pop().unwrap();
        lines.insert(0, swap);

        let new_text = Self::rebuild_region(&lines, at_buffer_end);

        let cursor_before = self.cursor;
        self.buffer.delete(region_start, region_end, cursor_before, cursor_before);
        self.buffer.insert(region_start, &new_text, cursor_before, cursor_before);
        self.buffer.finalize_undo_group();

        // Offset = swap line content + \n (it's always first, never last)
        let swap_content_len = lines[0].trim_end_matches('\n').len();
        let offset = swap_content_len + 1;
        self.cursor += offset;
        if let Some(ref mut sel) = self.selection_start {
            *sel += offset;
        }

        self.modified = true;
        self.reinit_syntax_highlighting();
    }

    /// Get the range of lines affected by the current cursor/selection
    fn affected_line_range(&self) -> (usize, usize) {
        if let Some(sel_start) = self.selection_start {
            let start = sel_start.min(self.cursor);
            let end = sel_start.max(self.cursor);
            let first_line = self.buffer.byte_to_line(start);
            let last_line = self.buffer.byte_to_line(end);
            (first_line, last_line)
        } else {
            let line = self.buffer.byte_to_line(self.cursor);
            (line, line)
        }
    }

    // Getters for the renderer

    pub fn cursor(&self) -> usize {
        self.cursor
    }
    
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }
    
    pub fn is_modified(&self) -> bool {
        // In a grid's text view the text is the document, so its own flag counts.
        if let Some(ss) = self.spreadsheet.as_ref().filter(|_| !self.grid_text_view) {
            return ss.is_modified();
        }
        self.modified
    }
    
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
    
    /// Get the current file path or the current directory for Save As prompt
    pub fn get_save_as_initial_path(&self) -> String {
        if let Some(ref path) = self.file_path {
            // Use the current file path
            path.to_string_lossy().to_string()
        } else {
            // Use current directory + "untitled.txt"
            if let Ok(cwd) = std::env::current_dir() {
                cwd.join("untitled.txt").to_string_lossy().to_string()
            } else {
                "untitled.txt".to_string()
            }
        }
    }
    
    /// Get cursor position as (line, display_column)
    /// The column value accounts for Unicode character widths
    /// Check if syntax highlighting has pending work
    pub fn has_syntax_work(&self) -> bool {
        self.syntax.has_dirty_lines()
    }
    
    /// Direct paste method for bracketed paste support
    pub fn paste_text(&mut self, text: String) {
        // Normalize: CRLF → LF, tabs → spaces, remove invisible characters
        // (CSV/TSV data shown as text: kept as is, or refused)
        let text = match self.input_text(text) {
            Ok(text) => text,
            Err(why) => {
                self.status_message = Some((why, true));
                return;
            }
        };

        // Delete selection first if any
        self.delete_selection();

        let line_before = self.buffer.byte_to_line(self.cursor);
        let cursor_before = self.cursor;
        self.buffer.insert(self.cursor, &text, cursor_before, self.cursor + text.len());
        self.cursor += text.len();
        self.modified = true;
        self.preferred_column = None; // Clear preferred column
        
        // Update syntax - check if we added newlines
        let line_after = self.buffer.byte_to_line(self.cursor);
        if line_after > line_before {
            let lines_added = line_after - line_before;
            self.syntax.lines_inserted(line_before + 1, lines_added);
        }
        self.syntax.line_modified(line_before);
    }
    
    /// Process syntax highlighting updates
    pub fn update_syntax_highlighting(&mut self) {
        // Update viewport for large files
        let line_count = self.buffer.len_lines();
        if line_count > 50_000 {
            // Calculate current viewport from cursor position
            let (cursor_line, _) = self.cursor_position();
            let _ = cursor_line;
            let viewport_height = 50; // Approximate visible lines
            let viewport_start = self.viewport_offset.0;
            let viewport_end = viewport_start + viewport_height;
            
            // Set viewport for syntax highlighter
            self.syntax.set_viewport(viewport_start, viewport_end, line_count);
        }
        
        // Process any pending dirty lines
        self.syntax.process_dirty_lines(|line_index| {
            if line_index < self.buffer.len_lines() {
                Some(self.buffer.line(line_index).to_string())
            } else {
                None
            }
        });
    }
    
    /// Update viewport for syntax highlighting in large files
    pub fn update_syntax_viewport(&mut self, viewport_height: usize) {
        let line_count = self.buffer.len_lines();
        if line_count > 50_000 || self.syntax.is_viewport_mode() {
            // Use actual viewport from renderer
            let viewport_start = self.viewport_offset.0;
            let viewport_end = (viewport_start + viewport_height).min(line_count);
            self.syntax.set_viewport(viewport_start, viewport_end, line_count);
        }
    }
    
    /// Get syntax spans for a line (immutable access)
    pub fn get_syntax_spans(&self, line_index: usize) -> Option<&[crate::syntax::HighlightSpan]> {
        self.syntax.get_line_spans(line_index)
    }
    
    /// Reinitialize syntax highlighting (e.g., after undo/redo that changes line count)
    fn reinit_syntax_highlighting(&mut self) {
        let line_count = self.buffer.len_lines();

        // For large files, use viewport mode; otherwise init all lines
        if line_count <= 50_000 {
            self.syntax.init_all_lines(line_count);
            self.syntax.process_dirty_lines(|line_index| {
                if line_index < self.buffer.len_lines() {
                    Some(self.buffer.line(line_index).to_string())
                } else {
                    None
                }
            });
        } else {
            // For large files, just mark all lines as dirty and let viewport mode handle it
            self.syntax.init_all_lines(line_count);
        }
    }

    /// Set the syntax highlighting language
    pub fn set_language(&mut self, language: crate::syntax::Language) {
        self.syntax.set_language(language);
    }

    /// Get the current syntax highlighting language
    pub fn get_language(&self) -> &crate::syntax::Language {
        self.syntax.get_language()
    }

}
