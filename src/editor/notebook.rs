use crate::cell::{Cell, parse_cells, get_cell_at_position, get_cell_content};
use crate::kernel::Kernel;
use std::io;

use super::Editor;

impl Editor {
    /// Enable REPL mode
    pub fn enable_repl_mode(&mut self) {
        self.repl_mode = true;
        self.update_cells();
    }

    /// Check if in REPL mode
    pub fn is_repl_mode(&self) -> bool {
        self.repl_mode
    }

    /// Set the active kernel
    pub fn set_kernel(&mut self, kernel: Box<dyn Kernel>) {
        self.kernel = Some(kernel);
    }

    /// Get kernel info
    pub fn get_kernel_info(&self) -> Option<String> {
        self.kernel.as_ref().map(|k| k.info().display_name)
    }

    /// Take ownership of the kernel (for background execution)
    pub fn take_kernel(&mut self) -> Option<Box<dyn Kernel>> {
        self.kernel.take()
    }

    /// Get reference to cells
    pub fn get_cells_ref(&self) -> &[Cell] {
        &self.cells
    }

    /// Get reference to buffer rope
    pub fn buffer_rope(&self) -> &ropey::Rope {
        self.buffer.rope()
    }

    /// Get the word at cursor position (for autocomplete)
    /// Supports dot-completion (e.g., "pandas.read_csv")
    pub fn get_word_at_cursor(&self) -> String {
        let rope = self.buffer.rope();
        let cursor_pos = self.cursor;

        // Find start of word (alphanumeric, underscore, or dot)
        let mut start = cursor_pos;
        while start > 0 {
            let char_idx = rope.byte_to_char(start.saturating_sub(1));
            if let Some(ch) = rope.get_char(char_idx) {
                if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                    start -= ch.len_utf8();
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        // Extract word from start to cursor
        if start < cursor_pos {
            rope.slice(start..cursor_pos).to_string()
        } else {
            String::new()
        }
    }

    /// Get completion context for method chaining and SQL
    /// Returns (base_callable, prefix, is_sql_context) for cases like:
    /// - "duckdb.sql(...).p" -> (Some("duckdb.sql"), "p", false)
    /// - db.sql("SELECT * FROM u") -> (None, "u", true)  [inside SQL string]
    pub fn get_completion_context(&self) -> (Option<String>, String, bool) {
        let rope = self.buffer.rope();
        let cursor_pos = self.cursor;

        // Check if we're in a SQL string context
        let is_sql_context = crate::sql_context::is_in_sql_context(rope, cursor_pos);

        // First, get the simple word at cursor
        let simple_word = self.get_word_at_cursor();

        // Strip leading dot if present (get_word_at_cursor includes dots for paths like "pandas.read_csv")
        let prefix = simple_word.trim_start_matches('.').to_string();

        // Check if we're in a method chain (look back for `)` followed by `.` and then our word)
        let mut pos = cursor_pos;

        // Move back through just the alphanumeric part (prefix), not the dot
        if !prefix.is_empty() {
            pos = pos.saturating_sub(prefix.len());
        }

        // Check if there's a dot right before (where we are now)
        if pos > 0 {
            let char_before_idx = rope.byte_to_char(pos.saturating_sub(1));
            if let Some(ch) = rope.get_char(char_before_idx) {
                if ch == '.' {
                    pos = pos.saturating_sub(1);

                    // Now look for closing paren
                    let mut paren_depth = 0;
                    let mut found_callable = false;
                    let mut callable_end = pos;

                    // Move back through whitespace
                    while pos > 0 {
                        let idx = rope.byte_to_char(pos.saturating_sub(1));
                        if let Some(ch) = rope.get_char(idx) {
                            if ch.is_whitespace() {
                                pos -= ch.len_utf8();
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }

                    // Check for closing paren
                    if pos > 0 {
                        let idx = rope.byte_to_char(pos.saturating_sub(1));
                        if let Some(ch) = rope.get_char(idx) {
                            if ch == ')' {
                                paren_depth = 1;
                                pos -= ch.len_utf8();
                                callable_end = pos;
                                found_callable = true;
                            }
                        }
                    }

                    // If we found a method chain, extract the base callable
                    if found_callable {
                        // Move back to find the matching opening paren
                        while pos > 0 && paren_depth > 0 {
                            let idx = rope.byte_to_char(pos.saturating_sub(1));
                            if let Some(ch) = rope.get_char(idx) {
                                if ch == ')' {
                                    paren_depth += 1;
                                } else if ch == '(' {
                                    paren_depth -= 1;
                                }
                                pos -= ch.len_utf8();
                            } else {
                                break;
                            }
                        }

                        // Now extract the callable name before the opening paren
                        let mut callable_start = pos;
                        while callable_start > 0 {
                            let idx = rope.byte_to_char(callable_start.saturating_sub(1));
                            if let Some(ch) = rope.get_char(idx) {
                                if ch.is_alphanumeric() || ch == '_' || ch == '.' {
                                    callable_start -= ch.len_utf8();
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }

                        if callable_start < pos {
                            let base_callable = rope.slice(callable_start..pos).to_string();
                            // Debug logging
                            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
                                use std::io::Write;
                                let _ = writeln!(f, "DEBUG get_completion_context: FOUND METHOD CHAIN - base_callable='{}', prefix='{}', is_sql={}", base_callable, prefix, is_sql_context);
                            }
                            return (Some(base_callable), prefix.clone(), is_sql_context);
                        }
                    }
                }
            }
        }

        // Not in a method chain, return None for base and the prefix (without leading dot)
        // Debug logging
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
            use std::io::Write;
            let _ = writeln!(f, "DEBUG get_completion_context: NO METHOD CHAIN - base_callable=None, prefix='{}', is_sql={}", prefix, is_sql_context);
        }
        (None, prefix, is_sql_context)
    }

    /// Update cells by parsing the buffer
    pub fn update_cells(&mut self) {
        self.cells = parse_cells(self.buffer.rope());
    }

    /// Get cells for rendering
    pub fn get_cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Execute all cells within selection (or current cell if no selection)
    /// Returns: Vec<(execution_count, cell_line, output_text, is_error, elapsed_secs)>
    pub fn execute_selected_cells_with_output(&mut self) -> Vec<(usize, usize, String, bool, f64)> {
        let mut results = Vec::new();

        if !self.repl_mode || self.kernel.is_none() {
            return results;
        }

        // Parse cells
        self.update_cells();

        // Find cells to execute
        let cells_to_execute: Vec<usize> = if let Some((sel_start, sel_end)) = self.get_selection() {
            // Execute all cells that overlap with the selection
            self.cells.iter().enumerate()
                .filter(|(_, cell)| {
                    // Cell overlaps with selection if:
                    // cell.start < sel_end && cell.end > sel_start
                    cell.start < sel_end && cell.end > sel_start
                })
                .map(|(idx, _)| idx)
                .collect()
        } else {
            // No selection - execute current cell only
            if let Some(cell_idx) = get_cell_at_position(&self.cells, self.cursor) {
                vec![cell_idx]
            } else {
                vec![]
            }
        };

        // Save total cell count before loop consumes the vector
        let total_cells_to_execute = cells_to_execute.len();

        // Execute each cell in order
        for cell_idx in cells_to_execute {
            let cell = &self.cells[cell_idx];
            let code = get_cell_content(self.buffer.rope(), cell);
            let cell_number = cell_idx + 1; // Cell number is 1-indexed

            // Execute code with timing
            if let Some(kernel) = self.kernel.as_mut() {
                let start_time = std::time::Instant::now();

                match kernel.execute(&code) {
                    Ok(result) => {
                        let elapsed = start_time.elapsed().as_secs_f64();

                        // Store result in cell
                        self.cells[cell_idx].output = Some(result.clone());

                        let execution_count = result.execution_count.unwrap_or(0);
                        let output_text = crate::cell::format_output(&result);
                        let is_error = !result.success;

                        results.push((execution_count, cell_number, output_text, is_error, elapsed));

                        // Stop execution if this cell had an error
                        if is_error {
                            break;
                        }
                    }
                    Err(e) => {
                        let elapsed = start_time.elapsed().as_secs_f64();
                        results.push((0, cell_number, format!("Error: {}", e), true, elapsed));
                        // Stop execution on kernel error
                        break;
                    }
                }
            }
        }

        // Set status message
        if !results.is_empty() {
            if results.len() == 1 {
                let (_, cell_num, _, is_error, elapsed) = &results[0];
                if *is_error {
                    self.status_message = Some((format!("Cell {} error ({:.3}s)", cell_num, elapsed), true));
                } else {
                    self.status_message = Some((format!("Cell {} executed ({:.3}s)", cell_num, elapsed), false));
                }
            } else {
                let error_count = results.iter().filter(|(_, _, _, is_err, _)| *is_err).count();
                let total_time: f64 = results.iter().map(|(_, _, _, _, elapsed)| elapsed).sum();

                if error_count > 0 {
                    // Check if execution stopped early due to error
                    if results.len() < total_cells_to_execute {
                        self.status_message = Some((
                            format!("Executed {} of {} cells (stopped on error, {:.3}s)", results.len(), total_cells_to_execute, total_time),
                            true
                        ));
                    } else {
                        self.status_message = Some((
                            format!("Executed {} cells ({} errors, {:.3}s)", results.len(), error_count, total_time),
                            true
                        ));
                    }
                } else {
                    self.status_message = Some((
                        format!("Executed {} cells ({:.3}s)", results.len(), total_time),
                        false
                    ));
                }
            }
            self.status_message_persistent = false;
        }

        results
    }

    /// Execute the current cell and return output info for output pane
    /// Returns: Option<(execution_count, cell_line, output_text, is_error, elapsed_secs)>
    pub fn execute_current_cell_with_output(&mut self) -> Option<(usize, usize, String, bool, f64)> {
        // Use the new method and return first result
        let results = self.execute_selected_cells_with_output();
        results.into_iter().next()
    }

    /// Execute the current cell (cell containing cursor) - legacy method
    pub fn execute_current_cell(&mut self) -> io::Result<()> {
        if !self.repl_mode {
            self.status_message = Some(("Not in REPL mode. Press Ctrl+K to select a kernel.".to_string(), true));
            return Ok(());
        }

        if self.kernel.is_none() {
            self.status_message = Some(("No kernel connected. Press Ctrl+K to select a kernel.".to_string(), true));
            return Ok(());
        }

        // Parse cells
        self.update_cells();

        // Find cell at cursor position
        if let Some(cell_idx) = get_cell_at_position(&self.cells, self.cursor) {
            let cell = &self.cells[cell_idx];
            let code = get_cell_content(self.buffer.rope(), cell);

            // Get cell line number for display
            let cell_line = self.buffer.rope().byte_to_line(cell.start) + 1;

            // Execute code
            if let Some(kernel) = self.kernel.as_mut() {
                match kernel.execute(&code) {
                    Ok(result) => {
                        // Store result in cell
                        self.cells[cell_idx].output = Some(result.clone());

                        if result.success {
                            // Format output for display
                            let output_text = self.format_execution_output(&result);
                            if output_text.is_empty() {
                                self.status_message = Some((format!("Cell {} executed (no output)", cell_line), false));
                            } else {
                                self.status_message = Some((format!("Cell {}: {}", cell_line, output_text), false));
                            }
                            self.status_message_persistent = false; // Will clear on next action
                        } else {
                            // Show error (keep persistent until user acknowledges)
                            let error_text = self.format_execution_output(&result);
                            self.status_message = Some((format!("Cell {} error: {}", cell_line, error_text), true));
                            self.status_message_persistent = true; // Errors stay visible
                        }
                    }
                    Err(e) => {
                        self.status_message = Some((format!("Cell {} error: {}", cell_line, e), true));
                    }
                }
            }
        } else {
            self.status_message = Some(("Cursor not in a cell. Use # %% to define cells.".to_string(), true));
        }

        Ok(())
    }

    /// Format execution output for status display
    fn format_execution_output(&self, result: &crate::kernel::ExecutionResult) -> String {
        let mut parts = Vec::new();

        for exec_output in &result.outputs {
            match exec_output {
                crate::kernel::ExecutionOutput::Result(text) => {
                    parts.push(text.trim().to_string());
                }
                crate::kernel::ExecutionOutput::Stdout(text) => {
                    // Replace newlines with space to keep output on single line
                    let formatted = text.trim().replace('\n', " ");
                    if !formatted.is_empty() {
                        parts.push(formatted);
                    }
                }
                crate::kernel::ExecutionOutput::Stderr(text) => {
                    let formatted = text.trim().replace('\n', " ");
                    if !formatted.is_empty() {
                        parts.push(formatted);
                    }
                }
                crate::kernel::ExecutionOutput::Error { ename, evalue, .. } => {
                    parts.push(format!("{}: {}", ename, evalue));
                }
                crate::kernel::ExecutionOutput::Display { data, .. } => {
                    let formatted = data.trim().replace('\n', " ");
                    if !formatted.is_empty() {
                        parts.push(formatted);
                    }
                }
            }
        }

        // Join parts with separator to distinguish different outputs
        let output = parts.join(" → ");

        // Truncate if too long
        if output.len() > 200 {
            format!("{}...", &output[..200])
        } else {
            output
        }
    }

    /// Check if kernel is connected
    pub fn is_kernel_connected(&self) -> bool {
        self.kernel.as_ref().map(|k| k.is_connected()).unwrap_or(false)
    }

    /// Connect to the kernel
    pub fn connect_kernel(&mut self) -> Result<(), String> {
        if let Some(kernel) = self.kernel.as_mut() {
            kernel.connect().map_err(|e| e.to_string())?;
            self.status_message = Some(("Connected to kernel".to_string(), false));
        }
        Ok(())
    }

    /// Disconnect kernel
    pub fn disconnect_kernel(&mut self) -> Result<(), String> {
        if let Some(kernel) = self.kernel.as_mut() {
            kernel.disconnect().map_err(|e| e.to_string())?;
            self.status_message = Some(("Kernel disconnected".to_string(), false));
        }
        Ok(())
    }
}
