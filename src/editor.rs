use crate::buffer::Buffer;
use crate::commands::Command;
use arboard::Clipboard;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

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
}

impl Editor {
    /// Normalize text by removing invisible characters and converting line endings/tabs
    fn normalize_text(text: String) -> String {
        text.chars()
            .filter_map(|c| match c {
                // Convert tabs to 4 spaces
                '\t' => Some("    ".to_string()),
                // Remove carriage returns (handled separately for CRLF)
                '\r' => None,
                // Remove zero-width and invisible characters
                '\u{200B}' | // Zero-width space
                '\u{200C}' | // Zero-width non-joiner
                '\u{200D}' | // Zero-width joiner
                '\u{200E}' | // Left-to-right mark
                '\u{200F}' | // Right-to-left mark
                '\u{202A}' | // Left-to-right embedding
                '\u{202B}' | // Right-to-left embedding
                '\u{202C}' | // Pop directional formatting
                '\u{202D}' | // Left-to-right override
                '\u{202E}' | // Right-to-left override
                '\u{2060}' | // Word joiner
                '\u{2061}' | // Function application
                '\u{2062}' | // Invisible times
                '\u{2063}' | // Invisible separator
                '\u{2064}' | // Invisible plus
                '\u{2066}' | // Left-to-right isolate
                '\u{2067}' | // Right-to-left isolate
                '\u{2068}' | // First strong isolate
                '\u{2069}' | // Pop directional isolate
                '\u{206A}' | // Inhibit symmetric swapping
                '\u{206B}' | // Activate symmetric swapping
                '\u{206C}' | // Inhibit Arabic form shaping
                '\u{206D}' | // Activate Arabic form shaping
                '\u{206E}' | // National digit shapes
                '\u{206F}' | // Nominal digit shapes
                '\u{FEFF}' | // Zero-width no-break space (BOM)
                '\u{FFF9}' | // Interlinear annotation anchor
                '\u{FFFA}' | // Interlinear annotation separator
                '\u{FFFB}' | // Interlinear annotation terminator
                '\u{00AD}' | // Soft hyphen
                '\u{034F}' | // Combining grapheme joiner
                '\u{061C}' | // Arabic letter mark
                '\u{115F}' | // Hangul choseong filler
                '\u{1160}' | // Hangul jungseong filler
                '\u{17B4}' | // Khmer vowel inherent AQ
                '\u{17B5}' | // Khmer vowel inherent AA
                '\u{180E}' | // Mongolian vowel separator
                '\u{3164}' | // Hangul filler
                '\u{FFA0}' | // Halfwidth hangul filler
                '\u{FE00}'..='\u{FE0F}' | // Variation selectors
                '\u{E0100}'..='\u{E01EF}' => None, // Variation selectors supplement
                // Keep normal characters
                _ => Some(c.to_string()),
            })
            .collect::<String>()
            // Handle CRLF -> LF conversion after filtering
            .replace("\r\n", "\n")
    }
    
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
        }
    }
    
    pub fn load_file(&mut self, path: &str) -> io::Result<()> {
        let content = fs::read_to_string(path)?;
        // Normalize: CRLF → LF, tabs → spaces, remove invisible characters
        let content = Self::normalize_text(content);
        self.buffer = Buffer::from_string(content);
        self.file_path = Some(PathBuf::from(path));
        self.cursor = 0;
        self.selection_start = None;
        self.modified = false;
        self.viewport_offset = (0, 0);
        self.last_saved_undo_len = 0;
        self.mouse_selecting = false;
        Ok(())
    }
    
    pub fn save(&mut self) -> io::Result<()> {
        if let Some(ref path) = self.file_path {
            fs::write(path, self.buffer.to_string())?;
            self.modified = false;
            self.last_saved_undo_len = 0; // Reset save point
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "No file path set"))
        }
    }
    
    pub fn save_as(&mut self, path: PathBuf) -> io::Result<()> {
        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        fs::write(&path, self.buffer.to_string())?;
        self.file_path = Some(path);
        self.modified = false;
        self.last_saved_undo_len = 0; // Reset save point
        Ok(())
    }
    
    /// Get the current selection as (start, end) byte positions
    fn get_selection(&self) -> Option<(usize, usize)> {
        self.selection_start.map(|start| {
            if start < self.cursor {
                (start, self.cursor)
            } else {
                (self.cursor, start)
            }
        })
    }
    
    /// Get selected text
    fn get_selected_text(&self) -> Option<String> {
        self.get_selection().map(|(start, end)| {
            self.buffer.rope().byte_slice(start..end).to_string()
        })
    }
    
    /// Delete the current selection
    fn delete_selection(&mut self) -> bool {
        if let Some((start, end)) = self.get_selection() {
            let cursor_before = self.cursor;
            self.buffer.delete(start, end, cursor_before, start);
            self.cursor = start;
            self.selection_start = None;
            self.modified = true;
            true
        } else {
            false
        }
    }
    
    pub fn execute(&mut self, cmd: Command) -> io::Result<()> {
        // Clear mouse selection mode on any keyboard input
        self.mouse_selecting = false;
        
        // For non-selection movement commands, clear selection
        match cmd {
            Command::MoveUp | Command::MoveDown | Command::MoveLeft | Command::MoveRight |
            Command::MoveHome | Command::MoveEnd | Command::PageUp | Command::PageDown => {
                self.selection_start = None;
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
            }
            
            Command::InsertNewline => {
                // Delete selection first if any
                self.delete_selection();
                
                let cursor_before = self.cursor;
                self.buffer.insert(self.cursor, "\n", cursor_before, self.cursor + 1);
                self.cursor += 1;
                self.modified = true;
            }
            
            Command::InsertTab => {
                // Delete selection first if any
                self.delete_selection();
                
                let cursor_before = self.cursor;
                self.buffer.insert(self.cursor, "    ", cursor_before, self.cursor + 4);
                self.cursor += 4;
                self.modified = true;
            }
            
            Command::Backspace => {
                // If there's a selection, delete it
                if !self.delete_selection() {
                    // Otherwise delete character before cursor
                    if self.cursor > 0 {
                        let cursor_before = self.cursor;
                        
                        // Find the previous character boundary
                        let char_pos = self.buffer.byte_to_char(self.cursor);
                        if char_pos > 0 {
                            let prev_char_pos = char_pos - 1;
                            let prev_byte = self.buffer.char_to_byte(prev_char_pos);
                            
                            self.buffer.delete(prev_byte, self.cursor, cursor_before, prev_byte);
                            self.cursor = prev_byte;
                            self.modified = true;
                        }
                    }
                }
            }
            
            Command::Delete => {
                // If there's a selection, delete it
                if !self.delete_selection() {
                    // Otherwise delete character after cursor
                    if self.cursor < self.buffer.len_bytes() {
                        let cursor_before = self.cursor;
                        
                        // Find the next character boundary
                        let char_pos = self.buffer.byte_to_char(self.cursor);
                        let next_char_pos = char_pos + 1;
                        let next_byte = self.buffer.char_to_byte(next_char_pos);
                        
                        self.buffer.delete(self.cursor, next_byte, cursor_before, self.cursor);
                        self.modified = true;
                    }
                }
            }
            
            Command::MoveLeft => {
                if self.cursor > 0 {
                    let char_pos = self.buffer.byte_to_char(self.cursor);
                    if char_pos > 0 {
                        self.cursor = self.buffer.char_to_byte(char_pos - 1);
                    }
                }
            }
            
            Command::MoveRight => {
                if self.cursor < self.buffer.len_bytes() {
                    let char_pos = self.buffer.byte_to_char(self.cursor);
                    self.cursor = self.buffer.char_to_byte(char_pos + 1);
                }
            }
            
            Command::MoveUp => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                if current_line > 0 {
                    // Get current display column
                    let (_, current_display_col) = self.cursor_position();
                    
                    let new_line = current_line - 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_text = self.buffer.line(new_line);
                    
                    // Find byte position for the same display column in the new line
                    let mut byte_pos = 0;
                    let mut display_col = 0;
                    
                    for ch in new_line_text.chars() {
                        if display_col >= current_display_col {
                            break;
                        }
                        let char_width = ch.width().unwrap_or(1);
                        if display_col + char_width > current_display_col {
                            // We'd overshoot, stop here
                            break;
                        }
                        display_col += char_width;
                        byte_pos += ch.len_utf8();
                    }
                    
                    self.cursor = new_line_start + byte_pos;
                }
            }
            
            Command::MoveDown => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                if current_line < self.buffer.len_lines() - 1 {
                    // Get current display column
                    let (_, current_display_col) = self.cursor_position();
                    
                    let new_line = current_line + 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_text = self.buffer.line(new_line);
                    
                    // Find byte position for the same display column in the new line
                    let mut byte_pos = 0;
                    let mut display_col = 0;
                    
                    for ch in new_line_text.chars() {
                        if display_col >= current_display_col {
                            break;
                        }
                        let char_width = ch.width().unwrap_or(1);
                        if display_col + char_width > current_display_col {
                            // We'd overshoot, stop here
                            break;
                        }
                        display_col += char_width;
                        byte_pos += ch.len_utf8();
                    }
                    
                    self.cursor = new_line_start + byte_pos;
                }
            }
            
            Command::MoveHome => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                self.cursor = self.buffer.line_to_byte(current_line);
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
                    self.selection_start = Some(self.cursor);
                }
                if self.cursor > 0 {
                    let char_pos = self.buffer.byte_to_char(self.cursor);
                    if char_pos > 0 {
                        self.cursor = self.buffer.char_to_byte(char_pos - 1);
                    }
                }
            }
            
            Command::SelectRight => {
                if self.selection_start.is_none() {
                    self.selection_start = Some(self.cursor);
                }
                if self.cursor < self.buffer.len_bytes() {
                    let char_pos = self.buffer.byte_to_char(self.cursor);
                    self.cursor = self.buffer.char_to_byte(char_pos + 1);
                }
            }
            
            Command::SelectUp => {
                if self.selection_start.is_none() {
                    self.selection_start = Some(self.cursor);
                }
                let current_line = self.buffer.byte_to_line(self.cursor);
                if current_line > 0 {
                    // Get current display column
                    let (_, current_display_col) = self.cursor_position();
                    
                    let new_line = current_line - 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_text = self.buffer.line(new_line);
                    
                    // Find byte position for the same display column in the new line
                    let mut byte_pos = 0;
                    let mut display_col = 0;
                    
                    for ch in new_line_text.chars() {
                        if display_col >= current_display_col {
                            break;
                        }
                        let char_width = ch.width().unwrap_or(1);
                        if display_col + char_width > current_display_col {
                            break;
                        }
                        display_col += char_width;
                        byte_pos += ch.len_utf8();
                    }
                    
                    self.cursor = new_line_start + byte_pos;
                }
            }
            
            Command::SelectDown => {
                if self.selection_start.is_none() {
                    self.selection_start = Some(self.cursor);
                }
                let current_line = self.buffer.byte_to_line(self.cursor);
                if current_line < self.buffer.len_lines() - 1 {
                    // Get current display column
                    let (_, current_display_col) = self.cursor_position();
                    
                    let new_line = current_line + 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_text = self.buffer.line(new_line);
                    
                    // Find byte position for the same display column in the new line
                    let mut byte_pos = 0;
                    let mut display_col = 0;
                    
                    for ch in new_line_text.chars() {
                        if display_col >= current_display_col {
                            break;
                        }
                        let char_width = ch.width().unwrap_or(1);
                        if display_col + char_width > current_display_col {
                            break;
                        }
                        display_col += char_width;
                        byte_pos += ch.len_utf8();
                    }
                    
                    self.cursor = new_line_start + byte_pos;
                }
            }
            
            Command::SelectHome => {
                if self.selection_start.is_none() {
                    self.selection_start = Some(self.cursor);
                }
                let current_line = self.buffer.byte_to_line(self.cursor);
                self.cursor = self.buffer.line_to_byte(current_line);
            }
            
            Command::SelectEnd => {
                if self.selection_start.is_none() {
                    self.selection_start = Some(self.cursor);
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
            }
            
            Command::SelectAll => {
                self.selection_start = Some(0);
                self.cursor = self.buffer.len_bytes();
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
                        
                        let cursor_before = self.cursor;
                        self.buffer.insert(self.cursor, &text, cursor_before, self.cursor + text.len());
                        self.cursor += text.len();
                        self.modified = true;
                    }
                    Err(e) => {
                        eprintln!("Failed to paste from clipboard: {}", e);
                    }
                }
            }
            
            Command::Undo => {
                if let Some(cursor) = self.buffer.undo() {
                    self.cursor = cursor.min(self.buffer.len_bytes());
                    self.modified = self.buffer.can_undo();
                }
            }
            
            Command::Redo => {
                if let Some(cursor) = self.buffer.redo() {
                    self.cursor = cursor.min(self.buffer.len_bytes());
                    self.modified = true;
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
            
            Command::None => {}
        }
        
        Ok(())
    }
    
    // Getters for the renderer
    
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    
    pub fn selection(&self) -> Option<(usize, usize)> {
        self.get_selection()
    }
    
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }
    
    pub fn viewport_offset(&self) -> (usize, usize) {
        self.viewport_offset
    }
    
    pub fn set_viewport_offset(&mut self, offset: (usize, usize)) {
        self.viewport_offset = offset;
    }
    
    pub fn is_modified(&self) -> bool {
        self.modified
    }
    
    pub fn file_path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }
    
    pub fn file_name(&self) -> &str {
        self.file_path
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("[No Name]")
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
    
    /// Find all occurrences of a string in the buffer
    pub fn find_all(&self, search_text: &str) -> Vec<(usize, usize)> {
        if search_text.is_empty() {
            return Vec::new();
        }
        
        let mut matches = Vec::new();
        let content = self.buffer.to_string();
        let search_lower = search_text.to_lowercase();
        let content_lower = content.to_lowercase();
        
        let mut start = 0;
        while let Some(pos) = content_lower[start..].find(&search_lower) {
            let match_start = start + pos;
            let match_end = match_start + search_text.len();
            matches.push((match_start, match_end));
            start = match_start + 1; // Move forward by 1 to find overlapping matches
        }
        
        matches
    }
    
    /// Move cursor to a specific position
    pub fn move_cursor_to(&mut self, position: usize) {
        self.cursor = position.min(self.buffer.len_bytes());
        self.selection_start = None;
    }
    
    /// Select a range of text
    pub fn select_range(&mut self, start: usize, end: usize) {
        self.selection_start = Some(start);
        self.cursor = end;
    }
    
    /// Replace the currently selected text
    pub fn replace_selection(&mut self, replacement: &str) -> bool {
        if let Some((start, end)) = self.get_selection() {
            let cursor_before = self.cursor;
            self.buffer.delete(start, end, cursor_before, start);
            self.buffer.insert(start, replacement, start, start + replacement.len());
            self.cursor = start + replacement.len();
            self.selection_start = None;
            self.modified = true;
            true
        } else {
            false
        }
    }
    
    /// Replace text at a specific position
    pub fn replace_at(&mut self, start: usize, end: usize, replacement: &str) {
        let cursor_before = self.cursor;
        self.buffer.delete(start, end, cursor_before, start);
        self.buffer.insert(start, replacement, start, start + replacement.len());
        
        // Adjust cursor if it was after the replacement
        if self.cursor > end {
            let diff = (end - start) as isize - replacement.len() as isize;
            self.cursor = (self.cursor as isize - diff) as usize;
        } else if self.cursor >= start && self.cursor <= end {
            self.cursor = start + replacement.len();
        }
        
        self.modified = true;
    }
    
    /// Get cursor position as (line, display_column)
    /// The column value accounts for Unicode character widths
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
    
    /// Update viewport to follow cursor with scrolloff
    pub fn update_viewport(&mut self, viewport_height: usize, viewport_width: usize) {
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
        
        // Horizontal scrolling
        if cursor_col < self.viewport_offset.1 + scrolloff {
            self.viewport_offset.1 = cursor_col.saturating_sub(scrolloff);
        } else if cursor_col >= self.viewport_offset.1 + viewport_width - scrolloff {
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
    
    /// Start a mouse selection
    pub fn start_mouse_selection(&mut self, position: usize) {
        let now = Instant::now();
        let double_click_time = Duration::from_millis(500);
        
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
            }
            3 => {
                // Triple click - select line
                self.select_line_at(position);
                self.mouse_selecting = false; // Don't continue selecting on drag
            }
            _ => {
                // Single click - start normal selection
                self.cursor = position;
                self.selection_start = None;
                self.mouse_selecting = true;
            }
        }
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
        while start_char > 0 && chars[start_char - 1].is_alphanumeric() {
            start_char -= 1;
        }
        
        // Move end forward to end of word
        while end_char < chars.len() && chars[end_char].is_alphanumeric() {
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
            self.selection_start = Some(start_byte);
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
        
        self.selection_start = Some(line_start);
        self.cursor = line_end;
    }
    
    /// Update mouse selection while dragging
    pub fn update_mouse_selection(&mut self, position: usize) {
        if self.mouse_selecting {
            if self.selection_start.is_none() {
                // Start selection from the initial cursor position
                self.selection_start = Some(self.cursor);
            }
            // Update cursor to current mouse position
            self.cursor = position;
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