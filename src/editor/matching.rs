use super::Editor;

impl Editor {
    /// Find matching opening bracket scanning backward
    fn find_matching_open(&self, bytes: &[u8], close_pos: usize, close_char: char) -> Option<usize> {
        let open_char = match close_char {
            ')' => '(',
            ']' => '[',
            '}' => '{',
            _ => return None,
        };

        let mut depth = 1;
        let mut pos = close_pos;

        while pos > 0 {
            pos -= 1;
            let ch = bytes[pos] as char;

            if ch == close_char {
                depth += 1;
            } else if ch == open_char {
                depth -= 1;
                if depth == 0 {
                    return Some(pos);
                }
            }
        }

        None
    }

    /// Find matching closing bracket scanning forward
    fn find_matching_close(&self, bytes: &[u8], open_pos: usize, open_char: char) -> Option<usize> {
        let close_char = match open_char {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            _ => return None,
        };

        let mut depth = 1;
        let mut pos = open_pos + 1;

        while pos < bytes.len() {
            let ch = bytes[pos] as char;

            if ch == open_char {
                depth += 1;
            } else if ch == close_char {
                depth -= 1;
                if depth == 0 {
                    return Some(pos);
                }
            }

            pos += 1;
        }

        None
    }

    fn find_matching_brackets(&mut self) {
        self.matching_brackets = None;

        if self.cursor > self.buffer.len_bytes() {
            return;
        }

        let text = self.buffer.to_string();
        let bytes = text.as_bytes();

        // Only check character at cursor position (when block cursor is covering the bracket)
        if self.cursor < bytes.len() {
            let char_at_cursor = bytes[self.cursor] as char;

            // Check for opening bracket
            if matches!(char_at_cursor, '(' | '[' | '{') {
                if let Some(match_pos) = self.find_matching_close(bytes, self.cursor, char_at_cursor) {
                    self.matching_brackets = Some((self.cursor, match_pos));
                    return;
                }
            }

            // Check for closing bracket
            if matches!(char_at_cursor, ')' | ']' | '}') {
                if let Some(match_pos) = self.find_matching_open(bytes, self.cursor, char_at_cursor) {
                    self.matching_brackets = Some((match_pos, self.cursor));
                    return;
                }
            }
        }
    }

    /// Find all occurrences of the selected text in the buffer
    fn find_matching_text(&mut self) {
        self.matching_text_positions.clear();

        if let Some((start, end)) = self.get_selection() {
            // Only highlight if selection is more than 2 characters
            if end - start <= 2 {
                return;
            }

            // Ensure start and end are on character boundaries by converting through char positions
            let start_char = self.buffer.byte_to_char(start);
            let end_char = self.buffer.byte_to_char(end);
            let start = self.buffer.char_to_byte(start_char);
            let end = self.buffer.char_to_byte(end_char);

            // Validate the range
            if start >= end || end > self.buffer.len_bytes() {
                return;
            }

            // Try to get the selected text, but handle any potential panics gracefully
            let selected_text = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.buffer.rope().byte_slice(start..end).to_string()
            })) {
                Ok(text) => text,
                Err(_) => {
                    // If byte_slice panics, just skip highlighting
                    eprintln!("Warning: byte_slice failed with start={}, end={}, len={}",
                             start, end, self.buffer.len_bytes());
                    return;
                }
            };
            let buffer_text = self.buffer.to_string();

            // Find all matches
            let mut search_pos = 0;
            while search_pos < buffer_text.len() {
                // Ensure search_pos is on a character boundary before slicing
                if !buffer_text.is_char_boundary(search_pos) {
                    search_pos += 1;
                    continue;
                }

                if let Some(pos) = buffer_text[search_pos..].find(&selected_text) {
                    let match_start = search_pos + pos;
                    let match_end = match_start + selected_text.len();

                    // Don't include the current selection itself
                    if match_start != start {
                        self.matching_text_positions.push((match_start, match_end));
                    }

                    // Move to next position, ensuring we stay on character boundaries
                    // Use the length of the selected text to skip past this match
                    search_pos = match_start + selected_text.len();
                } else {
                    break;
                }
            }
        }
    }

    /// Update bracket matching and text matching
    pub fn update_matching(&mut self) {
        self.find_matching_brackets();
        self.find_matching_text();
    }

    /// Get matching brackets for rendering
    pub fn get_matching_brackets(&self) -> Option<(usize, usize)> {
        self.matching_brackets
    }

    /// Get matching text positions for rendering
    pub fn get_matching_text_positions(&self) -> &[(usize, usize)] {
        &self.matching_text_positions
    }

    /// Set find matches from find/replace window
    pub fn set_find_matches(&mut self, matches: Vec<(usize, usize)>, current_match: Option<usize>) {
        self.find_matches = matches;
        self.current_find_match = current_match;
    }

    /// Clear find matches
    pub fn clear_find_matches(&mut self) {
        self.find_matches.clear();
        self.current_find_match = None;
    }

    /// Get find matches for rendering
    pub fn get_find_matches(&self) -> &[(usize, usize)] {
        &self.find_matches
    }

    /// Get current find match index
    pub fn get_current_find_match(&self) -> Option<usize> {
        self.current_find_match
    }

    /// Find all occurrences of a string in the buffer (case-insensitive)
    pub fn find_all(&self, search_text: &str) -> Vec<(usize, usize)> {
        // Skip search if text is too short to avoid performance issues
        // (single characters can match thousands of times in large files)
        if search_text.len() < 2 {
            return Vec::new();
        }

        let buffer_text = self.buffer.to_string();
        let search_lower = search_text.to_lowercase();
        let buffer_lower = buffer_text.to_lowercase();
        let mut matches = Vec::new();

        // Build a mapping from lowercase byte positions to original byte positions.
        // This is needed because lowercasing can change byte lengths (e.g. 'İ' 2 bytes -> 'i̇' 3 bytes).
        let mut lower_to_orig: Vec<usize> = Vec::with_capacity(buffer_lower.len() + 1);
        {
            let mut orig_chars = buffer_text.char_indices().peekable();
            for (_lower_idx, lower_ch) in buffer_lower.char_indices() {
                if let Some(&(orig_idx, _orig_ch)) = orig_chars.peek() {
                    // Map each byte of this lowercase char to the original byte position
                    for _ in 0..lower_ch.len_utf8() {
                        lower_to_orig.push(orig_idx);
                    }
                    // Only advance the original iterator when we've consumed a full original character.
                    // A single original char may map to multiple lowercase chars (rare), but for
                    // the common case (same number of chars), advance one-to-one.
                    orig_chars.next();
                }
            }
            // Sentinel for end position
            lower_to_orig.push(buffer_text.len());
        }

        let mut pos = 0;
        while pos < buffer_lower.len() {
            if !buffer_lower.is_char_boundary(pos) {
                pos += 1;
                continue;
            }

            if let Some(found) = buffer_lower[pos..].find(&search_lower) {
                let lower_start = pos + found;
                let lower_end = lower_start + search_lower.len();
                let orig_start = lower_to_orig[lower_start];
                let orig_end = if lower_end < lower_to_orig.len() {
                    lower_to_orig[lower_end]
                } else {
                    buffer_text.len()
                };
                matches.push((orig_start, orig_end));
                pos = lower_end;
            } else {
                break;
            }
        }

        matches
    }
}
