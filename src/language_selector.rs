use crate::syntax::Language;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal,
};
use std::io::{self, Write};

pub struct LanguageSelector {
    languages: Vec<(Language, &'static str)>,
    selected_index: usize,
}

impl LanguageSelector {
    pub fn new(current_language: &Language) -> Self {
        let languages = vec![
            (Language::PlainText, "Plain Text"),
            (Language::Python, "Python"),
            (Language::Sql, "SQL"),
            (Language::Rust, "Rust"),
            (Language::R, "R"),
            (Language::Yaml, "YAML"),
            (Language::Markdown, "Markdown"),
            (Language::Json, "JSON"),
            (Language::Shell, "Shell"),
            (Language::Toml, "TOML"),
        ];

        // Find the index of the current language
        let selected_index = languages
            .iter()
            .position(|(lang, _)| lang == current_language)
            .unwrap_or(0);

        LanguageSelector {
            languages,
            selected_index,
        }
    }

    pub fn run<W: Write>(&mut self, writer: &mut W) -> io::Result<Option<Language>> {
        loop {
            self.draw(writer)?;

            // Wait for user input
            match event::read()? {
                Event::Key(key) => {
                    // Ignore key release events
                    #[cfg(target_os = "windows")]
                    if key.kind == event::KeyEventKind::Release {
                        continue;
                    }

                    match key.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            if self.selected_index > 0 {
                                self.selected_index -= 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            if self.selected_index < self.languages.len() - 1 {
                                self.selected_index += 1;
                            }
                        }
                        KeyCode::Enter => {
                            let (language, _) = self.languages[self.selected_index];
                            return Ok(Some(language));
                        }
                        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            return Ok(None);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    fn draw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        let box_width = width.min(50);
        let list_height = self.languages.len();
        let box_height = list_height + 4;

        // Calculate centering
        let start_col = (width.saturating_sub(box_width)) / 2;
        let start_row = (height.saturating_sub(box_height as u16)) / 2;

        // Draw top border
        execute!(
            writer,
            cursor::MoveTo(start_col, start_row),
            SetForegroundColor(Color::Cyan),
            Print("┌"),
            Print("─".repeat((box_width - 2) as usize)),
            Print("┐"),
            ResetColor
        )?;

        // Title
        execute!(
            writer,
            cursor::MoveTo(start_col, start_row + 1),
            SetForegroundColor(Color::Cyan),
            Print("│"),
            ResetColor,
            Print(" Select Language"),
            cursor::MoveTo(start_col + box_width - 1, start_row + 1),
            SetForegroundColor(Color::Cyan),
            Print("│"),
            ResetColor
        )?;

        // Separator
        execute!(
            writer,
            cursor::MoveTo(start_col, start_row + 2),
            SetForegroundColor(Color::Cyan),
            Print("├"),
            Print("─".repeat((box_width - 2) as usize)),
            Print("┤"),
            ResetColor
        )?;

        // Language list
        for (i, (_, display_name)) in self.languages.iter().enumerate() {
            let row = start_row + 3 + i as u16;
            execute!(writer, cursor::MoveTo(start_col, row))?;

            if i == self.selected_index {
                execute!(
                    writer,
                    SetForegroundColor(Color::Cyan),
                    Print("│"),
                    ResetColor,
                    SetBackgroundColor(Color::DarkGrey),
                    SetForegroundColor(Color::White),
                    Print(format!(" > {:<width$}", display_name, width = (box_width - 4) as usize)),
                    ResetColor,
                    SetForegroundColor(Color::Cyan),
                    Print("│"),
                    ResetColor
                )?;
            } else {
                execute!(
                    writer,
                    SetForegroundColor(Color::Cyan),
                    Print("│"),
                    ResetColor,
                    Print(format!("   {:<width$}", display_name, width = (box_width - 4) as usize)),
                    SetForegroundColor(Color::Cyan),
                    Print("│"),
                    ResetColor
                )?;
            }
        }

        // Bottom border
        let bottom_row = start_row + 3 + list_height as u16;
        execute!(
            writer,
            cursor::MoveTo(start_col, bottom_row),
            SetForegroundColor(Color::Cyan),
            Print("└"),
            Print("─".repeat((box_width - 2) as usize)),
            Print("┘"),
            ResetColor
        )?;

        // Instructions
        let instructions = "↑↓/jk: Navigate  Enter: Select  Esc: Cancel";
        execute!(
            writer,
            cursor::MoveTo(start_col, bottom_row + 1),
            SetForegroundColor(Color::DarkGrey),
            Print(instructions),
            ResetColor
        )?;

        writer.flush()?;
        Ok(())
    }
}
