use crate::kernel::{discover_kernels, KernelInfo};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal::{self, Clear, ClearType},
};
use std::io::{self, Write};

pub struct KernelSelector {
    pub kernels: Vec<KernelInfo>,
    selected_index: usize,
}

impl KernelSelector {
    pub fn new() -> Self {
        let kernels = discover_kernels();
        KernelSelector {
            kernels,
            selected_index: 0,
        }
    }

    pub fn run<W: Write>(&mut self, writer: &mut W) -> io::Result<Option<KernelInfo>> {
        if self.kernels.is_empty() {
            self.show_error(writer, "No Python kernels found! Install Python first.")?;
            return Ok(None);
        }

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
                            if self.selected_index < self.kernels.len() - 1 {
                                self.selected_index += 1;
                            }
                        }
                        KeyCode::Enter => {
                            use std::fs::OpenOptions;
                            use std::io::Write;
                            if let Ok(mut log) = OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
                                let _ = writeln!(log, "Enter pressed in kernel selector");
                                let _ = log.flush();
                            }
                            let kernel = self.kernels[self.selected_index].clone();
                            if let Ok(mut log) = OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
                                let _ = writeln!(log, "Returning kernel: {}", kernel.display_name);
                                let _ = log.flush();
                            }
                            return Ok(Some(kernel));
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
        let box_width = width.min(100);  // Increased width for longer paths
        let max_list_height = (height as usize).saturating_sub(10);  // Leave room for borders
        let list_height = self.kernels.len().min(max_list_height);
        let box_height = list_height + 4;

        // Calculate centering
        let start_col = (width.saturating_sub(box_width)) / 2;
        let start_row = (height.saturating_sub(box_height as u16)) / 2;

        // Calculate scroll offset for selected item
        let scroll_offset = if self.selected_index >= list_height {
            self.selected_index - list_height + 1
        } else {
            0
        };

        // Draw box
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
            Print(" Select Python Kernel"),
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

        // Kernel list (with scrolling)
        for i in 0..list_height {
            let kernel_idx = i + scroll_offset;
            if kernel_idx >= self.kernels.len() {
                break;
            }

            let kernel = &self.kernels[kernel_idx];
            let row = start_row + 3 + i as u16;
            execute!(writer, cursor::MoveTo(start_col, row))?;

            // Truncate display name if too long
            let max_display_len = (box_width as usize).saturating_sub(6);
            let display_text = if kernel.display_name.len() > max_display_len {
                format!("{}...", &kernel.display_name[..max_display_len - 3])
            } else {
                kernel.display_name.clone()
            };

            if kernel_idx == self.selected_index {
                execute!(
                    writer,
                    SetForegroundColor(Color::Cyan),
                    Print("│"),
                    ResetColor,
                    SetBackgroundColor(Color::DarkGrey),
                    SetForegroundColor(Color::White),
                    Print(format!(" > {:<width$}", display_text, width = (box_width - 4) as usize)),
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
                    Print(format!("   {:<width$}", display_text, width = (box_width - 4) as usize)),
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
        let instructions = if self.kernels.len() > list_height {
            format!("↑↓/jk: Navigate  Enter: Select  Esc: Cancel  [{}/{}]",
                    self.selected_index + 1, self.kernels.len())
        } else {
            "↑↓/jk: Navigate  Enter: Select  Esc: Cancel".to_string()
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

    fn show_error<W: Write>(&self, writer: &mut W, message: &str) -> io::Result<()> {
        let (width, height) = terminal::size()?;
        let box_width = width.min(60);

        let start_col = (width.saturating_sub(box_width)) / 2;
        let start_row = height / 2;

        execute!(
            writer,
            cursor::MoveTo(start_col, start_row),
            SetForegroundColor(Color::Red),
            Print("┌"),
            Print("─".repeat((box_width - 2) as usize)),
            Print("┐"),
            ResetColor
        )?;

        execute!(
            writer,
            cursor::MoveTo(start_col, start_row + 1),
            SetForegroundColor(Color::Red),
            Print("│"),
            ResetColor,
            Print(format!(" {:<width$}", message, width = (box_width - 3) as usize)),
            SetForegroundColor(Color::Red),
            Print("│"),
            ResetColor
        )?;

        execute!(
            writer,
            cursor::MoveTo(start_col, start_row + 2),
            SetForegroundColor(Color::Red),
            Print("└"),
            Print("─".repeat((box_width - 2) as usize)),
            Print("┘"),
            ResetColor
        )?;

        writer.flush()?;

        // Wait for key press
        loop {
            if let Event::Key(_) = event::read()? {
                break;
            }
        }

        Ok(())
    }
}
