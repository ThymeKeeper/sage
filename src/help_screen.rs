use crossterm::{
    cursor,
    execute,
    style::{Color, Print, SetBackgroundColor, SetForegroundColor},
    terminal,
};
use std::io::{self, Write};

pub struct HelpScreen {
    scroll_offset: usize,
}

impl HelpScreen {
    pub fn new() -> Self {
        HelpScreen {
            scroll_offset: 0,
        }
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset += 1;
    }

    pub fn draw<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let (width, height) = terminal::size()?;

        // Match output pane background: ANSI 256 color 235, one shade brighter than editor bg (234)
        let bg_color = Color::Rgb { r: 33, g: 33, b: 33 };

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
                ("Ctrl+Shift+S", "Save as"),
                ("Ctrl+Q", "Quit"),
            ]),
            ("EDITING", vec![
                ("Ctrl+Z", "Undo"),
                ("Ctrl+Shift+Z", "Redo"),
                ("Ctrl+X", "Cut line/selection"),
                ("Ctrl+C", "Copy line/selection"),
                ("Ctrl+V", "Paste"),
                ("Tab", "Indent selection / Autocomplete"),
                ("Shift+Tab", "Unindent selection"),
                ("Ctrl+Alt+Up/Down", "Move line(s) up/down"),
            ]),
            ("NAVIGATION", vec![
                ("Arrow Keys", "Move cursor"),
                ("Ctrl+Arrow Keys", "Move by word/paragraph"),
                ("Home / End", "Start / End of line"),
                ("Page Up / Page Down", "Scroll page"),
            ]),
            ("SEARCH & REPLACE", vec![
                ("Ctrl+F", "Find / Find next"),
                ("Ctrl+Shift+F", "Find previous"),
                ("Ctrl+H", "Replace current and find next"),
                ("Ctrl+Shift+H", "Replace all"),
            ]),
            ("SELECTION", vec![
                ("Shift+Arrow Keys", "Select text"),
                ("Ctrl+Shift+Arrow Keys", "Select by word/paragraph"),
                ("Ctrl+A", "Select all"),
                ("Mouse Click+Drag", "Select text"),
            ]),
            ("PYTHON REPL MODE", vec![
                ("Ctrl+E / Ctrl+Enter", "Execute cell/code"),
                ("Ctrl+Backspace", "Cancel execution (resets kernel)"),
                ("Ctrl+K", "Select kernel"),
                ("Ctrl+L", "Clear output pane"),
                ("Ctrl+O", "Toggle output pane visibility"),
                ("Esc", "Toggle output pane focus"),
                ("Alt+Up / Alt+Down", "Resize output pane"),
            ]),
            ("SNOWFLAKE REPL MODE", vec![
                ("Ctrl+E / Ctrl+Enter", "Execute statement (cells split on semicolons)"),
                ("Ctrl+Backspace", "Cancel query (server-side abort, session preserved)"),
                ("Ctrl+K", "Select kernel"),
                ("F9", "Export full result to CSV (Downloads folder)"),
                ("Ctrl+D", "Open results in a new sage session (first 10k rows)"),
                ("Ctrl+L", "Clear output pane"),
                ("Ctrl+O", "Toggle output pane visibility"),
                ("Esc", "Toggle output pane focus"),
                ("Alt+Up / Alt+Down", "Resize output pane"),
            ]),
            ("TABLE OUTPUT (mouse)", vec![
                ("Click on cell", "Select that cell"),
                ("Click + drag in table", "Select a rectangular cell range"),
                ("Ctrl+C (multi-cell)", "Copy as TSV with column headers"),
                ("Ctrl+Up / Ctrl+Down", "Jump between cell outputs (pane focused)"),
            ]),
            ("LANGUAGE", vec![
                ("Ctrl+Y", "Select language"),
            ]),
            ("SNIPPETS", vec![
                ("Ctrl+J", "Open snippet library"),
            ]),
            ("OTHER", vec![
                ("F1", "Toggle this help screen"),
            ]),
        ];

        // Calculate layout
        let title = "SAGE - Keyboard Shortcuts";
        let footer = "Use Up/Down arrows to scroll | Press F1 or Esc to close";

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

        // Track lines to skip based on scroll offset
        let mut lines_to_skip = self.scroll_offset;

        // Draw each section
        for (section_name, hotkeys) in &sections {
            // Skip section header if needed
            if lines_to_skip > 0 {
                lines_to_skip -= 1;
            } else {
                if current_row >= height - 2 {
                    break; // No more space
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
            }

            // Draw hotkeys in this section
            for (key, description) in hotkeys {
                if lines_to_skip > 0 {
                    lines_to_skip -= 1;
                } else {
                    if current_row >= height - 2 {
                        break; // No more space
                    }
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
            }

            // Extra space between sections
            if lines_to_skip > 0 {
                lines_to_skip -= 1;
            } else {
                current_row += 1;
            }
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
