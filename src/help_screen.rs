use crossterm::{
    cursor,
    execute,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal,
};
use std::io::{self, Write};

pub struct HelpScreen;

impl HelpScreen {
    pub fn new() -> Self {
        HelpScreen
    }

    pub fn draw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let (width, height) = terminal::size()?;

        // Define consistent background color
        let bg_color = Color::Rgb { r: 40, g: 40, b: 40 };

        // Clear the screen and set background color
        execute!(
            writer,
            SetBackgroundColor(bg_color),
            terminal::Clear(terminal::ClearType::All)
        )?;

        // Define hotkey groups
        let sections = vec![
            ("FILE OPERATIONS", vec![
                ("Ctrl+S", "Save file"),
                ("Ctrl+O", "Open file"),
                ("Ctrl+Q", "Quit"),
            ]),
            ("EDITING", vec![
                ("Ctrl+Z", "Undo"),
                ("Ctrl+Y / Ctrl+Shift+Z", "Redo"),
                ("Ctrl+X", "Cut line/selection"),
                ("Ctrl+C", "Copy line/selection"),
                ("Ctrl+V", "Paste"),
                ("Ctrl+D", "Duplicate line"),
                ("Ctrl+/", "Toggle line comment"),
                ("Tab", "Indent / Autocomplete"),
                ("Shift+Tab", "Unindent"),
                ("Ctrl+]", "Indent selection"),
                ("Ctrl+[", "Unindent selection"),
            ]),
            ("NAVIGATION", vec![
                ("Arrow Keys", "Move cursor"),
                ("Home / End", "Start / End of line"),
                ("Ctrl+Home / Ctrl+End", "Start / End of file"),
                ("Page Up / Page Down", "Scroll page"),
                ("Ctrl+G", "Go to line"),
                ("Ctrl+P", "Matching bracket"),
            ]),
            ("SEARCH & REPLACE", vec![
                ("Ctrl+F", "Find / Find next"),
                ("Ctrl+Shift+F", "Find previous"),
                ("Ctrl+H", "Replace current and find next"),
                ("Ctrl+Shift+H", "Replace all"),
            ]),
            ("SELECTION", vec![
                ("Shift+Arrow Keys", "Select text"),
                ("Ctrl+A", "Select all"),
                ("Mouse Click+Drag", "Select text"),
                ("Esc", "Clear selection"),
            ]),
            ("PYTHON REPL MODE", vec![
                ("Ctrl+E", "Execute cell/code"),
                ("Ctrl+K", "Select Python kernel"),
                ("Ctrl+N", "Clear output pane"),
                ("Esc", "Toggle output pane focus"),
            ]),
            ("LANGUAGE", vec![
                ("Ctrl+Y", "Select language"),
            ]),
            ("OTHER", vec![
                ("F1", "Toggle this help screen"),
                ("Ctrl+L", "Refresh screen"),
            ]),
        ];

        // Calculate layout
        let title = "SAGE - Keyboard Shortcuts";
        let footer = "Press F1 or Esc to close";

        let mut current_row = 2u16;

        // Draw title
        let title_col = (width.saturating_sub(title.len() as u16)) / 2;
        execute!(
            writer,
            cursor::MoveTo(title_col, current_row),
            SetBackgroundColor(bg_color),
            SetForegroundColor(Color::Cyan),
            Print(title),
            SetForegroundColor(Color::Reset)
        )?;

        current_row += 2;

        // Draw each section
        for (section_name, hotkeys) in &sections {
            if current_row + hotkeys.len() as u16 + 2 >= height - 2 {
                // Not enough space, skip remaining sections
                break;
            }

            // Draw section header
            execute!(
                writer,
                cursor::MoveTo(4, current_row),
                SetBackgroundColor(bg_color),
                SetForegroundColor(Color::Yellow),
                Print(section_name),
                SetForegroundColor(Color::Reset)
            )?;
            current_row += 1;

            // Draw hotkeys in this section
            for (key, description) in hotkeys {
                execute!(
                    writer,
                    cursor::MoveTo(6, current_row),
                    SetBackgroundColor(bg_color),
                    SetForegroundColor(Color::Green),
                    Print(format!("{:30}", key)),
                    SetForegroundColor(Color::White),
                    Print(description),
                    SetForegroundColor(Color::Reset)
                )?;
                current_row += 1;
            }

            current_row += 1; // Extra space between sections
        }

        // Draw footer
        let footer_row = height.saturating_sub(2);
        let footer_col = (width.saturating_sub(footer.len() as u16)) / 2;
        execute!(
            writer,
            cursor::MoveTo(footer_col, footer_row),
            SetBackgroundColor(bg_color),
            SetForegroundColor(Color::Cyan),
            Print(footer),
            SetForegroundColor(Color::Reset),
            cursor::Hide
        )?;

        writer.flush()?;
        Ok(())
    }
}
