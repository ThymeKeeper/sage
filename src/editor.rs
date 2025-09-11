use crate::buffer::Buffer;
use crate::commands::Command;
use arboard::Clipboard;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct Editor {
    buffer: Buffer,
    cursor: usize,           // Byte position in the buffer
    selection_start: Option<usize>,  // Start of selection (if any)
    file_path: Option<PathBuf>,
    modified: bool,
    viewport_offset: (usize, usize),  // (row, col) offset for scrolling
    last_saved_undo_len: usize,       // Track save point for modified flag
    clipboard: Clipboard,             // System clipboard
}

impl Editor {
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
        }
    }
    
    pub fn load_file(&mut self, path: &str) -> io::Result<()> {
        let content = fs::read_to_string(path)?;
        // Replace all CRLF with LF and tabs with 4 spaces
        let content = content.replace("\r\n", "\n").replace('\t', "    ");
        self.buffer = Buffer::from_string(content);
        self.file_path = Some(PathBuf::from(path));
        self.cursor = 0;
        self.selection_start = None;
        self.modified = false;
        self.viewport_offset = (0, 0);
        self.last_saved_undo_len = 0;
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
                let text = if c == '\t' {
                    "    ".to_string() // Convert tabs to 4 spaces
                } else if c == '\r' {
                    // Skip carriage return characters entirely
                    return Ok(());
                } else {
                    c.to_string()
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
                    let line_start = self.buffer.line_to_byte(current_line);
                    let col = self.cursor - line_start;
                    
                    let new_line = current_line - 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_len = self.buffer.line(new_line).len();
                    
                    self.cursor = new_line_start + col.min(new_line_len.saturating_sub(1));
                }
            }
            
            Command::MoveDown => {
                let current_line = self.buffer.byte_to_line(self.cursor);
                if current_line < self.buffer.len_lines() - 1 {
                    let line_start = self.buffer.line_to_byte(current_line);
                    let col = self.cursor - line_start;
                    
                    let new_line = current_line + 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_len = self.buffer.line(new_line).len();
                    
                    self.cursor = new_line_start + col.min(new_line_len.saturating_sub(1));
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
                    let line_start = self.buffer.line_to_byte(current_line);
                    let col = self.cursor - line_start;
                    
                    let new_line = current_line - 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_len = self.buffer.line(new_line).len();
                    
                    self.cursor = new_line_start + col.min(new_line_len.saturating_sub(1));
                }
            }
            
            Command::SelectDown => {
                if self.selection_start.is_none() {
                    self.selection_start = Some(self.cursor);
                }
                let current_line = self.buffer.byte_to_line(self.cursor);
                if current_line < self.buffer.len_lines() - 1 {
                    let line_start = self.buffer.line_to_byte(current_line);
                    let col = self.cursor - line_start;
                    
                    let new_line = current_line + 1;
                    let new_line_start = self.buffer.line_to_byte(new_line);
                    let new_line_len = self.buffer.line(new_line).len();
                    
                    self.cursor = new_line_start + col.min(new_line_len.saturating_sub(1));
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
                        
                        // Replace all CRLF with LF and tabs with 4 spaces
                        let text = text.replace("\r\n", "\n").replace('\t', "    ");
                        
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
    
    /// Get cursor position as (line, column)
    pub fn cursor_position(&self) -> (usize, usize) {
        let line = self.buffer.byte_to_line(self.cursor);
        let line_start = self.buffer.line_to_byte(line);
        let col = self.cursor - line_start;
        (line, col)
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
}