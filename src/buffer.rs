use ropey::Rope;
use std::time::{Duration, Instant};

/// Represents a single edit operation for undo/redo
#[derive(Debug, Clone)]
pub enum EditOp {
    Insert {
        pos: usize,      // byte position
        text: String,    // text that was inserted
    },
    Delete {
        pos: usize,      // byte position
        text: String,    // text that was deleted
    },
}

/// Groups related edit operations (e.g., continuous typing)
#[derive(Debug)]
struct UndoGroup {
    ops: Vec<EditOp>,
    cursor_before: usize,
    cursor_after: usize,
}

/// Text buffer with undo/redo support
pub struct Buffer {
    rope: Rope,
    undo_stack: Vec<UndoGroup>,
    redo_stack: Vec<UndoGroup>,
    current_group: Option<UndoGroup>,
    last_edit_time: Option<Instant>,
    group_timeout: Duration,
}

impl Buffer {
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            current_group: None,
            last_edit_time: None,
            group_timeout: Duration::from_millis(300), // Group edits within 300ms
        }
    }
    
    pub fn from_string(s: String) -> Self {
        // Normalize text: remove invisible characters, CRLF -> LF
        let s = s.chars()
            .filter(|&c| match c {
                // Remove zero-width and invisible characters
                '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{200E}' | '\u{200F}' |
                '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' |
                '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}' |
                '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' |
                '\u{17B4}' | '\u{17B5}' | '\u{180E}' | '\u{3164}' | '\u{FFA0}' |
                '\u{FE00}'..='\u{FE0F}' => false,
                _ if c >= '\u{E0100}' && c <= '\u{E01EF}' => false, // Variation selectors
                _ => true, // Keep everything else
            })
            .collect::<String>()
            .replace("\r\n", "\n"); // CRLF -> LF
        
        Self {
            rope: Rope::from_str(&s),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            current_group: None,
            last_edit_time: None,
            group_timeout: Duration::from_millis(300),
        }
    }
    
    /// Get the rope for reading
    pub fn rope(&self) -> &Rope {
        &self.rope
    }
    
    /// Get total byte length
    pub fn len_bytes(&self) -> usize {
        self.rope.len_bytes()
    }
    
    /// Get total number of lines
    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }
    
    /// Convert byte position to char position
    pub fn byte_to_char(&self, byte_pos: usize) -> usize {
        self.rope.byte_to_char(byte_pos.min(self.rope.len_bytes()))
    }
    
    /// Convert char position to byte position
    pub fn char_to_byte(&self, char_pos: usize) -> usize {
        self.rope.char_to_byte(char_pos.min(self.rope.len_chars()))
    }
    
    /// Get line number from byte position
    pub fn byte_to_line(&self, byte_pos: usize) -> usize {
        let char_pos = self.byte_to_char(byte_pos);
        self.rope.char_to_line(char_pos)
    }
    
    /// Get the start byte position of a line
    pub fn line_to_byte(&self, line: usize) -> usize {
        if line >= self.rope.len_lines() {
            return self.rope.len_bytes();
        }
        let char_pos = self.rope.line_to_char(line);
        self.char_to_byte(char_pos)
    }
    
    /// Get a line as a string
    pub fn line(&self, line_idx: usize) -> String {
        if line_idx < self.rope.len_lines() {
            self.rope.line(line_idx).to_string()
        } else {
            String::new()
        }
    }
    
    /// Insert text at byte position
    pub fn insert(&mut self, pos: usize, text: &str, cursor_before: usize, cursor_after: usize) {
        let char_pos = self.byte_to_char(pos);
        self.rope.insert(char_pos, text);
        
        let op = EditOp::Insert {
            pos,
            text: text.to_string(),
        };
        
        self.push_op(op, cursor_before, cursor_after);
        self.redo_stack.clear();
    }
    
    /// Delete a range of bytes
    pub fn delete(&mut self, start: usize, end: usize, cursor_before: usize, cursor_after: usize) {
        if start >= end || start >= self.len_bytes() {
            return;
        }
        
        let end = end.min(self.len_bytes());
        let text = self.rope.byte_slice(start..end).to_string();
        
        let char_start = self.byte_to_char(start);
        let char_end = self.byte_to_char(end);
        self.rope.remove(char_start..char_end);
        
        let op = EditOp::Delete {
            pos: start,
            text,
        };
        
        self.push_op(op, cursor_before, cursor_after);
        self.redo_stack.clear();
    }
    
    /// Push an operation to the current undo group
    fn push_op(&mut self, op: EditOp, cursor_before: usize, cursor_after: usize) {
        let now = Instant::now();
        let should_start_new_group = self.last_edit_time
            .map(|t| now.duration_since(t) > self.group_timeout)
            .unwrap_or(true);
        
        if should_start_new_group {
            self.finalize_undo_group();
            self.current_group = Some(UndoGroup {
                ops: vec![op],
                cursor_before,
                cursor_after,
            });
        } else if let Some(ref mut group) = self.current_group {
            group.ops.push(op);
            group.cursor_after = cursor_after;
        }
        
        self.last_edit_time = Some(now);
    }
    
    /// Finalize the current undo group
    pub fn finalize_undo_group(&mut self) {
        if let Some(group) = self.current_group.take() {
            if !group.ops.is_empty() {
                self.undo_stack.push(group);
            }
        }
    }
    
    /// Undo the last group of operations
    pub fn undo(&mut self) -> Option<usize> {
        self.finalize_undo_group();
        
        if let Some(group) = self.undo_stack.pop() {
            let cursor = group.cursor_before;
            
            // Apply operations in reverse
            for op in group.ops.iter().rev() {
                match op {
                    EditOp::Insert { pos, text } => {
                        // Undo an insert by deleting
                        let char_start = self.byte_to_char(*pos);
                        let char_end = char_start + text.chars().count();
                        self.rope.remove(char_start..char_end);
                    }
                    EditOp::Delete { pos, text } => {
                        // Undo a delete by inserting
                        let char_pos = self.byte_to_char(*pos);
                        self.rope.insert(char_pos, text);
                    }
                }
            }
            
            self.redo_stack.push(group);
            return Some(cursor);
        }
        
        None
    }
    
    /// Redo the last undone group
    pub fn redo(&mut self) -> Option<usize> {
        if let Some(group) = self.redo_stack.pop() {
            let cursor = group.cursor_after;
            
            // Apply operations forward
            for op in &group.ops {
                match op {
                    EditOp::Insert { pos, text } => {
                        let char_pos = self.byte_to_char(*pos);
                        self.rope.insert(char_pos, text);
                    }
                    EditOp::Delete { pos, text } => {
                        let char_start = self.byte_to_char(*pos);
                        let char_end = char_start + text.chars().count();
                        self.rope.remove(char_start..char_end);
                    }
                }
            }
            
            self.undo_stack.push(group);
            return Some(cursor);
        }
        
        None
    }
    
    /// Get the entire buffer as a string (for saving)
    pub fn to_string(&self) -> String {
        self.rope.to_string()
    }
    
    /// Check if there are undo operations available
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty() || self.current_group.is_some()
    }
    
    /// Check if there are redo operations available
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }
}