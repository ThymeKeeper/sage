use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor, ResetColor},
    terminal,
};
use std::io::{self, Write};

pub struct ExitPrompt {
    selected_option: ExitOption,
}

#[derive(Clone, Copy, PartialEq)]
pub enum ExitOption {
    Save,
    ExitWithoutSaving,
    Cancel,
}

impl ExitPrompt {
    pub fn new() -> Self {
        Self {
            selected_option: ExitOption::Save,
        }
    }
    
    /// Draw only the options line (for updates without flickering)
    fn draw_options_line(&self, stdout: &mut io::Stdout, prompt_x: usize, prompt_y: usize, prompt_width: usize) -> io::Result<()> {
        let y = 5; // Options are on line 5
        
        execute!(
            stdout,
            MoveTo(prompt_x as u16, (prompt_y + y) as u16),
            SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
            SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
        )?;
        
        write!(stdout, "│ ")?;
        
        // Calculate button positions for centering
        let buttons_width = 13 + 2 + 12 + 2 + 8; // "Save & Exit" + spaces + "Don't Save" + spaces + "Cancel"
        let available_width = prompt_width - 2; // 2 for borders
        let padding_left = (available_width - buttons_width) / 2;
        
        // Add left padding
        for _ in 0..padding_left {
            write!(stdout, " ")?;
        }
        
        // Save and Exit option
        let save_selected = self.selected_option == ExitOption::Save;
        execute!(
            stdout,
            SetBackgroundColor(if save_selected {
                Color::Rgb { r: 60, g: 120, b: 60 }
            } else {
                Color::Rgb { r: 40, g: 40, b: 45 }
            }),
            SetForegroundColor(if save_selected {
                Color::White
            } else {
                Color::Rgb { r: 150, g: 150, b: 155 }
            }),
            Print(" Save & Exit "),
            SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
        )?;
        
        write!(stdout, "  ")?;
        
        // Exit without saving option
        let exit_selected = self.selected_option == ExitOption::ExitWithoutSaving;
        execute!(
            stdout,
            SetBackgroundColor(if exit_selected {
                Color::Rgb { r: 120, g: 60, b: 60 }
            } else {
                Color::Rgb { r: 40, g: 40, b: 45 }
            }),
            SetForegroundColor(if exit_selected {
                Color::White
            } else {
                Color::Rgb { r: 150, g: 150, b: 155 }
            }),
            Print(" Don't Save "),
            SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
        )?;
        
        write!(stdout, "  ")?;
        
        // Cancel option
        let cancel_selected = self.selected_option == ExitOption::Cancel;
        execute!(
            stdout,
            SetBackgroundColor(if cancel_selected {
                Color::Rgb { r: 60, g: 60, b: 120 }
            } else {
                Color::Rgb { r: 40, g: 40, b: 45 }
            }),
            SetForegroundColor(if cancel_selected {
                Color::White
            } else {
                Color::Rgb { r: 150, g: 150, b: 155 }
            }),
            Print(" Cancel"),  // Fixed: removed trailing space
            SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
            SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
        )?;
        
        // Fill rest of line to border
        let used = 1 + padding_left + buttons_width; // 1 for initial space after border
        let remaining = prompt_width - 1 - used; // -1 for closing border
        for _ in 0..remaining {
            write!(stdout, " ")?;
        }
        write!(stdout, "│")?;
        
        execute!(
            stdout,
            ResetColor,
            Hide
        )?;
        
        stdout.flush()?;
        Ok(())
    }
    
    /// Draw the exit prompt as a floating window
    pub fn draw(&self, stdout: &mut io::Stdout, filename: &str) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        
        // Calculate prompt dimensions and position
        let prompt_width = (width as usize * 2 / 3).min(60).max(40);
        let prompt_height = 7;  // Reduced from 8 to 7 (no help text)
        let prompt_x = (width as usize - prompt_width) / 2;
        let prompt_y = (height as usize - prompt_height) / 2;
        
        // Draw shadow effect
        for y in 1..prompt_height {
            execute!(
                stdout,
                MoveTo((prompt_x + 2) as u16, (prompt_y + y) as u16),
                SetBackgroundColor(Color::Black),
                Print(" ".repeat(prompt_width))
            )?;
        }
        
        // Draw main window
        for y in 0..prompt_height {
            execute!(
                stdout,
                MoveTo(prompt_x as u16, (prompt_y + y) as u16),
                SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
            )?;
            
            if y == 0 {
                // Top border with rounded corners
                write!(stdout, "╭")?;
                for _ in 1..prompt_width - 1 {
                    write!(stdout, "─")?;
                }
                write!(stdout, "╮")?;
            } else if y == prompt_height - 1 {
                // Bottom border
                write!(stdout, "╰")?;
                for _ in 1..prompt_width - 1 {
                    write!(stdout, "─")?;
                }
                write!(stdout, "╯")?;
            } else if y == 2 {
                // Title line
                write!(stdout, "│ ")?;
                execute!(
                    stdout,
                    SetAttribute(Attribute::Bold),
                    SetForegroundColor(Color::White),
                    Print("Save changes?"),
                    SetAttribute(Attribute::Reset),
                    SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                    SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                )?;
                let title_padding = prompt_width - 2 - 14; // 2 for border+space, 14 for "Save changes?"
                for _ in 0..title_padding {
                    write!(stdout, " ")?;
                }
                write!(stdout, "│")?;
            } else if y == 3 {
                // File info line
                write!(stdout, "│ ")?;
                let info = format!("{} has unsaved changes", filename);
                let truncated_info = if info.len() > prompt_width - 4 {
                    format!("{}...", &info[..prompt_width - 7])
                } else {
                    info.clone()
                };
                execute!(
                    stdout,
                    SetForegroundColor(Color::Rgb { r: 180, g: 180, b: 185 }),
                    Print(&truncated_info),
                    SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                )?;
                let info_padding = prompt_width - 3 - truncated_info.len(); // 2 for "│ ", 1 for closing "│"
                for _ in 0..info_padding {
                    write!(stdout, " ")?;
                }
                write!(stdout, "│")?;
            } else if y == 5 {
                // Options line - delegate to separate function
                // We still need to draw it here for the initial render
                write!(stdout, "│ ")?;
                
                // Calculate button positions for centering
                let buttons_width = 13 + 2 + 12 + 2 + 8; // "Save & Exit" + spaces + "Don't Save" + spaces + "Cancel"
                let available_width = prompt_width - 2; // 2 for borders
                let padding_left = (available_width - buttons_width) / 2;
                
                // Add left padding
                for _ in 0..padding_left {
                    write!(stdout, " ")?;
                }
                
                // Save and Exit option
                let save_selected = self.selected_option == ExitOption::Save;
                execute!(
                    stdout,
                    SetBackgroundColor(if save_selected {
                        Color::Rgb { r: 60, g: 120, b: 60 }
                    } else {
                        Color::Rgb { r: 40, g: 40, b: 45 }
                    }),
                    SetForegroundColor(if save_selected {
                        Color::White
                    } else {
                        Color::Rgb { r: 150, g: 150, b: 155 }
                    }),
                    Print(" Save & Exit "),
                    SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                )?;
                
                write!(stdout, "  ")?;
                
                // Exit without saving option
                let exit_selected = self.selected_option == ExitOption::ExitWithoutSaving;
                execute!(
                    stdout,
                    SetBackgroundColor(if exit_selected {
                        Color::Rgb { r: 120, g: 60, b: 60 }
                    } else {
                        Color::Rgb { r: 40, g: 40, b: 45 }
                    }),
                    SetForegroundColor(if exit_selected {
                        Color::White
                    } else {
                        Color::Rgb { r: 150, g: 150, b: 155 }
                    }),
                    Print(" Don't Save "),
                    SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                )?;
                
                write!(stdout, "  ")?;
                
                // Cancel option
                let cancel_selected = self.selected_option == ExitOption::Cancel;
                execute!(
                    stdout,
                    SetBackgroundColor(if cancel_selected {
                        Color::Rgb { r: 60, g: 60, b: 120 }
                    } else {
                        Color::Rgb { r: 40, g: 40, b: 45 }
                    }),
                    SetForegroundColor(if cancel_selected {
                        Color::White
                    } else {
                        Color::Rgb { r: 150, g: 150, b: 155 }
                    }),
                    Print(" Cancel"),  // Fixed: removed trailing space
                    SetBackgroundColor(Color::Rgb { r: 40, g: 40, b: 45 }),
                    SetForegroundColor(Color::Rgb { r: 200, g: 200, b: 205 }),
                )?;
                
                // Fill rest of line to border
                let used = 1 + padding_left + buttons_width; // 1 for initial space after border
                let remaining = prompt_width - 1 - used; // -1 for closing border
                for _ in 0..remaining {
                    write!(stdout, " ")?;
                }
                write!(stdout, "│")?;
            } else {
                // Empty lines
                write!(stdout, "│")?;
                for _ in 1..prompt_width - 1 {
                    write!(stdout, " ")?;
                }
                write!(stdout, "│")?;
            }
        }
        
        execute!(
            stdout,
            ResetColor,
            Hide
        )?;
        
        stdout.flush()?;
        Ok(())
    }
    
    /// Handle keyboard input and return the selected option
    pub fn run(&mut self, stdout: &mut io::Stdout, filename: &str) -> io::Result<ExitOption> {
        // Draw the full window once
        self.draw(stdout, filename)?;
        
        // Store dimensions for partial redraws
        let (width, height) = terminal::size()?;
        let prompt_width = (width as usize * 2 / 3).min(60).max(40);
        let prompt_x = (width as usize - prompt_width) / 2;
        let prompt_y = (height as usize - 7) / 2;  // prompt_height = 7
        
        loop {
            if let Event::Key(key) = event::read()? {
                // Windows: ignore key release events
                #[cfg(target_os = "windows")]
                if key.kind == event::KeyEventKind::Release {
                    continue;
                }
                
                let old_option = self.selected_option;
                
                match key.code {
                    KeyCode::Enter => {
                        return Ok(self.selected_option);
                    }
                    KeyCode::Esc => {
                        return Ok(ExitOption::Cancel);
                    }
                    KeyCode::Left => {
                        self.selected_option = match self.selected_option {
                            ExitOption::Save => ExitOption::Cancel,
                            ExitOption::ExitWithoutSaving => ExitOption::Save,
                            ExitOption::Cancel => ExitOption::ExitWithoutSaving,
                        };
                    }
                    KeyCode::Right | KeyCode::Tab => {
                        self.selected_option = match self.selected_option {
                            ExitOption::Save => ExitOption::ExitWithoutSaving,
                            ExitOption::ExitWithoutSaving => ExitOption::Cancel,
                            ExitOption::Cancel => ExitOption::Save,
                        };
                    }
                    // Number shortcuts
                    KeyCode::Char('1') => self.selected_option = ExitOption::Save,
                    KeyCode::Char('2') => self.selected_option = ExitOption::ExitWithoutSaving,
                    KeyCode::Char('3') => self.selected_option = ExitOption::Cancel,
                    // Letter shortcuts
                    KeyCode::Char('s') | KeyCode::Char('S') => self.selected_option = ExitOption::Save,
                    KeyCode::Char('d') | KeyCode::Char('D') => self.selected_option = ExitOption::ExitWithoutSaving,
                    KeyCode::Char('c') | KeyCode::Char('C') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.selected_option = ExitOption::Cancel;
                    }
                    _ => {}
                }
                
                // Only redraw the options line if the selection changed
                if old_option != self.selected_option {
                    self.draw_options_line(stdout, prompt_x, prompt_y, prompt_width)?;
                }
            }
        }
    }
}
