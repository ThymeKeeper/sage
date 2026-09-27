use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io;
use std::path::Path;
use unicode_width::UnicodeWidthChar;

pub const MIN_COL_WIDTH: usize = 4;
/// Width of a ghost column (past the data) until something is typed into it.
pub const GHOST_COL_WIDTH: usize = 8;
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
    /// Row 1 is the header. Filters and sorts are view-only: `rows` keeps the
    /// file's order and Save writes every row. Grid positions (cursor, scroll,
    /// selection, `cell()`) are display rows, mapped through `view`.
    ///
    /// Display order while a filter or sort is active: the index into `rows`
    /// shown at each grid row, the header (0) always first. `None` is the file
    /// order, every row, so an unfiltered grid costs nothing on huge files.
    view: Option<Vec<usize>>,
    /// Active filters: column -> the filter keys (see `filter_key`) left visible.
    filters: BTreeMap<usize, HashSet<String>>,
    /// Sort levels as (column, descending), oldest first. Each is re-applied in
    /// order as a stable sort, so the latest is the primary key and earlier ones
    /// break its ties, as with successive sorts in Excel.
    sorts: Vec<(usize, bool)>,
    /// Undo history, oldest first: one step per committed edit or range clear.
    undo: Vec<UndoStep>,
    redo: Vec<UndoStep>,
    /// `undo.len()` at the last save (or load); `None` once that state can't
    /// be reached again (a new change after undoing past it). Drives `modified`.
    save_point: Option<usize>,
    /// The selection a click or drag on column letters or row numbers made,
    /// while it stands: it tells Ctrl+= and Ctrl+- to act on whole columns or
    /// rows even where the shape alone can't (a one-column file, say).
    picked_lines: Option<PickedLines>,
}

/// Whole rows or columns picked by their numbers or letters, with the
/// selection that made, so a later change to the selection retires it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PickedLines {
    columns: bool,
    anchor: Option<(usize, usize)>,
    cursor: (usize, usize),
}

/// What Ctrl+= and Ctrl+- act on: display rows or columns, first to last.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Lines {
    Rows(usize, usize),
    Cols(usize, usize),
}

/// One cell of an undo step, addressed by file row so it holds under any
/// filter or sort. `other` is the value on the far side of the step: undo and
/// redo swap it with the cell, so neither copies the text.
struct CellChange {
    row: usize,
    col: usize,
    other: String,
    other_null: bool,
}

struct UndoStep {
    changes: Vec<CellChange>,
    /// A column filter the step rewrote (date conversion maps its values so it
    /// keeps matching the same rows), held like `CellChange::other`: the filter
    /// on the far side of the step, swapped in by undo/redo.
    filter: Option<(usize, Option<HashSet<String>>)>,
    /// How the data grew to hold a value typed into a ghost cell; undo shrinks
    /// it back, redo grows it again.
    growth: Option<Growth>,
    /// Rows or columns inserted or deleted (such a step changes no cells).
    reshape: Option<Reshape>,
}

/// Rows or columns inserted or deleted (Ctrl+= / Ctrl+-). Each holds what the
/// step's other direction needs: undo reverses it, redo carries it out again.
/// Rows are file rows, as everywhere in undo; undo and redo run in order, so
/// the row and column numbers of the steps around it stay true.
enum Reshape {
    /// `count` blank rows put in at file row `at`, shown from display row
    /// `shown` while a filter or sort is on.
    InsertRows { at: usize, count: usize, shown: usize },
    DeleteRows(DeletedRows),
    /// `count` blank columns put in at column `at`.
    InsertCols { at: usize, count: usize },
    DeleteCols(DeletedCols),
}

struct DeletedRows {
    /// Each deleted row in file order: its file row, its display row while a
    /// filter or sort was on, and (while deleted) its cells and null mask.
    rows: Vec<(usize, Option<usize>, Vec<String>, Vec<bool>)>,
    /// With the header deleted, the length the row that took its place had
    /// before it was widened to the header's width.
    header_len: Option<usize>,
    /// Every row went, leaving an empty header row in their place.
    emptied: bool,
    /// With every row gone, the filters and sorts, which no column is left
    /// to hold (one can be put on between an undo and a redo of this).
    views: Option<(BTreeMap<usize, HashSet<String>>, Vec<(usize, bool)>)>,
}

struct DeletedCols {
    at: usize,
    count: usize,
    /// (While deleted) each row the columns cut into, with the cells taken.
    cells: Vec<(usize, Vec<String>, Vec<bool>)>,
    widths: Vec<usize>,
    /// Filters and sort levels (with their place in the sort order) on the
    /// deleted columns, which went with them.
    filters: Vec<(usize, HashSet<String>)>,
    sorts: Vec<(usize, usize, bool)>,
    /// With every column gone, how many (now empty) rows there were: the
    /// grid is then an empty one, a single empty header row.
    rows_before: Option<usize>,
}

/// The data's shape before it grew to hold typed or pasted cells.
#[derive(Debug, Clone)]
struct Growth {
    rows: usize,
    cols: usize,
    widths: usize,
    /// Data rows that already existed and were lengthened, each with its length
    /// before (the header's is `cols`). A row listed twice is shortest first.
    extended: Vec<(usize, usize)>,
}

/// How a cell orders in a sort: numbers (and ISO dates, as epoch seconds)
/// before text, text ignoring case, blanks last in either direction.
#[derive(Debug, Clone)]
enum SortKey {
    Num(f64),
    Text(String),
    Blank,
}

impl SortKey {
    fn of(raw: &str, is_null: bool) -> SortKey {
        // Blank means empty or null, as in the filter; a cell of spaces is text.
        if is_null || raw.is_empty() {
            return SortKey::Blank;
        }
        let t = raw.trim();
        if let Some(n) = parse_number(t) {
            SortKey::Num(n)
        } else if let Some((secs, _, _)) = parse_iso_datetime(t) {
            SortKey::Num(secs as f64)
        } else {
            SortKey::Text(raw.to_lowercase())
        }
    }

    fn cmp(&self, other: &SortKey, descending: bool) -> Ordering {
        let ord = match (self, other) {
            (SortKey::Blank, SortKey::Blank) => return Ordering::Equal,
            (SortKey::Blank, _) => return Ordering::Greater,
            (_, SortKey::Blank) => return Ordering::Less,
            (SortKey::Num(a), SortKey::Num(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            (SortKey::Num(_), SortKey::Text(_)) => Ordering::Less,
            (SortKey::Text(_), SortKey::Num(_)) => Ordering::Greater,
            (SortKey::Text(a), SortKey::Text(b)) => a.cmp(b),
        };
        if descending { ord.reverse() } else { ord }
    }
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
        Ok(Self::from_text(&content, delimiter, false).expect("lenient reading never refuses"))
    }

    /// Build a grid from delimited text. The header row (row 1) sets the width;
    /// rows shorter than it keep their missing trailing cells as nulls, which
    /// Save writes back as empty fields.
    ///
    /// `strict` is for switching a text buffer to a grid (Ctrl+Y): it refuses
    /// text that doesn't read as delimited data, with the reason: no delimiter
    /// at all, a quote left open, or a data row with more fields than the
    /// header row. Opening a .csv/.tsv is lenient and widens the header instead.
    pub fn from_text(text: &str, delimiter: u8, strict: bool) -> Result<Self, String> {
        // Strip a leading UTF-8 BOM. It is zero-width per Unicode, so it doesn't
        // count toward the column width, yet many terminals still render it as a
        // cell — which makes the first header cell look one column too wide. Text
        // mode strips it in Buffer::from_string; the grid loader must too.
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        // Empty text is an empty grid: no data, every cell a ghost cell to type into.
        if text.trim().is_empty() {
            return Ok(Self::new_empty(delimiter));
        }
        let (kind, seps) = if delimiter == b'\t' { ("TSV", "tabs") } else { ("CSV", "commas") };
        if strict {
            if !text.contains(delimiter as char) {
                return Err(format!("there are no {} in it, so it doesn't read as {}", seps, kind));
            }
            if let Some(line) = crate::dsv::open_quote_line(text) {
                return Err(format!("a quote opened on line {} is never closed", line));
            }
        }

        // Parse with null awareness: an unquoted-empty field is a null (shown as
        // ∅), a quoted "" is an empty string. `rows` holds "" for both; the
        // distinction lives in `null_mask`.
        let records = crate::dsv::parse(text, delimiter);
        if strict {
            let header = records.first().map_or(0, |r| r.len());
            if let Some(wide) = records.iter().skip(1).find(|r| r.len() > header) {
                // Quote the row itself (blank lines and multi-line values make a
                // row number differ from the editor's line number).
                let joined: String = wide
                    .iter()
                    .map(|f| f.as_deref().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join(&(delimiter as char).to_string())
                    .replace(['\n', '\r'], " ");
                let shown: String = joined.chars().take(40).collect();
                let more = if joined.chars().count() > 40 { "..." } else { "" };
                return Err(format!(
                    "the row \"{}{}\" has {} fields but the header row has only {}; the header needs a field (empty is fine) for every column",
                    shown.replace('\t', " "),
                    more,
                    wide.len(),
                    header
                ));
            }
        }
        let mut rows: Vec<Vec<String>> = Vec::with_capacity(records.len());
        let mut null_mask: Vec<Vec<bool>> = Vec::with_capacity(records.len());
        for record in records {
            let mut row = Vec::with_capacity(record.len());
            let mut mask = Vec::with_capacity(record.len());
            for field in record {
                mask.push(field.is_none());
                row.push(field.unwrap_or_default());
            }
            rows.push(row);
            null_mask.push(mask);
        }
        if rows.is_empty() {
            // Only blank lines: an empty grid (the header row, with no cells).
            rows = vec![Vec::new()];
            null_mask = vec![Vec::new()];
        }
        // The header row sets the width: widen it, with nulls, to the widest row.
        let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        while rows[0].len() < max_cols {
            rows[0].push(String::new());
            null_mask[0].push(true);
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
            view: None,
            filters: BTreeMap::new(),
            sorts: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            save_point: Some(0),
            picked_lines: None,
        };
        ss.recompute_column_widths();
        Ok(ss)
    }

    /// Mark the grid as holding changes the file on disk doesn't have (it was
    /// built from unsaved text), so undo can't clear the unsaved marker.
    pub fn mark_unsaved(&mut self) {
        self.modified = true;
        self.save_point = None;
    }

    /// Column `col`'s width on screen; ghost columns past the data get a default.
    pub fn col_width(&self, col: usize) -> usize {
        self.column_widths.get(col).copied().unwrap_or(GHOST_COL_WIDTH)
    }

    /// An empty grid: no data, so every row and column is a ghost; typing a
    /// value anywhere grows the data out to that cell. The header row is always
    /// there (row 1), just with no cells yet.
    pub fn new_empty(delimiter: u8) -> Self {
        Self {
            rows: vec![Vec::new()],
            null_mask: vec![Vec::new()],
            cursor: (0, 0),
            selection_anchor: None,
            column_widths: Vec::new(),
            scroll_row: 0,
            scroll_col: 0,
            delimiter,
            modified: false,
            editing: None,
            mouse_mode: MouseMode::None,
            view: None,
            filters: BTreeMap::new(),
            sorts: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            save_point: Some(0),
            picked_lines: None,
        }
    }

    pub fn save(&mut self, path: &Path) -> io::Result<()> {
        use std::io::Write;
        let mut writer = io::BufWriter::new(File::create(path)?);
        let mut line = String::new();
        for r in 0..self.rows_to_write() {
            line.clear();
            self.serialize_row(r, &mut line);
            writer.write_all(line.as_bytes())?;
        }
        writer.flush()?;
        self.modified = false;
        self.save_point = Some(self.undo.len());
        Ok(())
    }

    /// The grid as the text Save writes (for showing it as editable text).
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for r in 0..self.rows_to_write() {
            self.serialize_row(r, &mut out);
        }
        out
    }

    /// Rows Save writes: all of them, or none for an empty grid (no columns
    /// yet), which is an empty file.
    fn rows_to_write(&self) -> usize {
        if self.num_cols() == 0 { 0 } else { self.rows.len() }
    }

    /// Append file row `r` as a line of delimited text. The null-aware
    /// serializer keeps null cells as unquoted-empty fields and empty strings
    /// as quoted "" (see crate::dsv). Every row goes out to the header's width:
    /// a short row's missing cells are nulls, so they become empty fields and
    /// loaders see one column count throughout.
    fn serialize_row(&self, r: usize, out: &mut String) {
        let start = out.len();
        let empty: Vec<String> = Vec::new();
        let row = self.rows.get(r).unwrap_or(&empty);
        for c in 0..self.num_cols() {
            if c > 0 {
                out.push(self.delimiter as char);
            }
            let field = match row.get(c) {
                Some(cell) if !self.file_is_null(r, c) => Some(cell.as_str()),
                _ => None,
            };
            crate::dsv::serialize_field(out, field, self.delimiter);
        }
        // In a one-column file an empty row would be a blank line, which
        // readers (sage's included) skip, shifting every later row up.
        // Write it as an empty string so the row survives a reload.
        if out.len() == start {
            out.push_str("\"\"");
        }
        out.push('\n');
    }

    /// Rows in the grid as displayed (the header plus the rows a filter leaves).
    pub fn num_rows(&self) -> usize {
        self.view.as_ref().map_or(self.rows.len(), |v| v.len())
    }

    /// Rows in the file, header included, whatever the view shows.
    pub fn file_row_count(&self) -> usize {
        self.rows.len()
    }

    /// Index into `rows` (the file's row order) of display row `row`. Ghost rows
    /// past the data map to the file rows they would become.
    pub fn file_row(&self, row: usize) -> usize {
        match &self.view {
            Some(v) => v.get(row).copied().unwrap_or_else(|| self.rows.len() + (row - v.len())),
            None => row,
        }
    }

    /// Columns in the data: the header row's width.
    pub fn num_cols(&self) -> usize {
        self.rows.first().map(|r| r.len()).unwrap_or(0)
    }

    /// Width of the row-number gutter for a screen showing `visible_rows` data
    /// rows: sized to the largest row number on it (file rows, or the ghost rows
    /// past them), plus a trailing space, never narrower than the default.
    /// Sizing to the file keeps the header letters aligned with the data columns
    /// while scrolling, even when row numbers reach the millions.
    pub fn row_num_width(&self, visible_rows: usize) -> usize {
        let last_shown = self.scroll_row + visible_rows.max(1) - 1;
        let largest = self.file_row_count().max(self.file_row(last_shown) + 1);
        let digits = largest.max(1).to_string().len();
        (digits + 1).max(ROW_NUM_WIDTH)
    }

    /// Text of the cell at display row `row`.
    pub fn cell(&self, row: usize, col: usize) -> &str {
        self.file_cell(self.file_row(row), col)
    }

    /// Whether the cell at display row `row` is null (an unquoted-empty source
    /// field) as opposed to an empty string. Null cells hold `""` in `rows` but
    /// render as `∅`.
    pub fn is_null(&self, row: usize, col: usize) -> bool {
        self.file_is_null(self.file_row(row), col)
    }

    fn file_cell(&self, file_row: usize, col: usize) -> &str {
        self.rows
            .get(file_row)
            .and_then(|r| r.get(col))
            .map(|s| s.as_str())
            .unwrap_or("")
    }

    fn file_is_null(&self, file_row: usize, col: usize) -> bool {
        match self.null_mask.get(file_row) {
            // Past the end of a short row, a cell inside the header's width is
            // missing: a null, like the empty field Save writes for it.
            Some(mask) => mask.get(col).copied().unwrap_or(col < self.num_cols()),
            // Ghost rows past the data are blank, not null.
            None => false,
        }
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

    // --- Filter & sort (view-only; row 1 is the header) ---------------------

    /// The value a filter matches a cell on: its text, with nulls and empty
    /// strings both "" (listed as "(Blanks)").
    fn filter_key(&self, file_row: usize, col: usize) -> &str {
        if self.file_is_null(file_row, col) {
            ""
        } else {
            self.file_cell(file_row, col)
        }
    }

    fn passes_filters(&self, file_row: usize, skip_col: Option<usize>) -> bool {
        self.filters.iter().all(|(&col, allowed)| {
            Some(col) == skip_col || allowed.contains(self.filter_key(file_row, col))
        })
    }

    /// Distinct values in column `col` with how many rows hold each, over the
    /// data rows the other columns' filters leave (what Excel's dropdown lists).
    /// "" (blanks: empty or null) first, then as an ascending sort orders them.
    pub fn column_values(&self, col: usize) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for r in 1..self.rows.len() {
            if self.passes_filters(r, Some(col)) {
                *counts.entry(self.filter_key(r, col)).or_insert(0) += 1;
            }
        }
        let mut values: Vec<(SortKey, String, usize)> = counts
            .into_iter()
            .map(|(v, n)| (SortKey::of(v, false), v.to_string(), n))
            .collect();
        values.sort_by(|a, b| {
            // Blanks lead the list (true sorts before false here).
            b.1.is_empty()
                .cmp(&a.1.is_empty())
                .then_with(|| a.0.cmp(&b.0, false))
                .then_with(|| a.1.cmp(&b.1))
        });
        values.into_iter().map(|(_, v, n)| (v, n)).collect()
    }

    /// The values column `col`'s filter leaves visible, if it has a filter.
    pub fn column_filter(&self, col: usize) -> Option<&HashSet<String>> {
        self.filters.get(&col)
    }

    pub fn is_filtered(&self) -> bool {
        !self.filters.is_empty()
    }

    /// Direction of column `col` if it is a sort level (true = descending).
    pub fn column_sort(&self, col: usize) -> Option<bool> {
        self.sorts.iter().find(|(c, _)| *c == col).map(|&(_, d)| d)
    }

    pub fn is_sorted(&self) -> bool {
        !self.sorts.is_empty()
    }

    /// Data rows (header excluded) the grid shows.
    pub fn visible_data_rows(&self) -> usize {
        self.num_rows().saturating_sub(1)
    }

    /// Data rows (header excluded) in the file.
    pub fn file_data_rows(&self) -> usize {
        self.rows.len().saturating_sub(1)
    }

    /// Show only rows whose value in `col` is in `allowed`; `None` drops the filter.
    pub fn set_filter(&mut self, col: usize, allowed: Option<HashSet<String>>) {
        match allowed {
            Some(values) => {
                self.filters.insert(col, values);
            }
            None => {
                self.filters.remove(&col);
            }
        }
        self.rebuild_view();
    }

    pub fn clear_filters(&mut self) {
        self.filters.clear();
        self.rebuild_view();
    }

    /// Sort the view by `col`, making it the primary key; earlier sorts break ties.
    pub fn sort_by(&mut self, col: usize, descending: bool) {
        self.sorts.retain(|(c, _)| *c != col);
        self.sorts.push((col, descending));
        self.rebuild_view();
    }

    /// Back to the file's row order (filters stay).
    pub fn clear_sort(&mut self) {
        self.sorts.clear();
        self.rebuild_view();
    }

    /// Recompute the displayed rows from the filters and sort levels, keeping
    /// the cursor on the same file row while that row is still shown.
    fn rebuild_view(&mut self) {
        let cursor_file_row = self.file_row(self.cursor.0);
        if self.filters.is_empty() && self.sorts.is_empty() {
            self.view = None;
        } else {
            let mut shown: Vec<usize> = (1..self.rows.len())
                .filter(|&r| self.passes_filters(r, None))
                .collect();
            for &(col, descending) in &self.sorts {
                let keys: Vec<SortKey> = shown
                    .iter()
                    .map(|&r| SortKey::of(self.file_cell(r, col), self.file_is_null(r, col)))
                    .collect();
                let mut order: Vec<usize> = (0..shown.len()).collect();
                order.sort_by(|&a, &b| keys[a].cmp(&keys[b], descending)); // stable
                shown = order.into_iter().map(|i| shown[i]).collect();
            }
            shown.insert(0, 0); // the header row always leads
            self.view = Some(shown);
        }
        let last = self.num_rows().saturating_sub(1);
        let new_row = match &self.view {
            None => cursor_file_row.min(last),
            Some(v) => v
                .iter()
                .position(|&r| r == cursor_file_row)
                .unwrap_or(self.cursor.0.min(last)),
        };
        self.cursor.0 = new_row;
        self.selection_anchor = None;
        // Back to the top; the caller's ensure_cursor_visible then scrolls just
        // far enough to show the cursor, so a short result fills the screen.
        self.scroll_row = 0;
    }

    pub fn cursor_label(&self) -> String {
        // The file's row number, like Excel's row headers under a filter.
        format!("{}{}", col_letter(self.cursor.1), self.file_row(self.cursor.0).saturating_add(1))
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

    /// Down one row; past the last row the cursor walks into ghost rows.
    pub fn move_down(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        self.cursor.0 = self.cursor.0.saturating_add(1);
    }

    pub fn move_left(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        if self.cursor.1 > 0 {
            self.cursor.1 -= 1;
        }
    }

    /// Right one column; past the last column the cursor walks into ghost columns.
    pub fn move_right(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        self.cursor.1 = self.cursor.1.saturating_add(1);
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

    /// Jump to the first row, keeping the current column (Ctrl+Up).
    pub fn move_first_row(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        self.cursor.0 = 0;
    }

    /// Jump to the last row, keeping the current column (Ctrl+Down). O(1)
    /// regardless of row count, so it stays instant on multi-million-row files.
    pub fn move_last_row(&mut self, with_selection: bool) {
        self.prepare_selection(with_selection);
        self.cursor.0 = self.num_rows().saturating_sub(1);
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

    /// Down a page; like Excel, it carries on into ghost rows past the data
    /// (Ctrl+Down jumps to the last data row).
    pub fn page_down(&mut self, visible_rows: usize, with_selection: bool) {
        self.prepare_selection(with_selection);
        let step = visible_rows.max(1);
        self.cursor.0 = self.cursor.0.saturating_add(step);
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
        let (display_row, c) = self.cursor;
        // Write to the file row shown there. The view isn't re-applied, so an
        // edited row stays put until the filter or sort next changes (as in Excel).
        let r = self.file_row(display_row);
        // A value typed into a ghost cell past the data (or into a missing cell
        // of a short row) grows the data to hold it. Typing nothing grows
        // nothing, though a short row's missing cell becomes real, so an empty
        // commit turns it into an empty string just as it does a stored null.
        let mut growth = self.shape();
        let grew = if edit.text.is_empty() {
            self.fill_short_row(r, c, &mut growth)
        } else {
            self.grow_to(r, c, &mut growth)
        };
        let growth = grew.then_some(growth);
        // An edited cell holds a real value, never a null — even if the
        // committed text is empty (that's now an empty string).
        let was_null = self.file_is_null(r, c);
        let mut before = None;
        if let Some(cell) = self.rows.get_mut(r).and_then(|row| row.get_mut(c)) {
            if *cell != edit.text || was_null {
                before = Some(std::mem::replace(cell, edit.text));
            }
        }
        if let Some(before) = before {
            if let Some(m) = self.null_mask.get_mut(r).and_then(|row| row.get_mut(c)) {
                *m = false;
            }
            self.record_step(UndoStep {
                changes: vec![CellChange { row: r, col: c, other: before, other_null: was_null }],
                filter: None,
                growth,
                reshape: None,
            });
        }
        self.recompute_col_width(c);
    }

    /// The data's shape now, for `grow_to` to record growth against.
    fn shape(&self) -> Growth {
        Growth {
            rows: self.rows.len(),
            cols: self.num_cols(),
            widths: self.column_widths.len(),
            extended: Vec::new(),
        }
    }

    /// Grow the data so file cell (row, col) exists. New columns widen the
    /// header; new rows are appended in file order (and shown at the bottom of
    /// an active filter or sort); every other new cell is a null, which Save
    /// writes as an empty field. `before` is the shape taken before the first
    /// of the cells grown for one step; each existing row this lengthens is
    /// noted in it, so undo can shrink them all back. Returns whether the data
    /// grew (false if the cell already existed).
    fn grow_to(&mut self, row: usize, col: usize, before: &mut Growth) -> bool {
        // Past the header's width a cell only counts once the header covers it.
        let exists = col < self.num_cols() && self.rows.get(row).map_or(false, |r| col < r.len());
        if exists {
            return false;
        }
        self.note_lengthened(row, before);
        while self.rows[0].len() <= col {
            self.rows[0].push(String::new());
            self.null_mask[0].push(true);
        }
        while self.column_widths.len() < self.num_cols() {
            self.column_widths.push(GHOST_COL_WIDTH);
        }
        while self.rows.len() <= row {
            self.rows.push(Vec::new());
            self.null_mask.push(Vec::new());
            let new_row = self.rows.len() - 1;
            if let Some(view) = self.view.as_mut() {
                view.push(new_row);
            }
        }
        while self.rows[row].len() <= col {
            self.rows[row].push(String::new());
            self.null_mask[row].push(true);
        }
        true
    }

    /// Note in `before` that file row `row` is about to be lengthened, if it
    /// existed then. Rows appended since are truncated whole, and the header is
    /// cut back to `cols`, so neither needs noting. A row's cells are grown
    /// together, so checking the last entry keeps it to one per row.
    fn note_lengthened(&self, row: usize, before: &mut Growth) {
        if row > 0 && row < before.rows && before.extended.last().map(|&(r, _)| r) != Some(row) {
            before.extended.push((row, self.rows[row].len()));
        }
    }

    /// Undo a growth: back to the shape recorded before it.
    fn shrink(&mut self, g: &Growth) {
        for &(row, len) in &g.extended {
            if let Some(r) = self.rows.get_mut(row) {
                r.truncate(len);
            }
            if let Some(m) = self.null_mask.get_mut(row) {
                m.truncate(len);
            }
        }
        self.rows.truncate(g.rows);
        self.null_mask.truncate(g.rows);
        self.rows[0].truncate(g.cols);
        self.null_mask[0].truncate(g.cols);
        self.column_widths.truncate(g.widths);
        let rows = self.rows.len();
        if let Some(view) = self.view.as_mut() {
            view.retain(|&r| r < rows);
        }
        // A filter or sort on a column that no longer exists would keep hiding
        // or ordering rows by it, with no menu left to clear it: drop them.
        let cols = self.num_cols();
        let stale = self.filters.keys().any(|&c| c >= cols) || self.sorts.iter().any(|&(c, _)| c >= cols);
        if stale {
            self.filters.retain(|&c, _| c < cols);
            self.sorts.retain(|&(c, _)| c < cols);
            let cursor = self.cursor;
            self.rebuild_view();
            self.cursor.1 = cursor.1;
        }
    }

    /// Give a short row a real (null) cell at `col` when `col` is inside the
    /// header's width. A missing cell and a stored null look and save the same;
    /// making it real lets Delete and edits treat both alike. The row is noted
    /// in `before` as growth is, so undo shortens it again: a row left longer
    /// than the header would keep later values past the header from Save.
    /// Returns whether the row grew.
    fn fill_short_row(&mut self, row: usize, col: usize, before: &mut Growth) -> bool {
        if col >= self.num_cols() || self.rows.get(row).map_or(true, |r| col < r.len()) {
            return false;
        }
        self.note_lengthened(row, before);
        if let (Some(cells), Some(mask)) = (self.rows.get_mut(row), self.null_mask.get_mut(row)) {
            while cells.len() <= col {
                cells.push(String::new());
                mask.push(true);
            }
        }
        true
    }

    pub fn clear_selection_content(&mut self) {
        let ((r0, c0), (r1, c1)) = self.selected_range();
        let mut changes = Vec::new();
        let mut growth = self.shape();
        let mut filled = false;
        // Display rows only: cells a filter hides are left alone, as in Excel.
        for display_row in r0..=r1 {
            let r = self.file_row(display_row);
            for c in c0..=c1 {
                // A short row's missing cells clear like stored nulls.
                filled |= self.fill_short_row(r, c, &mut growth);
                // Clearing yields an empty string, not a null.
                let was_null = self.file_is_null(r, c);
                let Some(cell) = self.rows.get_mut(r).and_then(|row| row.get_mut(c)) else { continue };
                if cell.is_empty() && !was_null {
                    continue;
                }
                let before = std::mem::take(cell);
                if let Some(m) = self.null_mask.get_mut(r).and_then(|row| row.get_mut(c)) {
                    *m = false;
                }
                changes.push(CellChange { row: r, col: c, other: before, other_null: was_null });
            }
        }
        // A filled cell was a null, so it always cleared: filling comes with changes.
        self.record_step(UndoStep { changes, filter: None, growth: filled.then_some(growth), reshape: None });
    }

    // --- Insert & delete rows and columns (Ctrl+= / Ctrl+-) --------------------

    /// Note that the selection now is whole rows or columns picked by their
    /// numbers or letters.
    fn pick_lines(&mut self, columns: bool) {
        self.picked_lines = Some(PickedLines { columns, anchor: self.selection_anchor, cursor: self.cursor });
    }

    /// What Ctrl+= and Ctrl+- act on: whole columns when they were picked by
    /// their letters, or the selection runs the grid's full height (but not
    /// its full width); otherwise the rows the selection or cursor covers.
    fn selected_lines(&self) -> Lines {
        let ((r0, c0), (r1, c1)) = self.selected_range();
        let picked = self
            .picked_lines
            .filter(|p| p.anchor == self.selection_anchor && p.cursor == self.cursor)
            .map(|p| p.columns);
        let full_height = r0 == 0 && r1 + 1 >= self.num_rows();
        let full_width = c0 == 0 && c1 + 1 >= self.num_cols();
        let columns = picked.unwrap_or(self.has_selection() && full_height && !full_width);
        if columns { Lines::Cols(c0, c1) } else { Lines::Rows(r0, r1) }
    }

    /// Insert blank rows above the selection, or blank columns left of it, as
    /// many as it covers (Ctrl+=). Rows go into the file just above the
    /// selection's top row and show where they went, whatever a filter or sort
    /// would say, as an edit's row stays put. The selection moves onto them.
    /// Nothing happens past the data, which is blank already. Refused (with
    /// the reason) above the header row while a filter or sort is on, since
    /// the header can't move then.
    pub fn insert_lines(&mut self) -> Result<(), &'static str> {
        if self.num_cols() == 0 {
            return Ok(()); // an empty grid: every cell is a ghost
        }
        let mut reshape = match self.selected_lines() {
            Lines::Rows(r0, r1) => {
                if r0 >= self.num_rows() {
                    return Ok(());
                }
                if r0 == 0 && self.view.is_some() {
                    return Err("Can't insert above the header row while filtered or sorted");
                }
                Reshape::InsertRows { at: self.file_row(r0), count: r1 - r0 + 1, shown: r0 }
            }
            Lines::Cols(c0, c1) => {
                if c0 >= self.num_cols() {
                    return Ok(());
                }
                Reshape::InsertCols { at: c0, count: c1 - c0 + 1 }
            }
        };
        let (anchor, cursor) = (self.selection_anchor, self.cursor);
        self.reshape_forward(&mut reshape);
        self.record_step(UndoStep { changes: Vec::new(), filter: None, growth: None, reshape: Some(reshape) });
        // Keep the selection, now on the new rows or columns.
        self.selection_anchor = anchor;
        self.cursor = cursor;
        Ok(())
    }

    /// Delete the rows or columns the selection covers (Ctrl+-). Like Delete,
    /// it sees only the rows shown: rows a filter hides stay, and while a
    /// filter or sort is on the header row stays too. Deleting a column drops
    /// its filter and sort, so rows only its filter hid come back.
    pub fn delete_lines(&mut self) {
        if self.num_cols() == 0 {
            return; // an empty grid: nothing to delete
        }
        let lines = self.selected_lines();
        // Land where the deleted rows or columns were.
        let col = match lines {
            Lines::Cols(c0, _) => c0,
            Lines::Rows(..) => self.cursor.1,
        };
        let mut reshape = match lines {
            Lines::Rows(r0, r1) => {
                let first = if self.view.is_some() { r0.max(1) } else { r0 };
                let last = r1.min(self.num_rows().saturating_sub(1));
                if first > last {
                    return;
                }
                let mut rows: Vec<usize> = (first..=last).map(|d| self.file_row(d)).collect();
                rows.sort_unstable();
                let rows = rows.into_iter().map(|r| (r, None, Vec::new(), Vec::new())).collect();
                Reshape::DeleteRows(DeletedRows { rows, header_len: None, emptied: false, views: None })
            }
            Lines::Cols(c0, c1) => {
                let cols = self.num_cols();
                if c0 >= cols {
                    return;
                }
                let count = c1.min(cols - 1) - c0 + 1;
                Reshape::DeleteCols(DeletedCols {
                    at: c0,
                    count,
                    cells: Vec::new(),
                    widths: Vec::new(),
                    filters: Vec::new(),
                    sorts: Vec::new(),
                    rows_before: None,
                })
            }
        };
        let r0 = self.selected_range().0 .0;
        self.reshape_forward(&mut reshape);
        self.record_step(UndoStep { changes: Vec::new(), filter: None, growth: None, reshape: Some(reshape) });
        self.selection_anchor = None;
        self.cursor = (
            r0.min(self.num_rows().saturating_sub(1)),
            col.min(self.num_cols().saturating_sub(1)),
        );
    }

    /// Carry out `reshape`, for a new step or a redo, leaving in it what undo needs.
    fn reshape_forward(&mut self, reshape: &mut Reshape) {
        match reshape {
            Reshape::InsertRows { at, count, shown } => {
                let (at, count) = (*at, *count);
                let cols = self.num_cols();
                self.rows.splice(at..at, std::iter::repeat_with(Vec::new).take(count));
                self.null_mask.splice(at..at, std::iter::repeat_with(Vec::new).take(count));
                if at == 0 {
                    // A new header row: as wide as the old one, or the data
                    // past its width would drop out of the file.
                    self.rows[0] = vec![String::new(); cols];
                    self.null_mask[0] = vec![true; cols];
                }
                if let Some(view) = self.view.as_mut() {
                    for r in view.iter_mut() {
                        if *r >= at {
                            *r += count;
                        }
                    }
                    let shown = (*shown).min(view.len());
                    view.splice(shown..shown, at..at + count);
                }
                self.header_changed(at == 0);
            }
            Reshape::DeleteRows(d) => self.delete_file_rows(d),
            Reshape::InsertCols { at, count } => {
                let (at, count) = (*at, *count);
                for (row, mask) in self.rows.iter_mut().zip(self.null_mask.iter_mut()) {
                    if row.len() > at {
                        row.splice(at..at, std::iter::repeat_with(String::new).take(count));
                        mask.splice(at..at, std::iter::repeat(true).take(count));
                    }
                }
                let w = at.min(self.column_widths.len());
                self.column_widths.splice(w..w, std::iter::repeat(GHOST_COL_WIDTH).take(count));
                self.shift_columns(at, count as isize);
            }
            Reshape::DeleteCols(d) => self.delete_cols(d),
        }
    }

    /// Reverse `reshape`, for an undo, leaving in it what redo needs.
    fn reshape_back(&mut self, reshape: &mut Reshape) {
        match reshape {
            Reshape::InsertRows { at, count, .. } => {
                let (at, count) = (*at, *count);
                self.rows.drain(at..at + count);
                self.null_mask.drain(at..at + count);
                if let Some(view) = self.view.as_mut() {
                    view.retain(|&r| r < at || r >= at + count);
                    for r in view.iter_mut() {
                        if *r >= at + count {
                            *r -= count;
                        }
                    }
                }
                if at == 0 && self.view.is_some() {
                    // The old header is the header again (a filter put on
                    // since may have hidden it as a data row): it leads the view.
                    self.rebuild_view_keeping_column();
                }
                self.header_changed(at == 0);
            }
            Reshape::DeleteRows(d) => self.restore_file_rows(d),
            Reshape::InsertCols { at, count } => {
                let (at, count) = (*at, *count);
                // A filter or sort put on the new columns since goes with them.
                let dropped = self.drop_column_views(at, count);
                self.shift_columns(at + count, -(count as isize));
                for (row, mask) in self.rows.iter_mut().zip(self.null_mask.iter_mut()) {
                    if row.len() > at {
                        let end = (at + count).min(row.len());
                        row.drain(at..end);
                        mask.drain(at..end.min(mask.len()));
                    }
                }
                let w = at.min(self.column_widths.len());
                let end = (at + count).min(self.column_widths.len());
                self.column_widths.drain(w..end);
                if dropped {
                    self.rebuild_view_keeping_column();
                }
            }
            Reshape::DeleteCols(d) => self.restore_cols(d),
        }
    }

    /// Delete the file rows `d` lists (in file order), keeping their cells and
    /// display rows in it for undo.
    fn delete_file_rows(&mut self, d: &mut DeletedRows) {
        let n = self.rows.len();
        let cols = self.num_cols();
        let mut gone = vec![false; n];
        for &(r, ..) in &d.rows {
            gone[r] = true;
        }
        if let Some(view) = &self.view {
            for (shown, &r) in view.iter().enumerate() {
                if gone[r] {
                    if let Ok(i) = d.rows.binary_search_by_key(&r, |e| e.0) {
                        d.rows[i].1 = Some(shown);
                    }
                }
            }
        }
        let rows = std::mem::take(&mut self.rows);
        let masks = std::mem::take(&mut self.null_mask);
        let mut moved_to = vec![usize::MAX; n];
        let mut next = 0;
        for (r, (row, mask)) in rows.into_iter().zip(masks).enumerate() {
            if gone[r] {
                d.rows[next].2 = row;
                d.rows[next].3 = mask;
                next += 1;
            } else {
                moved_to[r] = self.rows.len();
                self.rows.push(row);
                self.null_mask.push(mask);
            }
        }
        if let Some(view) = self.view.as_mut() {
            *view = view.iter().filter(|&&r| !gone[r]).map(|&r| moved_to[r]).collect();
        }
        if self.rows.is_empty() {
            // Every row went: an empty grid, as a new file starts. No column
            // is left to hold a filter or sort, so they go with the rows.
            self.rows.push(Vec::new());
            self.null_mask.push(Vec::new());
            d.emptied = true;
            d.views = Some((std::mem::take(&mut self.filters), std::mem::take(&mut self.sorts)));
            self.view = None;
        } else if gone[0] {
            // The next row is the header now; widen it to the old header's
            // width, or the data past its end would drop out of the file.
            d.header_len = Some(self.rows[0].len());
            while self.rows[0].len() < cols {
                self.rows[0].push(String::new());
                self.null_mask[0].push(true);
            }
        }
        if gone[0] && self.view.is_some() {
            self.rebuild_view_keeping_column(); // the header leads the view
        }
        self.header_changed(gone[0]);
    }

    /// Put back the rows `delete_file_rows` took, where they were in the file
    /// and (while a filter or sort is on) on screen.
    fn restore_file_rows(&mut self, d: &mut DeletedRows) {
        if d.emptied {
            self.rows.clear();
            self.null_mask.clear();
            if let Some((filters, sorts)) = d.views.take() {
                self.filters = filters;
                self.sorts = sorts;
            }
            self.view = None; // rebuilt below, once the rows are back
            d.emptied = false;
        } else if let Some(len) = d.header_len.take() {
            self.rows[0].truncate(len);
            self.null_mask[0].truncate(len);
        }
        let header_back = d.rows.first().map_or(false, |e| e.0 == 0);
        let survivors = std::mem::take(&mut self.rows).into_iter().zip(std::mem::take(&mut self.null_mask));
        let mut survivors = survivors.peekable();
        let total = survivors.len() + d.rows.len();
        let mut moved_to = Vec::with_capacity(total - d.rows.len());
        let mut deleted = d.rows.iter_mut().peekable();
        for r in 0..total {
            match deleted.next_if(|e| e.0 == r) {
                Some(e) => {
                    self.rows.push(std::mem::take(&mut e.2));
                    self.null_mask.push(std::mem::take(&mut e.3));
                }
                None => {
                    let (row, mask) = survivors.next().expect("a surviving row");
                    moved_to.push(r);
                    self.rows.push(row);
                    self.null_mask.push(mask);
                }
            }
        }
        // A row deleted with no filter or sort on has no place in a view put
        // on since: the view is rebuilt to take it.
        let unplaced = self.view.is_some() && d.rows.iter().any(|e| e.1.is_none());
        if let Some(view) = self.view.as_mut() {
            // Back to where they showed, in one pass: the old view's rows,
            // with each restored row put in at its display row.
            let mut shown: Vec<(usize, usize)> = d.rows.iter().filter_map(|e| e.1.map(|s| (s, e.0))).collect();
            shown.sort_unstable();
            let mut shown = shown.into_iter().peekable();
            let mut merged = Vec::with_capacity(view.len() + d.rows.len());
            for &r in view.iter() {
                while let Some((_, restored)) = shown.next_if(|&(s, _)| s <= merged.len()) {
                    merged.push(restored);
                }
                merged.push(moved_to[r]);
            }
            merged.extend(shown.map(|(_, restored)| restored));
            *view = merged;
        }
        for e in d.rows.iter_mut() {
            e.1 = None;
        }
        // The header leads the view; and rows put back where no view placed them.
        if (header_back || unplaced) && (self.is_filtered() || self.is_sorted()) {
            self.rebuild_view_keeping_column();
        }
        self.header_changed(header_back);
    }

    /// Delete columns `d.at..d.at + d.count`, keeping their cells, widths,
    /// filters and sort levels in `d` for undo.
    fn delete_cols(&mut self, d: &mut DeletedCols) {
        let (at, count) = (d.at, d.count);
        for (r, (row, mask)) in self.rows.iter_mut().zip(self.null_mask.iter_mut()).enumerate() {
            if row.len() > at {
                let end = (at + count).min(row.len());
                let cells = row.drain(at..end).collect();
                let nulls = mask.drain(at..end.min(mask.len())).collect();
                d.cells.push((r, cells, nulls));
            }
        }
        let w = at.min(self.column_widths.len());
        let end = (at + count).min(self.column_widths.len());
        d.widths = self.column_widths.drain(w..end).collect();
        for c in at..at + count {
            if let Some(allowed) = self.filters.remove(&c) {
                d.filters.push((c, allowed));
            }
        }
        let mut level = 0;
        self.sorts.retain(|&(c, descending)| {
            let keep = c < at || c >= at + count;
            if !keep {
                d.sorts.push((level, c, descending));
            }
            level += 1;
            keep
        });
        self.shift_columns(at + count, -(count as isize));
        if self.num_cols() == 0 && self.rows.len() > 1 {
            // Every column went: an empty grid, not rows of nothing that
            // would come back as rows of "" once a column is typed again.
            d.rows_before = Some(self.rows.len());
            self.rows.truncate(1);
            self.null_mask.truncate(1);
        }
        if !d.filters.is_empty() || !d.sorts.is_empty() {
            self.rebuild_view_keeping_column();
        }
    }

    /// Put back the columns `delete_cols` took, with their filters and sorts.
    fn restore_cols(&mut self, d: &mut DeletedCols) {
        let at = d.at;
        if let Some(rows) = d.rows_before.take() {
            self.rows.resize_with(rows, Vec::new);
            self.null_mask.resize_with(rows, Vec::new);
        }
        self.shift_columns(at, d.count as isize);
        for (r, cells, nulls) in d.cells.drain(..) {
            self.rows[r].splice(at..at, cells);
            self.null_mask[r].splice(at..at, nulls);
        }
        let w = at.min(self.column_widths.len());
        self.column_widths.splice(w..w, d.widths.drain(..));
        let refilter = !d.filters.is_empty() || !d.sorts.is_empty();
        for (c, allowed) in d.filters.drain(..) {
            self.filters.insert(c, allowed);
        }
        for (level, c, descending) in d.sorts.drain(..) {
            let level = level.min(self.sorts.len());
            self.sorts.insert(level, (c, descending));
        }
        if refilter {
            self.rebuild_view_keeping_column();
        }
    }

    /// Move the filters and sort levels on columns `from` and later by `by`.
    fn shift_columns(&mut self, from: usize, by: isize) {
        let shift = |c: usize| if c >= from { c.saturating_add_signed(by) } else { c };
        self.filters = std::mem::take(&mut self.filters).into_iter().map(|(c, v)| (shift(c), v)).collect();
        for (c, _) in self.sorts.iter_mut() {
            *c = shift(*c);
        }
    }

    /// Drop the filters and sort levels on columns `at..at + count`. Returns
    /// whether there were any.
    fn drop_column_views(&mut self, at: usize, count: usize) -> bool {
        let before = self.filters.len() + self.sorts.len();
        self.filters.retain(|&c, _| c < at || c >= at + count);
        self.sorts.retain(|&(c, _)| c < at || c >= at + count);
        before != self.filters.len() + self.sorts.len()
    }

    /// Re-apply the filters and sorts (their columns moved or went), keeping
    /// the cursor's column.
    fn rebuild_view_keeping_column(&mut self) {
        let col = self.cursor.1;
        self.rebuild_view();
        self.cursor.1 = col;
    }

    /// Refit the columns after the header row changed, as it sizes them.
    fn header_changed(&mut self, changed: bool) {
        if changed {
            self.recompute_column_widths();
        }
    }

    // --- Undo / redo ---------------------------------------------------------

    /// Push a step onto the undo history (nothing to record is a no-op).
    fn record_step(&mut self, step: UndoStep) {
        if step.changes.is_empty() && step.reshape.is_none() {
            return;
        }
        // A new change after undoing past the save point makes that point
        // unreachable: the file on disk no longer matches any state here.
        if self.save_point.map_or(false, |p| p > self.undo.len()) {
            self.save_point = None;
        }
        self.redo.clear();
        self.undo.push(step);
        self.modified = self.save_point != Some(self.undo.len());
    }

    fn swap_filter(&mut self, col: usize, stored: &mut Option<HashSet<String>>) {
        let current = self.filters.remove(&col);
        if let Some(filter) = stored.take() {
            self.filters.insert(col, filter);
        }
        *stored = current;
    }

    // --- Date conversion (a data change, unlike sort and filter) ---------------

    /// Whether column `col` holds any date the conversion would rewrite. Stops
    /// at the first, so the column menu stays quick on big date columns.
    pub fn has_dates_to_convert(&self, col: usize) -> bool {
        (1..self.rows.len())
            .any(|r| !self.file_is_null(r, col) && crate::dates::needs_conversion(self.file_cell(r, col)))
    }

    /// How column `col`'s values (data rows, hidden ones included, nulls
    /// skipped) would convert to ISO 8601.
    pub fn analyze_dates(&self, col: usize) -> crate::dates::Analysis {
        let values: Vec<&str> = (1..self.rows.len())
            .filter(|&r| !self.file_is_null(r, col))
            .map(|r| self.file_cell(r, col))
            .collect();
        crate::dates::analyze(&values)
    }

    /// Rewrite column `col`'s dates as ISO 8601 in every data row, rows a
    /// filter hides included, as one undo step. `plan` says how to read d/m/y
    /// and year-first values. Returns how many cells changed.
    pub fn convert_dates(&mut self, col: usize, plan: crate::dates::Plan) -> usize {
        let mut changes = Vec::new();
        for r in 1..self.rows.len() {
            if self.file_is_null(r, col) {
                continue;
            }
            let Some(iso) = crate::dates::to_iso(self.file_cell(r, col), plan) else { continue };
            if let Some(cell) = self.rows.get_mut(r).and_then(|row| row.get_mut(col)) {
                let before = std::mem::replace(cell, iso);
                changes.push(CellChange { row: r, col, other: before, other_null: false });
            }
        }
        let converted = changes.len();
        if converted == 0 {
            return 0;
        }
        // A filter on this column lists the old spellings; map them the same way
        // so it keeps matching the same rows the next time the view is rebuilt.
        let filter = self.filters.get(&col).map(|allowed| {
            allowed
                .iter()
                .map(|v| crate::dates::to_iso(v, plan).unwrap_or_else(|| v.clone()))
                .collect::<HashSet<String>>()
        });
        let filter = filter.map(|mapped| (col, self.filters.insert(col, mapped)));
        self.record_step(UndoStep { changes, filter, growth: None, reshape: None });
        self.recompute_col_width(col);
        converted
    }

    /// Undo the last edit or range clear (Ctrl+Z). Returns false with nothing to undo.
    pub fn undo(&mut self) -> bool {
        let Some(mut step) = self.undo.pop() else { return false };
        for change in step.changes.iter_mut().rev() {
            self.swap_cell(change);
        }
        if let Some((col, stored)) = step.filter.as_mut() {
            self.swap_filter(*col, stored);
        }
        if let Some(growth) = &step.growth {
            self.shrink(growth);
        }
        if let Some(reshape) = step.reshape.as_mut() {
            self.reshape_back(reshape);
        }
        self.finish_step(&step);
        self.redo.push(step);
        self.modified = self.save_point != Some(self.undo.len());
        true
    }

    /// Redo the last undone step (Ctrl+Shift+Z). Returns false with nothing to redo.
    pub fn redo(&mut self) -> bool {
        let Some(mut step) = self.redo.pop() else { return false };
        // Grow back to the step's full extent: every changed cell that growth
        // created (the rest already exist, so growing to them does nothing).
        if step.growth.is_some() {
            let mut regrowth = self.shape();
            for change in &step.changes {
                self.grow_to(change.row, change.col, &mut regrowth);
            }
        }
        for change in step.changes.iter_mut() {
            self.swap_cell(change);
        }
        if let Some((col, stored)) = step.filter.as_mut() {
            self.swap_filter(*col, stored);
        }
        if let Some(reshape) = step.reshape.as_mut() {
            self.reshape_forward(reshape);
        }
        self.finish_step(&step);
        self.undo.push(step);
        self.modified = self.save_point != Some(self.undo.len());
        true
    }

    fn swap_cell(&mut self, change: &mut CellChange) {
        if let Some(cell) = self.rows.get_mut(change.row).and_then(|r| r.get_mut(change.col)) {
            std::mem::swap(cell, &mut change.other);
        }
        if let Some(m) = self.null_mask.get_mut(change.row).and_then(|r| r.get_mut(change.col)) {
            std::mem::swap(m, &mut change.other_null);
        }
    }

    /// After an undo/redo: refit the touched columns and put the cursor on the
    /// step's first cell when a filter isn't hiding its row.
    fn finish_step(&mut self, step: &UndoStep) {
        let cols: std::collections::BTreeSet<usize> = step.changes.iter().map(|c| c.col).collect();
        for col in cols {
            self.recompute_col_width(col);
        }
        if let Some(first) = step.changes.first() {
            let shown = match &self.view {
                None => Some(first.row),
                Some(v) => v.iter().position(|&r| r == first.row),
            };
            if let Some(display_row) = shown {
                self.cursor = (display_row, first.col);
            }
        }
        // An insert or delete: onto where the rows or columns are (or were).
        if let Some(reshape) = &step.reshape {
            let (row, col) = match reshape {
                Reshape::InsertRows { at, .. } => (Some(*at), None),
                Reshape::DeleteRows(d) => (d.rows.first().map(|e| e.0), None),
                Reshape::InsertCols { at, .. } => (None, Some(*at)),
                Reshape::DeleteCols(d) => (None, Some(d.at)),
            };
            if let Some(display_row) = row.and_then(|r| self.display_row_of(r)) {
                self.cursor.0 = display_row;
            }
            if let Some(col) = col {
                self.cursor.1 = col;
            }
            self.cursor.0 = self.cursor.0.min(self.num_rows().saturating_sub(1));
            self.cursor.1 = self.cursor.1.min(self.num_cols().saturating_sub(1));
        }
        self.selection_anchor = None;
    }

    /// The display row showing file row `row`, if one does.
    fn display_row_of(&self, row: usize) -> Option<usize> {
        match &self.view {
            None => (row < self.rows.len()).then_some(row),
            Some(v) => v.iter().position(|&r| r == row),
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
                // Quoted as Paste reads it back: a leading BOM would be taken
                // for the clipboard's own.
                if cell.contains(['\t', '\n', '\r', '"']) || cell.starts_with('\u{FEFF}') {
                    out.push('"');
                    out.push_str(&cell.replace('"', "\"\""));
                    out.push('"');
                } else {
                    out.push_str(cell);
                }
            }
        }
        // An empty last line would read as the trailing line end a paste drops;
        // one more keeps it a row.
        if r1 > r0 && out.ends_with('\n') {
            out.push('\n');
        }
        out
    }

    /// Paste clipboard text: into the cell being edited, or else as a block of
    /// cells (tab-separated rows, as Copy writes and spreadsheets put on the
    /// clipboard) at the selection's top-left, or the cursor.
    ///
    /// Like Copy and Delete, a paste sees only the rows the grid shows: block
    /// row i goes to the i-th shown row, in the order shown, and rows a filter
    /// hides are left alone. Past the last row or column it grows the data as
    /// typing into a ghost cell does. One value fills a selection, and a
    /// selection a whole number of blocks tall and wide is filled with copies
    /// of the block. One undo step; the view isn't re-applied, as with an
    /// edit; the pasted cells end selected.
    pub fn paste(&mut self, text: &str) {
        if self.editing.is_some() {
            self.edit_paste(text);
            return;
        }
        let block = parse_clipboard(text);
        let h = block.len();
        let w = block.iter().map(|line| line.len()).max().unwrap_or(1);
        let ((r0, c0), (r1, c1)) = self.selected_range();
        let (sel_h, sel_w) = (r1 - r0 + 1, c1 - c0 + 1);
        let (out_h, out_w) = if self.has_selection() && sel_h % h == 0 && sel_w % w == 0 {
            (sel_h, sel_w)
        } else {
            (h, w)
        };
        // The file row behind each target row, fixed before anything grows. A
        // ghost row past the view maps to the row appended for it, and stays
        // mapped as rows are appended (each is shown at the bottom of the view).
        let targets: Vec<usize> = (r0..r0 + out_h).map(|d| self.file_row(d)).collect();
        // Visit each value with its file row and column. A line shorter than
        // the block leaves the cells past its end alone.
        let each = |f: &mut dyn FnMut(usize, usize, &String)| {
            for (i, &r) in targets.iter().enumerate() {
                let line = &block[i % h];
                for copy in 0..out_w / w {
                    for (k, v) in line.iter().enumerate() {
                        f(r, c0 + copy * w + k, v);
                    }
                }
            }
        };

        // As with typing, only a value grows the data to hold it.
        let mut growth = self.shape();
        let mut grew = false;
        each(&mut |r, c, v| {
            if !v.is_empty() {
                grew |= self.grow_to(r, c, &mut growth);
            }
        });

        let mut changes = Vec::new();
        each(&mut |r, c, v| {
            let was_null = self.file_is_null(r, c);
            // No cell here only for an empty value (past a short row, or a ghost).
            let Some(cell) = self.rows.get_mut(r).and_then(|row| row.get_mut(c)) else { return };
            // An empty value clears a cell as Delete does, but leaves an empty
            // one (null or not) as it is: sage's Copy writes both as an empty
            // field, so copying and pasting them changes nothing.
            let unchanged = if v.is_empty() { cell.is_empty() } else { cell == v && !was_null };
            if unchanged {
                return;
            }
            let before = std::mem::replace(cell, v.clone());
            if let Some(m) = self.null_mask.get_mut(r).and_then(|row| row.get_mut(c)) {
                *m = false;
            }
            changes.push(CellChange { row: r, col: c, other: before, other_null: was_null });
        });
        // Only a written value grows the data, so growth always comes with changes.
        let cols: std::collections::BTreeSet<usize> = changes.iter().map(|c| c.col).collect();
        self.record_step(UndoStep { changes, filter: None, growth: grew.then_some(growth), reshape: None });
        for col in cols {
            self.recompute_col_width(col);
        }
        self.cursor = (r0, c0);
        self.selection_anchor = (out_h > 1 || out_w > 1).then_some((r0 + out_h - 1, c0 + out_w - 1));
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
        // Line ends as the grid stores them. A cell copied from a spreadsheet
        // ends in one, which isn't part of its value.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text = text.strip_suffix('\n').unwrap_or(&text);
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

        let rw = self.row_num_width(visible_data_rows);
        if col < rw {
            if is_data_row {
                let row_idx = self.scroll_row + (row - data_start);
                if row_idx < self.num_rows() {
                    return GridHit::RowNumber { row: row_idx };
                }
            }
            return GridHit::Outside;
        }

        // Fixed separator between row-num and first visible column
        if col == rw {
            return GridHit::Outside;
        }

        // Columns run on past the data as ghost columns to the screen's edge;
        // rows likewise past the last row. Ghost cells are clickable; ghost
        // column letters and separators aren't (nothing there to select or size).
        let mut pos = rw + 1;
        let mut cur_col = self.scroll_col;
        while pos < term_width as usize {
            let ghost_col = cur_col >= self.num_cols();
            let w = self.col_width(cur_col);
            if col >= pos && col < pos + w {
                if is_header_row {
                    return if ghost_col { GridHit::Outside } else { GridHit::ColumnHeader { col: cur_col } };
                }
                let row_idx = self.scroll_row + (row - data_start);
                return GridHit::DataCell { row: row_idx, col: cur_col };
            }
            let sep_pos = pos + w;
            if col == sep_pos {
                return if ghost_col { GridHit::Outside } else { GridHit::ColumnSeparator { col: cur_col } };
            }
            pos = sep_pos + 1;
            cur_col += 1;
        }
        GridHit::Outside
    }

    pub fn move_to(&mut self, row: usize, col: usize, with_selection: bool) {
        // An open edit belongs to the cell it started in: commit it there before
        // the cursor leaves (the find pane can move the cursor mid-edit).
        if self.is_editing() {
            self.commit_edit();
        }
        self.prepare_selection(with_selection);
        // Ghost cells past the data are valid places to land (a click, a drag).
        self.cursor = (row, col);
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
        self.pick_lines(true);
    }

    /// Extend an in-progress column-header drag to include column `col`.
    pub fn extend_column_selection(&mut self, col: usize) {
        if self.num_rows() == 0 || self.num_cols() == 0 {
            return;
        }
        let last_row = self.num_rows() - 1;
        let col = col.min(self.num_cols() - 1);
        self.selection_anchor = Some((last_row, col));
        self.pick_lines(true);
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
        self.pick_lines(false);
    }

    /// Extend an in-progress row-number drag to include row `row`.
    pub fn extend_row_selection(&mut self, row: usize) {
        if self.num_rows() == 0 || self.num_cols() == 0 {
            return;
        }
        let last_col = self.num_cols() - 1;
        let row = row.min(self.num_rows() - 1);
        self.selection_anchor = Some((row, last_col));
        self.pick_lines(false);
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

    /// Case-insensitive substring search across the displayed cells (rows a
    /// filter hides are skipped). Returns display (row, col) in row-major order.
    pub fn find_cells(&self, needle: &str) -> Vec<(usize, usize)> {
        if needle.is_empty() {
            return Vec::new();
        }
        let needle_lower = needle.to_lowercase();
        let mut out = Vec::new();
        for r in 0..self.num_rows() {
            let Some(row) = self.rows.get(self.file_row(r)) else { continue };
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

    /// Mouse-wheel scrolling. It can reach the data's last row and column, or
    /// as far out into the ghost cells as the cursor (or the view) already is,
    /// so a wheel tick out there doesn't snap the view back to the data.
    pub fn scroll_by(&mut self, row_delta: i32, col_delta: i32) {
        let rows = self.num_rows().max(self.cursor.0 + 1).max(self.scroll_row + 1);
        let cols = self.num_cols().max(self.cursor.1 + 1).max(self.scroll_col + 1);
        let shift = |from: usize, delta: i32, limit: usize| -> usize {
            let moved = if delta < 0 { from.saturating_sub(delta.unsigned_abs() as usize) } else { from.saturating_add(delta as usize) };
            moved.min(limit.saturating_sub(1))
        };
        self.scroll_row = shift(self.scroll_row, row_delta, rows);
        self.scroll_col = shift(self.scroll_col, col_delta, cols);
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
            let mut width_needed = self.row_num_width(visible_rows) + 1;
            for c in self.scroll_col..=self.cursor.1 {
                width_needed += self.col_width(c) + 1;
            }
            while width_needed > visible_width && self.scroll_col < self.cursor.1 {
                width_needed -= self.col_width(self.scroll_col) + 1;
                self.scroll_col += 1;
            }
        }
    }

    /// The auto-fit cap for a cell on row `r`. The first row usually holds column
    /// headers, so it may size a column to fit fully (up to the resize cap) and
    /// its label is never truncated on load; data rows stay capped at the
    /// narrower MAX_COL_WIDTH so a single long value can't blow a column open.
    fn col_width_cap(r: usize) -> usize {
        if r == 0 {
            MAX_RESIZE_WIDTH
        } else {
            MAX_COL_WIDTH
        }
    }

    fn recompute_column_widths(&mut self) {
        let num_cols = self.num_cols();
        let mut widths = vec![MIN_COL_WIDTH; num_cols];
        for (r, row) in self.rows.iter().enumerate() {
            let cap = Self::col_width_cap(r);
            for (c, cell) in row.iter().enumerate() {
                if c >= widths.len() {
                    continue;
                }
                let w = cell_grid_width(cell).min(cap);
                if w > widths[c] {
                    widths[c] = w;
                }
            }
        }
        for w in widths.iter_mut() {
            *w = (*w).clamp(MIN_COL_WIDTH, MAX_RESIZE_WIDTH);
        }
        self.column_widths = widths;
    }

    fn recompute_col_width(&mut self, col: usize) {
        let mut w = MIN_COL_WIDTH;
        for (r, row) in self.rows.iter().enumerate() {
            if let Some(cell) = row.get(col) {
                let cw = cell_grid_width(cell).min(Self::col_width_cap(r));
                if cw > w {
                    w = cw;
                }
            }
        }
        if col < self.column_widths.len() {
            self.column_widths[col] = w.clamp(MIN_COL_WIDTH, MAX_RESIZE_WIDTH);
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

/// Split clipboard text into lines of fields: tab-separated, as Copy writes
/// them and spreadsheets put cells on the clipboard, whatever the file's own
/// delimiter. Line ends may be CRLF or CR; one trailing line end is dropped,
/// and blank lines are kept (a line of one empty field). A field that starts
/// with `"` is quoted, with `""` for a quote, and may hold tabs and line
/// breaks; one whose quote doesn't end it (`"12" pipe`, or never closed) is
/// plain text to the next tab or line break. Always at least one field.
fn parse_clipboard(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut rest = text.strip_suffix('\n').unwrap_or(&text);
    let mut lines = vec![Vec::new()];
    loop {
        let (field, after) = clipboard_field(rest);
        lines.last_mut().expect("a line").push(field);
        match after.as_bytes().first() {
            Some(b'\t') => {}
            Some(_) => lines.push(Vec::new()), // '\n'
            None => return lines,
        }
        rest = &after[1..];
    }
}

/// The field at the start of `s`, and the rest of `s` from the tab or line
/// break that ends it (empty at the end of the text).
fn clipboard_field(s: &str) -> (String, &str) {
    if let Some(body) = s.strip_prefix('"') {
        let mut value = String::new();
        let mut from = 0;
        while let Some(q) = body[from..].find('"').map(|i| from + i) {
            value.push_str(&body[from..q]);
            match body.as_bytes().get(q + 1) {
                Some(b'"') => {
                    value.push('"');
                    from = q + 2;
                }
                None | Some(b'\t') | Some(b'\n') => return (value, &body[q + 1..]),
                Some(_) => break,
            }
        }
    }
    let end = s.find(['\t', '\n']).unwrap_or(s.len());
    (s[..end].to_string(), &s[end..])
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
        // ±HHMM. ASCII only: a 4-byte non-ASCII body (e.g. "1€") would split a char.
        None if body.len() == 4 && body.is_ascii() => (&body[0..2], &body[2..4]),
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
    fn header_row_drives_column_width() {
        // The first row usually holds column headers; a long header must not be
        // truncated on load even when the data below it is short. Data cells stay
        // capped at the narrower MAX_COL_WIDTH.
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = vec![
            vec!["Transaction Description Field".into(), "n".into()], // 29-char header
            vec!["12".into(), "a data value far longer than twenty characters".into()],
        ];
        ss.recompute_column_widths();
        assert_eq!(ss.column_widths[0], 29); // header drives the full width
        assert_eq!(ss.column_widths[1], MAX_COL_WIDTH); // long data stays capped
    }

    #[test]
    fn strips_leading_bom_from_first_header_cell() {
        use std::io::Write;
        // Excel-style UTF-8 BOM before the first header field.
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        write!(tmp, "\u{FEFF}GTWY,DEST\nYYZ,PUJ\n").unwrap();
        tmp.flush().unwrap();
        let ss = Spreadsheet::from_file(tmp.path()).unwrap();
        // The BOM is gone: the header cell is "GTWY" (4 cols), not "\u{FEFF}GTWY".
        assert_eq!(ss.cell(0, 0), "GTWY");
        assert_eq!(ss.column_widths[0], 4);
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

    // --- filter & sort (view-only) ---

    fn grid(rows: &[&[&str]]) -> Spreadsheet {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = rows.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect();
        ss.null_mask = ss.rows.iter().map(|r| vec![false; r.len()]).collect();
        ss
    }

    fn column(ss: &Spreadsheet, col: usize) -> Vec<String> {
        (0..ss.num_rows()).map(|r| ss.cell(r, col).to_string()).collect()
    }

    fn set(values: &[&str]) -> HashSet<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn filter_keeps_the_header_and_maps_reads_and_edits_to_file_rows() {
        let mut ss = grid(&[&["gw", "n"], &["YYC", "3"], &["YYZ", "10"], &["YYC", "2"]]);
        ss.set_filter(0, Some(set(&["YYC"])));
        assert_eq!(column(&ss, 1), vec!["n", "3", "2"]);
        assert_eq!(ss.file_row(2), 3);
        assert_eq!((ss.visible_data_rows(), ss.file_data_rows()), (2, 3));

        // Copy and the status-bar metrics see only the visible rows.
        ss.selection_anchor = Some((1, 0));
        ss.cursor = (2, 1);
        assert_eq!(ss.copy_selection_tsv(), "YYC\t3\nYYC\t2");
        assert_eq!(ss.selection_metrics().numbers, vec![3.0, 2.0]);

        // An edit lands on the file row shown there.
        ss.selection_anchor = None;
        ss.enter_edit_mode_replace('7');
        ss.commit_edit();
        assert_eq!(ss.rows[3][1], "7");
        assert_eq!(ss.rows[1][1], "3");

        // Save writes every row, in the file's order.
        let tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        ss.save(tmp.path()).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path()).unwrap(), "gw,n\nYYC,3\nYYZ,10\nYYC,7\n");

        ss.clear_filters();
        assert_eq!(column(&ss, 1), vec!["n", "3", "10", "7"]);
    }

    #[test]
    fn sort_orders_numbers_then_text_with_blanks_last_and_never_touches_the_file() {
        let mut ss = grid(&[&["v"], &["10"], &["b"], &[""], &["9"], &["A"]]);
        ss.sort_by(0, false);
        assert_eq!(column(&ss, 0), vec!["v", "9", "10", "A", "b", ""]);
        ss.sort_by(0, true);
        assert_eq!(column(&ss, 0), vec!["v", "b", "A", "10", "9", ""]);
        assert_eq!(ss.rows.iter().map(|r| r[0].as_str()).collect::<Vec<_>>(), vec!["v", "10", "b", "", "9", "A"]);
        assert!(!ss.is_modified());
        ss.clear_sort();
        assert_eq!(column(&ss, 0), vec!["v", "10", "b", "", "9", "A"]);
    }

    #[test]
    fn sorting_twice_gives_a_two_level_sort() {
        let mut ss = grid(&[&["k", "n"], &["x", "2"], &["y", "1"], &["x", "1"], &["y", "2"]]);
        ss.sort_by(1, false); // secondary
        ss.sort_by(0, false); // primary
        let rows: Vec<(String, String)> = (1..ss.num_rows())
            .map(|r| (ss.cell(r, 0).to_string(), ss.cell(r, 1).to_string()))
            .collect();
        let expected = [("x", "1"), ("x", "2"), ("y", "1"), ("y", "2")];
        assert_eq!(rows, expected.map(|(a, b)| (a.to_string(), b.to_string())));
    }

    #[test]
    fn sort_keeps_the_cursor_on_its_row_and_iso_dates_sort_as_dates() {
        let mut ss = grid(&[&["d"], &["2026-10-01"], &["2025-12-31"], &["2026-01-15"]]);
        ss.cursor = (1, 0); // on 2026-10-01
        ss.sort_by(0, false);
        assert_eq!(column(&ss, 0), vec!["d", "2025-12-31", "2026-01-15", "2026-10-01"]);
        assert_eq!(ss.cell(ss.cursor.0, 0), "2026-10-01");
    }

    #[test]
    fn column_values_follow_the_other_filters_with_blanks_first() {
        let mut ss = grid(&[&["a", "b"], &["1", "x"], &["2", "x"], &["1", "y"], &["", "x"]]);
        ss.null_mask[4][0] = true; // a null counts as a blank too
        ss.set_filter(1, Some(set(&["x"])));
        assert_eq!(
            ss.column_values(0),
            vec![(String::new(), 1), ("1".to_string(), 1), ("2".to_string(), 1)]
        );
        // A column's own filter doesn't narrow its own list.
        assert_eq!(ss.column_values(1), vec![("x".to_string(), 3), ("y".to_string(), 1)]);
        // Filtering to blanks keeps the null row.
        ss.set_filter(0, Some(set(&[""])));
        assert_eq!(ss.visible_data_rows(), 1);
        assert_eq!(ss.file_row(1), 4);
    }

    #[test]
    fn non_ascii_timezone_suffix_is_rejected_not_a_panic() {
        assert_eq!(parse_iso_datetime("2026-01-01 10:00-1\u{20ac}"), None);
        let mut ss = grid(&[&["t"], &["2026-01-01 10:00-1\u{20ac}"], &["b"]]);
        ss.sort_by(0, false);
        assert_eq!(ss.column_values(0).len(), 2);
    }

    #[test]
    fn a_cell_of_spaces_sorts_as_text_not_blank() {
        let mut ss = grid(&[&["v"], &["b"], &[" "], &["a"], &[""]]);
        ss.sort_by(0, false);
        assert_eq!(column(&ss, 0), vec!["v", " ", "a", "b", ""]);
    }

    #[test]
    fn a_filter_scrolls_back_to_the_top() {
        let mut rows: Vec<Vec<&str>> = vec![vec!["k"]];
        rows.extend((0..100).map(|i| vec![if i % 10 == 0 { "hit" } else { "miss" }]));
        let refs: Vec<&[&str]> = rows.iter().map(|r| r.as_slice()).collect();
        let mut ss = grid(&refs);
        ss.cursor = (91, 0); // a "hit" row near the bottom
        ss.scroll_row = 80;
        ss.set_filter(0, Some(set(&["hit"])));
        ss.ensure_cursor_visible(20, 200);
        assert_eq!(ss.scroll_row, 0); // header plus all ten hits fit on screen
        assert_eq!(ss.cell(ss.cursor.0, 0), "hit");
    }

    #[test]
    fn undo_and_redo_an_edit_track_the_save_point() {
        let mut ss = grid(&[&["a", "b"], &["1", "2"]]);
        ss.cursor = (1, 0);
        ss.enter_edit_mode_replace('9');
        ss.commit_edit();
        assert_eq!(ss.cell(1, 0), "9");
        assert!(ss.is_modified());
        assert!(ss.undo());
        assert_eq!(ss.cell(1, 0), "1");
        assert!(!ss.is_modified()); // back to the file as loaded
        assert!(ss.redo());
        assert_eq!(ss.cell(1, 0), "9");
        assert!(ss.is_modified());
        assert!(!ss.redo());
        assert!(ss.undo() && !ss.undo()); // one step only
    }

    #[test]
    fn undo_restores_a_filtered_range_clear_nulls_included() {
        let mut ss = grid(&[&["c", "d"], &["x", "1"], &["y", "7"], &["x", ""]]);
        ss.null_mask[3][1] = true;
        ss.set_filter(0, Some(set(&["x"]))); // file rows 1 and 3 shown
        ss.selection_anchor = Some((1, 0));
        ss.cursor = (2, 1);
        ss.clear_selection_content();
        assert_eq!((ss.rows[1].clone(), ss.rows[3].clone()), (vec!["".to_string(), "".into()], vec!["".to_string(), "".into()]));
        assert!(!ss.null_mask[3][1]);
        assert!(ss.undo());
        assert_eq!(ss.rows[1], vec!["x", "1"]);
        assert_eq!(ss.rows[3], vec!["x", ""]);
        assert!(ss.null_mask[3][1]); // the null comes back as a null
        assert_eq!(ss.rows[2], vec!["y", "7"]); // the hidden row was never touched
        assert!(ss.redo());
        assert_eq!(ss.rows[1], vec!["", ""]);
    }

    #[test]
    fn a_new_change_after_undoing_past_a_save_keeps_the_file_marked_unsaved() {
        let tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        let mut ss = grid(&[&["a", "b"], &["1", "2"]]);
        ss.cursor = (1, 0);
        ss.enter_edit_mode_replace('9');
        ss.commit_edit();
        ss.save(tmp.path()).unwrap(); // disk: 9
        assert!(ss.undo());
        assert!(ss.is_modified()); // grid: 1, disk: 9
        ss.cursor = (1, 1);
        ss.enter_edit_mode_replace('5');
        ss.commit_edit();
        assert!(!ss.redo()); // the new change dropped the redo branch
        assert!(ss.undo());
        assert!(ss.is_modified()); // grid 1,2 still differs from disk 9,2
    }

    #[test]
    fn undo_under_a_sort_returns_the_cursor_to_the_changed_row() {
        let mut ss = grid(&[&["n"], &["3"], &["1"], &["2"]]);
        ss.sort_by(0, false); // display: header, 1, 2, 3 (file rows 2, 3, 1)
        ss.cursor = (3, 0);
        ss.enter_edit_mode_replace('9');
        ss.commit_edit();
        assert_eq!(ss.rows[1][0], "9");
        ss.cursor = (1, 0);
        assert!(ss.undo());
        assert_eq!(ss.cursor, (3, 0));
        assert_eq!(ss.cell(3, 0), "3");
    }

    #[test]
    fn converting_dates_covers_hidden_rows_keeps_the_filter_and_undoes_in_one_step() {
        use crate::dates::{Order, Plan};
        let mut ss = grid(&[
            &["d", "k"],
            &["25/04/26", "a"],
            &["03/05/2026 2:15 PM", "b"],
            &["TBD", "a"],
            &["", "a"],
        ]);
        ss.null_mask[4][0] = true;
        ss.set_filter(1, Some(set(&["a"]))); // hides file row 2
        ss.set_filter(0, Some(set(&["25/04/26", "TBD", ""]))); // a filter on the date column itself
        assert_eq!(ss.visible_data_rows(), 3);

        assert_eq!(ss.convert_dates(0, Plan { dmy: Some(Order::DayFirst), ydm: false }), 2);
        assert_eq!(ss.rows[1][0], "2026-04-25");
        assert_eq!(ss.rows[2][0], "2026-05-03 14:15:00"); // the hidden row converts too
        assert_eq!(ss.rows[3][0], "TBD");
        assert!(ss.null_mask[4][0]); // nulls stay null
        assert!(ss.is_modified());
        // The date column's filter follows the new spelling, so rebuilding the
        // view keeps the same rows.
        ss.set_filter(1, Some(set(&["a"])));
        assert_eq!(ss.visible_data_rows(), 3);

        // One Ctrl+Z restores every cell and the filter.
        assert!(ss.undo());
        assert_eq!(ss.rows[1][0], "25/04/26");
        assert_eq!(ss.rows[2][0], "03/05/2026 2:15 PM");
        ss.set_filter(1, Some(set(&["a"])));
        assert_eq!(ss.visible_data_rows(), 3);
        assert!(!ss.is_modified());
    }

    // --- reading text strictly, ghost cells, growth ---

    fn saved(ss: &mut Spreadsheet) -> String {
        let tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        ss.save(tmp.path()).unwrap();
        std::fs::read_to_string(tmp.path()).unwrap()
    }

    #[test]
    fn strict_reading_refuses_text_that_isnt_delimited_data() {
        let refuse = |text: &str, delim: u8| Spreadsheet::from_text(text, delim, true).err().unwrap_or_default();
        assert!(refuse("just some prose\nover two lines", b',').contains("no commas"));
        assert!(refuse("a\tb\n1\t2", b',').contains("no commas"));
        assert!(refuse("a,b\n1,\"open\n2,3\n", b',').contains("line 2"));
        let wide = refuse("a,b\n\n1,2\n1,2,3\n", b',');
        assert!(wide.contains("\"1,2,3\" has 3 fields") && wide.contains("header row has only 2"), "{wide}");
    }

    #[test]
    fn empty_text_is_an_all_ghost_grid_that_grows_as_you_type() {
        for text in ["", "\n\n", "  \n"] {
            for strict in [true, false] {
                let ss = Spreadsheet::from_text(text, b',', strict).unwrap();
                assert_eq!((ss.num_cols(), ss.visible_data_rows()), (0, 0), "{text:?}");
            }
        }
        let mut ss = Spreadsheet::from_text("", b',', true).unwrap();
        assert_eq!(saved(&mut ss), ""); // an empty grid saves as an empty file
        // Typing in B3 makes a two-column header (A1 and B1 null) and three rows.
        ss.cursor = (2, 1);
        ss.enter_edit_mode_replace('x');
        ss.commit_edit();
        assert_eq!((ss.num_cols(), ss.num_rows()), (2, 3));
        assert_eq!(saved(&mut ss), ",\n,\n,x\n");
        assert!(ss.undo());
        assert_eq!(ss.num_cols(), 0);
        assert_eq!(saved(&mut ss), "");
        // A new file starts the same way.
        let mut fresh = Spreadsheet::new_empty(b'\t');
        assert_eq!(fresh.num_cols(), 0);
        assert_eq!(saved(&mut fresh), "");
    }

    #[test]
    fn undoing_a_grown_column_drops_its_filter_and_sort() {
        let mut ss = grid(&[&["a", "b"], &["1", "2"], &["3", "4"]]);
        ss.cursor = (1, 2); // C2, a ghost column
        ss.enter_edit_mode_replace('x');
        ss.commit_edit();
        ss.set_filter(2, Some(set(&["x"])));
        ss.sort_by(2, false);
        assert_eq!(ss.visible_data_rows(), 1);
        assert!(ss.undo());
        assert!(!ss.is_filtered() && !ss.is_sorted());
        assert_eq!(ss.visible_data_rows(), 2); // both rows back
        assert_eq!(ss.column_values(0).len(), 2); // other menus list values again
    }

    #[test]
    fn a_one_column_empty_row_survives_save_and_reload() {
        let mut ss = grid(&[&["id"], &["1"]]);
        ss.cursor = (3, 0); // two ghost rows down
        ss.enter_edit_mode_replace('9');
        ss.commit_edit();
        let text = saved(&mut ss);
        assert_eq!(text, "id\n1\n\"\"\n9\n");
        let back = Spreadsheet::from_text(&text, b',', false).unwrap();
        assert_eq!(back.num_rows(), 4);
        assert_eq!(back.cell(3, 0), "9");
    }

    #[test]
    fn missing_cells_clear_and_commit_like_stored_nulls() {
        let mut ss = Spreadsheet::from_text("a,b,c\n1,,\n2\n", b',', false).unwrap();
        // B2:C2 are stored nulls, B3:C3 missing; Delete turns all four into empty strings.
        ss.selection_anchor = Some((1, 1));
        ss.cursor = (2, 2);
        ss.clear_selection_content();
        for (r, c) in [(1, 1), (1, 2), (2, 1), (2, 2)] {
            assert!(!ss.is_null(r, c), "{r},{c}");
        }
        assert!(ss.undo());
        assert!(ss.is_null(2, 1) && ss.is_null(2, 2)); // back to null
        // Enter then Enter on a missing cell: an empty string, as for a stored null.
        let mut ss = Spreadsheet::from_text("a,b\n1\n", b',', false).unwrap();
        ss.cursor = (1, 1);
        ss.enter_edit_mode();
        ss.commit_edit();
        assert!(!ss.is_null(1, 1));
        assert_eq!(saved(&mut ss), "a,b\n1,\"\"\n");
    }

    #[test]
    fn wheel_scrolling_out_in_the_ghost_cells_does_not_snap_back() {
        let mut ss = grid(&[&["a"], &["1"]]);
        ss.cursor = (72, 0);
        ss.scroll_row = 49;
        ss.scroll_by(3, 0);
        assert_eq!(ss.scroll_row, 52);
        ss.scroll_by(-3, 0);
        assert_eq!(ss.scroll_row, 49);
        ss.scroll_by(-100, 0);
        assert_eq!(ss.scroll_row, 0);
    }

    #[test]
    fn short_rows_are_fine_when_the_header_covers_them() {
        // Empty header fields still count: the header says there are three columns.
        let mut ss = Spreadsheet::from_text("a,,\n1\n2,3\n4,5,6\n", b',', true).unwrap();
        assert_eq!(ss.num_cols(), 3);
        assert_eq!(ss.cell(1, 0), "1");
        assert!(ss.is_null(1, 1) && ss.is_null(1, 2)); // missing cells are nulls
        assert!(!ss.is_null(3, 2));
        // Save writes every row to the header's width with empty fields.
        assert_eq!(saved(&mut ss), "a,,\n1,,\n2,3,\n4,5,6\n");
    }

    #[test]
    fn a_value_typed_into_a_ghost_cell_grows_the_data_and_undo_shrinks_it() {
        let mut ss = grid(&[&["a", "b"], &["1", "2"]]);
        let widths_before = ss.column_widths.len();
        ss.move_down(false);
        ss.move_down(false);
        ss.move_down(false); // display row 3: two rows past the data
        ss.move_right(false);
        ss.move_right(false);
        ss.move_right(false); // column D: two past the data
        assert_eq!(ss.cursor, (3, 3));
        ss.enter_edit_mode_replace('x');
        ss.commit_edit();
        assert_eq!((ss.num_rows(), ss.num_cols()), (4, 4));
        assert_eq!(ss.cell(3, 3), "x");
        assert!(ss.is_null(3, 0) && ss.is_null(2, 1) && ss.is_null(0, 3));
        assert!(ss.is_modified());
        assert_eq!(saved(&mut ss), "a,b,,\n1,2,,\n,,,\n,,,x\n");

        assert!(ss.undo());
        assert_eq!((ss.num_rows(), ss.num_cols()), (2, 2));
        assert_eq!(ss.column_widths.len(), widths_before);
        assert_eq!(saved(&mut ss), "a,b\n1,2\n");

        assert!(ss.redo());
        assert_eq!(ss.cell(3, 3), "x");
        assert_eq!((ss.num_rows(), ss.num_cols()), (4, 4));
    }

    #[test]
    fn typing_nothing_into_a_ghost_cell_grows_nothing() {
        let mut ss = grid(&[&["a"], &["1"]]);
        ss.cursor = (5, 5);
        ss.enter_edit_mode();
        ss.commit_edit();
        assert_eq!((ss.num_rows(), ss.num_cols()), (2, 1));
        assert!(!ss.undo());
    }

    #[test]
    fn a_missing_cell_in_a_short_row_fills_in_place() {
        let mut ss = Spreadsheet::from_text("a,b,c\n1\n", b',', false).unwrap();
        ss.cursor = (1, 2);
        ss.enter_edit_mode_replace('z');
        ss.commit_edit();
        assert_eq!(ss.cell(1, 2), "z");
        assert!(ss.is_null(1, 1));
        assert_eq!(saved(&mut ss), "a,b,c\n1,,z\n");
        assert!(ss.undo());
        assert_eq!(saved(&mut ss), "a,b,c\n1,,\n");
    }

    #[test]
    fn a_row_grown_under_a_filter_shows_at_the_bottom_of_the_view() {
        let mut ss = grid(&[&["k"], &["x"], &["y"], &["x"]]);
        ss.set_filter(0, Some(set(&["x"]))); // shows file rows 1 and 3
        assert_eq!(ss.num_rows(), 3);
        ss.cursor = (3, 0); // the ghost row right under the view
        ss.enter_edit_mode_replace('n');
        ss.commit_edit();
        assert_eq!(ss.rows.len(), 5); // appended to the file
        assert_eq!(ss.num_rows(), 4);
        assert_eq!(ss.cell(3, 0), "n");
        assert_eq!(ss.file_row(3), 4);
        assert!(ss.undo());
        assert_eq!((ss.rows.len(), ss.num_rows()), (4, 3));
    }

    #[test]
    fn the_cursor_walks_into_ghost_cells_but_ctrl_down_finds_the_data_edge() {
        let mut ss = grid(&[&["a", "b"], &["1", "2"]]);
        ss.page_down(10, false);
        assert_eq!(ss.cursor.0, 10);
        ss.move_last_row(false);
        assert_eq!(ss.cursor.0, 1);
        ss.move_end(false);
        assert_eq!(ss.cursor.1, 1);
        ss.move_to(7, 9, false); // a click far out in the ghost area
        assert_eq!(ss.cursor, (7, 9));
        assert_eq!(ss.cursor_label(), "J8");
    }

    #[test]
    fn ghost_cells_are_clickable_and_ghost_row_numbers_widen_the_gutter() {
        let ss = grid(&[&["a"], &["1"]]);
        // Row 5 on screen is the first data row (formula bar, divider, letters above).
        match ss.hit_test(30, (FORMULA_BAR_HEIGHT + 2 + 4) as u16, 80, 30) {
            GridHit::DataCell { row, col } => assert!(row >= 2 && col >= 1, "{row},{col}"),
            other => panic!("expected a ghost data cell, got {:?}", other),
        }
        let mut far = grid(&[&["a"], &["1"]]);
        far.scroll_row = 99_990;
        assert_eq!(far.row_num_width(20), 7); // labels reach 100,009
    }

    #[test]
    fn find_and_clear_see_only_visible_rows() {
        let mut ss = grid(&[&["c"], &["apple"], &["banana"], &["apricot"]]);
        ss.set_filter(0, Some(set(&["apple", "apricot"])));
        assert_eq!(ss.find_cells("ap"), vec![(1, 0), (2, 0)]);
        ss.selection_anchor = Some((1, 0));
        ss.cursor = (2, 0);
        ss.clear_selection_content();
        assert_eq!(ss.rows[2][0], "banana");
        assert_eq!((ss.rows[1][0].as_str(), ss.rows[3][0].as_str()), ("", ""));
    }

    // --- range paste ---

    fn rows_of(ss: &Spreadsheet) -> Vec<Vec<String>> {
        ss.rows.clone()
    }

    #[test]
    fn clipboard_text_splits_on_tabs_and_lines_keeping_blank_lines_and_quoted_fields() {
        let p = |t: &str| parse_clipboard(t);
        let v = |rows: &[&[&str]]| -> Vec<Vec<String>> {
            rows.iter().map(|r| r.iter().map(|s| s.to_string()).collect()).collect()
        };
        // Excel's clipboard: CRLF after every row, the last one included.
        assert_eq!(p("a\tb\r\n1\t2\r\n"), v(&[&["a", "b"], &["1", "2"]]));
        // A blank cell in a one-column copy is a row, not a gap (sage's Copy and Excel's).
        assert_eq!(p("a\n\nb"), v(&[&["a"], &[""], &["b"]]));
        assert_eq!(p("a\r\n\r\nb\r\n"), v(&[&["a"], &[""], &["b"]]));
        assert_eq!(p("a\t\n\tb"), v(&[&["a", ""], &["", "b"]]));
        // Lone CR line ends, and a BOM.
        assert_eq!(p("\u{FEFF}a\rb"), v(&[&["a"], &["b"]]));
        // Quoted fields hold tabs, line breaks and doubled quotes, as Copy writes them.
        assert_eq!(p("\"x\ty\"\t\"say \"\"hi\"\"\"\n\"two\r\nlines\"\tz"), v(&[&["x\ty", "say \"hi\""], &["two\nlines", "z"]]));
        // A quote that doesn't end its field is text; an unclosed one doesn't swallow the rest.
        assert_eq!(p("\"12\" pipe\t10\nx\ty"), v(&[&["\"12\" pipe", "10"], &["x", "y"]]));
        assert_eq!(p("5'10\"\ta\n\"open\tb"), v(&[&["5'10\"", "a"], &["\"open", "b"]]));
        // Nothing (or one line end) is one empty field: pasting it clears a cell.
        assert_eq!(p(""), v(&[&[""]]));
        assert_eq!(p("\r\n"), v(&[&[""]]));
    }

    #[test]
    fn a_pasted_block_lands_at_the_cursor_selected_and_undoes_in_one_step() {
        let mut ss = grid(&[&["a", "b", "c"], &["1", "2", "3"], &["4", "5", "6"]]);
        ss.cursor = (1, 1);
        ss.paste("x\ty\r\nz\tw\r\n");
        assert_eq!(rows_of(&ss), vec![vec!["a", "b", "c"], vec!["1", "x", "y"], vec!["4", "z", "w"]]);
        assert_eq!(ss.selected_range(), ((1, 1), (2, 2)));
        assert_eq!(ss.cursor, (1, 1));
        assert!(ss.is_modified());
        assert!(ss.undo());
        assert_eq!(saved(&mut ss), "a,b,c\n1,2,3\n4,5,6\n");
        assert!(!ss.is_modified());
        assert!(!ss.undo()); // one step
        assert!(ss.redo());
        assert_eq!(saved(&mut ss), "a,b,c\n1,x,y\n4,z,w\n");
    }

    #[test]
    fn a_paste_under_a_filter_writes_only_the_visible_rows() {
        let mut ss = grid(&[&["st", "who"], &["Open", "a"], &["Closed", "b"], &["Open", "c"], &["Closed", "d"], &["Open", "e"]]);
        ss.set_filter(0, Some(set(&["Open"]))); // hides file rows 2 and 4
        ss.cursor = (1, 1);
        ss.paste("X\nY\nZ");
        assert_eq!(saved(&mut ss), "st,who\nOpen,X\nClosed,b\nOpen,Y\nClosed,d\nOpen,Z\n");
        // Exactly the pasted cells are selected: a rectangle of shown rows.
        assert_eq!(ss.selected_range(), ((1, 1), (3, 1)));
        assert!(ss.undo());
        assert_eq!(saved(&mut ss), "st,who\nOpen,a\nClosed,b\nOpen,c\nClosed,d\nOpen,e\n");
    }

    #[test]
    fn a_paste_under_a_sort_follows_the_order_shown() {
        let mut ss = grid(&[&["n", "tag"], &["3", ""], &["1", ""], &["2", ""]]);
        ss.sort_by(0, false); // shows 1 2 3: file rows 2 3 1
        ss.cursor = (1, 1);
        ss.paste("one\ntwo\nthree");
        assert_eq!(column(&ss, 1), vec!["tag", "one", "two", "three"]);
        assert_eq!(saved(&mut ss), "n,tag\n3,three\n1,one\n2,two\n");
    }

    #[test]
    fn copy_then_paste_in_one_filtered_sorted_view_lines_up_row_for_row() {
        let mut ss = grid(&[
            &["gw", "n", "copy"],
            &["YYC", "3", ""],
            &["YYZ", "10", ""],
            &["YYC", "1", ""],
            &["YVR", "7", ""],
            &["YYC", "2", ""],
        ]);
        ss.set_filter(0, Some(set(&["YYC"])));
        ss.sort_by(1, true); // shows file rows 1 (3), 5 (2), 3 (1)
        ss.selection_anchor = Some((1, 1));
        ss.cursor = (3, 1);
        let clip = ss.copy_selection_tsv();
        ss.selection_anchor = None;
        ss.cursor = (1, 2);
        ss.paste(&clip);
        for r in 1..ss.rows.len() {
            let (n, copy) = (&ss.rows[r][1], &ss.rows[r][2]);
            assert!(copy.is_empty() || copy == n, "file row {r}: {n} vs {copy}");
        }
        assert_eq!(saved(&mut ss), "gw,n,copy\nYYC,3,3\nYYZ,10,\"\"\nYYC,1,1\nYVR,7,\"\"\nYYC,2,2\n");
    }

    #[test]
    fn a_block_past_the_last_shown_row_appends_rows_and_undo_removes_them() {
        let mut ss = grid(&[&["k", "v"], &["x", "1"], &["y", "2"], &["x", "3"]]);
        ss.set_filter(0, Some(set(&["x"]))); // shows file rows 1 and 3
        ss.cursor = (2, 1); // the last shown row
        ss.paste("a\nb\nc");
        assert_eq!(ss.rows.len(), 6); // two rows appended to the file
        assert_eq!(column(&ss, 1), vec!["v", "1", "a", "b", "c"]); // shown under the view
        assert_eq!(ss.selected_range(), ((2, 1), (4, 1)));
        assert_eq!(saved(&mut ss), "k,v\nx,1\ny,2\nx,a\n,b\n,c\n");
        assert!(ss.undo());
        assert_eq!(ss.num_rows(), 3);
        assert_eq!(saved(&mut ss), "k,v\nx,1\ny,2\nx,3\n");
        assert!(ss.redo());
        assert_eq!(saved(&mut ss), "k,v\nx,1\ny,2\nx,a\n,b\n,c\n");
    }

    #[test]
    fn a_block_wider_than_the_data_adds_columns_and_undo_restores_every_row() {
        let mut ss = grid(&[&["a", "b"], &["1", "2"], &["3", "4"]]);
        ss.cursor = (1, 1);
        ss.paste("p\tq\nr\ts");
        assert_eq!(saved(&mut ss), "a,b,\n1,p,q\n3,r,s\n");
        assert!(ss.undo());
        assert_eq!(ss.num_cols(), 2);
        assert!(ss.rows.iter().all(|r| r.len() == 2), "{:?}", ss.rows);
        assert_eq!(saved(&mut ss), "a,b\n1,2\n3,4\n");
        // A value typed into the ghost column afterwards still saves.
        ss.move_to(1, 2, false);
        ss.enter_edit_mode_replace('z');
        ss.commit_edit();
        assert_eq!(saved(&mut ss), "a,b,\n1,2,z\n3,4,\n");
    }

    #[test]
    fn redo_of_a_paste_into_ghost_cells_regrows_every_cell() {
        let mut ss = grid(&[&["a"], &["1"]]);
        ss.cursor = (2, 1); // a ghost row and column
        ss.paste("p\tq\nr\ts");
        assert_eq!(saved(&mut ss), "a,,\n1,,\n,p,q\n,r,s\n");
        assert!(ss.undo());
        assert_eq!(saved(&mut ss), "a\n1\n");
        assert!(ss.redo());
        assert_eq!(saved(&mut ss), "a,,\n1,,\n,p,q\n,r,s\n");
    }

    #[test]
    fn one_value_fills_the_visible_cells_of_a_selection() {
        let mut ss = grid(&[&["k", "v"], &["x", "1"], &["y", "2"], &["x", "3"]]);
        ss.set_filter(0, Some(set(&["x"])));
        ss.selection_anchor = Some((1, 1));
        ss.cursor = (2, 1);
        ss.paste("0\r\n");
        assert_eq!(saved(&mut ss), "k,v\nx,0\ny,2\nx,0\n");
        assert_eq!(ss.selected_range(), ((1, 1), (2, 1)));
    }

    #[test]
    fn a_block_repeats_over_a_selection_it_divides_and_otherwise_pastes_once() {
        let mut ss = grid(&[&["h"], &[""], &[""], &[""], &[""], &[""]]);
        ss.selection_anchor = Some((1, 0));
        ss.cursor = (4, 0); // four rows: two copies of a two-row block
        ss.paste("a\nb");
        assert_eq!(column(&ss, 0), vec!["h", "a", "b", "a", "b", ""]);
        // Three rows don't hold whole copies: the block goes once, at full size.
        let mut ss = grid(&[&["h"], &[""], &[""], &[""], &[""], &[""]]);
        ss.selection_anchor = Some((3, 0));
        ss.cursor = (1, 0);
        ss.paste("a\nb");
        assert_eq!(column(&ss, 0), vec!["h", "a", "b", "", "", ""]);
        assert_eq!(ss.selected_range(), ((1, 0), (2, 0)));
    }

    #[test]
    fn empty_and_missing_fields_leave_empty_cells_alone_and_clear_values() {
        let mut ss = Spreadsheet::from_text("a,b,c\n1,,\"\"\n4,5,6\n", b',', true).unwrap();
        assert!(ss.is_null(1, 1) && !ss.is_null(1, 2));
        ss.cursor = (1, 0);
        // Row 1: empties over a value, a null and an empty string. Row 2 is
        // short: its missing cells leave 5 and 6 alone.
        ss.paste("\t\t\n7");
        assert_eq!(saved(&mut ss), "a,b,c\n\"\",,\"\"\n7,5,6\n");
        assert!(ss.is_null(1, 1));
        // Pasting what's there already changes nothing and records nothing.
        let mut same = grid(&[&["a", "b"], &["1", "2"]]);
        same.cursor = (1, 0);
        same.paste("1\t2");
        assert!(!same.is_modified());
        assert!(!same.undo());
    }

    #[test]
    fn empty_values_never_grow_the_data() {
        let mut ss = grid(&[&["a"], &["1"]]);
        ss.cursor = (1, 0);
        ss.paste("x\t\n\t");
        assert_eq!(saved(&mut ss), "a\nx\n");
        assert_eq!(ss.num_cols(), 1);
    }

    #[test]
    fn a_paste_on_the_header_under_a_filter_continues_on_the_first_shown_row() {
        let mut ss = grid(&[&["k"], &["y"], &["x"]]);
        ss.set_filter(0, Some(set(&["x"])));
        ss.cursor = (0, 0);
        ss.paste("key\nX");
        assert_eq!(saved(&mut ss), "key\ny\nX\n");
    }

    #[test]
    fn a_paste_while_editing_goes_into_the_cell_without_its_line_end() {
        let mut ss = grid(&[&["a"], &["1"]]);
        ss.cursor = (1, 0);
        ss.enter_edit_mode();
        ss.paste("2\t3\r\n");
        assert!(ss.is_editing());
        ss.commit_edit();
        assert_eq!(ss.cell(1, 0), "12\t3");
    }

    #[test]
    fn copy_quotes_a_carriage_return_so_it_pastes_back_as_one_cell() {
        let mut ss = grid(&[&["a", "b"], &["x\ry", "z"], &["", ""]]);
        ss.selection_anchor = Some((1, 0));
        ss.cursor = (1, 1);
        let clip = ss.copy_selection_tsv();
        assert_eq!(clip, "\"x\ry\"\tz");
        ss.selection_anchor = None;
        ss.cursor = (2, 0);
        ss.paste(&clip);
        // Line ends are made LF on the way in, as everywhere in the grid.
        assert_eq!((ss.cell(2, 0), ss.cell(2, 1)), ("x\ny", "z"));
    }

    #[test]
    fn undoing_a_clear_of_short_rows_shortens_them_so_later_values_still_save() {
        let mut ss = Spreadsheet::from_text("a,b\n1,2\n3,4\n", b',', false).unwrap();
        ss.cursor = (1, 2);
        ss.paste("p\tq\n\t"); // widens the header; row 2 stays short
        ss.clear_selection_content(); // the pasted block is selected: row 2's missing cells become real
        assert!(ss.undo() && ss.undo());
        assert!(ss.rows.iter().all(|r| r.len() <= ss.num_cols()), "{:?}", ss.rows);
        ss.selection_anchor = None;
        ss.cursor = (2, 2);
        ss.paste("v\tw");
        assert_eq!(saved(&mut ss), "a,b,,\n1,2,,\n3,4,v,w\n");

        // Redo brings back the filled cells too, so the clear lands on every one.
        let mut ss = Spreadsheet::from_text("a,b\n1,2\n", b',', false).unwrap();
        ss.cursor = (2, 0);
        ss.paste("x"); // appends the short row [x]
        ss.selection_anchor = Some((2, 1));
        ss.clear_selection_content();
        let tip = saved(&mut ss);
        assert_eq!(tip, "a,b\n1,2\n\"\",\"\"\n");
        assert!(ss.undo() && ss.undo() && ss.redo() && ss.redo());
        assert_eq!(saved(&mut ss), tip);
    }

    #[test]
    fn undoing_an_empty_commit_into_a_short_row_shortens_it_again() {
        let mut ss = Spreadsheet::from_text("a,b\n1\n", b',', false).unwrap();
        assert_eq!(ss.rows[1].len(), 1);
        ss.cursor = (1, 1);
        ss.enter_edit_mode();
        ss.commit_edit(); // the missing cell becomes an empty string
        assert_eq!(saved(&mut ss), "a,b\n1,\"\"\n");
        assert!(ss.undo());
        assert_eq!(ss.rows[1].len(), 1);
        assert!(ss.redo());
        assert_eq!(saved(&mut ss), "a,b\n1,\"\"\n");
    }

    #[test]
    fn a_one_column_copy_ending_in_blanks_pastes_back_every_row() {
        let mut ss = Spreadsheet::from_text("h,k\nx,1\n,2\n\"\",3\n", b',', false).unwrap();
        assert!(ss.is_null(2, 0) && !ss.is_null(3, 0));
        ss.selection_anchor = Some((1, 0));
        ss.cursor = (3, 0);
        let clip = ss.copy_selection_tsv();
        assert_eq!(parse_clipboard(&clip).len(), 3);
        // Pasted back over its own selection: nothing changes.
        ss.paste(&clip);
        assert!(!ss.is_modified());
        // Into another column: the blanks clear what was there.
        let mut other = grid(&[&["h", "k"], &["1", "a"], &["2", "b"], &["3", "c"]]);
        other.cursor = (1, 1);
        other.paste(&clip);
        assert_eq!(column(&other, 1), vec!["k", "x", "", ""]);
        // Over a selection twice its height: two copies, blanks included.
        let mut tall = grid(&[&["h"], &["1"], &["2"], &["3"], &["4"], &["5"], &["6"]]);
        tall.selection_anchor = Some((1, 0));
        tall.cursor = (6, 0);
        tall.paste(&clip);
        assert_eq!(column(&tall, 0), vec!["h", "x", "", "", "x", "", ""]);
        // Only blanks: still one row each.
        let mut blanks = grid(&[&["h"], &[""], &[""]]);
        blanks.selection_anchor = Some((1, 0));
        blanks.cursor = (2, 0);
        assert_eq!(parse_clipboard(&blanks.copy_selection_tsv()).len(), 2);
    }

    #[test]
    fn a_cell_starting_with_a_bom_copies_and_pastes_intact() {
        let mut ss = grid(&[&["h", "k"], &["\u{FEFF}x", ""]]);
        ss.cursor = (1, 0);
        let clip = ss.copy_selection_tsv();
        ss.cursor = (1, 1);
        ss.paste(&clip);
        assert_eq!(ss.cell(1, 1), "\u{FEFF}x");
    }

    #[test]
    fn pasted_values_that_fail_the_filter_stay_shown_until_it_changes() {
        let mut ss = grid(&[&["st"], &["Open"], &["Closed"], &["Open"]]);
        ss.set_filter(0, Some(set(&["Open"])));
        ss.cursor = (1, 0);
        ss.paste("Closed\nClosed");
        assert_eq!(column(&ss, 0), vec!["st", "Closed", "Closed"]);
        ss.set_filter(0, Some(set(&["Open"])));
        assert_eq!(ss.num_rows(), 1);
    }

    // --- insert & delete rows and columns (Ctrl+= / Ctrl+-) ---

    fn text(ss: &Spreadsheet) -> String {
        ss.to_text()
    }

    #[test]
    fn ctrl_equals_inserts_as_many_blank_rows_as_are_selected_above_them() {
        let mut ss = grid(&[&["h", "k"], &["1", "a"], &["2", "b"], &["3", "c"]]);
        ss.selection_anchor = Some((2, 0));
        ss.cursor = (3, 1);
        ss.insert_lines().unwrap();
        assert_eq!(text(&ss), "h,k\n1,a\n,\n,\n2,b\n3,c\n");
        assert_eq!(ss.selected_range(), ((2, 0), (3, 1))); // on the new rows
        assert!(ss.is_modified());
        assert!(ss.undo());
        assert_eq!(text(&ss), "h,k\n1,a\n2,b\n3,c\n");
        assert!(!ss.is_modified());
        assert!(ss.redo());
        assert_eq!(text(&ss), "h,k\n1,a\n,\n,\n2,b\n3,c\n");
    }

    #[test]
    fn ctrl_minus_deletes_the_rows_the_selection_covers() {
        let mut ss = grid(&[&["h", "k"], &["1", "a"], &["2", "b"], &["3", "c"]]);
        ss.selection_anchor = Some((1, 1));
        ss.cursor = (2, 0);
        ss.delete_lines();
        assert_eq!(text(&ss), "h,k\n3,c\n");
        assert_eq!(ss.cursor, (1, 0));
        assert!(!ss.has_selection());
        assert!(ss.undo());
        assert_eq!(text(&ss), "h,k\n1,a\n2,b\n3,c\n");
        assert!(ss.redo());
        assert_eq!(text(&ss), "h,k\n3,c\n");
    }

    #[test]
    fn under_a_filter_and_sort_rows_go_in_by_the_cursor_and_only_shown_rows_go_out() {
        let original = "gw,n\nYYC,3\nYYZ,10\nYYC,1\nYVR,7\nYYC,2\n";
        let mut ss = grid(&[&["gw", "n"], &["YYC", "3"], &["YYZ", "10"], &["YYC", "1"], &["YVR", "7"], &["YYC", "2"]]);
        ss.set_filter(0, Some(set(&["YYC"])));
        ss.sort_by(1, false); // shows file rows 3 (1), 5 (2), 1 (3)
        ss.cursor = (2, 0); // on file row 5
        ss.insert_lines().unwrap();
        // Into the file just above row 5, and on screen where the cursor is.
        assert_eq!(column(&ss, 1), vec!["n", "1", "", "2", "3"]);
        assert_eq!(text(&ss), "gw,n\nYYC,3\nYYZ,10\nYYC,1\nYVR,7\n,\nYYC,2\n");
        // Delete the three rows shown under the header: YYZ and YVR, hidden, stay.
        ss.selection_anchor = Some((1, 0));
        ss.cursor = (3, 1);
        ss.delete_lines();
        assert_eq!(text(&ss), "gw,n\nYYC,3\nYYZ,10\nYVR,7\n");
        assert_eq!(column(&ss, 1), vec!["n", "3"]);
        assert!(ss.undo());
        assert_eq!(column(&ss, 1), vec!["n", "1", "", "2", "3"]); // back where they showed
        assert!(ss.undo());
        assert_eq!(text(&ss), original);
        assert_eq!(column(&ss, 1), vec!["n", "1", "2", "3"]);
    }

    #[test]
    fn the_header_row_moves_only_without_a_filter_or_sort() {
        let mut ss = grid(&[&["h", "k", "m"], &["1"], &["2", "b", "c"]]);
        ss.cursor = (0, 1);
        ss.insert_lines().unwrap(); // a blank header row, as wide as the old one
        assert_eq!(ss.num_cols(), 3);
        assert_eq!(text(&ss), ",,\nh,k,m\n1,,\n2,b,c\n");
        assert!(ss.undo());
        // Deleting the header: the next row, short, takes its place at full width.
        ss.cursor = (0, 0);
        ss.delete_lines();
        assert_eq!(ss.num_cols(), 3);
        assert_eq!(text(&ss), "1,,\n2,b,c\n");
        assert!(ss.undo());
        assert_eq!(text(&ss), "h,k,m\n1,,\n2,b,c\n");
        assert_eq!(ss.rows[1].len(), 1); // short again
        // While filtered, the header stays put.
        ss.set_filter(1, Some(set(&["b"])));
        ss.cursor = (0, 0);
        assert!(ss.insert_lines().is_err());
        ss.select_all();
        ss.delete_lines(); // the shown data row; the header and the hidden row stay
        assert_eq!(text(&ss), "h,k,m\n1,,\n");
    }

    #[test]
    fn whole_columns_go_in_to_the_left_and_out_with_their_filters() {
        let original = "a,b,c\n1,x,p\n2,y,q\n3,x,r\n";
        let mut ss = grid(&[&["a", "b", "c"], &["1", "x", "p"], &["2", "y", "q"], &["3", "x", "r"]]);
        ss.set_filter(1, Some(set(&["x"]))); // hides file row 2
        ss.sort_by(2, true);
        ss.select_column(1, false);
        ss.insert_lines().unwrap();
        assert_eq!(text(&ss), "a,,b,c\n1,,x,p\n2,,y,q\n3,,x,r\n");
        assert!(ss.column_filter(2).is_some() && ss.column_filter(1).is_none());
        assert_eq!(ss.column_sort(3), Some(true));
        // Delete the filtered column (now C): its filter goes, the row it hid comes back.
        ss.select_column(2, false);
        ss.delete_lines();
        assert_eq!(text(&ss), "a,,c\n1,,p\n2,,q\n3,,r\n");
        assert!(!ss.is_filtered());
        assert_eq!(ss.visible_data_rows(), 3);
        assert_eq!(ss.column_sort(2), Some(true));
        assert!(ss.undo()); // the column, its filter, and the hidden row
        assert!(ss.column_filter(2).is_some());
        assert_eq!(ss.visible_data_rows(), 2);
        assert!(ss.undo());
        assert_eq!(text(&ss), original);
        assert!(ss.column_filter(1).is_some());
        assert_eq!(ss.column_sort(2), Some(true));
        assert!(ss.redo() && ss.redo());
        assert_eq!(text(&ss), "a,,c\n1,,p\n2,,q\n3,,r\n");
    }

    #[test]
    fn a_selection_the_full_height_is_columns_and_a_letter_pick_is_even_in_one_column() {
        // Shift-selecting top to bottom across two columns deletes those columns.
        let mut ss = grid(&[&["a", "b", "c"], &["1", "2", "3"]]);
        ss.selection_anchor = Some((0, 0));
        ss.cursor = (1, 1);
        ss.delete_lines();
        assert_eq!(text(&ss), "c\n3\n");
        // In a one-column file the letter picks the column, though that
        // selection is also the full width.
        let mut one = grid(&[&["a"], &["1"], &["2"]]);
        one.select_column(0, false);
        one.insert_lines().unwrap();
        assert_eq!(text(&one), ",a\n,1\n,2\n");
        // A row picked by its number is a row, though the grid is one row tall.
        let mut flat = grid(&[&["a", "b"]]);
        flat.select_row(0, false);
        flat.insert_lines().unwrap();
        assert_eq!(text(&flat), ",\na,b\n");
    }

    #[test]
    fn deleting_every_row_leaves_an_empty_grid_that_undo_refills() {
        let mut ss = grid(&[&["a", "b"], &["1", "2"]]);
        ss.select_all();
        ss.delete_lines();
        assert_eq!(text(&ss), "");
        assert_eq!(ss.num_cols(), 0);
        assert!(ss.undo());
        assert_eq!(text(&ss), "a,b\n1,2\n");
        assert!(ss.redo() && ss.undo());
        assert_eq!(text(&ss), "a,b\n1,2\n");
    }

    #[test]
    fn inserting_or_deleting_past_the_data_does_nothing() {
        let mut ss = grid(&[&["a", "b"], &["1", "2"]]);
        ss.cursor = (5, 0);
        ss.insert_lines().unwrap();
        ss.delete_lines();
        ss.selection_anchor = Some((0, 4));
        ss.cursor = (1, 5); // ghost columns, full height
        ss.insert_lines().unwrap();
        ss.delete_lines();
        assert_eq!(text(&ss), "a,b\n1,2\n");
        assert!(!ss.is_modified());
        assert!(!ss.undo());
    }

    fn assert_consistent(ss: &Spreadsheet, context: &str) {
        assert!(!ss.rows.is_empty(), "{context}");
        assert_eq!(ss.rows.len(), ss.null_mask.len(), "{context}");
        for (r, (row, mask)) in ss.rows.iter().zip(&ss.null_mask).enumerate() {
            assert_eq!(row.len(), mask.len(), "{context}: row {r}");
            assert!(row.len() <= ss.num_cols(), "{context}: row {r} is longer than the header: {:?}", ss.rows);
        }
        if let Some(view) = &ss.view {
            assert_eq!(view.first(), Some(&0), "{context}: the header leads the view");
            let mut seen = HashSet::new();
            for &r in view {
                assert!(r < ss.rows.len() && seen.insert(r), "{context}: view {:?}", view);
            }
            if ss.filters.is_empty() {
                assert_eq!(view.len(), ss.rows.len(), "{context}: a sort alone hides no row");
            }
        }
        assert!(ss.filters.keys().all(|&c| c < ss.num_cols()), "{context}: a filter on a column that's gone");
        assert!(ss.sorts.iter().all(|&(c, _)| c < ss.num_cols()), "{context}: a sort on a column that's gone");
    }

    #[test]
    fn random_row_and_column_changes_undo_and_redo_exactly() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |n: usize| -> usize {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n.max(1) as u64) as usize
        };
        for round in 0..400 {
            let mut ss = Spreadsheet::from_text("h,k,m\n1,a,x\n2,b\n3,,z\n4,a,w\n5,b,v\n", b',', false).unwrap();
            let mut states = vec![text(&ss)];
            for op in 0..14 {
                let context = format!("round {round}, op {op}");
                let steps = ss.undo.len();
                let (rows, cols) = (ss.num_rows() + 2, ss.num_cols() + 2);
                ss.selection_anchor = Some((next(rows), next(cols)));
                ss.cursor = (next(rows), next(cols));
                match next(12) {
                    0 | 1 => {
                        let _ = ss.insert_lines();
                    }
                    2 | 3 => ss.delete_lines(),
                    4 => {
                        ss.select_column(next(cols), false);
                        let _ = ss.insert_lines();
                    }
                    5 => {
                        ss.select_column(next(cols), next(2) == 0);
                        ss.delete_lines();
                    }
                    6 => {
                        ss.select_row(next(rows), next(2) == 0);
                        if next(2) == 0 { ss.delete_lines() } else { let _ = ss.insert_lines(); }
                    }
                    7 => {
                        ss.selection_anchor = None;
                        ss.enter_edit_mode_replace('q');
                        ss.commit_edit();
                    }
                    8 => ss.paste("p\tq\n\tr"),
                    9 => ss.clear_selection_content(),
                    _ => {
                        let c = next(ss.num_cols().max(1));
                        if ss.num_cols() > 0 {
                            match next(4) {
                                0 => ss.set_filter(c, Some(set(&["a", "b", "", "q"]))),
                                1 => ss.sort_by(c, next(2) == 0),
                                2 => ss.clear_filters(),
                                _ => ss.clear_sort(),
                            }
                        }
                    }
                }
                assert_consistent(&ss, &context);
                if ss.undo.len() > steps {
                    states.push(text(&ss));
                }
                assert_eq!(&text(&ss), states.last().unwrap(), "{context}: a change with no undo step");
            }
            // Filters and sorts aren't undo steps: change them between undos
            // and redos too, which must still give back the same data.
            let shuffle_view = |ss: &mut Spreadsheet, pick: usize, col: usize, desc: bool| {
                if ss.num_cols() == 0 {
                    return;
                }
                let c = col % ss.num_cols();
                match pick {
                    0 => ss.set_filter(c, Some(set(&["a", "b", "", "q"]))),
                    1 => ss.sort_by(c, desc),
                    2 => ss.clear_filters(),
                    3 => ss.clear_sort(),
                    _ => {}
                }
            };
            for i in (0..states.len() - 1).rev() {
                shuffle_view(&mut ss, next(8), next(8), next(2) == 0);
                assert!(ss.undo(), "round {round}");
                assert_consistent(&ss, &format!("round {round}, undo to {i}"));
                assert_eq!(text(&ss), states[i], "round {round}, undo to {i}");
            }
            assert!(!ss.undo());
            for (i, state) in states.iter().enumerate().skip(1) {
                shuffle_view(&mut ss, next(8), next(8), next(2) == 0);
                assert!(ss.redo(), "round {round}");
                assert_consistent(&ss, &format!("round {round}, redo to {i}"));
                assert_eq!(&text(&ss), state, "round {round}, redo to {i}");
            }
        }
    }

    #[test]
    fn rows_deleted_with_no_view_come_back_into_a_sort_put_on_since() {
        let mut ss = Spreadsheet::from_text("name,n\na,3\nb,1\nc,2\n", b',', false).unwrap();
        ss.cursor = (2, 0);
        ss.delete_lines(); // b
        ss.sort_by(1, false);
        assert!(ss.undo());
        assert_eq!(column(&ss, 0), vec!["name", "b", "c", "a"]);
        assert_eq!(ss.find_cells("b"), vec![(1, 0)]);
        assert_eq!(ss.cursor.0, 1); // on the restored row
    }

    #[test]
    fn undoing_a_big_delete_under_a_sort_puts_every_row_back_in_its_place() {
        let n = 20_000;
        let mut text_in = String::from("k,v\n");
        for i in 0..n {
            text_in.push_str(&format!("{},{}\n", i, (i * 7919) % n));
        }
        let mut ss = Spreadsheet::from_text(&text_in, b',', false).unwrap();
        ss.sort_by(1, true);
        let before = column(&ss, 0);
        ss.selection_anchor = Some((5, 0));
        ss.cursor = (n / 2, 1);
        ss.delete_lines();
        assert_eq!(ss.num_rows(), n + 1 - (n / 2 - 4));
        let started = std::time::Instant::now();
        assert!(ss.undo());
        assert!(started.elapsed() < std::time::Duration::from_secs(2), "undo took {:?}", started.elapsed());
        assert_eq!(column(&ss, 0), before);
        assert_eq!(text(&ss), text_in);
    }

    #[test]
    fn deleting_every_column_leaves_an_empty_grid() {
        let mut ss = Spreadsheet::from_text("a,b\n1,2\n3,4\n", b',', false).unwrap();
        ss.select_column(0, false);
        ss.extend_column_selection(1);
        ss.delete_lines();
        assert_eq!((text(&ss), ss.rows.len()), (String::new(), 1));
        // Typing into it starts afresh: no rows of "" come back.
        ss.cursor = (0, 0);
        ss.enter_edit_mode_replace('x');
        ss.commit_edit();
        assert_eq!(text(&ss), "x\n");
        assert!(ss.undo() && ss.undo());
        assert_eq!(text(&ss), "a,b\n1,2\n3,4\n");
    }

    #[test]
    fn ctrl_equals_and_ctrl_minus_do_nothing_in_an_empty_grid() {
        let mut ss = Spreadsheet::from_text("", b',', false).unwrap();
        ss.insert_lines().unwrap();
        ss.delete_lines();
        assert!(!ss.is_modified());
        assert!(!ss.undo());
        assert_eq!(ss.rows.len(), 1);
    }

    #[test]
    fn a_filter_put_on_before_redoing_a_delete_of_every_row_goes_and_comes_back() {
        let mut ss = grid(&[&["a", "b"], &["1", "x"]]);
        ss.select_all();
        ss.delete_lines();
        assert!(ss.undo());
        ss.set_filter(1, Some(set(&["x"])));
        assert!(ss.redo());
        assert!(!ss.is_filtered()); // no column left to hold it
        assert!(ss.undo());
        assert_eq!(ss.column_filter(1), Some(&set(&["x"])));
        assert_eq!(column(&ss, 1), vec!["b", "x"]);
    }

    #[test]
    fn the_cursor_lands_on_the_change_after_a_delete_and_its_undo() {
        let mut ss = grid(&[&["a", "b", "c", "d"], &["1", "2", "3", "4"]]);
        // Columns B:C selected left to right by keys: the cursor ends on C,
        // and lands on B.
        ss.selection_anchor = Some((0, 1));
        ss.cursor = (1, 2);
        ss.delete_lines();
        assert_eq!(text(&ss), "a,d\n1,4\n");
        assert_eq!(ss.cursor.1, 1);
        ss.cursor = (0, 0);
        assert!(ss.undo());
        assert_eq!(ss.cursor.1, 1); // on the restored columns
        let mut rows = grid(&[&["h"], &["1"], &["2"], &["3"], &["4"]]);
        rows.cursor = (3, 0);
        rows.delete_lines();
        rows.cursor = (0, 0);
        assert!(rows.undo());
        assert_eq!(rows.cursor.0, 3);
    }
}
