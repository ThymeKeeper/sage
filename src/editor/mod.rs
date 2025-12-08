use crate::buffer::Buffer;
use crate::commands::Command;
use crate::syntax::SyntaxHighlighter;
use crate::cell::{Cell, parse_cells};
use crate::kernel::Kernel;
use arboard::Clipboard;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

// Submodules
mod file_ops;
mod selection;
mod mouse;
mod viewport;
mod matching;
mod notebook;

/// Token type for word boundary detection
#[derive(Debug, PartialEq, Copy, Clone)]
enum TokenType {
    Word,      // Alphanumeric and underscore
    Operator,  // Programming operators
    Space,     // Whitespace
    Other,     // Everything else
}

/// Categorize a character into a token type for word navigation
fn get_token_type(ch: char) -> TokenType {
    if ch.is_alphanumeric() || ch == '_' {
        TokenType::Word
    } else if ch.is_whitespace() {
        TokenType::Space
    } else if ">=<!=+-*/%&|^~.".contains(ch) {
        TokenType::Operator
    } else {
        TokenType::Other
    }
}

pub struct Editor {
    buffer: Buffer,
    cursor: usize,           // Byte position in the buffer
    pub selection_start: Option<usize>,  // Start of selection (if any)
    file_path: Option<PathBuf>,
    modified: bool,
    viewport_offset: (usize, usize),  // (row, col) offset for scrolling
    last_saved_undo_len: usize,       // Track save point for modified flag
    clipboard: Clipboard,             // System clipboard
    mouse_selecting: bool,            // Track if we're actively selecting with mouse
    last_click_time: Option<Instant>, // Track time of last click for double/triple click
    last_click_position: Option<usize>, // Track position of last click
    click_count: usize,               // Track consecutive clicks (1=single, 2=double, 3=triple)
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
            last_saved_undo_len: 0,
            clipboard: Clipboard::new().expect("Failed to access clipboard"),
            mouse_selecting: false,
            last_click_time: None,
            last_click_position: None,
            click_count: 0,
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
        }
    }

    pub fn execute(&mut self, cmd: Command) -> io::Result<()> {
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
                
                // Filter out invisible characters
                let text = match c {
                    '\t' => "    ".to_string(), // Convert tabs to 4 spaces
                    '\r' => return Ok(()), // Skip carriage returns
                    // Skip zero-width and invisible characters
                    '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{200E}' | '\u{200F}' |
                    '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' |
                    '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}' |
                    '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' |
                    '\u{17B4}' | '\u{17B5}' | '\u{180E}' | '\u{3164}' | '\u{FFA0}' |
                    '\u{FE00}'..='\u{FE0F}' => return Ok(()), // Skip invisible chars
                    _ if c >= '\u{E0100}' && c <= '\u{E01EF}' => return Ok(()), // Variation selectors
                    _ => c.to_string(),
                };
                
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
                if !is_at_line_start {
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
                self.buffer.insert(self.cursor, "    ", cursor_before, self.cursor + 4);
                self.cursor += 4;
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
                if self.selection_start.is_none() {
                    // First, we need to adjust the anchor position based on current token
                    let current_line = self.buffer.byte_to_line(self.cursor);
                    let line_start = self.buffer.line_to_byte(current_line);
                    let line_text = self.buffer.line(current_line);
                    let cursor_in_line = self.cursor - line_start;
                    
                    // Find the end of the current token
                    let mut anchor_pos = self.cursor;
                    
                    // Build token list to find current token boundaries
                    let mut tokens = Vec::new();
                    let mut byte_pos = 0;
                    let mut current_token_start = 0;
                    let mut last_token_type = None;
                    
                    for ch in line_text.chars() {
                        let token_type = get_token_type(ch);
                        
                        if last_token_type.is_none() || last_token_type != Some(token_type) {
                            if last_token_type.is_some() {
                                tokens.push((current_token_start, byte_pos, last_token_type.unwrap()));
                            }
                            current_token_start = byte_pos;
                            last_token_type = Some(token_type);
                        }
                        
                        byte_pos += ch.len_utf8();
                    }
                    
                    // Don't forget the last token
                    if last_token_type.is_some() {
                        tokens.push((current_token_start, byte_pos, last_token_type.unwrap()));
                    }
                    
                    // Find which token contains the cursor and use its end
                    for &(start, end, _token_type) in &tokens {
                        if cursor_in_line >= start && cursor_in_line < end {
                            anchor_pos = line_start + end;
                            break;
                        }
                    }
                    
                    self.selection_start = Some(anchor_pos);
                }
                
                let current_line = self.buffer.byte_to_line(self.cursor);
                let line_start = self.buffer.line_to_byte(current_line);
                let line_text = self.buffer.line(current_line);
                let cursor_in_line = self.cursor - line_start;
                
                if cursor_in_line > 0 {
                    // Build a list of tokens with their positions
                    let mut tokens = Vec::new();
                    let mut byte_pos = 0;
                    let mut current_token_start = 0;
                    let mut last_token_type = None;
                    
                    for ch in line_text.chars() {
                        let token_type = get_token_type(ch);
                        
                        // Check if we're starting a new token
                        if last_token_type.is_none() || last_token_type != Some(token_type) {
                            if last_token_type.is_some() && last_token_type != Some(TokenType::Space) {
                                tokens.push((current_token_start, byte_pos));
                            }
                            current_token_start = byte_pos;
                            last_token_type = Some(token_type);
                        }
                        
                        byte_pos += ch.len_utf8();
                    }
                    
                    // Don't forget the last token if it's not a space
                    if last_token_type.is_some() && last_token_type != Some(TokenType::Space) {
                        tokens.push((current_token_start, byte_pos));
                    }
                    
                    // Find the token to move to
                    let mut target_pos = 0;
                    for &(start, _end) in &tokens {
                        if start >= cursor_in_line {
                            break;
                        }
                        target_pos = start;
                    }
                    
                    self.cursor = line_start + target_pos;
                } else {
                    // Already at start of line, stay there
                    self.cursor = line_start;
                }
                self.preferred_column = None;
                cursor_moved = true;
            }
            
            Command::SelectWordRight => {
                if self.selection_start.is_none() {
                    // First, we need to adjust the anchor position based on current token
                    let current_line = self.buffer.byte_to_line(self.cursor);
                    let line_start = self.buffer.line_to_byte(current_line);
                    let line_text = self.buffer.line(current_line);
                    let cursor_in_line = self.cursor - line_start;
                    
                    // Find the start of the current token
                    let mut anchor_pos = self.cursor;
                    if cursor_in_line > 0 {
                        let mut byte_pos = 0;
                        let mut current_token_start = 0;
                        let mut current_token_type = None;
                        let mut cursor_token_type = None;
                        let mut cursor_token_start = 0;
                        
                        // First, determine what token the cursor is in
                        for ch in line_text.chars() {
                            let token_type = get_token_type(ch);
                            
                            if byte_pos <= cursor_in_line && byte_pos + ch.len_utf8() > cursor_in_line {
                                cursor_token_type = Some(token_type);
                                cursor_token_start = current_token_start;
                            }
                            
                            if current_token_type != Some(token_type) {
                                current_token_start = byte_pos;
                                current_token_type = Some(token_type);
                            }
                            
                            byte_pos += ch.len_utf8();
                        }
                        
                        // Special handling for spaces
                        if cursor_token_type == Some(TokenType::Space) {
                            // Check if we're in indentation spaces
                            let mut is_indentation = true;
                            let mut check_pos = 0;
                            for ch in line_text.chars() {
                                if check_pos >= cursor_token_start {
                                    break;
                                }
                                if ch != ' ' && ch != '\t' {
                                    is_indentation = false;
                                    break;
                                }
                                check_pos += ch.len_utf8();
                            }
                            
                            if !is_indentation {
                                // For non-indentation spaces, move to end of spaces
                                let mut pos = cursor_token_start;
                                for ch in line_text[cursor_token_start..].chars() {
                                    if get_token_type(ch) != TokenType::Space {
                                        break;
                                    }
                                    pos += ch.len_utf8();
                                }
                                anchor_pos = line_start + pos;
                            } else {
                                // For indentation spaces, use start of token
                                anchor_pos = line_start + cursor_token_start;
                            }
                        } else {
                            // For other tokens, move to beginning of token
                            anchor_pos = line_start + cursor_token_start;
                        }
                    }
                    
                    self.selection_start = Some(anchor_pos);
                }
                
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
                    // Build a list of tokens with their positions
                    let mut tokens = Vec::new();
                    let mut byte_pos = 0;
                    let mut current_token_start = 0;
                    let mut last_token_type = None;
                    
                    for ch in line_without_newline.chars() {
                        let token_type = get_token_type(ch);
                        
                        // Check if we're starting a new token
                        if last_token_type.is_none() || last_token_type != Some(token_type) {
                            if last_token_type.is_some() && last_token_type != Some(TokenType::Space) {
                                tokens.push((current_token_start, byte_pos));
                            }
                            current_token_start = byte_pos;
                            last_token_type = Some(token_type);
                        }
                        
                        byte_pos += ch.len_utf8();
                    }
                    
                    // Don't forget the last token if it's not a space
                    if last_token_type.is_some() && last_token_type != Some(TokenType::Space) {
                        tokens.push((current_token_start, byte_pos));
                    }
                    
                    // Find the next token end after cursor position
                    let mut target_pos = line_without_newline.len();
                    for &(_start, end) in &tokens {
                        if end > cursor_in_line {
                            target_pos = end;
                            break;
                        }
                    }
                    
                    self.cursor = line_start + target_pos;
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
                        eprintln!("Failed to copy to clipboard: {}", e);
                    }
                }
            }
            
            Command::Cut => {
                if let Some(text) = self.get_selected_text() {
                    if let Err(e) = self.clipboard.set_text(text) {
                        eprintln!("Failed to copy to clipboard: {}", e);
                    } else {
                        self.delete_selection();
                    }
                }
            }
            
            Command::Paste => {
                match self.clipboard.get_text() {
                    Ok(text) => {
                        // Delete selection first if any
                        self.delete_selection();
                        
                        // Normalize: CRLF → LF, tabs → spaces, remove invisible characters
                        let text = Self::normalize_text(text);
                        
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
                    Err(e) => {
                        eprintln!("Failed to paste from clipboard: {}", e);
                    }
                }
            }
            
            Command::Undo => {
                if let Some(cursor) = self.buffer.undo() {
                    let cursor = cursor.min(self.buffer.len_bytes());
                    self.cursor = self.ensure_char_boundary(cursor);
                    self.modified = self.buffer.can_undo();
                    cursor_moved = true;

                    // Reinitialize syntax highlighting after undo
                    self.reinit_syntax_highlighting();
                }
            }

            Command::Redo => {
                if let Some(cursor) = self.buffer.redo() {
                    let cursor = cursor.min(self.buffer.len_bytes());
                    self.cursor = self.ensure_char_boundary(cursor);
                    self.modified = true;
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
            Command::SelectParagraphUp | Command::SelectParagraphDown
        ) {
            self.update_viewport_for_cursor();
            // Update bracket and text matching after cursor/selection changes
            self.update_matching();
        }

        Ok(())
    }
    
    // Getters for the renderer
    

    
    // Getters for the renderer
    
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }
    
    pub fn is_modified(&self) -> bool {
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
    
    /// Move cursor to a specific position
    pub fn move_cursor_to(&mut self, position: usize) {
        self.cursor = position.min(self.buffer.len_bytes());
        self.selection_start = None;
        self.preferred_column = None; // Clear preferred column
        self.update_viewport_for_cursor();
    }
    
    /// Get cursor position as (line, display_column)
    /// The column value accounts for Unicode character widths
    /// Check if syntax highlighting has pending work
    pub fn has_syntax_work(&self) -> bool {
        self.syntax.has_dirty_lines()
    }
    
    /// Direct paste method for bracketed paste support
    pub fn paste_text(&mut self, text: String) {
        // Delete selection first if any
        self.delete_selection();
        
        // Normalize: CRLF → LF, tabs → spaces, remove invisible characters
        let text = Self::normalize_text(text);
        
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
    
    /// Clear the status message
    pub fn clear_status_message(&mut self) {
        self.status_message = None;
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


}
