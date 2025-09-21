use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor, ResetColor},
    terminal,
};
use arboard::Clipboard;
use std::io::{self, Write};

pub struct FindReplace {
    find_text: String,
    replace_text: String,
    cursor_pos: usize,
    selection_start: Option<usize>,  // Selection start position in active field
    active_field: Field,
    current_match: usize,
    total_matches: usize,
    matches: Vec<(usize, usize)>, // (start_byte, end_byte) positions
    clipboard: Clipboard,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Find,
    Replace,
}

impl FindReplace {
    pub fn new() -> Self {
        Self {
            find_text: String::new(),
            replace_text: String::new(),
            cursor_pos: 0,
            selection_start: None,
            active_field: Field::Find,
            current_match: 0,
            total_matches: 0,
            matches: Vec::new(),
            clipboard: Clipboard::new().expect("Failed to access clipboard"),
        }
    }
    
    /// Update the search results
    pub fn update_matches(&mut self, matches: Vec<(usize, usize)>) {
        self.matches = matches;
        self.total_matches = self.matches.len();
        if self.total_matches > 0 && self.current_match >= self.total_matches {
            self.current_match = self.total_matches - 1;
        }
    }
    
    /// Get the current match position
    pub fn current_match_position(&self) -> Option<(usize, usize)> {
        if self.total_matches > 0 && self.current_match < self.total_matches {
            Some(self.matches[self.current_match])
        } else {
            None
        }
    }
    
    /// Move to next match
    pub fn next_match(&mut self) -> Option<(usize, usize)> {
        if self.total_matches > 0 {
            self.current_match = (self.current_match + 1) % self.total_matches;
            self.current_match_position()
        } else {
            None
        }
    }
    
    /// Move to previous match
    pub fn prev_match(&mut self) -> Option<(usize, usize)> {
        if self.total_matches > 0 {
            if self.current_match == 0 {
                self.current_match = self.total_matches - 1;
            } else {
                self.current_match -= 1;
            }
            self.current_match_position()
        } else {
            None
        }
    }
    
    /// Reset match index (call after replace operations)
    pub fn reset_current_match(&mut self) {
        self.current_match = 0;
    }
    
    /// Get the find text
    pub fn find_text(&self) -> &str {
        &self.find_text
    }
    
    /// Get the replace text
    pub fn replace_text(&self) -> &str {
        &self.replace_text
    }
    
    /// Check if find text is empty
    pub fn is_empty(&self) -> bool {
        self.find_text.is_empty()
    }
    
    /// Get the current selection in the active field (start, end)
    fn get_selection(&self) -> Option<(usize, usize)> {
        self.selection_start.map(|start| {
            if start < self.cursor_pos {
                (start, self.cursor_pos)
            } else {
                (self.cursor_pos, start)
            }
        })
    }
    
    /// Delete the selected text in the active field
    fn delete_selection(&mut self) -> bool {
        if let Some((start, end)) = self.get_selection() {
            let text = if self.active_field == Field::Find {
                &mut self.find_text
            } else {
                &mut self.replace_text
            };
            
            text.drain(start..end);
            self.cursor_pos = start;
            self.selection_start = None;
            true
        } else {
            false
        }
    }
    
    /// Get selected text in the active field
    fn get_selected_text(&self) -> Option<String> {
        self.get_selection().map(|(start, end)| {
            let text = if self.active_field == Field::Find {
                &self.find_text
            } else {
                &self.replace_text
            };
            text[start..end].to_string()
        })
    }
    
    /// Draw the find/replace window at bottom of screen
    pub fn draw(&self, stdout: &mut io::Stdout) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        
        // Position at bottom, above status bar
        let window_height = 3;  // Still 3 lines but simpler
        let window_y = height.saturating_sub(window_height + 1) as usize; // -1 for status bar
        
        // Calculate counter string first to know its actual length - we'll use this for both drawing and cursor positioning
        let counter_str = if self.total_matches > 0 || !self.find_text.is_empty() {
            if self.total_matches > 0 {
                format!(" [{}/{}]", self.current_match + 1, self.total_matches)
            } else {
                " [0/0]".to_string()
            }
        } else {
            String::new()
        };
        let actual_counter_len = counter_str.len();
        
        // Calculate field widths - split available space
        let total_width = width as usize - 4; // Subtract borders and padding
        let available_for_fields = total_width.saturating_sub(6 + 9 + actual_counter_len + 4); // "Find: " + "Replace: " + counter + spacing
        let field_width = available_for_fields / 2;
        
        // Draw window background with border
        for y in 0..window_height as usize {
            execute!(
                stdout,
                MoveTo(0, (window_y + y) as u16),
                SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
            )?;
            
            if y == 0 {
                // Top border with rounded corners
                write!(stdout, "╭")?;
                for _ in 1..width - 1 {
                    write!(stdout, "─")?;
                }
                write!(stdout, "╮")?;
            } else if y == 1 {
                // Both fields on the same line
                write!(stdout, "│ ")?;
                
                // Find label and field
                execute!(
                    stdout,
                    SetForegroundColor(if self.active_field == Field::Find {
                        Color::White
                    } else {
                        Color::Rgb { r: 150, g: 150, b: 155 }
                    }),
                    Print("Find: "),
                    SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                )?;
                
                // Find input field
                let visible_find = self.get_visible_text_with_selection(
                    &self.find_text, 
                    field_width, 
                    if self.active_field == Field::Find { self.cursor_pos } else { 0 },
                    if self.active_field == Field::Find { self.get_selection() } else { None }
                );
                
                execute!(
                    stdout,
                    SetBackgroundColor(if self.active_field == Field::Find {
                        Color::Rgb { r: 20, g: 20, b: 25 }
                    } else {
                        Color::Rgb { r: 30, g: 30, b: 35 }
                    }),
                    SetForegroundColor(Color::Rgb { r: 220, g: 220, b: 230 }),
                )?;
                
                // Write the field content with selection highlighting
                write!(stdout, "{}", visible_find)?;
                
                // Pad to field width
                let visible_len = self.visible_length(&visible_find);
                for _ in visible_len..field_width {
                    write!(stdout, " ")?;
                }
                
                execute!(stdout, SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }))?;
                
                // Match counter
                if !counter_str.is_empty() {
                    execute!(
                        stdout,
                        SetForegroundColor(Color::Rgb { r: 150, g: 200, b: 150 }),
                        Print(&counter_str),
                        SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                    )?;
                }
                
                // Spacing between fields
                write!(stdout, "  ")?;
                
                // Replace label and field
                execute!(
                    stdout,
                    SetForegroundColor(if self.active_field == Field::Replace {
                        Color::White
                    } else {
                        Color::Rgb { r: 150, g: 150, b: 155 }
                    }),
                    Print("Replace: "),
                    SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                )?;
                
                // Replace input field
                let visible_replace = self.get_visible_text_with_selection(
                    &self.replace_text, 
                    field_width,
                    if self.active_field == Field::Replace { self.cursor_pos } else { 0 },
                    if self.active_field == Field::Replace { self.get_selection() } else { None }
                );
                
                execute!(
                    stdout,
                    SetBackgroundColor(if self.active_field == Field::Replace {
                        Color::Rgb { r: 20, g: 20, b: 25 }
                    } else {
                        Color::Rgb { r: 30, g: 30, b: 35 }
                    }),
                    SetForegroundColor(Color::Rgb { r: 220, g: 220, b: 230 }),
                )?;
                
                // Write the field content with selection highlighting
                write!(stdout, "{}", visible_replace)?;
                
                // Pad to field width
                let visible_len = self.visible_length(&visible_replace);
                for _ in visible_len..field_width {
                    write!(stdout, " ")?;
                }
                
                execute!(
                    stdout,
                    SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                    SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                )?;
                
                // Fill rest of line
                let used = 2 + 6 + field_width + actual_counter_len + 2 + 9 + field_width;
                // Make sure we don't overflow
                if used < width as usize - 1 {
                    for _ in used..width as usize - 1 {
                        write!(stdout, " ")?;
                    }
                }
                write!(stdout, "│")?;
                
            } else if y == 2 {
                // Bottom border - clean and simple
                write!(stdout, "╰")?;
                for _ in 1..width - 1 {
                    write!(stdout, "─")?;
                }
                write!(stdout, "╯")?;
            }
        }
        
        // Position cursor in active field
        // Use the same counter length and field width that we calculated for drawing
        
        // Calculate the actual cursor display position within the visible field
        let (active_text, active_cursor) = if self.active_field == Field::Find {
            (&self.find_text, self.cursor_pos)
        } else {
            (&self.replace_text, self.cursor_pos)
        };
        
        // Calculate scrolling offset for the field
        let scroll_offset = if active_text.len() <= field_width {
            0
        } else if active_cursor > field_width - 1 {
            active_cursor - (field_width - 1)
        } else {
            0
        };
        
        // The cursor's visual position within the field
        let cursor_display_pos = active_cursor.saturating_sub(scroll_offset);
        
        let cursor_x = if self.active_field == Field::Find {
            2 + 6 + cursor_display_pos // "│ " + "Find: "
        } else {
            2 + 6 + field_width + actual_counter_len + 2 + 9 + cursor_display_pos // All the way to Replace field
        };
        
        // Just position the cursor, don't change style
        execute!(
            stdout,
            MoveTo(cursor_x as u16, (window_y + 1) as u16),  // Both fields are on line 1
            ResetColor,
            Show
        )?;
        
        stdout.flush()?;
        Ok(())
    }
    
    /// Get visible portion of text for scrolling
    fn get_visible_text(&self, text: &str, width: usize, cursor: usize) -> String {
        if text.len() <= width {
            text.to_string()
        } else if cursor > width - 1 {
            let start = cursor - (width - 1);
            let end = (start + width).min(text.len());
            text[start..end].to_string()
        } else {
            text[..width].to_string()
        }
    }
    
    /// Get visible portion of text with selection highlighting
    fn get_visible_text_with_selection(&self, text: &str, width: usize, cursor: usize, selection: Option<(usize, usize)>) -> String {
        let visible = self.get_visible_text(text, width, cursor);
        
        // Calculate offset for visible portion
        let offset = if text.len() <= width {
            0
        } else if cursor > width - 1 {
            cursor - (width - 1)
        } else {
            0
        };
        
        // Apply selection highlighting if needed
        if let Some((sel_start, sel_end)) = selection {
            let mut result = String::new();
            for (i, ch) in visible.chars().enumerate() {
                let abs_pos = offset + i;
                if abs_pos >= sel_start && abs_pos < sel_end {
                    // Selected character - use inverted colors
                    result.push_str("\x1b[48;2;95;158;160m\x1b[38;2;0;0;0m");
                    result.push(ch);
                    result.push_str("\x1b[0m");
                    // Restore the field background color
                    result.push_str("\x1b[48;2;20;20;25m\x1b[38;2;220;220;230m");
                } else {
                    result.push(ch);
                }
            }
            result
        } else {
            visible
        }
    }
    
    /// Calculate visible length accounting for ANSI escape sequences
    fn visible_length(&self, s: &str) -> usize {
        let mut len = 0;
        let mut in_escape = false;
        for ch in s.chars() {
            if ch == '\x1b' {
                in_escape = true;
            } else if in_escape {
                if ch == 'm' {
                    in_escape = false;
                }
            } else {
                len += 1;
            }
        }
        len
    }
    
    /// Handle keyboard input
    pub fn handle_input(&mut self, key: KeyCode, modifiers: KeyModifiers) -> InputResult {
        match key {
            KeyCode::Esc => {
                self.selection_start = None; // Clear selection when closing
                InputResult::Close
            }
            
            KeyCode::Tab => {
                // Clear selection when switching fields
                self.selection_start = None;
                // Switch between fields
                self.active_field = match self.active_field {
                    Field::Find => {
                        self.cursor_pos = self.replace_text.len();
                        Field::Replace
                    }
                    Field::Replace => {
                        self.cursor_pos = self.find_text.len();
                        Field::Find
                    }
                };
                InputResult::Continue
            }
            
            KeyCode::Enter => {
                // Clear selection
                self.selection_start = None;
                // Enter in find field = find next
                if self.active_field == Field::Find {
                    InputResult::FindNext
                } else {
                    InputResult::Continue
                }
            }
            
            // Handle Ctrl+A (Select All)
            KeyCode::Char('a') | KeyCode::Char('A') if modifiers.contains(KeyModifiers::CONTROL) => {
                // Select all text in current field
                self.selection_start = Some(0);
                let text = if self.active_field == Field::Find {
                    &self.find_text
                } else {
                    &self.replace_text
                };
                self.cursor_pos = text.len();
                InputResult::Continue
            }
            
            // Handle Ctrl+C (Copy)
            KeyCode::Char('c') | KeyCode::Char('C') if modifiers.contains(KeyModifiers::CONTROL) => {
                // Copy selected text or entire field if no selection
                let text_to_copy = if let Some(selected) = self.get_selected_text() {
                    selected
                } else {
                    // No selection, copy entire field
                    if self.active_field == Field::Find {
                        self.find_text.clone()
                    } else {
                        self.replace_text.clone()
                    }
                };
                
                if let Err(e) = self.clipboard.set_text(text_to_copy) {
                    eprintln!("Failed to copy to clipboard: {}", e);
                }
                InputResult::Continue
            }
            
            // Handle Ctrl+X (Cut)
            KeyCode::Char('x') | KeyCode::Char('X') if modifiers.contains(KeyModifiers::CONTROL) => {
                // Cut selected text or entire field if no selection
                let text_to_cut = if let Some(selected) = self.get_selected_text() {
                    selected
                } else {
                    // No selection, cut entire field
                    let text = if self.active_field == Field::Find {
                        &mut self.find_text
                    } else {
                        &mut self.replace_text
                    };
                    let all = text.clone();
                    text.clear();
                    self.cursor_pos = 0;
                    all
                };
                
                if let Err(e) = self.clipboard.set_text(text_to_cut) {
                    eprintln!("Failed to copy to clipboard: {}", e);
                } else if self.selection_start.is_some() {
                    // Delete the selection after copying
                    self.delete_selection();
                }
                
                if self.active_field == Field::Find {
                    InputResult::FindTextChanged
                } else {
                    InputResult::Continue
                }
            }
            
            // Handle Ctrl+V (Paste)
            KeyCode::Char('v') | KeyCode::Char('V') if modifiers.contains(KeyModifiers::CONTROL) => {
                match self.clipboard.get_text() {
                    Ok(clipboard_text) => {
                        // Truncate at first newline for single-line fields
                        let text_to_paste = clipboard_text
                            .lines()
                            .next()
                            .unwrap_or("")
                            .to_string();
                        
                        // Delete any selected text first
                        self.delete_selection();
                        
                        let text = if self.active_field == Field::Find {
                            &mut self.find_text
                        } else {
                            &mut self.replace_text
                        };
                        
                        // Insert clipboard content at cursor position
                        text.insert_str(self.cursor_pos, &text_to_paste);
                        self.cursor_pos += text_to_paste.len();
                        
                        if self.active_field == Field::Find {
                            InputResult::FindTextChanged
                        } else {
                            InputResult::Continue
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to paste from clipboard: {}", e);
                        InputResult::Continue
                    }
                }
            }
            
            KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
                // Delete any selected text first
                let was_selection = self.delete_selection();
                
                let text = if self.active_field == Field::Find {
                    &mut self.find_text
                } else {
                    &mut self.replace_text
                };
                
                text.insert(self.cursor_pos, c);
                self.cursor_pos += 1;
                
                if self.active_field == Field::Find || was_selection {
                    InputResult::FindTextChanged
                } else {
                    InputResult::Continue
                }
            }
            
            KeyCode::Backspace => {
                // If there's a selection, delete it
                if self.delete_selection() {
                    if self.active_field == Field::Find {
                        InputResult::FindTextChanged
                    } else {
                        InputResult::Continue
                    }
                } else {
                    // No selection, delete character before cursor
                    let text = if self.active_field == Field::Find {
                        &mut self.find_text
                    } else {
                        &mut self.replace_text
                    };
                    
                    if self.cursor_pos > 0 {
                        self.cursor_pos -= 1;
                        text.remove(self.cursor_pos);
                        
                        if self.active_field == Field::Find {
                            InputResult::FindTextChanged
                        } else {
                            InputResult::Continue
                        }
                    } else {
                        InputResult::Continue
                    }
                }
            }
            
            KeyCode::Delete => {
                // If there's a selection, delete it
                if self.delete_selection() {
                    if self.active_field == Field::Find {
                        InputResult::FindTextChanged
                    } else {
                        InputResult::Continue
                    }
                } else {
                    // No selection, delete character after cursor
                    let text = if self.active_field == Field::Find {
                        &mut self.find_text
                    } else {
                        &mut self.replace_text
                    };
                    
                    if self.cursor_pos < text.len() {
                        text.remove(self.cursor_pos);
                        
                        if self.active_field == Field::Find {
                            InputResult::FindTextChanged
                        } else {
                            InputResult::Continue
                        }
                    } else {
                        InputResult::Continue
                    }
                }
            }
            
            KeyCode::Left => {
                if modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+Left = select left
                    if self.selection_start.is_none() {
                        self.selection_start = Some(self.cursor_pos);
                    }
                } else {
                    // Regular Left = clear selection and move
                    self.selection_start = None;
                }
                
                if self.cursor_pos > 0 {
                    self.cursor_pos -= 1;
                }
                InputResult::Continue
            }
            
            KeyCode::Right => {
                let text = if self.active_field == Field::Find {
                    &self.find_text
                } else {
                    &self.replace_text
                };
                
                if modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+Right = select right
                    if self.selection_start.is_none() {
                        self.selection_start = Some(self.cursor_pos);
                    }
                } else {
                    // Regular Right = clear selection and move
                    self.selection_start = None;
                }
                
                if self.cursor_pos < text.len() {
                    self.cursor_pos += 1;
                }
                InputResult::Continue
            }
            
            KeyCode::Home => {
                if modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+Home = select to beginning
                    if self.selection_start.is_none() {
                        self.selection_start = Some(self.cursor_pos);
                    }
                } else {
                    // Regular Home = clear selection and move
                    self.selection_start = None;
                }
                self.cursor_pos = 0;
                InputResult::Continue
            }
            
            KeyCode::End => {
                let text = if self.active_field == Field::Find {
                    &self.find_text
                } else {
                    &self.replace_text
                };
                
                if modifiers.contains(KeyModifiers::SHIFT) {
                    // Shift+End = select to end
                    if self.selection_start.is_none() {
                        self.selection_start = Some(self.cursor_pos);
                    }
                } else {
                    // Regular End = clear selection and move
                    self.selection_start = None;
                }
                self.cursor_pos = text.len();
                InputResult::Continue
            }
            
            _ => InputResult::Continue,
        }
    }
}

pub enum InputResult {
    Continue,
    FindTextChanged,
    FindNext,
    Close,
}