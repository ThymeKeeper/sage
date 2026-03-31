use crate::config::Snippet;
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal,
};
use std::io::{self, Write};

pub struct SnippetPicker {
    snippets: Vec<Snippet>,
    selected_index: usize,
}

impl SnippetPicker {
    pub fn new(snippets: Vec<Snippet>) -> Self {
        SnippetPicker {
            snippets,
            selected_index: 0,
        }
    }

    pub fn run<W: Write>(&mut self, writer: &mut W) -> io::Result<Option<String>> {
        if self.snippets.is_empty() {
            self.show_empty_message(writer)?;
            return Ok(None);
        }

        loop {
            self.draw(writer)?;

            match event::read()? {
                Event::Key(key) => {
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
                            if self.selected_index < self.snippets.len() - 1 {
                                self.selected_index += 1;
                            }
                        }
                        KeyCode::Enter => {
                            let text = self.snippets[self.selected_index].text.clone();
                            return Ok(Some(text));
                        }
                        // Number keys 1-9 for quick selection
                        KeyCode::Char(c @ '1'..='9') => {
                            let idx = (c as usize) - ('1' as usize);
                            if idx < self.snippets.len() {
                                let text = self.snippets[idx].text.clone();
                                return Ok(Some(text));
                            }
                        }
                        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                            return Ok(None);
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
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

        // Size box to fit config path if needed
        let path_str = crate::config::Config::config_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "~/.config/sage/config.toml".to_string());
        let min_width = (path_str.len() + 5).max(70);
        let box_width = width.min(min_width as u16);

        let max_list_height = (height as usize).saturating_sub(10);
        let list_height = self.snippets.len().min(max_list_height);
        let box_height = list_height + 6; // +2 extra for config path + separator

        let start_col = (width.saturating_sub(box_width)) / 2;
        let start_row = (height.saturating_sub(box_height as u16)) / 2;
        let inner = (box_width as usize).saturating_sub(3);

        // Scroll offset
        let scroll_offset = if self.selected_index >= list_height {
            self.selected_index - list_height + 1
        } else {
            0
        };

        // Top border
        execute!(
            writer,
            cursor::MoveTo(start_col, start_row),
            SetForegroundColor(Color::Cyan),
            Print("\u{250c}"),
            Print("\u{2500}".repeat((box_width - 2) as usize)),
            Print("\u{2510}"),
            ResetColor
        )?;

        // Title
        execute!(
            writer,
            cursor::MoveTo(start_col, start_row + 1),
            SetForegroundColor(Color::Cyan),
            Print("\u{2502}"),
            ResetColor,
            Print(" Snippet Library"),
            cursor::MoveTo(start_col + box_width - 1, start_row + 1),
            SetForegroundColor(Color::Cyan),
            Print("\u{2502}"),
            ResetColor
        )?;

        // Config path subtitle
        execute!(
            writer,
            cursor::MoveTo(start_col, start_row + 2),
            SetForegroundColor(Color::Cyan),
            Print("\u{2502}"),
            ResetColor,
            SetForegroundColor(Color::DarkGrey),
            Print(format!(" {:<w$}", path_str, w = inner)),
            ResetColor,
            SetForegroundColor(Color::Cyan),
            Print("\u{2502}"),
            ResetColor
        )?;

        // Separator
        execute!(
            writer,
            cursor::MoveTo(start_col, start_row + 3),
            SetForegroundColor(Color::Cyan),
            Print("\u{251c}"),
            Print("\u{2500}".repeat((box_width - 2) as usize)),
            Print("\u{2524}"),
            ResetColor
        )?;

        // Snippet list
        for i in 0..list_height {
            let snippet_idx = i + scroll_offset;
            if snippet_idx >= self.snippets.len() {
                break;
            }

            let snippet = &self.snippets[snippet_idx];
            let row = start_row + 4 + i as u16;
            execute!(writer, cursor::MoveTo(start_col, row))?;

            // Format: "1. name" with number for first 9
            let number_prefix = if snippet_idx < 9 {
                format!("{}. ", snippet_idx + 1)
            } else {
                "   ".to_string()
            };

            let max_name_len = (box_width as usize).saturating_sub(7 + number_prefix.len());
            let display_name = if snippet.name.len() > max_name_len {
                format!("{}...", &snippet.name[..max_name_len.saturating_sub(3)])
            } else {
                snippet.name.clone()
            };

            let entry = format!("{}{}", number_prefix, display_name);
            let padded_width = (box_width as usize).saturating_sub(4);

            if snippet_idx == self.selected_index {
                execute!(
                    writer,
                    SetForegroundColor(Color::Cyan),
                    Print("\u{2502}"),
                    ResetColor,
                    SetBackgroundColor(Color::DarkGrey),
                    SetForegroundColor(Color::White),
                    Print(format!(" > {:<width$}", entry, width = padded_width)),
                    ResetColor,
                    SetForegroundColor(Color::Cyan),
                    Print("\u{2502}"),
                    ResetColor
                )?;
            } else {
                execute!(
                    writer,
                    SetForegroundColor(Color::Cyan),
                    Print("\u{2502}"),
                    ResetColor,
                    Print(format!("   {:<width$}", entry, width = padded_width)),
                    SetForegroundColor(Color::Cyan),
                    Print("\u{2502}"),
                    ResetColor
                )?;
            }
        }

        // Bottom border
        let bottom_row = start_row + 4 + list_height as u16;
        execute!(
            writer,
            cursor::MoveTo(start_col, bottom_row),
            SetForegroundColor(Color::Cyan),
            Print("\u{2514}"),
            Print("\u{2500}".repeat((box_width - 2) as usize)),
            Print("\u{2518}"),
            ResetColor
        )?;

        // Instructions
        let instructions = if self.snippets.len() > list_height {
            format!(
                "\u{2191}\u{2193}/jk: Navigate  1-9: Quick pick  Enter: Insert  Esc: Cancel  [{}/{}]",
                self.selected_index + 1,
                self.snippets.len()
            )
        } else {
            "\u{2191}\u{2193}/jk: Navigate  1-9: Quick pick  Enter: Insert  Esc: Cancel".to_string()
        };
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

    fn show_empty_message<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let (width, height) = terminal::size()?;

        // Size the box to fit the config path without truncation
        let path_str = crate::config::Config::config_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "~/.config/sage/config.toml".to_string());

        // +5 accounts for border chars + padding: "│  " + content + " │"
        let min_width = (path_str.len() + 5).max(40);
        let box_width = width.min(min_width as u16);
        let inner = (box_width as usize).saturating_sub(3); // usable chars after "│ "

        let start_col = (width.saturating_sub(box_width)) / 2;
        let start_row = height / 2 - 2;

        // Helper: draw a line inside the box
        macro_rules! box_line {
            ($writer:expr, $col:expr, $row:expr, $fg:expr, $text:expr) => {
                execute!(
                    $writer,
                    cursor::MoveTo($col, $row),
                    SetForegroundColor(Color::Yellow),
                    Print("\u{2502}"),
                    ResetColor,
                    SetForegroundColor($fg),
                    Print(format!(" {:<w$}", $text, w = inner)),
                    ResetColor,
                    SetForegroundColor(Color::Yellow),
                    Print("\u{2502}"),
                    ResetColor
                )?;
            };
        }

        // Top border
        execute!(
            writer,
            cursor::MoveTo(start_col, start_row),
            SetForegroundColor(Color::Yellow),
            Print("\u{250c}"),
            Print("\u{2500}".repeat((box_width - 2) as usize)),
            Print("\u{2510}"),
            ResetColor
        )?;

        box_line!(writer, start_col, start_row + 1, Color::Yellow, "No snippets configured!");
        box_line!(writer, start_col, start_row + 2, Color::White, "Add snippets to:");
        box_line!(writer, start_col, start_row + 3, Color::White, &path_str);
        box_line!(writer, start_col, start_row + 4, Color::Yellow, "");
        box_line!(writer, start_col, start_row + 5, Color::DarkGrey, "Example:");

        let examples = [
            "[[snippets]]",
            "name = \"pandas read csv\"",
            "text = \"pd.read_csv('file.csv')\"",
        ];

        let mut row = start_row + 6;
        for line in &examples {
            execute!(
                writer,
                cursor::MoveTo(start_col, row),
                SetForegroundColor(Color::Yellow),
                Print("\u{2502}"),
                ResetColor,
                SetForegroundColor(Color::DarkGrey),
                Print(format!("   {:<width$}", line, width = (box_width as usize).saturating_sub(5))),
                ResetColor,
                SetForegroundColor(Color::Yellow),
                Print("\u{2502}"),
                ResetColor
            )?;
            row += 1;
        }

        // Bottom border
        execute!(
            writer,
            cursor::MoveTo(start_col, row),
            SetForegroundColor(Color::Yellow),
            Print("\u{2514}"),
            Print("\u{2500}".repeat((box_width - 2) as usize)),
            Print("\u{2518}"),
            ResetColor
        )?;

        // Instructions
        execute!(
            writer,
            cursor::MoveTo(start_col, row + 1),
            SetForegroundColor(Color::DarkGrey),
            Print("Press any key to close"),
            ResetColor
        )?;

        writer.flush()?;

        // Wait for key press
        loop {
            if let Event::Key(key) = event::read()? {
                #[cfg(target_os = "windows")]
                if key.kind == event::KeyEventKind::Release {
                    continue;
                }
                break;
            }
        }

        Ok(())
    }
}
