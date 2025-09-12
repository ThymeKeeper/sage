use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor, ResetColor},
    terminal,
};
use std::io::{self, Write};

pub struct FindReplace {
    find_text: String,
    replace_text: String,
    cursor_pos: usize,
    active_field: Field,
    current_match: usize,
    total_matches: usize,
    matches: Vec<(usize, usize)>, // (start_byte, end_byte) positions
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
            active_field: Field::Find,
            current_match: 0,
            total_matches: 0,
            matches: Vec::new(),
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
    
    /// Draw the find/replace window at bottom of screen
    pub fn draw(&self, stdout: &mut io::Stdout) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        
        // Position at bottom, above status bar
        let window_height = 3;  // Still 3 lines but simpler
        let window_y = height.saturating_sub(window_height + 1) as usize; // -1 for status bar
        
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
                
                // Calculate counter string first to know its actual length
                let counter_str = if self.total_matches > 0 || !self.find_text.is_empty() {
                    if self.total_matches > 0 {
                        format!(" [{}/{}]", self.current_match + 1, self.total_matches)
                    } else {
                        " [0/0]".to_string()
                    }
                } else {
                    String::new()
                };
                
                // Calculate field widths - split available space
                let total_width = width as usize - 4; // Subtract borders and padding
                let actual_counter_len = counter_str.len();
                let available_for_fields = total_width.saturating_sub(6 + 9 + actual_counter_len + 4); // "Find: " + "Replace: " + counter + spacing
                let field_width = available_for_fields / 2;
                
                // Find input field
                let visible_find = self.get_visible_text(&self.find_text, field_width, 
                    if self.active_field == Field::Find { self.cursor_pos } else { 0 });
                
                execute!(
                    stdout,
                    SetBackgroundColor(if self.active_field == Field::Find {
                        Color::Rgb { r: 20, g: 20, b: 25 }
                    } else {
                        Color::Rgb { r: 30, g: 30, b: 35 }
                    }),
                    SetForegroundColor(Color::Rgb { r: 220, g: 220, b: 230 }),
                    Print(format!("{:<width$}", visible_find, width = field_width)),
                    SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                )?;
                
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
                let visible_replace = self.get_visible_text(&self.replace_text, field_width,
                    if self.active_field == Field::Replace { self.cursor_pos } else { 0 });
                
                execute!(
                    stdout,
                    SetBackgroundColor(if self.active_field == Field::Replace {
                        Color::Rgb { r: 20, g: 20, b: 25 }
                    } else {
                        Color::Rgb { r: 30, g: 30, b: 35 }
                    }),
                    SetForegroundColor(Color::Rgb { r: 220, g: 220, b: 230 }),
                    Print(format!("{:<width$}", visible_replace, width = field_width)),
                    SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                    SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                )?;
                
                // Fill rest of line
                let actual_counter_len = counter_str.len();
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
        // Recalculate counter string length for cursor positioning
        let counter_len = if self.total_matches > 0 || !self.find_text.is_empty() {
            if self.total_matches > 0 {
                format!(" [{}/{}]", self.current_match + 1, self.total_matches).len()
            } else {
                7 // " [0/0]"
            }
        } else {
            0
        };
        
        // Calculate field widths again for cursor positioning
        let total_width = width as usize - 4;
        let available_for_fields = total_width.saturating_sub(6 + 9 + counter_len + 4);
        let field_width = available_for_fields / 2;
        
        let cursor_display_pos = if (self.active_field == Field::Find && self.cursor_pos > field_width - 1) ||
                                    (self.active_field == Field::Replace && self.cursor_pos > field_width - 1) {
            field_width - 1
        } else {
            self.cursor_pos
        };
        
        let cursor_x = if self.active_field == Field::Find {
            2 + 6 + cursor_display_pos // "│ " + "Find: "
        } else {
            2 + 6 + field_width + counter_len + 2 + 9 + cursor_display_pos // All the way to Replace field
        };
        
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
    
    /// Handle keyboard input
    pub fn handle_input(&mut self, key: KeyCode, modifiers: KeyModifiers) -> InputResult {
        match key {
            KeyCode::Esc => InputResult::Close,
            
            KeyCode::Tab => {
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
                // Enter in find field = find next
                if self.active_field == Field::Find {
                    InputResult::FindNext
                } else {
                    InputResult::Continue
                }
            }
            
            KeyCode::Char(c) if !modifiers.contains(KeyModifiers::CONTROL) => {
                let text = if self.active_field == Field::Find {
                    &mut self.find_text
                } else {
                    &mut self.replace_text
                };
                
                text.insert(self.cursor_pos, c);
                self.cursor_pos += 1;
                
                if self.active_field == Field::Find {
                    InputResult::FindTextChanged
                } else {
                    InputResult::Continue
                }
            }
            
            KeyCode::Backspace => {
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
            
            KeyCode::Delete => {
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
            
            KeyCode::Left => {
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
                
                if self.cursor_pos < text.len() {
                    self.cursor_pos += 1;
                }
                InputResult::Continue
            }
            
            KeyCode::Home => {
                self.cursor_pos = 0;
                InputResult::Continue
            }
            
            KeyCode::End => {
                let text = if self.active_field == Field::Find {
                    &self.find_text
                } else {
                    &self.replace_text
                };
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
