use std::fs::File;
use std::io;
use std::path::Path;
use unicode_width::UnicodeWidthChar;

pub const MIN_COL_WIDTH: usize = 4;
pub const MAX_COL_WIDTH: usize = 20;
pub const MAX_RESIZE_WIDTH: usize = 200;
pub const ROW_NUM_WIDTH: usize = 5;
pub const FORMULA_BAR_HEIGHT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    None,
    CellSelect,
    FormulaBarSelect,
    ColumnResize {
        col: usize,
        anchor_screen_col: u16,
        anchor_width: usize,
    },
    ColumnSelect,
    RowSelect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridHit {
    Outside,
    FormulaBar { row: usize, text_col: usize },
    Divider,
    ColumnHeader { col: usize },
    ColumnSeparator { col: usize },
    DataCell { row: usize, col: usize },
    RowNumber { row: usize },
}

/// Accumulated timezone state across the date/datetime cells in a selection,
/// used to decide how aggregates are displayed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum TzAgg {
    /// No timezone-aware value seen (naive times, or date-only).
    #[default]
    None,
    /// Every timezone-aware value shares this offset (seconds); shown as-is.
    Uniform(i64),
    /// Timezone-aware values with differing offsets; aggregates shown in UTC.
    Mixed,
}

#[derive(Debug, Default, Clone)]
pub struct SelectionMetrics {
    pub total_cells: usize,
    pub non_empty: usize,
    pub numbers: Vec<f64>,
    /// Epoch seconds (UTC) for each date/datetime cell. Date-only cells land at
    /// midnight; `dates_have_time` records whether any carried a time-of-day.
    pub dates: Vec<i64>,
    /// True if any value in `dates` included a time, so min/max/avg render as
    /// `YYYY-MM-DD HH:MM:SS` rather than date-only.
    pub dates_have_time: bool,
    /// Timezone offset shared by the tz-aware values (for display), or whether
    /// they were naive / mixed.
    tz: TzAgg,
}

impl SelectionMetrics {
    pub fn format(&self) -> String {
        if self.total_cells == 0 {
            return String::new();
        }
        let mut parts: Vec<String> = Vec::new();
        if self.total_cells == 1 {
            parts.push(format!("n {}", self.non_empty));
        } else {
            parts.push(format!("n {}/{}", self.non_empty, self.total_cells));
        }

        // Show sum/avg only when ALL non-empty cells are the same parseable type.
        let all_numeric = self.non_empty > 0 && self.numbers.len() == self.non_empty;
        let all_date = self.non_empty > 0 && self.dates.len() == self.non_empty;

        if all_numeric {
            let sum: f64 = self.numbers.iter().sum();
            let avg = sum / self.numbers.len() as f64;
            let min = self
                .numbers
                .iter()
                .cloned()
                .fold(f64::INFINITY, f64::min);
            let max = self
                .numbers
                .iter()
                .cloned()
                .fold(f64::NEG_INFINITY, f64::max);
            parts.push(format!("sum {}", fmt_num(sum)));
            parts.push(format!("avg {}", fmt_num(avg)));
            parts.push(format!("min {}", fmt_num(min)));
            parts.push(format!("max {}", fmt_num(max)));
        } else if all_date {
            if self.dates_have_time {
                // Aggregate in UTC seconds, then render in the selection's
                // shared offset (or UTC, labelled +00:00, if they differ).
                let min = *self.dates.iter().min().unwrap();
                let max = *self.dates.iter().max().unwrap();
                let sum: i64 = self.dates.iter().sum();
                let avg = (sum as f64 / self.dates.len() as f64).round() as i64;
                let fmt = |utc: i64| match self.tz {
                    TzAgg::Uniform(off) => {
                        format!("{}{}", format_iso_datetime(utc + off), format_tz_offset(off))
                    }
                    TzAgg::Mixed => format!("{}+00:00", format_iso_datetime(utc)),
                    TzAgg::None => format_iso_datetime(utc),
                };
                parts.push(format!("min {}", fmt(min)));
                parts.push(format!("max {}", fmt(max)));
                parts.push(format!("avg {}", fmt(avg)));
            } else {
                // All date-only: aggregate in whole days so the average rounds
                // to a calendar day rather than flooring to the previous one.
                let days: Vec<i64> = self.dates.iter().map(|s| s.div_euclid(86_400)).collect();
                let min = *days.iter().min().unwrap();
                let max = *days.iter().max().unwrap();
                let sum: i64 = days.iter().sum();
                let avg = (sum as f64 / days.len() as f64).round() as i64;
                parts.push(format!("min {}", format_iso_date(min)));
                parts.push(format!("max {}", format_iso_date(max)));
                parts.push(format!("avg {}", format_iso_date(avg)));
            }
        }

        parts.join("  ")
    }
}

pub struct Spreadsheet {
    pub rows: Vec<Vec<String>>,
    /// Parallel to `rows`: true where the source had an unquoted-empty field
    /// (a SQL null, per the [`crate::dsv`] convention) rather than an empty
    /// string. Null cells hold `""` in `rows` but render as `∅`. Same shape as
    /// `rows`; kept in sync wherever cells are written.
    pub null_mask: Vec<Vec<bool>>,
    pub cursor: (usize, usize),
    pub selection_anchor: Option<(usize, usize)>,
    pub column_widths: Vec<usize>,
    pub scroll_row: usize,
    pub scroll_col: usize,
    pub delimiter: u8,
    pub modified: bool,
    pub editing: Option<CellEdit>,
    pub mouse_mode: MouseMode,
}

/// Sentinel rendered in the grid for a null cell (an unquoted-empty CSV/TSV
/// field). Distinct from a blank cell, which is an empty string.
pub const NULL_SENTINEL: &str = "∅";

pub struct CellEdit {
    pub text: String,
    pub cursor: usize,
    pub selection_start: Option<usize>,
}

impl Spreadsheet {
    pub fn from_file(path: &Path) -> io::Result<Self> {
        let delimiter = detect_delimiter(path);
        let content = std::fs::read_to_string(path)?;

        // Parse with null awareness: an unquoted-empty field is a null (shown as
        // ∅), a quoted "" is an empty string. `rows` holds "" for both; the
        // distinction lives in `null_mask`.
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut null_mask: Vec<Vec<bool>> = Vec::new();
        for record in crate::dsv::parse(&content, delimiter) {
            let mut row = Vec::with_capacity(record.len());
            let mut mask = Vec::with_capacity(record.len());
            for field in record {
                mask.push(field.is_none());
                row.push(field.unwrap_or_default());
            }
            rows.push(row);
            null_mask.push(mask);
        }

        let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
        for (row, mask) in rows.iter_mut().zip(null_mask.iter_mut()) {
            // Pad short rows with empty strings (missing trailing fields are
            // treated as empty, not null).
            while row.len() < max_cols {
                row.push(String::new());
                mask.push(false);
            }
        }

        if rows.is_empty() {
            rows = vec![vec![String::new(); max_cols.max(1)]];
            null_mask = vec![vec![false; max_cols.max(1)]];
        }

        let mut ss = Self {
            rows,
            null_mask,
            cursor: (0, 0),
            selection_anchor: None,
            column_widths: Vec::new(),
            scroll_row: 0,
            scroll_col: 0,
            delimiter,
            modified: false,
            editing: None,
            mouse_mode: MouseMode::None,
        };
        ss.recompute_column_widths();
        Ok(ss)
    }

    pub fn new_empty(delimiter: u8) -> Self {
        Self {
            rows: vec![vec![String::new()]],
            null_mask: vec![vec![false]],
            cursor: (0, 0),
            selection_anchor: None,
            column_widths: vec![MIN_COL_WIDTH],
            scroll_row: 0,
            scroll_col: 0,
            delimiter,
            modified: false,
            editing: None,
            mouse_mode: MouseMode::None,
        }
    }

    pub fn save(&mut self, path: &Path) -> io::Result<()> {
        use std::io::Write;
        // Write through the null-aware serializer so null cells round-trip as
        // unquoted-empty fields and empty strings as quoted "" (see crate::dsv).
        let mut writer = io::BufWriter::new(File::create(path)?);
        let mut line = String::new();
        for (r, row) in self.rows.iter().enumerate() {
            line.clear();
            for (c, cell) in row.iter().enumerate() {
                if c > 0 {
                    line.push(self.delimiter as char);
                }
                let is_null = self
                    .null_mask
                    .get(r)
                    .and_then(|m| m.get(c))
                    .copied()
                    .unwrap_or(false);
                let field = if is_null { None } else { Some(cell.as_str()) };
                crate::dsv::serialize_field(&mut line, field, self.delimiter);
            }
            line.push('\n');
            writer.write_all(line.as_bytes())?;
        }
        writer.flush()?;
        self.modified = false;
        Ok(())
    }

    pub fn num_rows(&self) -> usize {
        self.rows.len()
    }

    pub fn num_cols(&self) -> usize {
        self.rows.first().map(|r| r.len()).unwrap_or(0)
    }

    pub fn cell(&self, row: usize, col: usize) -> &str {
        self.rows
            .get(row)
            .and_then(|r| r.get(col))
            .map(|s| s.as_str())
            .unwrap_or("")
    }

    /// Whether the cell is null (an unquoted-empty source field) as opposed to
    /// an empty string. Null cells hold `""` in `rows` but render as `∅`.
    pub fn is_null(&self, row: usize, col: usize) -> bool {
        self.null_mask
            .get(row)
            .and_then(|r| r.get(col))
            .copied()
            .unwrap_or(false)
    }

    pub fn focused_cell_text(&self) -> &str {
        if let Some(edit) = &self.editing {
            &edit.text
        } else {
            self.cell(self.cursor.0, self.cursor.1)
        }
    }

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn has_selection(&self) -> bool {
        match self.selection_anchor {
            Some(anchor) => anchor != self.cursor,
            None => false,
        }
    }

    pub fn selected_range(&self) -> ((usize, usize), (usize, usize)) {
        let anchor = self.selection_anchor.unwrap_or(self.cursor);
        let (ar, ac) = anchor;
        let (cr, cc) = self.cursor;
        ((ar.min(cr), ac.min(cc)), (ar.max(cr), ac.max(cc)))
    }

    pub fn delimiter_name(&self) -> &'static str {
        if self.delimiter == b'\t' { "TSV" } else { "CSV" }
    }

    pub fn cursor_label(&self) -> String {
        format!("{}{}", col_letter(self.cursor.1), self.cursor.0 + 1)
    }

    fn prepare_selection(&mut self, with_selection: bool) {
        if with_selection {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(self.cursor);
            }
        } else {
            self.selection_anchor = None;
        }
    }

    pub fn move_up(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        if self.cursor.0 > 0 {
            self.cursor.0 -= 1;
        }
    }

    pub fn move_down(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        if self.cursor.0 + 1 < self.num_rows() {
            self.cursor.0 += 1;
        }
    }

    pub fn move_left(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
        }
    }

    pub fn move_right(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        if self.cursor.1 + 1 < self.num_cols() {
            self.cursor.1 += 1;
        }
    }

    pub fn move_home(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        self.cursor.1 = 0;
    }

    pub fn move_end(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        let last = self.num_cols().saturating_sub(1);
        self.cursor.1 = last;
    }

    pub fn move_top_left(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        self.cursor = (0, 0);
    }

    pub fn move_bottom_right(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        self.cursor = (
            self.num_rows().saturating_sub(1),
            self.num_cols().saturating_sub(1),
        );
    }

    pub fn page_up(&mut self, visible_rows: usize, with_selection: bool) {
        self.prepare_selection(with_selection);
        let step = visible_rows.max(1);
        self.cursor.0 = self.cursor.0.saturating_sub(step);
    }

    pub fn page_down(&mut self, visible_rows: usize, with_selection: bool) {
        self.prepare_selection(with_selection);
        let step = visible_rows.max(1);
        let last = self.num_rows().saturating_sub(1);
        self.cursor.0 = (self.cursor.0 + step).min(last);
    }

    pub fn select_all(&mut self) {
        self.selection_anchor = Some((0, 0));
        self.cursor = (
            self.num_rows().saturating_sub(1),
            self.num_cols().saturating_sub(1),
        );
    }

    pub fn enter_edit_mode(&mut self) {
        let text = self.cell(self.cursor.0, self.cursor.1).to_string();
        let cursor = text.len();
        self.editing = Some(CellEdit {
            text,
            cursor,
            selection_start: None,
        });
    }

    pub fn enter_edit_mode_replace(&mut self, initial: char) {
        let mut text = String::new();
        text.push(initial);
        let cursor = text.len();
        self.editing = Some(CellEdit {
            text,
            cursor,
            selection_start: None,
        });
    }

    pub fn cancel_edit(&mut self) {
        self.editing = None;
    }

    pub fn commit_edit(&mut self) {
        let Some(edit) = self.editing.take() else { return };
        let (r, c) = self.cursor;
        if let Some(row) = self.rows.get_mut(r) {
            if let Some(cell) = row.get_mut(c) {
                // An edited cell holds a real value, never a null — even if the
                // committed text is empty (that's now an empty string).
                let was_null = self.null_mask.get(r).and_then(|m| m.get(c)).copied().unwrap_or(false);
                if *cell != edit.text || was_null {
                    *cell = edit.text;
                    self.modified = true;
                }
                if let Some(m) = self.null_mask.get_mut(r).and_then(|row| row.get_mut(c)) {
                    *m = false;
                }
            }
        }
        self.recompute_col_width(c);
    }

    pub fn clear_selection_content(&mut self) {
        let ((r0, c0), (r1, c1)) = self.selected_range();
        let mut changed = false;
        for r in r0..=r1 {
            for c in c0..=c1 {
                if let Some(row) = self.rows.get_mut(r) {
                    if let Some(cell) = row.get_mut(c) {
                        if !cell.is_empty() {
                            cell.clear();
                            changed = true;
                        }
                    }
                }
                // Clearing yields an empty string, not a null.
                if let Some(m) = self.null_mask.get_mut(r).and_then(|row| row.get_mut(c)) {
                    if *m {
                        *m = false;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.modified = true;
        }
    }

    pub fn copy_selection_tsv(&self) -> String {
        let ((r0, c0), (r1, c1)) = self.selected_range();
        let mut out = String::new();
        for r in r0..=r1 {
            if r > r0 {
                out.push('\n');
            }
            for c in c0..=c1 {
                if c > c0 {
                    out.push('\t');
                }
                let cell = self.cell(r, c);
                if cell.contains('\t') || cell.contains('\n') || cell.contains('"') {
                    out.push('"');
                    out.push_str(&cell.replace('"', "\"\""));
                    out.push('"');
                } else {
                    out.push_str(cell);
                }
            }
        }
        out
    }

    pub fn edit_insert_char(&mut self, ch: char) {
        let Some(edit) = self.editing.as_mut() else { return };
        edit.delete_selection();
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        edit.text.insert_str(edit.cursor, s);
        edit.cursor += s.len();
    }

    pub fn edit_insert_newline(&mut self) {
        self.edit_insert_char('\n');
    }

    pub fn edit_backspace(&mut self) {
        let Some(edit) = self.editing.as_mut() else { return };
        if edit.delete_selection() {
            return;
        }
        if edit.cursor == 0 {
            return;
        }
        let prev = prev_char_boundary(&edit.text, edit.cursor);
        edit.text.replace_range(prev..edit.cursor, "");
        edit.cursor = prev;
    }

    pub fn edit_delete(&mut self) {
        let Some(edit) = self.editing.as_mut() else { return };
        if edit.delete_selection() {
            return;
        }
        if edit.cursor >= edit.text.len() {
            return;
        }
        let next = next_char_boundary(&edit.text, edit.cursor);
        edit.text.replace_range(edit.cursor..next, "");
    }

    pub fn edit_move_left(&mut self, with_selection: bool) {
        let Some(edit) = self.editing.as_mut() else { return };
        edit.prepare_selection(with_selection);
        if edit.cursor > 0 {
            edit.cursor = prev_char_boundary(&edit.text, edit.cursor);
        }
    }

    pub fn edit_move_right(&mut self, with_selection: bool) {
        let Some(edit) = self.editing.as_mut() else { return };
        edit.prepare_selection(with_selection);
        if edit.cursor < edit.text.len() {
            edit.cursor = next_char_boundary(&edit.text, edit.cursor);
        }
    }

    pub fn edit_move_up(&mut self, with_selection: bool) {
        let Some(edit) = self.editing.as_mut() else { return };
        edit.prepare_selection(with_selection);
        let (line, col) = edit.cursor_line_col();
        if line == 0 {
            edit.cursor = 0;
            return;
        }
        edit.cursor = edit.line_col_to_byte(line - 1, col);
    }

    pub fn edit_move_down(&mut self, with_selection: bool) {
        let Some(edit) = self.editing.as_mut() else { return };
        edit.prepare_selection(with_selection);
        let (line, col) = edit.cursor_line_col();
        let total_lines = edit.line_count();
        if line + 1 >= total_lines {
            edit.cursor = edit.text.len();
            return;
        }
        edit.cursor = edit.line_col_to_byte(line + 1, col);
    }

    pub fn edit_move_home(&mut self, with_selection: bool) {
        let Some(edit) = self.editing.as_mut() else { return };
        edit.prepare_selection(with_selection);
        let (line, _) = edit.cursor_line_col();
        edit.cursor = edit.line_col_to_byte(line, 0);
    }

    pub fn edit_move_end(&mut self, with_selection: bool) {
        let Some(edit) = self.editing.as_mut() else { return };
        edit.prepare_selection(with_selection);
        let (line, _) = edit.cursor_line_col();
        edit.cursor = edit.line_end_byte(line);
    }

    pub fn edit_select_all(&mut self) {
        let Some(edit) = self.editing.as_mut() else { return };
        edit.selection_start = Some(0);
        edit.cursor = edit.text.len();
    }

    pub fn edit_get_selected_text(&self) -> Option<String> {
        let edit = self.editing.as_ref()?;
        let start = edit.selection_start?;
        if start == edit.cursor {
            return None;
        }
        let (a, b) = if start < edit.cursor {
            (start, edit.cursor)
        } else {
            (edit.cursor, start)
        };
        Some(edit.text[a..b].to_string())
    }

    pub fn edit_paste(&mut self, text: &str) {
        let Some(edit) = self.editing.as_mut() else { return };
        edit.delete_selection();
        edit.text.insert_str(edit.cursor, text);
        edit.cursor += text.len();
    }

    pub fn formula_bar_label_width(&self) -> usize {
        if self.is_editing() {
            format!(" {} (editing) ", self.cursor_label()).chars().count()
        } else {
            format!(" {} ", self.cursor_label()).chars().count()
        }
    }

    pub fn hit_test(
        &self,
        screen_col: u16,
        screen_row: u16,
        term_width: u16,
        term_height: u16,
    ) -> GridHit {
        let col = screen_col as usize;
        let row = screen_row as usize;

        if row < FORMULA_BAR_HEIGHT {
            let label_width = self.formula_bar_label_width();
            let text_col = col.saturating_sub(label_width);
            return GridHit::FormulaBar { row, text_col };
        }
        if row == FORMULA_BAR_HEIGHT {
            return GridHit::Divider;
        }

        let header_row = FORMULA_BAR_HEIGHT + 1;
        let data_start = header_row + 1;
        let status_row = (term_height as usize).saturating_sub(1);
        let visible_data_rows = status_row.saturating_sub(data_start);

        let is_header_row = row == header_row;
        let is_data_row = row >= data_start && row < data_start + visible_data_rows;

        if !is_header_row && !is_data_row {
            return GridHit::Outside;
        }

        if col < ROW_NUM_WIDTH {
            if is_data_row {
                let row_idx = self.scroll_row + (row - data_start);
                if row_idx < self.num_rows() {
                    return GridHit::RowNumber { row: row_idx };
                }
            }
            return GridHit::Outside;
        }

        // Fixed separator between row-num and first visible column
        if col == ROW_NUM_WIDTH {
            return GridHit::Outside;
        }

        let mut pos = ROW_NUM_WIDTH + 1;
        let mut cur_col = self.scroll_col;
        while cur_col < self.num_cols() && pos < term_width as usize {
            let w = self
                .column_widths
                .get(cur_col)
                .copied()
                .unwrap_or(MIN_COL_WIDTH);
            if col >= pos && col < pos + w {
                if is_header_row {
                    return GridHit::ColumnHeader { col: cur_col };
                }
                let row_idx = self.scroll_row + (row - data_start);
                if row_idx < self.num_rows() {
                    return GridHit::DataCell { row: row_idx, col: cur_col };
                }
                return GridHit::Outside;
            }
            let sep_pos = pos + w;
            if col == sep_pos {
                return GridHit::ColumnSeparator { col: cur_col };
            }
            pos = sep_pos + 1;
            cur_col += 1;
        }
        GridHit::Outside
    }

    pub fn move_to(&mut self, row: usize, col: usize, with_selection: bool) {
        self.prepare_selection(with_selection);
        let last_row = self.num_rows().saturating_sub(1);
        let last_col = self.num_cols().saturating_sub(1);
        self.cursor = (row.min(last_row), col.min(last_col));
    }

    /// Select the entire column `col`. Cursor lands at (0, col); anchor at (last_row, col).
    /// When `extend` is true, keeps the existing cursor column and moves the anchor to `col`,
    /// spanning all rows between the two columns.
    pub fn select_column(&mut self, col: usize, extend: bool) {
        if self.num_rows() == 0 || self.num_cols() == 0 {
            return;
        }
        let last_row = self.num_rows() - 1;
        let col = col.min(self.num_cols() - 1);
        if extend {
            self.cursor.0 = 0;
            self.selection_anchor = Some((last_row, col));
        } else {
            self.cursor = (0, col);
            self.selection_anchor = Some((last_row, col));
        }
    }

    /// Extend an in-progress column-header drag to include column `col`.
    pub fn extend_column_selection(&mut self, col: usize) {
        if self.num_rows() == 0 || self.num_cols() == 0 {
            return;
        }
        let last_row = self.num_rows() - 1;
        let col = col.min(self.num_cols() - 1);
        self.selection_anchor = Some((last_row, col));
    }

    /// Select the entire row `row`. Cursor lands at (row, 0); anchor at (row, last_col).
    pub fn select_row(&mut self, row: usize, extend: bool) {
        if self.num_rows() == 0 || self.num_cols() == 0 {
            return;
        }
        let last_col = self.num_cols() - 1;
        let row = row.min(self.num_rows() - 1);
        if extend {
            self.cursor.1 = 0;
            self.selection_anchor = Some((row, last_col));
        } else {
            self.cursor = (row, 0);
            self.selection_anchor = Some((row, last_col));
        }
    }

    /// Extend an in-progress row-number drag to include row `row`.
    pub fn extend_row_selection(&mut self, row: usize) {
        if self.num_rows() == 0 || self.num_cols() == 0 {
            return;
        }
        let last_col = self.num_cols() - 1;
        let row = row.min(self.num_rows() - 1);
        self.selection_anchor = Some((row, last_col));
    }

    pub fn set_column_width(&mut self, col: usize, width: usize) {
        if col < self.column_widths.len() {
            self.column_widths[col] = width.clamp(MIN_COL_WIDTH, MAX_RESIZE_WIDTH);
        }
    }

    /// Auto-size a column to fit its widest cell (plus some padding), capped at MAX_RESIZE_WIDTH.
    pub fn auto_size_column(&mut self, col: usize) {
        if col >= self.column_widths.len() {
            return;
        }
        let header_hint = col_letter(col).chars().count().max(MIN_COL_WIDTH);
        let mut w = header_hint;
        for row in &self.rows {
            if let Some(cell) = row.get(col) {
                let cw = cell_grid_width(cell);
                if cw > w {
                    w = cw;
                }
            }
        }
        self.column_widths[col] = w.clamp(MIN_COL_WIDTH, MAX_RESIZE_WIDTH);
    }

    /// Case-insensitive substring search across all cells. Returns matches in row-major order.
    pub fn find_cells(&self, needle: &str) -> Vec<(usize, usize)> {
        if needle.is_empty() {
            return Vec::new();
        }
        let needle_lower = needle.to_lowercase();
        let mut out = Vec::new();
        for (r, row) in self.rows.iter().enumerate() {
            for (c, cell) in row.iter().enumerate() {
                if cell.to_lowercase().contains(&needle_lower) {
                    out.push((r, c));
                }
            }
        }
        out
    }

    pub fn selection_metrics(&self) -> SelectionMetrics {
        let ((r0, c0), (r1, c1)) = self.selected_range();
        let mut m = SelectionMetrics::default();
        m.total_cells = (r1 - r0 + 1) * (c1 - c0 + 1);
        for r in r0..=r1 {
            for c in c0..=c1 {
                let cell = self.cell(r, c).trim();
                if cell.is_empty() {
                    continue;
                }
                m.non_empty += 1;
                if let Some(n) = parse_number(cell) {
                    m.numbers.push(n);
                } else if let Some((secs, has_time, tz_offset)) = parse_iso_datetime(cell) {
                    m.dates.push(secs);
                    m.dates_have_time |= has_time;
                    if let Some(off) = tz_offset {
                        // Track whether all tz-aware values share one offset.
                        m.tz = match m.tz {
                            TzAgg::None => TzAgg::Uniform(off),
                            TzAgg::Uniform(prev) if prev == off => TzAgg::Uniform(off),
                            _ => TzAgg::Mixed,
                        };
                    }
                }
            }
        }
        m
    }

    pub fn formula_bar_text_to_byte(&self, row: usize, text_col: usize) -> usize {
        let text = if let Some(edit) = &self.editing {
            edit.text.as_str()
        } else {
            self.focused_cell_text()
        };
        let mut line_start = 0usize;
        let mut line_idx = 0usize;
        let mut line_end = text.len();
        let mut found = false;
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                if line_idx == row {
                    line_end = i;
                    found = true;
                    break;
                }
                line_start = i + 1;
                line_idx += 1;
            }
        }
        if !found && line_idx < row {
            return text.len();
        }
        let slice = &text[line_start..line_end];
        let mut bytes = 0usize;
        let mut chars = 0usize;
        for ch in slice.chars() {
            if chars >= text_col {
                break;
            }
            bytes += ch.len_utf8();
            chars += 1;
        }
        line_start + bytes.min(line_end - line_start)
    }

    pub fn edit_set_cursor(&mut self, byte: usize, with_selection: bool) {
        if let Some(edit) = self.editing.as_mut() {
            if with_selection {
                if edit.selection_start.is_none() {
                    edit.selection_start = Some(edit.cursor);
                }
            } else {
                edit.selection_start = None;
            }
            let mut b = byte.min(edit.text.len());
            while b > 0 && !edit.text.is_char_boundary(b) {
                b -= 1;
            }
            edit.cursor = b;
        }
    }

    pub fn begin_mouse_cell_select(&mut self, row: usize, col: usize, shift: bool) {
        if self.is_editing() {
            self.commit_edit();
        }
        self.move_to(row, col, shift);
        self.mouse_mode = MouseMode::CellSelect;
    }

    pub fn begin_mouse_column_resize(&mut self, col: usize, anchor_screen_col: u16) {
        let anchor_width = self
            .column_widths
            .get(col)
            .copied()
            .unwrap_or(MIN_COL_WIDTH);
        self.mouse_mode = MouseMode::ColumnResize {
            col,
            anchor_screen_col,
            anchor_width,
        };
    }

    pub fn begin_mouse_formula_bar_select(&mut self, row: usize, text_col: usize, shift: bool) {
        if !self.is_editing() {
            self.enter_edit_mode();
        }
        let byte = self.formula_bar_text_to_byte(row, text_col);
        self.edit_set_cursor(byte, shift);
        self.mouse_mode = MouseMode::FormulaBarSelect;
    }

    pub fn end_mouse(&mut self) {
        self.mouse_mode = MouseMode::None;
    }

    pub fn scroll_by(&mut self, row_delta: i32, col_delta: i32) {
        let rows = self.num_rows();
        let cols = self.num_cols();
        self.scroll_row = ((self.scroll_row as i32 + row_delta)
            .max(0) as usize)
            .min(rows.saturating_sub(1));
        self.scroll_col = ((self.scroll_col as i32 + col_delta)
            .max(0) as usize)
            .min(cols.saturating_sub(1));
    }

    pub fn ensure_cursor_visible(&mut self, visible_rows: usize, visible_width: usize) {
        if self.cursor.0 < self.scroll_row {
            self.scroll_row = self.cursor.0;
        } else if visible_rows > 0 && self.cursor.0 >= self.scroll_row + visible_rows {
            self.scroll_row = self.cursor.0 + 1 - visible_rows;
        }

        if self.cursor.1 < self.scroll_col {
            self.scroll_col = self.cursor.1;
        } else {
            let mut width_needed = ROW_NUM_WIDTH + 1;
            for c in self.scroll_col..=self.cursor.1 {
                let cw = self.column_widths.get(c).copied().unwrap_or(MIN_COL_WIDTH);
                width_needed += cw + 1;
            }
            while width_needed > visible_width && self.scroll_col < self.cursor.1 {
                let cw = self
                    .column_widths
                    .get(self.scroll_col)
                    .copied()
                    .unwrap_or(MIN_COL_WIDTH);
                width_needed -= cw + 1;
                self.scroll_col += 1;
            }
        }
    }

    fn recompute_column_widths(&mut self) {
        let num_cols = self.num_cols();
        let mut widths = vec![MIN_COL_WIDTH; num_cols];
        for row in &self.rows {
            for (c, cell) in row.iter().enumerate() {
                if c >= widths.len() {
                    continue;
                }
                let w = cell_grid_width(cell);
                if w > widths[c] {
                    widths[c] = w.min(MAX_COL_WIDTH);
                }
            }
        }
        for w in widths.iter_mut() {
            *w = (*w).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH);
        }
        self.column_widths = widths;
    }

    fn recompute_col_width(&mut self, col: usize) {
        let mut w = MIN_COL_WIDTH;
        for row in &self.rows {
            if let Some(cell) = row.get(col) {
                let cw = cell_grid_width(cell);
                if cw > w {
                    w = cw.min(MAX_COL_WIDTH);
                }
            }
        }
        if col < self.column_widths.len() {
            self.column_widths[col] = w.clamp(MIN_COL_WIDTH, MAX_COL_WIDTH);
        }
    }
}

impl CellEdit {
    fn delete_selection(&mut self) -> bool {
        let Some(start) = self.selection_start.take() else { return false };
        if start == self.cursor {
            return false;
        }
        let (a, b) = if start < self.cursor {
            (start, self.cursor)
        } else {
            (self.cursor, start)
        };
        self.text.replace_range(a..b, "");
        self.cursor = a;
        true
    }

    fn prepare_selection(&mut self, with_selection: bool) {
        if with_selection {
            if self.selection_start.is_none() {
                self.selection_start = Some(self.cursor);
            }
        } else {
            self.selection_start = None;
        }
    }

    fn cursor_line_col(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.bytes().filter(|&b| b == b'\n').count();
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let col = self.text[line_start..self.cursor].chars().count();
        (line, col)
    }

    fn line_count(&self) -> usize {
        self.text.bytes().filter(|&b| b == b'\n').count() + 1
    }

    fn line_start_byte(&self, line: usize) -> usize {
        if line == 0 {
            return 0;
        }
        let mut count = 0;
        for (i, b) in self.text.bytes().enumerate() {
            if b == b'\n' {
                count += 1;
                if count == line {
                    return i + 1;
                }
            }
        }
        self.text.len()
    }

    fn line_end_byte(&self, line: usize) -> usize {
        let start = self.line_start_byte(line);
        match self.text[start..].find('\n') {
            Some(off) => start + off,
            None => self.text.len(),
        }
    }

    fn line_col_to_byte(&self, line: usize, col: usize) -> usize {
        let start = self.line_start_byte(line);
        let end = self.line_end_byte(line);
        let slice = &self.text[start..end];
        let mut bytes = 0;
        for (i, _) in slice.char_indices().take(col) {
            bytes = i + slice[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
        }
        start + bytes.min(end - start)
    }

}

fn detect_delimiter(path: &Path) -> u8 {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("tsv") => b'\t',
        _ => b',',
    }
}

fn cell_grid_width(s: &str) -> usize {
    let first_line = s.split('\n').next().unwrap_or("");
    first_line.chars().map(|c| c.width().unwrap_or(1)).sum()
}

pub fn col_letter(mut col: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (col % 26) as u8);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn prev_char_boundary(s: &str, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos - 1;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn next_char_boundary(s: &str, pos: usize) -> usize {
    if pos >= s.len() {
        return s.len();
    }
    let mut i = pos + 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Truncate a string to fit within a given display width. Replaces the last visible character
/// with '…' if truncation happens. Pads with spaces to exactly fill `width`.
pub fn render_cell_text(s: &str, width: usize) -> String {
    let first_line = s.split('\n').next().unwrap_or("");
    let has_more_lines = s.contains('\n');

    let mut total: usize = 0;
    let mut out = String::new();
    let mut chars = first_line.chars().peekable();

    while let Some(ch) = chars.next() {
        let cw = ch.width().unwrap_or(1);
        if total + cw > width {
            break;
        }
        out.push(ch);
        total += cw;
    }

    let truncated_line = out.chars().count() < first_line.chars().count();
    if (truncated_line || has_more_lines) && width >= 1 {
        while total >= width && !out.is_empty() {
            if let Some(last) = out.pop() {
                total -= last.width().unwrap_or(1);
            }
        }
        if total < width {
            out.push('…');
            total += 1;
        }
    }

    while total < width {
        out.push(' ');
        total += 1;
    }
    out
}

/// Parse a number from cell text, tolerant of common spreadsheet formatting.
/// Accepts: leading `$`, thousands separators (commas), leading/trailing `%`,
/// optional sign, scientific notation. Rejects empty/whitespace-only.
pub fn parse_number(raw: &str) -> Option<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let mut cleaned = String::with_capacity(s.len());
    let mut has_percent = false;
    let mut first = true;
    for ch in s.chars() {
        match ch {
            '$' if first => {}
            ',' => {}
            '%' => has_percent = true,
            _ => cleaned.push(ch),
        }
        first = false;
    }
    let n: f64 = cleaned.trim().parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    Some(if has_percent { n / 100.0 } else { n })
}

/// Parse an ISO-format date (YYYY-MM-DD). Returns days since 1970-01-01 (UNIX epoch).
pub fn parse_iso_date(raw: &str) -> Option<i64> {
    let s = raw.trim();
    // Fast reject: require len 10 and dashes at positions 4 and 7.
    if s.len() != 10 {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = s[0..4].parse().ok()?;
    let m: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    if m < 1 || m > 12 || d < 1 || d > 31 {
        return None;
    }
    Some(ymd_to_epoch_days(y, m, d))
}

/// Parse an ISO-style date or datetime into `(epoch_seconds_utc, has_time,
/// tz_offset)`. Accepts a `YYYY-MM-DD` date, optionally followed by a ` ` or
/// `T` separator and a `HH:MM`, `HH:MM:SS`, or `HH:MM:SS.fff` time (fractional
/// seconds are truncated), optionally followed by a `Z` or `±HH:MM` timezone.
/// The returned seconds are normalized to UTC; `tz_offset` is the offset in
/// seconds when one was present (`None` for a naive time). Recognizes the
/// `YYYY-MM-DD HH:MM:SS` and `YYYY-MM-DD HH:MM:SS±HH:MM` forms Snowflake renders
/// for TIMESTAMP / TIMESTAMP_TZ columns. Returns `None` for anything else.
pub fn parse_iso_datetime(raw: &str) -> Option<(i64, bool, Option<i64>)> {
    let s = raw.trim();
    // The leading 10 chars must be a valid YYYY-MM-DD date.
    let days = parse_iso_date(s.get(0..10)?)?;
    let mut secs = days * 86_400;
    let rest = &s[10..];
    if rest.is_empty() {
        return Some((secs, false, None));
    }
    // A separator (space or 'T') must follow, then HH:MM[:SS[.fff]][tz].
    match rest.as_bytes()[0] {
        b' ' | b'T' => {}
        _ => return None,
    }
    // Peel off a trailing timezone before splitting the time on ':'.
    let (time, tz_offset) = split_timezone(&rest[1..])?;
    let mut fields = time.split(':');
    let hh: i64 = fields.next()?.parse().ok()?;
    let mm: i64 = fields.next()?.parse().ok()?;
    let ss: i64 = match fields.next() {
        // Drop any fractional-second suffix before parsing.
        Some(sec) => sec.split('.').next().unwrap_or("").parse().ok()?,
        None => 0,
    };
    if !(0..24).contains(&hh) || !(0..60).contains(&mm) || !(0..60).contains(&ss) {
        return None;
    }
    secs += hh * 3600 + mm * 60 + ss;
    // Normalize to UTC: a +HH:MM offset means local time runs ahead of UTC.
    secs -= tz_offset.unwrap_or(0);
    Some((secs, true, tz_offset))
}

/// Split a `HH:MM[:SS[.fff]]` time from an optional trailing timezone. Returns
/// `(time, offset_seconds)`: offset is `None` for a naive time, `Some(0)` for
/// `Z`, and `Some(±seconds)` for a `±HH:MM` / `±HHMM` / `±HH` offset. The time
/// itself never contains `+`/`-`, so the first such char marks the offset.
fn split_timezone(s: &str) -> Option<(&str, Option<i64>)> {
    if let Some(time) = s.strip_suffix(|c| c == 'Z' || c == 'z') {
        return Some((time, Some(0)));
    }
    let Some(pos) = s.find(|c| c == '+' || c == '-') else {
        return Some((s, None));
    };
    let (time, tz) = s.split_at(pos);
    let sign = if tz.starts_with('-') { -1 } else { 1 };
    let body = &tz[1..];
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h, m),
        None if body.len() == 4 => (&body[0..2], &body[2..4]),
        None => (body, "0"),
    };
    let oh: i64 = h.parse().ok()?;
    let om: i64 = m.parse().ok()?;
    if !(0..24).contains(&oh) || !(0..60).contains(&om) {
        return None;
    }
    Some((time, Some(sign * (oh * 3600 + om * 60))))
}

/// Convert Y/M/D (proleptic Gregorian) to days since 1970-01-01.
/// Uses Howard Hinnant's algorithm.
pub fn ymd_to_epoch_days(y: i32, m: u32, d: u32) -> i64 {
    let y = y as i64;
    let m = m as i64;
    let d = d as i64;
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m_eff = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_eff + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn epoch_days_to_ymd(z: i64) -> (i32, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

pub fn format_iso_date(days: i64) -> String {
    let (y, m, d) = epoch_days_to_ymd(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Format epoch seconds (UTC) as `YYYY-MM-DD HH:MM:SS`.
pub fn format_iso_datetime(secs: i64) -> String {
    let tod = secs.rem_euclid(86_400);
    format!(
        "{} {:02}:{:02}:{:02}",
        format_iso_date(secs.div_euclid(86_400)),
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
    )
}

/// Format a timezone offset (seconds) as `±HH:MM` (e.g. `+05:30`, `-08:00`).
pub fn format_tz_offset(secs: i64) -> String {
    let sign = if secs < 0 { '-' } else { '+' };
    let a = secs.abs();
    format!("{}{:02}:{:02}", sign, a / 3600, (a % 3600) / 60)
}

fn fmt_num(n: f64) -> String {
    let abs = n.abs();
    if !n.is_finite() {
        return format!("{}", n);
    }
    if abs >= 1e15 {
        return format!("{:.3e}", n);
    }
    let is_integerish = (n - n.round()).abs() < 1e-9;
    if is_integerish && abs < 1e15 {
        fmt_int_with_commas(n as i64)
    } else {
        // Show up to 4 decimal places but strip trailing zeros.
        let raw = format!("{:.4}", n);
        let (int_part, frac_part) = match raw.split_once('.') {
            Some((a, b)) => (a.to_string(), b.trim_end_matches('0').to_string()),
            None => (raw, String::new()),
        };
        let int_with_commas = {
            let neg = int_part.starts_with('-');
            let digits: String = int_part.trim_start_matches('-').to_string();
            let mut out = String::new();
            for (i, ch) in digits.chars().rev().enumerate() {
                if i > 0 && i % 3 == 0 {
                    out.push(',');
                }
                out.push(ch);
            }
            let mut s: String = out.chars().rev().collect();
            if neg {
                s.insert(0, '-');
            }
            s
        };
        if frac_part.is_empty() {
            int_with_commas
        } else {
            format!("{}.{}", int_with_commas, frac_part)
        }
    }
}

fn fmt_int_with_commas(n: i64) -> String {
    let neg = n < 0;
    let abs_digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in abs_digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    let mut s: String = out.chars().rev().collect();
    if neg {
        s.insert(0, '-');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_tmp(name: &str, contents: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn parses_basic_csv() {
        let path = write_tmp("sage_test_basic.csv", "a,b,c\n1,2,3\n4,5,6\n");
        let ss = Spreadsheet::from_file(&path).unwrap();
        assert_eq!(ss.num_rows(), 3);
        assert_eq!(ss.num_cols(), 3);
        assert_eq!(ss.cell(0, 0), "a");
        assert_eq!(ss.cell(2, 2), "6");
    }

    #[test]
    fn handles_quoted_fields_with_commas() {
        let path = write_tmp(
            "sage_test_quoted.csv",
            "id,note\n1,\"hello, world\"\n2,plain\n",
        );
        let ss = Spreadsheet::from_file(&path).unwrap();
        assert_eq!(ss.cell(1, 1), "hello, world");
        assert_eq!(ss.cell(2, 1), "plain");
    }

    #[test]
    fn handles_newlines_in_cells() {
        let path = write_tmp(
            "sage_test_nl.csv",
            "a,b\n1,\"line1\nline2\"\n",
        );
        let ss = Spreadsheet::from_file(&path).unwrap();
        assert_eq!(ss.cell(1, 1), "line1\nline2");
    }

    #[test]
    fn normalizes_jagged_rows() {
        let path = write_tmp("sage_test_jagged.csv", "a,b,c\n1\n2,3\n");
        let ss = Spreadsheet::from_file(&path).unwrap();
        assert_eq!(ss.num_cols(), 3);
        assert_eq!(ss.cell(1, 0), "1");
        assert_eq!(ss.cell(1, 1), "");
        assert_eq!(ss.cell(2, 2), "");
    }

    #[test]
    fn navigation_moves_cursor() {
        let path = write_tmp("sage_test_nav.csv", "a,b\n1,2\n3,4\n");
        let mut ss = Spreadsheet::from_file(&path).unwrap();
        assert_eq!(ss.cursor, (0, 0));
        ss.move_down(false);
        ss.move_right(false);
        assert_eq!(ss.cursor, (1, 1));
        ss.move_home(false);
        assert_eq!(ss.cursor, (1, 0));
        ss.move_bottom_right(false);
        assert_eq!(ss.cursor, (2, 1));
    }

    #[test]
    fn selection_and_copy_tsv() {
        let path = write_tmp("sage_test_copy.csv", "a,b,c\n1,2,3\n4,5,6\n");
        let mut ss = Spreadsheet::from_file(&path).unwrap();
        ss.cursor = (1, 0);
        ss.selection_anchor = Some((2, 1));
        let tsv = ss.copy_selection_tsv();
        assert_eq!(tsv, "1\t2\n4\t5");
    }

    #[test]
    fn copy_tsv_quotes_special() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![vec!["has\ttab".to_string(), "has\nnewline".to_string()]];
        ss.column_widths = vec![MIN_COL_WIDTH, MIN_COL_WIDTH];
        ss.cursor = (0, 0);
        ss.selection_anchor = Some((0, 1));
        let tsv = ss.copy_selection_tsv();
        assert_eq!(tsv, "\"has\ttab\"\t\"has\nnewline\"");
    }

    #[test]
    fn edit_roundtrip() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.enter_edit_mode();
        ss.edit_insert_char('h');
        ss.edit_insert_char('i');
        ss.commit_edit();
        assert_eq!(ss.cell(0, 0), "hi");
        assert!(ss.is_modified());
    }

    #[test]
    fn edit_multiline_up_down() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.enter_edit_mode();
        for ch in "abc".chars() {
            ss.edit_insert_char(ch);
        }
        ss.edit_insert_newline();
        for ch in "de".chars() {
            ss.edit_insert_char(ch);
        }
        // cursor at byte 6 ("abc\nde")
        ss.edit_move_up(false);
        // should be on line 0 at col 2 (since line below had len 2)
        let edit = ss.editing.as_ref().unwrap();
        assert_eq!(&edit.text[..edit.cursor], "ab");
    }

    #[test]
    fn save_roundtrip_preserves_content() {
        let path = write_tmp(
            "sage_test_save.csv",
            "id,label\n1,\"has, comma\"\n2,plain\n",
        );
        let mut ss = Spreadsheet::from_file(&path).unwrap();
        ss.cursor = (1, 1);
        ss.enter_edit_mode();
        ss.edit_move_end(false);
        for ch in " edited".chars() {
            ss.edit_insert_char(ch);
        }
        ss.commit_edit();
        ss.save(&path).unwrap();

        let ss2 = Spreadsheet::from_file(&path).unwrap();
        assert_eq!(ss2.cell(1, 1), "has, comma edited");
        assert_eq!(ss2.cell(2, 1), "plain");
    }

    #[test]
    fn distinguishes_null_from_empty_on_load() {
        // Middle field unquoted-empty (,,) is a null; quoted "" is an empty string.
        let path = write_tmp("sage_test_nulls.csv", "a,b,c\n1,,3\n4,\"\",6\n");
        let ss = Spreadsheet::from_file(&path).unwrap();
        assert!(ss.is_null(1, 1));
        assert_eq!(ss.cell(1, 1), "");
        assert!(!ss.is_null(2, 1));
        assert_eq!(ss.cell(2, 1), "");
        assert!(!ss.is_null(1, 0)); // an ordinary value
    }

    #[test]
    fn save_preserves_null_vs_empty() {
        let path = write_tmp("sage_test_null_save.csv", "a,b,c\n1,,3\n4,\"\",6\n");
        let mut ss = Spreadsheet::from_file(&path).unwrap();
        ss.save(&path).unwrap();
        // Null stays an unquoted-empty field; the empty string stays quoted.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a,b,c\n1,,3\n4,\"\",6\n");
        let ss2 = Spreadsheet::from_file(&path).unwrap();
        assert!(ss2.is_null(1, 1));
        assert!(!ss2.is_null(2, 1));
    }

    #[test]
    fn editing_a_null_cell_clears_null() {
        let path = write_tmp("sage_test_null_edit.csv", "a,b\n1,\n");
        let mut ss = Spreadsheet::from_file(&path).unwrap();
        assert!(ss.is_null(1, 1));
        ss.cursor = (1, 1);
        ss.enter_edit_mode();
        ss.edit_insert_char('x');
        ss.commit_edit();
        assert!(!ss.is_null(1, 1));
        assert_eq!(ss.cell(1, 1), "x");
    }

    #[test]
    fn clearing_a_null_cell_makes_it_empty_string() {
        let path = write_tmp("sage_test_null_clear.csv", "a,b\n1,\n");
        let mut ss = Spreadsheet::from_file(&path).unwrap();
        assert!(ss.is_null(1, 1));
        ss.cursor = (1, 1);
        ss.selection_anchor = Some((1, 1));
        ss.clear_selection_content();
        assert!(!ss.is_null(1, 1));
    }

    #[test]
    fn col_letter_basic() {
        assert_eq!(col_letter(0), "A");
        assert_eq!(col_letter(25), "Z");
        assert_eq!(col_letter(26), "AA");
        assert_eq!(col_letter(27), "AB");
        assert_eq!(col_letter(51), "AZ");
        assert_eq!(col_letter(52), "BA");
    }

    #[test]
    fn hit_test_finds_cells() {
        // 3 cols with widths 4, 4, 4. Layout per row:
        // chars 0..5 = row num (5 chars: "    1" etc)
        // char 5 = separator │
        // chars 6..10 = col 0 (width 4)
        // char 10 = separator
        // chars 11..15 = col 1 (width 4)
        // char 15 = separator
        // chars 16..20 = col 2
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["a".into(), "b".into(), "c".into()],
            vec!["1".into(), "2".into(), "3".into()],
        ];
        ss.column_widths = vec![4, 4, 4];

        // Formula bar hit
        assert!(matches!(ss.hit_test(0, 0, 80, 24), GridHit::FormulaBar { .. }));
        // Header row — col 1 content
        match ss.hit_test(12, 4, 80, 24) {
            GridHit::ColumnHeader { col } => assert_eq!(col, 1),
            other => panic!("expected header col, got {:?}", other),
        }
        // Data row 0 — col 2
        match ss.hit_test(17, 5, 80, 24) {
            GridHit::DataCell { row, col } => {
                assert_eq!((row, col), (0, 2));
            }
            other => panic!("expected data cell, got {:?}", other),
        }
        // Separator between col 0 and col 1 (at position 10)
        match ss.hit_test(10, 5, 80, 24) {
            GridHit::ColumnSeparator { col } => assert_eq!(col, 0),
            other => panic!("expected col separator, got {:?}", other),
        }
        // Row number at row 1
        match ss.hit_test(2, 6, 80, 24) {
            GridHit::RowNumber { row } => assert_eq!(row, 1),
            other => panic!("expected row number, got {:?}", other),
        }
    }

    #[test]
    fn column_resize_updates_width() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![vec!["x".into(), "y".into()]];
        ss.column_widths = vec![4, 4];
        ss.begin_mouse_column_resize(0, 10);
        if let MouseMode::ColumnResize { col, anchor_screen_col, anchor_width } = ss.mouse_mode {
            assert_eq!(col, 0);
            assert_eq!(anchor_screen_col, 10);
            assert_eq!(anchor_width, 4);
        } else {
            panic!("expected ColumnResize mode");
        }
        ss.set_column_width(0, 12);
        assert_eq!(ss.column_widths[0], 12);
        // Clamp to min
        ss.set_column_width(0, 1);
        assert_eq!(ss.column_widths[0], MIN_COL_WIDTH);
        ss.end_mouse();
        assert_eq!(ss.mouse_mode, MouseMode::None);
    }

    #[test]
    fn formula_bar_byte_offset_multiline() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![vec!["first\nsecond".into()]];
        ss.column_widths = vec![4];
        // Not editing — we still compute offsets against cell text
        // byte offsets: "first" is 0..5, "\n" is 5, "second" is 6..12
        assert_eq!(ss.formula_bar_text_to_byte(0, 0), 0);
        assert_eq!(ss.formula_bar_text_to_byte(0, 5), 5);
        assert_eq!(ss.formula_bar_text_to_byte(1, 0), 6);
        assert_eq!(ss.formula_bar_text_to_byte(1, 3), 9);
        // Past the end clamps
        assert_eq!(ss.formula_bar_text_to_byte(1, 100), 12);
        assert_eq!(ss.formula_bar_text_to_byte(5, 0), 12);
    }

    #[test]
    fn edit_set_cursor_creates_selection() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.enter_edit_mode();
        for ch in "hello".chars() {
            ss.edit_insert_char(ch);
        }
        // Move cursor to byte 0, no selection
        ss.edit_set_cursor(0, false);
        assert!(ss.editing.as_ref().unwrap().selection_start.is_none());
        assert_eq!(ss.editing.as_ref().unwrap().cursor, 0);
        // Shift-click at byte 3: creates selection from 0 to 3
        ss.edit_set_cursor(3, true);
        let edit = ss.editing.as_ref().unwrap();
        assert_eq!(edit.selection_start, Some(0));
        assert_eq!(edit.cursor, 3);
    }

    #[test]
    fn find_cells_matches_partial_case_insensitive() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["Alice".into(), "engineer".into()],
            vec!["bob".into(), "ENGINE room".into()],
            vec!["carol".into(), "sales".into()],
        ];
        ss.column_widths = vec![8, 12];
        let hits = ss.find_cells("engine");
        assert_eq!(hits, vec![(0, 1), (1, 1)]);
        assert_eq!(ss.find_cells(""), Vec::<(usize, usize)>::new());
        assert_eq!(ss.find_cells("nothing"), Vec::<(usize, usize)>::new());
        assert_eq!(ss.find_cells("CAROL"), vec![(2, 0)]);
    }

    #[test]
    fn parse_number_accepts_common_formats() {
        assert_eq!(parse_number("42"), Some(42.0));
        assert_eq!(parse_number("  -1.5  "), Some(-1.5));
        assert_eq!(parse_number("$1,234.56"), Some(1234.56));
        assert_eq!(parse_number("50%"), Some(0.5));
        assert_eq!(parse_number("1e3"), Some(1000.0));
        assert_eq!(parse_number(""), None);
        assert_eq!(parse_number("hello"), None);
        assert_eq!(parse_number("12/31/2024"), None);
    }

    #[test]
    fn parse_date_roundtrip() {
        assert_eq!(parse_iso_date("2021-03-15"), Some(ymd_to_epoch_days(2021, 3, 15)));
        assert_eq!(parse_iso_date("1970-01-01"), Some(0));
        assert_eq!(format_iso_date(0), "1970-01-01");
        assert_eq!(format_iso_date(ymd_to_epoch_days(2024, 2, 29)), "2024-02-29");
        assert_eq!(parse_iso_date("2021/03/15"), None); // wrong separator
        assert_eq!(parse_iso_date("bad"), None);
        assert_eq!(parse_iso_date("2021-13-01"), None); // bad month
    }

    #[test]
    fn parse_datetime_accepts_timestamp_forms() {
        let midnight = ymd_to_epoch_days(2025, 10, 23) * 86_400;
        // Date only → midnight, has_time = false, no offset.
        assert_eq!(parse_iso_datetime("2025-10-23"), Some((midnight, false, None)));
        // Trailing whitespace is trimmed back to date-only.
        assert_eq!(parse_iso_datetime("  2025-10-23  "), Some((midnight, false, None)));
        // Space-separated datetime (the Snowflake NTZ rendering).
        let dt = midnight + 14 * 3600 + 30 * 60 + 15;
        assert_eq!(parse_iso_datetime("2025-10-23 14:30:15"), Some((dt, true, None)));
        // 'T' separator and fractional seconds (truncated to the second).
        assert_eq!(parse_iso_datetime("2025-10-23T14:30:15.500"), Some((dt, true, None)));
        // HH:MM with no seconds.
        assert_eq!(
            parse_iso_datetime("2025-10-23 14:30"),
            Some((midnight + 14 * 3600 + 30 * 60, true, None))
        );
        // Timezone offsets normalize to UTC and report the offset.
        // 05:30 at +05:30 == 00:00 UTC.
        assert_eq!(
            parse_iso_datetime("2025-10-23 05:30:00+05:30"),
            Some((midnight, true, Some(19_800)))
        );
        // 00:00 at -08:00 == 08:00 UTC the same day.
        assert_eq!(
            parse_iso_datetime("2025-10-23 00:00:00-08:00"),
            Some((midnight + 8 * 3600, true, Some(-28_800)))
        );
        // 'Z' == UTC.
        assert_eq!(parse_iso_datetime("2025-10-23T00:00:00Z"), Some((midnight, true, Some(0))));
        // Rejections.
        assert_eq!(parse_iso_datetime("2025-10-23 25:00:00"), None); // bad hour
        assert_eq!(parse_iso_datetime("2025-10-23 14:60"), None); // bad minute
        assert_eq!(parse_iso_datetime("2025-10-23x14:30"), None); // bad separator
        assert_eq!(parse_iso_datetime("not a date"), None);
    }

    #[test]
    fn format_datetime_renders_time() {
        assert_eq!(format_iso_datetime(0), "1970-01-01 00:00:00");
        let dt = ymd_to_epoch_days(2025, 10, 23) * 86_400 + 14 * 3600 + 30 * 60 + 15;
        assert_eq!(format_iso_datetime(dt), "2025-10-23 14:30:15");
    }

    #[test]
    fn metrics_numbers_sum_and_avg() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["10".into(), "20".into()],
            vec!["30".into(), "40".into()],
        ];
        ss.column_widths = vec![4, 4];
        ss.cursor = (0, 0);
        ss.selection_anchor = Some((1, 1));
        let m = ss.selection_metrics();
        assert_eq!(m.non_empty, 4);
        assert_eq!(m.numbers, vec![10.0, 20.0, 30.0, 40.0]);
        let s = m.format();
        assert!(s.contains("n 4/4"));
        assert!(s.contains("sum 100"));
        assert!(s.contains("avg 25"));
        assert!(s.contains("min 10"));
        assert!(s.contains("max 40"));
    }

    #[test]
    fn metrics_dates_min_max_avg() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["2020-01-01".into()],
            vec!["2022-01-01".into()],
            vec!["2024-01-01".into()],
        ];
        ss.column_widths = vec![12];
        ss.cursor = (0, 0);
        ss.selection_anchor = Some((2, 0));
        let m = ss.selection_metrics();
        assert_eq!(m.dates.len(), 3);
        assert!(!m.dates_have_time);
        let s = m.format();
        assert!(s.contains("min 2020-01-01"));
        assert!(s.contains("max 2024-01-01"));
        assert!(s.contains("avg 2022-01-01"));
    }

    #[test]
    fn metrics_datetimes_min_max_avg() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["2025-01-01 00:00:00".into()],
            vec!["2025-01-01 12:00:00".into()],
            vec!["2025-01-02 00:00:00".into()],
        ];
        ss.column_widths = vec![20];
        ss.cursor = (0, 0);
        ss.selection_anchor = Some((2, 0));
        let m = ss.selection_metrics();
        assert_eq!(m.dates.len(), 3);
        assert!(m.dates_have_time);
        let s = m.format();
        // Min/max/avg keep the time component (avg of 00:00, 12:00, +1d 00:00).
        assert!(s.contains("min 2025-01-01 00:00:00"), "got: {s}");
        assert!(s.contains("max 2025-01-02 00:00:00"), "got: {s}");
        assert!(s.contains("avg 2025-01-01 12:00:00"), "got: {s}");
    }

    #[test]
    fn metrics_tz_datetimes_keep_common_offset() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["2025-01-01 00:00:00+05:30".into()],
            vec!["2025-01-01 12:00:00+05:30".into()],
        ];
        ss.column_widths = vec![30];
        ss.cursor = (0, 0);
        ss.selection_anchor = Some((1, 0));
        let s = ss.selection_metrics().format();
        // A shared offset is preserved in the rendered aggregates.
        assert!(s.contains("min 2025-01-01 00:00:00+05:30"), "got: {s}");
        assert!(s.contains("max 2025-01-01 12:00:00+05:30"), "got: {s}");
        assert!(s.contains("avg 2025-01-01 06:00:00+05:30"), "got: {s}");
    }

    #[test]
    fn metrics_mixed_tz_offsets_fall_back_to_utc() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["2025-01-01 00:00:00+00:00".into()],
            vec!["2025-01-01 00:00:00+05:00".into()], // == 2024-12-31 19:00 UTC
        ];
        ss.column_widths = vec![30];
        ss.cursor = (0, 0);
        ss.selection_anchor = Some((1, 0));
        let s = ss.selection_metrics().format();
        // Differing offsets → aggregates rendered in UTC.
        assert!(s.contains("min 2024-12-31 19:00:00+00:00"), "got: {s}");
        assert!(s.contains("max 2025-01-01 00:00:00+00:00"), "got: {s}");
    }

    #[test]
    fn metrics_mixed_types_suppress_sum_avg() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["10".into(), "alice".into()],
            vec!["2021-03-15".into(), "20".into()],
        ];
        ss.column_widths = vec![12, 6];
        ss.cursor = (0, 0);
        ss.selection_anchor = Some((1, 1));
        let m = ss.selection_metrics();
        assert_eq!(m.non_empty, 4);
        // Has numbers AND a date AND a string — mixed → no sum/avg in output
        let s = m.format();
        assert!(s.starts_with("n 4/4"), "got: {}", s);
        assert!(!s.contains("sum"));
        assert!(!s.contains("avg"));
        assert!(!s.contains("min"));
        assert!(!s.contains("max"));
    }

    #[test]
    fn metrics_numbers_with_empties_still_sum() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["10".into(), "".into()],
            vec!["20".into(), "30".into()],
        ];
        ss.column_widths = vec![4, 4];
        ss.cursor = (0, 0);
        ss.selection_anchor = Some((1, 1));
        let m = ss.selection_metrics();
        // 3 non-empty, all numeric → sum/avg still shown
        let s = m.format();
        assert!(s.contains("n 3/4"));
        assert!(s.contains("sum 60"));
        assert!(s.contains("avg 20"));
    }

    #[test]
    fn select_column_spans_all_rows() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["a".into(), "b".into(), "c".into()],
            vec!["1".into(), "2".into(), "3".into()],
            vec!["4".into(), "5".into(), "6".into()],
        ];
        ss.column_widths = vec![4, 4, 4];
        ss.select_column(1, false);
        assert_eq!(ss.cursor, (0, 1));
        assert_eq!(ss.selection_anchor, Some((2, 1)));
        let ((r0, c0), (r1, c1)) = ss.selected_range();
        assert_eq!((r0, c0, r1, c1), (0, 1, 2, 1));
    }

    #[test]
    fn extend_column_selection_drags() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["a".into(), "b".into(), "c".into(), "d".into()],
            vec!["1".into(), "2".into(), "3".into(), "4".into()],
        ];
        ss.column_widths = vec![4, 4, 4, 4];
        // Click col A header
        ss.select_column(0, false);
        // Drag to col C
        ss.extend_column_selection(2);
        let ((r0, c0), (r1, c1)) = ss.selected_range();
        assert_eq!((r0, c0, r1, c1), (0, 0, 1, 2));
    }

    #[test]
    fn select_row_spans_all_cols() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["a".into(), "b".into(), "c".into()],
            vec!["1".into(), "2".into(), "3".into()],
            vec!["4".into(), "5".into(), "6".into()],
        ];
        ss.column_widths = vec![4, 4, 4];
        ss.select_row(1, false);
        assert_eq!(ss.cursor, (1, 0));
        assert_eq!(ss.selection_anchor, Some((1, 2)));
        let ((r0, c0), (r1, c1)) = ss.selected_range();
        assert_eq!((r0, c0, r1, c1), (1, 0, 1, 2));
    }

    #[test]
    fn extend_row_selection_drags() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["a".into(), "b".into()],
            vec!["1".into(), "2".into()],
            vec!["3".into(), "4".into()],
            vec!["5".into(), "6".into()],
        ];
        ss.column_widths = vec![4, 4];
        ss.select_row(0, false);
        ss.extend_row_selection(2);
        let ((r0, c0), (r1, c1)) = ss.selected_range();
        assert_eq!((r0, c0, r1, c1), (0, 0, 2, 1));
    }

    #[test]
    fn metrics_strings_only_count() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["alice".into(), "bob".into()],
            vec!["".into(), "carol".into()],
        ];
        ss.column_widths = vec![6, 6];
        ss.cursor = (0, 0);
        ss.selection_anchor = Some((1, 1));
        let m = ss.selection_metrics();
        assert_eq!(m.non_empty, 3);
        assert_eq!(m.total_cells, 4);
        assert!(m.numbers.is_empty());
        assert!(m.dates.is_empty());
        let s = m.format();
        assert!(s.contains("n 3/4"));
        assert!(!s.contains("sum"));
        assert!(!s.contains("avg"));
    }

    #[test]
    fn metrics_single_cell() {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![vec!["42".into()]];
        ss.column_widths = vec![4];
        let m = ss.selection_metrics();
        let s = m.format();
        // Single-cell: "n 1" without total
        assert!(s.starts_with("n 1"));
        assert!(s.contains("sum 42"));
        assert!(s.contains("avg 42"));
    }

    #[test]
    fn fmt_num_formats_correctly() {
        assert_eq!(fmt_num(1234.0), "1,234");
        assert_eq!(fmt_num(-1234567.0), "-1,234,567");
        assert_eq!(fmt_num(1234.5), "1,234.5");
        assert_eq!(fmt_num(0.0), "0");
        assert_eq!(fmt_num(1234.567), "1,234.567");
    }

    #[test]
    fn render_cell_truncation_and_padding() {
        // Short content: pad with spaces
        let out = render_cell_text("hi", 5);
        assert_eq!(out, "hi   ");
        // Long content: truncate and add ellipsis
        let out = render_cell_text("hello world", 6);
        assert_eq!(out.chars().count(), 6);
        assert!(out.ends_with('…'));
        // Multiline: show first line with ellipsis indicating more
        let out = render_cell_text("first\nsecond", 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.contains('…'));
    }
}
