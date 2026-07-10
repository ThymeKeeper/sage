use crate::cell::{Cell, parse_cells};
use crate::kernel::Kernel;

use super::Editor;

impl Editor {
    /// Enable REPL mode
    pub fn enable_repl_mode(&mut self) {
        self.repl_mode = true;
        self.update_cells();
    }

    /// Disable REPL mode
    pub fn disable_repl_mode(&mut self) {
        self.repl_mode = false;
    }

    /// Check if in REPL mode
    pub fn is_repl_mode(&self) -> bool {
        self.repl_mode
    }

    /// Set the active kernel
    pub fn set_kernel(&mut self, kernel: Box<dyn Kernel>) {
        self.executing_kernel_name = None; // Clear executing name since kernel is back
        self.kernel = Some(kernel);
    }

    /// Get kernel info (returns executing kernel name if kernel is temporarily taken)
    pub fn get_kernel_info(&self) -> Option<String> {
        self.kernel.as_ref().map(|k| k.info().display_name)
            .or_else(|| self.executing_kernel_name.clone())
    }

    /// Path to the current kernel's most recent result spool (if any).
    /// Used by the save-results command.
    pub fn kernel_latest_result_file(&self) -> Option<std::path::PathBuf> {
        self.kernel.as_ref().and_then(|k| k.latest_result_file())
    }

    /// Take ownership of the kernel (for background execution)
    /// Stores the kernel name so get_kernel_info() still works during execution
    pub fn take_kernel(&mut self) -> Option<Box<dyn Kernel>> {
        if let Some(ref kernel) = self.kernel {
            self.executing_kernel_name = Some(kernel.info().display_name.clone());
        }
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
                    let mut _callable_end = pos;

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
                                _callable_end = pos;
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

    /// Update cells by parsing the buffer. Dispatch by language:
    /// SQL uses statement-boundary (semicolon) splitting; everything else
    /// uses the `##--` delimiter convention from `cell::parse_cells`.
    pub fn update_cells(&mut self) {
        let rope = self.buffer.rope();
        self.cells = if *self.syntax.get_language() == crate::syntax::Language::Sql {
            crate::sql_split::parse_sql_cells(rope)
        } else {
            parse_cells(rope)
        };
    }

    /// Check if kernel is connected
    pub fn is_kernel_connected(&self) -> bool {
        self.kernel.as_ref().map(|k| k.is_connected()).unwrap_or(false)
    }

    /// Auto-discover and connect to the first available Python kernel
    pub fn auto_connect_kernel(&mut self) {
        let kernels = crate::kernel::discover_kernels();
        // Skip the Snowflake entry — discovery lists it first when a Snowflake
        // config exists, but this path auto-connects a Python interpreter (and
        // forcing the Snowflake sentinel into a DirectKernel just fails to
        // connect, leaving a .py file with no kernel).
        if let Some(kernel_info) = kernels
            .into_iter()
            .find(|k| k.name != crate::kernel::SNOWFLAKE_KERNEL_NAME)
        {
            let mut new_kernel: Box<dyn Kernel> = Box::new(
                crate::direct_kernel::DirectKernel::new(
                    kernel_info.python_path.clone(),
                    kernel_info.name.clone(),
                    kernel_info.display_name.clone(),
                )
            );
            if new_kernel.connect().is_ok() {
                self.kernel = Some(new_kernel);
                self.status_message = Some((
                    format!("Python mode enabled with {}", kernel_info.display_name),
                    false,
                ));
            } else {
                self.status_message = Some((
                    "Python mode enabled. Press Ctrl+K to select a kernel.".to_string(),
                    false,
                ));
            }
        } else {
            self.status_message = Some((
                "Python mode enabled but no Python found. Install Python first.".to_string(),
                true,
            ));
        }
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
