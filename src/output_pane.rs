use crossterm::{
    cursor,
    execute,
    terminal::{Clear, ClearType},
    style::{Color, Print, ResetColor, SetForegroundColor},
};
use std::io::{self, Write};

#[derive(Debug, Clone)]
pub struct OutputEntry {
    pub execution_count: usize,
    pub cell_line: usize,
    pub output: String,
    pub is_error: bool,
}

pub struct OutputPane {
    outputs: Vec<OutputEntry>,
    scroll_offset: usize,
}

impl OutputPane {
    pub fn new() -> Self {
        OutputPane {
            outputs: Vec::new(),
            scroll_offset: 0,
        }
    }

    pub fn add_output(&mut self, entry: OutputEntry) {
        self.outputs.push(entry);
        // Keep scroll at top to show all entries (scroll_offset = 0 means start from first entry)
        self.scroll_offset = 0;
    }

    pub fn clear(&mut self) {
        self.outputs.clear();
        self.scroll_offset = 0;
    }

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        if self.scroll_offset + 1 < self.outputs.len() {
            self.scroll_offset += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    pub fn draw<W: Write>(&self, writer: &mut W, start_row: u16, height: usize, width: u16) -> io::Result<()> {
        // Clear all rows in the output pane area first (to handle resizing)
        for row in start_row..=(start_row + height as u16) {
            execute!(
                writer,
                cursor::MoveTo(0, row),
                Clear(ClearType::CurrentLine)
            )?;
        }

        // Draw separator line
        execute!(
            writer,
            cursor::MoveTo(0, start_row),
            SetForegroundColor(Color::DarkGrey),
            Print("─".repeat(width as usize)),
            ResetColor
        )?;

        // Draw title
        let title = if self.outputs.is_empty() {
            " Output (Ctrl+O to toggle, Ctrl+Shift+O to clear) "
        } else {
            &format!(" Output [{} executions] (Ctrl+O to toggle, Ctrl+Shift+O to clear) ", self.outputs.len())
        };
        execute!(
            writer,
            cursor::MoveTo(2, start_row),
            SetForegroundColor(Color::Cyan),
            Print(title),
            ResetColor
        )?;

        if self.outputs.is_empty() {
            // Show hint
            execute!(
                writer,
                cursor::MoveTo(2, start_row + 1),
                SetForegroundColor(Color::DarkGrey),
                Print("No output yet. Execute a cell with Ctrl+E or Ctrl+Enter"),
                ResetColor
            )?;
            return Ok(());
        }

        // Draw outputs
        let max_visible = height.saturating_sub(1); // Reserve 1 line for separator
        let start_idx = self.scroll_offset;
        let end_idx = (start_idx + max_visible).min(self.outputs.len());

        for (i, entry) in self.outputs[start_idx..end_idx].iter().enumerate() {
            let row = start_row + 1 + i as u16;

            // Format: [1] (line 5): result
            let prefix = format!("[{}] (line {}): ", entry.execution_count, entry.cell_line);

            // Truncate output if too long
            let max_output_len = (width as usize).saturating_sub(prefix.len() + 2);
            let output_text = if entry.output.len() > max_output_len {
                format!("{}...", &entry.output[..max_output_len.saturating_sub(3)])
            } else {
                entry.output.clone()
            };

            // Position cursor for drawing
            execute!(writer, cursor::MoveTo(2, row))?;

            if entry.is_error {
                // Red for errors
                execute!(
                    writer,
                    SetForegroundColor(Color::Red),
                    Print(&prefix),
                    Print(&output_text),
                    ResetColor
                )?;
            } else {
                // Green for execution count, default color for output
                execute!(
                    writer,
                    SetForegroundColor(Color::Green),
                    Print(&format!("[{}]", entry.execution_count)),
                    ResetColor,
                    Print(&format!(" (line {}): ", entry.cell_line)),
                    Print(&output_text)
                )?;
            }
        }

        // Show scroll indicator if needed
        if self.outputs.len() > max_visible {
            let scroll_info = format!(" {}-{}/{} ",
                start_idx + 1,
                end_idx,
                self.outputs.len()
            );
            execute!(
                writer,
                cursor::MoveTo(width.saturating_sub(scroll_info.len() as u16 + 2), start_row),
                SetForegroundColor(Color::DarkGrey),
                Print(scroll_info),
                ResetColor
            )?;
        }

        writer.flush()?;
        Ok(())
    }
}
