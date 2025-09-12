use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor, ResetColor},
    terminal::{self, Clear, ClearType},
};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub struct Prompt {
    title: String,
    input: String,
    cursor_pos: usize,
    initial_path: String,
    // Cache window dimensions
    prompt_x: u16,
    prompt_y: u16,
    prompt_width: usize,
    prompt_height: usize,
    needs_full_redraw: bool,
}

impl Prompt {
    pub fn new(title: &str, initial_path: &str) -> Self {
        Self {
            title: title.to_string(),
            input: initial_path.to_string(),
            cursor_pos: initial_path.len(),
            initial_path: initial_path.to_string(),
            prompt_x: 0,
            prompt_y: 0,
            prompt_width: 0,
            prompt_height: 7,
            needs_full_redraw: true,
        }
    }
    
    /// Draw the complete prompt window (borders, title, etc.)
    fn draw_window(&mut self, stdout: &mut io::Stdout) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        
        // Calculate prompt dimensions and position
        self.prompt_width = (width as usize * 3 / 4).min(80).max(40);
        self.prompt_height = 7;
        self.prompt_x = ((width as usize - self.prompt_width) / 2) as u16;
        self.prompt_y = ((height as usize - self.prompt_height) / 2) as u16;
        
        // Draw shadow effect first (one row down, two columns right)
        for y in 1..self.prompt_height {
            execute!(
                stdout,
                MoveTo(self.prompt_x + 2, (self.prompt_y as usize + y) as u16),
                SetBackgroundColor(Color::Black),
                Print(" ".repeat(self.prompt_width))
            )?;
        }
        
        // Draw main window
        for y in 0..self.prompt_height {
            execute!(
                stdout,
                MoveTo(self.prompt_x, (self.prompt_y as usize + y) as u16),
                SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
            )?;
            
            if y == 0 {
                // Top border with rounded corners
                write!(stdout, "╭")?;
                for _ in 1..self.prompt_width - 1 {
                    write!(stdout, "─")?;
                }
                write!(stdout, "╮")?;
            } else if y == self.prompt_height - 1 {
                // Bottom border with rounded corners
                write!(stdout, "╰")?;
                for _ in 1..self.prompt_width - 1 {
                    write!(stdout, "─")?;
                }
                write!(stdout, "╯")?;
            } else if y == 2 {
                // Title line with emphasis
                write!(stdout, "│ ")?;
                execute!(
                    stdout,
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::White),
                    Print(&self.title),
                    SetAttribute(Attribute::Reset),
                    SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                    SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                )?;
                let title_padding = self.prompt_width - 3 - self.title.len();
                write!(stdout, "{:width$}│", "", width = title_padding)?;
            } else if y == 4 {
                // Input line - will be updated separately
                write!(stdout, "│{:width$}│", "", width = self.prompt_width - 2)?;
            } else if y == self.prompt_height - 2 {
                // Help text line
                let help_text = "[Enter: Save] [Esc: Cancel]";
                let padding = (self.prompt_width - 2 - help_text.len()) / 2;
                write!(stdout, "│")?;
                execute!(
                    stdout,
                    SetForegroundColor(Color::Rgb { r: 130, g: 130, b: 140 }),
                    Print(format!("{:>width$}{}{:<width2$}", "", help_text, "", 
                        width = padding, width2 = self.prompt_width - 2 - padding - help_text.len())),
                    SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                )?;
                write!(stdout, "│")?;
            } else {
                // Empty lines
                write!(stdout, "│{:width$}│", "", width = self.prompt_width - 2)?;
            }
        }
        
        stdout.flush()?;
        Ok(())
    }
    
    /// Update only the input field line
    fn update_input_field(&self, stdout: &mut io::Stdout) -> io::Result<()> {
        // Move to the input line
        execute!(
            stdout,
            MoveTo(self.prompt_x, self.prompt_y + 4),
            SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
            SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
        )?;
        
        // Draw the line with input field
        write!(stdout, "│ ")?;
        
        // Calculate visible portion of input
        let input_width = self.prompt_width - 4;
        let (visible_input, cursor_offset) = if self.input.len() > input_width {
            // Scroll to keep cursor visible
            let start = if self.cursor_pos > input_width - 1 {
                self.cursor_pos - (input_width - 1)
            } else {
                0
            };
            let end = (start + input_width).min(self.input.len());
            (&self.input[start..end], self.cursor_pos - start)
        } else {
            (&self.input[..], self.cursor_pos)
        };
        
        // Input field with distinct background
        execute!(
            stdout,
            SetBackgroundColor(Color::Rgb { r: 20, g: 20, b: 25 }),
            SetForegroundColor(Color::Rgb { r: 220, g: 220, b: 230 }),
            Print(format!("{:<width$}", visible_input, width = input_width)),
            SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
            SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
        )?;
        
        write!(stdout, " │")?;
        
        // Position cursor
        execute!(
            stdout,
            MoveTo(
                self.prompt_x + 2 + cursor_offset as u16,
                self.prompt_y + 4
            ),
            ResetColor,
            Show
        )?;
        
        stdout.flush()?;
        Ok(())
    }
    
    /// Handle input and return the final path when Enter is pressed
    pub fn run(&mut self, stdout: &mut io::Stdout) -> io::Result<Option<PathBuf>> {
        // Draw the complete window initially
        self.draw_window(stdout)?;
        self.update_input_field(stdout)?;
        
        loop {
            if let Event::Key(key) = event::read()? {
                // Windows: ignore key release events
                #[cfg(target_os = "windows")]
                if key.kind == event::KeyEventKind::Release {
                    continue;
                }
                
                let mut input_changed = false;
                
                match key.code {
                    KeyCode::Enter => {
                        if !self.input.is_empty() {
                            return Ok(Some(PathBuf::from(&self.input)));
                        }
                    }
                    KeyCode::Esc => {
                        return Ok(None);
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.input.insert(self.cursor_pos, c);
                        self.cursor_pos += 1;
                        input_changed = true;
                    }
                    KeyCode::Backspace => {
                        if self.cursor_pos > 0 {
                            self.cursor_pos -= 1;
                            self.input.remove(self.cursor_pos);
                            input_changed = true;
                        }
                    }
                    KeyCode::Delete => {
                        if self.cursor_pos < self.input.len() {
                            self.input.remove(self.cursor_pos);
                            input_changed = true;
                        }
                    }
                    KeyCode::Left => {
                        if self.cursor_pos > 0 {
                            self.cursor_pos -= 1;
                            input_changed = true;
                        }
                    }
                    KeyCode::Right => {
                        if self.cursor_pos < self.input.len() {
                            self.cursor_pos += 1;
                            input_changed = true;
                        }
                    }
                    KeyCode::Home => {
                        if self.cursor_pos != 0 {
                            self.cursor_pos = 0;
                            input_changed = true;
                        }
                    }
                    KeyCode::End => {
                        if self.cursor_pos != self.input.len() {
                            self.cursor_pos = self.input.len();
                            input_changed = true;
                        }
                    }
                    // Ctrl+U - Clear line
                    KeyCode::Char('u') | KeyCode::Char('U') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if !self.input.is_empty() {
                            self.input.clear();
                            self.cursor_pos = 0;
                            input_changed = true;
                        }
                    }
                    // Ctrl+K - Delete to end of line
                    KeyCode::Char('k') | KeyCode::Char('K') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if self.cursor_pos < self.input.len() {
                            self.input.truncate(self.cursor_pos);
                            input_changed = true;
                        }
                    }
                    _ => {}
                }
                
                // Only update the input field if something changed
                if input_changed {
                    self.update_input_field(stdout)?;
                }
            }
        }
    }
}
