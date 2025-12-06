use crossterm::{
    cursor,
    execute,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
};
use std::io::{self, Write};

/// Autocomplete suggestions dropdown
pub struct Autocomplete {
    suggestions: Vec<String>,
    selected_index: usize,
    visible: bool,
    filter_text: String,
    dynamic_completions: Vec<String>, // Completions from Python namespace
}

impl Autocomplete {
    pub fn new() -> Self {
        Autocomplete {
            suggestions: Vec::new(),
            selected_index: 0,
            visible: false,
            filter_text: String::new(),
            dynamic_completions: Vec::new(),
        }
    }

    /// Add dynamic completions from Python namespace
    pub fn add_dynamic_completions(&mut self, completions: Vec<String>) {
        self.dynamic_completions = completions;
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

    /// Update suggestions based on current word prefix
    pub fn update(&mut self, prefix: &str) {
        self.filter_text = prefix.to_string();

        if prefix.is_empty() {
            self.suggestions.clear();
            self.visible = false;
            return;
        }

        crate::debug_log(&format!("Autocomplete update: prefix='{}', {} dynamic completions available",
            prefix, self.dynamic_completions.len()));

        // Merge static and dynamic completions
        let mut all_suggestions = Vec::new();

        // Add dynamic completions first (they're more relevant)
        for completion in &self.dynamic_completions {
            if completion.starts_with(prefix) {
                all_suggestions.push(completion.clone());
            }
        }

        crate::debug_log(&format!("Found {} matching dynamic completions", all_suggestions.len()));

        // Add static Python completions (if not already present)
        let static_completions = Self::get_python_completions();
        for completion in static_completions {
            let comp_str = completion.to_string();
            if comp_str.starts_with(prefix) && !all_suggestions.contains(&comp_str) {
                all_suggestions.push(comp_str);
            }
        }

        crate::debug_log(&format!("Total suggestions after merging: {}", all_suggestions.len()));
        if !all_suggestions.is_empty() {
            crate::debug_log(&format!("First 5 suggestions: {:?}", all_suggestions.iter().take(5).collect::<Vec<_>>()));
        }

        self.suggestions = all_suggestions;
        self.visible = !self.suggestions.is_empty();
        self.selected_index = 0;
    }

    /// Show autocomplete at cursor position
    pub fn show(&mut self, prefix: &str) {
        self.update(prefix);
    }

    /// Hide autocomplete
    pub fn hide(&mut self) {
        self.visible = false;
        self.suggestions.clear();
        self.selected_index = 0;
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
        }
    }

    /// Move selection down
    pub fn select_next(&mut self) {
        if !self.suggestions.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.suggestions.len();
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
        &self,
        writer: &mut W,
        cursor_row: u16,
        cursor_col: u16,
        max_row: u16,
    ) -> io::Result<()> {
        if !self.visible || self.suggestions.is_empty() {
            return Ok(());
        }

        // Show up to 10 suggestions
        let max_suggestions = 10.min(self.suggestions.len());
        let dropdown_height = max_suggestions as u16;

        // Position dropdown below cursor (or above if not enough space)
        let dropdown_row = if cursor_row + dropdown_height + 1 < max_row {
            cursor_row + 1
        } else {
            cursor_row.saturating_sub(dropdown_height)
        };

        // Find longest suggestion for width
        let max_width = self.suggestions[..max_suggestions]
            .iter()
            .map(|s| s.len())
            .max()
            .unwrap_or(20)
            .max(20);

        // Draw each suggestion
        for (idx, suggestion) in self.suggestions.iter().take(max_suggestions).enumerate() {
            let row = dropdown_row + idx as u16;
            let is_selected = idx == self.selected_index;

            execute!(writer, cursor::MoveTo(cursor_col, row))?;

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
