use crossterm::{
    cursor,
    execute,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
};
use std::io::{self, Write};

/// SQL suggestions for the name being typed: the names known for this SQL
/// first, then the dialect's stock words (not after a dot, where only names
/// make sense). Case-sensitive.
fn sql_suggestions(
    c: &crate::sql_words::SqlCompletion,
    names: &crate::sql_words::SessionWords,
    stock: fn(&str) -> Vec<String>,
) -> Vec<String> {
    let mut suggestions = names.matches(c.qualifier.as_deref(), &c.prefix);
    if c.qualifier.is_none() && !c.prefix.is_empty() {
        for word in stock(&c.prefix) {
            if !suggestions.contains(&word) {
                suggestions.push(word);
            }
        }
    }
    suggestions
}

/// Autocomplete suggestions dropdown
pub struct Autocomplete {
    suggestions: Vec<String>,
    selected_index: usize,
    visible: bool,
    filter_text: String,
    dynamic_completions: Vec<String>, // Completions from Python namespace
    viewport_offset: usize, // Scroll offset for the visible window
    type_relationships: crate::kernel::TypeRelationships, // Type information for intelligent completion
    /// Snowflake SQL (SQL mode, and `sf.sql` strings in Python): names from
    /// this session's queries (in memory only).
    sql_session: crate::sql_words::SessionWords,
    /// Other SQL in Python (`db.sql` and the like): the tables, columns and
    /// functions the Python kernel reported after the last run.
    other_sql_names: crate::sql_words::SessionWords,
}

impl Autocomplete {
    pub fn new() -> Self {
        Autocomplete {
            suggestions: Vec::new(),
            selected_index: 0,
            visible: false,
            filter_text: String::new(),
            dynamic_completions: Vec::new(),
            viewport_offset: 0,
            type_relationships: crate::kernel::TypeRelationships::default(),
            sql_session: crate::sql_words::SessionWords::default(),
            other_sql_names: crate::sql_words::SessionWords::keep_all(),
        }
    }

    /// Remember the names in a Snowflake query that ran, and its result's
    /// column names, for the rest of this sage session.
    pub fn remember_sql(&mut self, sql: &str, result_columns: &[String]) {
        self.sql_session.add_query(sql, result_columns);
    }

    /// SQL mode: suggestions for the name being typed (see `show_sql`).
    pub fn update_sql(&mut self, completion: Option<&crate::sql_words::SqlCompletion>) {
        let suggestions = completion.map(|c| {
            sql_suggestions(c, &self.sql_session, crate::sql_words::stock_matches)
        });
        self.show_sql(completion, suggestions);
    }

    /// The caret in an SQL string in Python. Snowflake SQL (`sf.sql`) gets
    /// what SQL mode gets; other SQL (`db.sql`) gets the tables, columns and
    /// functions the kernel reported and the generic SQL words.
    pub fn update_embedded_sql(&mut self, embedded: &crate::sql_context::EmbeddedSql) {
        let c = &embedded.completion;
        let suggestions = match embedded.dialect {
            crate::sql_context::SqlDialect::Snowflake => {
                sql_suggestions(c, &self.sql_session, crate::sql_words::stock_matches)
            }
            crate::sql_context::SqlDialect::Other => {
                sql_suggestions(c, &self.other_sql_names, crate::sql_words::generic_matches)
            }
        };
        self.show_sql(Some(c), Some(suggestions));
    }

    /// Show SQL suggestions, matched case-sensitively. Hidden until a letter
    /// of the name is typed (so not straight after a dot), and when the only
    /// suggestion is what's already typed.
    fn show_sql(&mut self, completion: Option<&crate::sql_words::SqlCompletion>, suggestions: Option<Vec<String>>) {
        let (Some(c), Some(mut suggestions)) = (completion, suggestions) else {
            self.hide();
            return;
        };
        if c.prefix.is_empty() {
            self.hide();
            return;
        }
        self.filter_text = c.prefix.clone();
        if suggestions.len() == 1 && suggestions[0] == c.prefix {
            suggestions.clear();
        }
        self.visible = !suggestions.is_empty();
        self.suggestions = suggestions;
        self.selected_index = 0;
        self.viewport_offset = 0;
    }

    /// Add dynamic completions from Python namespace
    pub fn add_dynamic_completions(&mut self, completions: Vec<String>) {
        self.dynamic_completions = completions;
    }

    /// Set type relationships for intelligent completion
    pub fn set_type_relationships(&mut self, type_relationships: crate::kernel::TypeRelationships) {
        self.type_relationships = type_relationships;
    }

    /// The tables, columns (`table.column` and bare) and functions the Python
    /// kernel found (DuckDB, Spark), offered for SQL in Python that isn't
    /// Snowflake's. Replaces what the last run reported.
    pub fn set_sql_metadata(&mut self, sql_metadata: crate::kernel::SqlMetadata) {
        let mut names = crate::sql_words::SessionWords::keep_all();
        for name in sql_metadata.tables.iter().chain(&sql_metadata.columns).chain(&sql_metadata.functions) {
            names.add_name(name);
        }
        self.other_sql_names = names;
    }

    /// Get Python keywords and built-in functions
    fn get_python_completions() -> Vec<&'static str> {
        vec![
            // Keywords
            "False", "None", "True", "and", "as", "assert", "async", "await",
            "break", "class", "continue", "def", "del", "elif", "else", "except",
            "finally", "for", "from", "global", "if", "import", "in", "is",
            "lambda", "nonlocal", "not", "or", "pass", "raise", "return",
            "try", "while", "with", "yield",
            // Built-in functions
            "abs", "all", "any", "ascii", "bin", "bool", "bytearray", "bytes",
            "callable", "chr", "classmethod", "compile", "complex", "delattr",
            "dict", "dir", "divmod", "enumerate", "eval", "exec", "filter",
            "float", "format", "frozenset", "getattr", "globals", "hasattr",
            "hash", "help", "hex", "id", "input", "int", "isinstance",
            "issubclass", "iter", "len", "list", "locals", "map", "max",
            "memoryview", "min", "next", "object", "oct", "open", "ord",
            "pow", "print", "property", "range", "repr", "reversed", "round",
            "set", "setattr", "slice", "sorted", "staticmethod", "str", "sum",
            "super", "tuple", "type", "vars", "zip",
            // Common imports
            "pandas", "numpy", "matplotlib", "duckdb", "json", "os", "sys",
            "datetime", "collections", "itertools", "functools", "pathlib",
        ]
    }

    /// Update suggestions for Python, with method chain context (SQL strings
    /// go through `update_embedded_sql`).
    /// base_callable: Optional base function/method (e.g., "duckdb.sql" from "duckdb.sql(...).p")
    /// prefix: The prefix to filter by (e.g., "p" from "duckdb.sql(...).p")
    pub fn update_with_context(&mut self, base_callable: Option<String>, prefix: &str) {
        self.filter_text = prefix.to_string();

        // Debug output to file
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
            let _ = writeln!(f, "DEBUG autocomplete: base_callable={:?}, prefix='{}', dynamic_completions_count={}",
                      base_callable, prefix, self.dynamic_completions.len());
            if !self.dynamic_completions.is_empty() {
                let _ = writeln!(f, "DEBUG autocomplete: first 5 completions: {:?}",
                          &self.dynamic_completions[..self.dynamic_completions.len().min(5)]);
            }
        }

        // Nothing until a letter is typed: not on an empty word, and not
        // straight after a dot (`df.`, `db.sql(...).`).
        if prefix.is_empty() || prefix.ends_with('.') {
            self.suggestions.clear();
            self.visible = false;
            return;
        }

        let mut all_suggestions = Vec::new();

        // If we have a base callable, try to use type information
        if let Some(ref base) = base_callable {
            // Look up the return type of the base callable
            if let Some(return_type) = self.type_relationships.return_types.get(base) {
                // Get methods for that return type
                if let Some(methods) = self.type_relationships.type_methods.get(return_type) {
                    for method in methods {
                        if prefix.is_empty() || method.starts_with(prefix) {
                            all_suggestions.push(method.clone());
                        }
                    }
                }
            } else {
                // Fallback: If we don't know the return type, try to infer from common patterns
                // For example, if base is "module.function", look for types related to that module
                if let Some(module_name) = base.split('.').next() {
                    // Collect methods from types that might be related to this module
                    for (type_name, methods) in &self.type_relationships.type_methods {
                        // Heuristic: If type name contains module name or starts with it
                        if type_name.to_lowercase().contains(&module_name.to_lowercase()) ||
                           type_name.starts_with(&module_name.chars().next().unwrap().to_uppercase().collect::<String>()) {
                            for method in methods {
                                if (prefix.is_empty() || method.starts_with(prefix)) && !all_suggestions.contains(method) {
                                    all_suggestions.push(method.clone());
                                }
                            }
                        }
                    }
                }
            }

            // If we found suggestions from type info, use them
            if !all_suggestions.is_empty() {
                self.suggestions = all_suggestions;
                self.visible = true;
                self.selected_index = 0;
                self.viewport_offset = 0;
                return;
            }
        }

        // Fallback to regular prefix matching if no type info or no base callable
        // Add dynamic completions first (they're more relevant)
        for completion in &self.dynamic_completions {
            if completion.starts_with(prefix) {
                all_suggestions.push(completion.clone());
            }
        }

        // Add static Python completions (if not already present)
        let static_completions = Self::get_python_completions();
        for completion in static_completions {
            let comp_str = completion.to_string();
            if comp_str.starts_with(prefix) && !all_suggestions.contains(&comp_str) {
                all_suggestions.push(comp_str);
            }
        }

        self.suggestions = all_suggestions;
        self.visible = !self.suggestions.is_empty();
        self.selected_index = 0;
        self.viewport_offset = 0;

        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/sage_debug.log") {
            let _ = writeln!(f, "DEBUG autocomplete: final suggestions_count={}, visible={}",
                      self.suggestions.len(), self.visible);
            if !self.suggestions.is_empty() {
                let _ = writeln!(f, "DEBUG autocomplete: suggestions={:?}", &self.suggestions[..self.suggestions.len().min(5)]);
            }
        }
    }

    /// Hide autocomplete
    pub fn hide(&mut self) {
        self.visible = false;
        self.suggestions.clear();
        self.selected_index = 0;
        self.viewport_offset = 0;
    }

    /// Is autocomplete visible?
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Move selection up
    pub fn select_previous(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.suggestions.len() - 1
            } else {
                self.selected_index - 1
            };

            // Adjust viewport to keep selection visible
            const MAX_VISIBLE: usize = 10;
            if self.selected_index < self.viewport_offset {
                self.viewport_offset = self.selected_index;
            } else if self.selected_index >= self.viewport_offset + MAX_VISIBLE {
                self.viewport_offset = self.selected_index.saturating_sub(MAX_VISIBLE - 1);
            }
        }
    }

    /// Move selection down
    pub fn select_next(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.suggestions.len();

            // Adjust viewport to keep selection visible
            const MAX_VISIBLE: usize = 10;
            if self.selected_index < self.viewport_offset {
                self.viewport_offset = self.selected_index;
            } else if self.selected_index >= self.viewport_offset + MAX_VISIBLE {
                self.viewport_offset = self.selected_index.saturating_sub(MAX_VISIBLE - 1);
            }
        }
    }

    /// Get currently selected suggestion
    pub fn get_selected(&self) -> Option<&str> {
        if self.visible && self.selected_index < self.suggestions.len() {
            Some(&self.suggestions[self.selected_index])
        } else {
            None
        }
    }

    /// Draw autocomplete dropdown at given position
    pub fn draw<W: Write>(
        &mut self,
        writer: &mut W,
        cursor_row: u16,
        cursor_col: u16,
        max_row: u16,
        max_col: u16,
    ) -> io::Result<()> {
        if !self.visible || self.suggestions.is_empty() {
            return Ok(());
        }

        // Show up to 10 suggestions
        const MAX_VISIBLE: usize = 10;
        let visible_count = MAX_VISIBLE.min(self.suggestions.len());
        let dropdown_height = visible_count as u16;

        // Calculate the range of suggestions to show
        let start_idx = self.viewport_offset;
        let end_idx = (start_idx + visible_count).min(self.suggestions.len());

        // Position dropdown below cursor (or above if not enough space)
        let dropdown_row = if cursor_row + dropdown_height + 1 < max_row {
            cursor_row + 1
        } else {
            cursor_row.saturating_sub(dropdown_height)
        };

        // Find longest suggestion for width (only check visible ones)
        let max_width = self.suggestions[start_idx..end_idx]
            .iter()
            .map(|s| s.len())
            .max()
            .unwrap_or(20)
            .max(20);

        // Calculate dropdown width including padding (space + content + space)
        let dropdown_width = max_width + 2;

        // Adjust column position to prevent wrapping at viewport edge
        let dropdown_col = if cursor_col as usize + dropdown_width > max_col as usize {
            // Nudge left to keep within viewport
            (max_col as usize).saturating_sub(dropdown_width) as u16
        } else {
            cursor_col
        };

        // Draw each visible suggestion
        for (display_idx, actual_idx) in (start_idx..end_idx).enumerate() {
            let suggestion = &self.suggestions[actual_idx];
            let row = dropdown_row + display_idx as u16;
            let is_selected = actual_idx == self.selected_index;

            execute!(writer, cursor::MoveTo(dropdown_col, row))?;

            if is_selected {
                // Highlight selected item
                execute!(
                    writer,
                    SetBackgroundColor(Color::DarkBlue),
                    SetForegroundColor(Color::White),
                )?;
            } else {
                execute!(
                    writer,
                    SetBackgroundColor(Color::DarkGrey),
                    SetForegroundColor(Color::White),
                )?;
            }

            // Pad to max width
            let padded = format!(" {:<width$} ", suggestion, width = max_width);
            execute!(writer, Print(padded), ResetColor)?;
        }

        Ok(())
    }
}
