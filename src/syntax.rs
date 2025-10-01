/// Represents the syntactic state at a point in the text
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxState {
    Normal,
    StringDouble,     // Inside a double-quoted string "..."
    StringSingle,     // Inside a single-quoted string '...'
    LineComment,      // Inside a single-line comment // or --
    BlockComment,     // Inside a multi-line/block comment /* */
}

/// Represents a highlighted span within a line
#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub start: usize,      // Byte offset within the line
    pub end: usize,        // Byte offset within the line
    pub state: SyntaxState,
}

/// Tracks the syntax state for a single line
#[derive(Debug, Clone)]
pub struct LineState {
    /// The state we're in when we start processing this line (from previous line)
    pub entry_state: SyntaxState,
    
    /// The state we're in when we finish processing this line (for next line)
    pub exit_state: SyntaxState,
    
    /// All the highlight spans in this line
    pub spans: Vec<HighlightSpan>,
    
    /// Hash of the line content for change detection
    pub content_hash: u64,
}

impl LineState {
    fn new() -> Self {
        Self {
            entry_state: SyntaxState::Normal,
            exit_state: SyntaxState::Normal,
            spans: Vec::new(),
            content_hash: 0,
        }
    }
}

/// Manages syntax highlighting state for the entire buffer
pub struct SyntaxHighlighter {
    /// State for each line in the buffer
    line_states: Vec<LineState>,
    
    /// Lines that need to be re-highlighted (indices)
    dirty_lines: Vec<usize>,
    
    /// Current viewport range for large files
    viewport_start: usize,
    viewport_end: usize,
    
    /// Whether we're in viewport-only mode (for large files)
    viewport_mode: bool,
    
    /// Buffer size around viewport (lines before/after to process)
    viewport_buffer: usize,
}

impl SyntaxHighlighter {
    pub fn new() -> Self {
        Self {
            line_states: Vec::new(),
            dirty_lines: Vec::new(),
            viewport_start: 0,
            viewport_end: 0,
            viewport_mode: false,
            viewport_buffer: 500,
        }
    }
    
    /// Set viewport for large file mode
    pub fn set_viewport(&mut self, start: usize, end: usize, total_lines: usize) {
        // Check if we should be in viewport mode
        if total_lines > 50_000 && !self.viewport_mode {
            // Switch to viewport mode
            self.viewport_mode = true;
            self.line_states.clear(); // Clear full buffer
            self.dirty_lines.clear();
        } else if total_lines <= 50_000 && self.viewport_mode {
            // Switch back to full mode
            self.viewport_mode = false;
            self.init_all_lines(total_lines);
            return;
        }
        
        if !self.viewport_mode {
            return; // Not in viewport mode, nothing to do
        }
        
        // Calculate the range with buffer
        let buffer_start = start.saturating_sub(self.viewport_buffer);
        let buffer_end = (end + self.viewport_buffer).min(total_lines);
        
        // Check if viewport has moved significantly (more than 100 lines)
        let viewport_changed = 
            buffer_start < self.viewport_start.saturating_sub(100) ||
            buffer_end > self.viewport_end.saturating_add(100);
        
        if viewport_changed {
            self.viewport_start = buffer_start;
            self.viewport_end = buffer_end;
            
            // Mark new lines in range as dirty if they're not already processed
            for line in buffer_start..=buffer_end {
                if line >= self.line_states.len() {
                    // Extend the vector if needed
                    while self.line_states.len() <= line {
                        self.line_states.push(LineState::new());
                    }
                    self.mark_dirty(line);
                } else if self.line_states[line].content_hash == 0 {
                    // Line exists but hasn't been processed yet
                    self.mark_dirty(line);
                }
            }
        }
    }
    
    /// Check if we're in viewport mode
    pub fn is_viewport_mode(&self) -> bool {
        self.viewport_mode
    }
    
    /// Mark a line as needing re-highlighting
    pub fn mark_dirty(&mut self, line_index: usize) {
        if !self.dirty_lines.contains(&line_index) {
            self.dirty_lines.push(line_index);
        }
    }
    
    /// Check if there are any dirty lines to process
    pub fn has_dirty_lines(&self) -> bool {
        !self.dirty_lines.is_empty()
    }
    
    /// Mark a range of lines as dirty
    pub fn mark_range_dirty(&mut self, start: usize, end: usize) {
        for line in start..=end {
            self.mark_dirty(line);
        }
    }
    
    /// Process a single line and update its state
    pub fn process_line(&mut self, line_index: usize, line_content: &str) {
        // Ensure we have enough line states
        while self.line_states.len() <= line_index {
            self.line_states.push(LineState::new());
        }
        
        // Get the entry state from the previous line
        let entry_state = if line_index > 0 && line_index - 1 < self.line_states.len() {
            self.line_states[line_index - 1].exit_state
        } else {
            SyntaxState::Normal
        };
        
        // Parse the line and collect all the data we need
        let mut new_spans = Vec::new();
        let content_hash = calculate_hash(line_content);
        
        // Parse the line
        let mut current_state = entry_state;
        let mut current_pos = 0;
        let mut span_start = 0;
        let bytes = line_content.as_bytes();
        
        while current_pos < bytes.len() {
            match current_state {
                SyntaxState::Normal => {
                    // Check for comment starts (check // and -- before /* to handle them correctly)
                    if current_pos + 1 < bytes.len() {
                        if bytes[current_pos] == b'/' && bytes[current_pos + 1] == b'/' {
                            // Start C-style line comment
                            if current_pos > span_start {
                                new_spans.push(HighlightSpan {
                                    start: span_start,
                                    end: current_pos,
                                    state: SyntaxState::Normal,
                                });
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::LineComment;
                            current_pos += 2;
                            continue;
                        } else if bytes[current_pos] == b'-' && bytes[current_pos + 1] == b'-' {
                            // Start SQL-style line comment
                            if current_pos > span_start {
                                new_spans.push(HighlightSpan {
                                    start: span_start,
                                    end: current_pos,
                                    state: SyntaxState::Normal,
                                });
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::LineComment;
                            current_pos += 2;
                            continue;
                        } else if bytes[current_pos] == b'/' && bytes[current_pos + 1] == b'*' {
                            // Start block comment
                            if current_pos > span_start {
                                new_spans.push(HighlightSpan {
                                    start: span_start,
                                    end: current_pos,
                                    state: SyntaxState::Normal,
                                });
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::BlockComment;
                            current_pos += 2;
                            continue;
                        }
                    }
                    
                    // Check for string starts
                    if bytes[current_pos] == b'"' {
                        // Start double-quoted string
                        if current_pos > span_start {
                            new_spans.push(HighlightSpan {
                                start: span_start,
                                end: current_pos,
                                state: SyntaxState::Normal,
                            });
                        }
                        span_start = current_pos;
                        current_state = SyntaxState::StringDouble;
                        current_pos += 1;
                    // TODO: Enable single-quote string detection for programming languages
                    // For plain text, apostrophes are common and shouldn't be treated as strings
                    /*
                    } else if bytes[current_pos] == b'\'' {
                        // Start single-quoted string
                        if current_pos > span_start {
                            new_spans.push(HighlightSpan {
                                start: span_start,
                                end: current_pos,
                                state: SyntaxState::Normal,
                            });
                        }
                        span_start = current_pos;
                        current_state = SyntaxState::StringSingle;
                        current_pos += 1;
                    */
                    } else {
                        current_pos += 1;
                    }
                }
                
                SyntaxState::StringDouble => {
                    // Look for end of double-quoted string (handling escapes)
                    if bytes[current_pos] == b'\\' && current_pos + 1 < bytes.len() {
                        // Skip escaped character
                        current_pos += 2;
                    } else if bytes[current_pos] == b'"' {
                        // End of string
                        current_pos += 1;
                        new_spans.push(HighlightSpan {
                            start: span_start,
                            end: current_pos,
                            state: SyntaxState::StringDouble,
                        });
                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                    } else {
                        current_pos += 1;
                    }
                }
                
                // TODO: Re-enable when we add language detection
                SyntaxState::StringSingle => {
                    // This state should never be reached with single quotes disabled
                    // but we'll handle it gracefully just in case
                    current_pos += 1;
                    
                    /* Original single-quote string handling for future use:
                    // Look for end of single-quoted string (handling escapes)
                    if bytes[current_pos] == b'\\' && current_pos + 1 < bytes.len() {
                        // Skip escaped character
                        current_pos += 2;
                    } else if bytes[current_pos] == b'\'' {
                        // End of string
                        current_pos += 1;
                        new_spans.push(HighlightSpan {
                            start: span_start,
                            end: current_pos,
                            state: SyntaxState::StringSingle,
                        });
                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                    } else {
                        current_pos += 1;
                    }
                    */
                }
                
                SyntaxState::LineComment => {
                    // Line comment continues to end of line
                    current_pos = bytes.len();
                }
                
                SyntaxState::BlockComment => {
                    // Look for end of block comment
                    if current_pos + 1 < bytes.len() && 
                       bytes[current_pos] == b'*' && bytes[current_pos + 1] == b'/' {
                        // End of block comment
                        current_pos += 2;
                        new_spans.push(HighlightSpan {
                            start: span_start,
                            end: current_pos,
                            state: SyntaxState::BlockComment,
                        });
                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                    } else {
                        current_pos += 1;
                    }
                }
            }
        }
        
        // Add final span if needed
        if span_start < bytes.len() || (span_start == 0 && bytes.len() == 0) {
            new_spans.push(HighlightSpan {
                start: span_start,
                end: bytes.len(),
                state: current_state,
            });
        }
        
        // Set exit state (line comments don't carry over)
        let new_exit_state = match current_state {
            SyntaxState::LineComment => SyntaxState::Normal,
            other => other,
        };
        
        // Check if we need to mark the next line as dirty before updating
        let should_mark_next = if line_index + 1 < self.line_states.len() {
            self.line_states[line_index + 1].entry_state != new_exit_state
        } else {
            false
        };
        
        // Now update the line state
        let line_state = &mut self.line_states[line_index];
        line_state.entry_state = entry_state;
        line_state.exit_state = new_exit_state;
        line_state.spans = new_spans;
        line_state.content_hash = content_hash;
        
        // Mark next line as dirty if needed
        if should_mark_next {
            self.mark_dirty(line_index + 1);
        }
    }
    
    /// Process all dirty lines
    pub fn process_dirty_lines(&mut self, get_line: impl Fn(usize) -> Option<String>) {
        // Early exit if no dirty lines
        if self.dirty_lines.is_empty() {
            return;
        }
        
        // In viewport mode, only process lines within the buffer zone
        let process_limit = if self.viewport_mode { 100 } else { usize::MAX };
        let mut processed = 0;
        
        while let Some(line_index) = self.dirty_lines.pop() {
            // Skip lines outside viewport in viewport mode
            if self.viewport_mode && 
               (line_index < self.viewport_start || line_index > self.viewport_end) {
                continue;
            }
            
            if let Some(line_content) = get_line(line_index) {
                self.process_line(line_index, &line_content);
                processed += 1;
                
                // Limit processing per frame in viewport mode to maintain responsiveness
                if processed >= process_limit {
                    break;
                }
            }
        }
    }
    
    /// Get the highlight spans for a line
    pub fn get_line_spans(&self, line_index: usize) -> Option<&[HighlightSpan]> {
        // In viewport mode, only return spans for processed lines
        if self.viewport_mode && 
           (line_index < self.viewport_start || line_index > self.viewport_end) {
            return None; // Outside viewport, no highlighting
        }
        
        self.line_states.get(line_index).map(|state| state.spans.as_slice())
    }
    
    /// Called when lines are inserted
    pub fn lines_inserted(&mut self, at_line: usize, count: usize) {
        // Ensure we have enough line states before the insertion point
        while self.line_states.len() < at_line {
            self.line_states.push(LineState::new());
        }
        
        // Store the exit state before insertion (if it exists)
        let exit_state_before = if at_line > 0 && at_line - 1 < self.line_states.len() {
            Some(self.line_states[at_line - 1].exit_state)
        } else {
            None
        };
        
        // Insert new line states
        for _ in 0..count {
            self.line_states.insert(at_line.min(self.line_states.len()), LineState::new());
        }
        
        // Mark the inserted lines as dirty
        for i in at_line..at_line + count {
            if i < self.line_states.len() {
                self.mark_dirty(i);
            }
        }
        
        // Check if we need to mark subsequent lines as dirty
        // If the insertion might affect the syntactic state of following lines,
        // we need to mark them as dirty too
        if let Some(prev_exit_state) = exit_state_before {
            // After processing the inserted lines, their exit state might differ
            // from what the next line expects. Mark all subsequent lines that might
            // be affected until we find a stable point
            let mut check_line = at_line + count;
            while check_line < self.line_states.len() {
                // Mark this line as dirty
                self.mark_dirty(check_line);
                
                // In viewport mode, don't propagate too far
                if self.viewport_mode && check_line > at_line + count + 100 {
                    break;
                }
                
                // For small files, we can afford to mark more lines
                if !self.viewport_mode && check_line > at_line + count + 500 {
                    // Stop after checking 500 lines to avoid performance issues
                    break;
                }
                
                check_line += 1;
            }
        } else {
            // No previous state, just mark the immediate next line
            if at_line + count < self.line_states.len() {
                self.mark_dirty(at_line + count);
            }
        }
    }
    
    /// Called when lines are deleted
    pub fn lines_deleted(&mut self, at_line: usize, count: usize) {
        // Remove line states
        for _ in 0..count {
            if at_line < self.line_states.len() {
                self.line_states.remove(at_line);
            }
        }
        
        // Mark the next line as dirty
        if at_line < self.line_states.len() {
            self.mark_dirty(at_line);
        }
    }
    
    /// Called when a line is modified
    pub fn line_modified(&mut self, line_index: usize) {
        // Store the current exit state before marking as dirty
        let old_exit_state = if line_index < self.line_states.len() {
            Some(self.line_states[line_index].exit_state)
        } else {
            None
        };
        
        self.mark_dirty(line_index);
        
        // Mark subsequent lines that might be affected by state changes
        // This is important for multi-line constructs like block comments
        let mut check_line = line_index + 1;
        let max_check = if self.viewport_mode {
            line_index + 100  // Limited propagation in viewport mode
        } else {
            line_index + 500  // More extensive check for small files
        };
        
        while check_line < self.line_states.len() && check_line <= max_check {
            self.mark_dirty(check_line);
            
            // If we know the old exit state was Normal and the line had Normal entry,
            // we might be able to stop early (optimization for future)
            if let Some(SyntaxState::Normal) = old_exit_state {
                if check_line < self.line_states.len() {
                    if self.line_states[check_line].entry_state == SyntaxState::Normal {
                        // Mark one more line and stop
                        check_line += 1;
                        if check_line < self.line_states.len() {
                            self.mark_dirty(check_line);
                        }
                        break;
                    }
                }
            }
            
            check_line += 1;
        }
    }
    
    /// Initialize highlighting for all lines (only for small files)
    pub fn init_all_lines(&mut self, line_count: usize) {
        // Don't init all lines if file is too large
        if line_count > 50_000 {
            self.viewport_mode = true;
            return;
        }
        
        self.viewport_mode = false;
        self.line_states.clear();
        self.dirty_lines.clear();
        for i in 0..line_count {
            self.line_states.push(LineState::new());
            self.dirty_lines.push(i);
        }
    }
}

fn calculate_hash(content: &str) -> u64 {
    // Simple hash function for change detection
    let mut hash: u64 = 5381;
    for byte in content.bytes() {
        hash = ((hash << 5).wrapping_add(hash)).wrapping_add(byte as u64);
    }
    hash
}
