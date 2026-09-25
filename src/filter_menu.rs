//! Excel-style filter & sort menu for one spreadsheet column, opened with
//! Alt+Down or a right-click. Sorting and filtering only change what the grid
//! shows (see `Spreadsheet`): the file keeps its rows and their order. The menu
//! also offers the date conversion (`DateDialog`), which does change the data.

use crate::dates::{self, Order, Plan, Verdict, YearFirstVerdict};
use crate::spreadsheet::{col_letter, Spreadsheet};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind},
    queue,
    style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor},
    terminal,
};
use std::collections::HashSet;
use std::io::{self, Write};
use unicode_width::UnicodeWidthChar;

/// What the user chose.
#[derive(Debug, PartialEq)]
pub enum FilterAction {
    SortAscending,
    SortDescending,
    ClearSort,
    ClearFilter,
    /// Show rows whose value in the column is one of these; `None` shows all.
    Filter(Option<HashSet<String>>),
    /// Open the date-conversion dialog for the column.
    ConvertDates,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Command {
    SortAscending,
    SortDescending,
    ClearSort,
    ClearFilter,
    ConvertDates,
}

/// A column's name for titles: its header cell, else its letter.
fn column_name(ss: &Spreadsheet, col: usize) -> (String, String) {
    let letter = col_letter(col);
    let header = ss.cell(0, col).trim().replace(['\n', '\r', '\t'], " ");
    let name = if header.is_empty() { letter.clone() } else { header };
    (name, letter)
}

enum Step {
    Stay,
    Close(Option<FilterAction>),
}

/// Where the last draw put the box, for mouse hits.
#[derive(Default, Clone, Copy)]
struct Layout {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
}

const BOX_BG: Color = Color::Rgb { r: 40, g: 40, b: 45 };
const HIGHLIGHT_BG: Color = Color::Rgb { r: 0, g: 95, b: 135 };
const TEXT: Color = Color::Rgb { r: 220, g: 220, b: 225 };
const DIM: Color = Color::Rgb { r: 140, g: 140, b: 150 };
const BORDER: Color = Color::Cyan;
const OK_LABEL: &str = "[ OK ]";
const CANCEL_LABEL: &str = "[ Cancel ]";

pub struct FilterMenu {
    title: String,
    commands: Vec<(Command, String)>,
    /// Distinct values ("" = blanks) and their row counts, in list order.
    values: Vec<(String, usize)>,
    /// Lowercased display labels, for search.
    lower: Vec<String>,
    /// Ticks on the full list.
    checked: Vec<bool>,
    /// Ticks as opened, so OK with nothing changed leaves the filter alone.
    initial_checked: Vec<bool>,
    search: String,
    /// Values listed: indices into `values` (all of them when not searching).
    matches: Vec<usize>,
    /// Ticks while searching, parallel to `matches`. Excel starts every search
    /// result ticked, and OK keeps exactly the ticked results.
    search_checked: Vec<bool>,
    /// Highlighted row: the commands, then the search line, "(Select all)", values.
    highlight: usize,
    /// First listed value shown.
    scroll: usize,
    /// Value rows the box shows; fixed once opened so the box never resizes.
    list_rows: usize,
    box_width: usize,
    message: Option<&'static str>,
    layout: Layout,
}

impl FilterMenu {
    pub fn for_column(ss: &Spreadsheet, col: usize) -> Self {
        let (name, letter) = column_name(ss, col);

        let sort = ss.column_sort(col);
        let mut commands = vec![
            (
                Command::SortAscending,
                format!("Sort ascending (A to Z, 0 to 9){}", if sort == Some(false) { "  \u{2713}" } else { "" }),
            ),
            (
                Command::SortDescending,
                format!("Sort descending (Z to A, 9 to 0){}", if sort == Some(true) { "  \u{2713}" } else { "" }),
            ),
        ];
        if ss.is_sorted() {
            commands.push((Command::ClearSort, "Clear sort (back to file order)".to_string()));
        }
        if ss.column_filter(col).is_some() {
            commands.push((Command::ClearFilter, format!("Clear filter from {}", name)));
        }
        if ss.has_dates_to_convert(col) {
            commands.push((Command::ConvertDates, "Convert dates to ISO 8601 (yyyy-mm-dd)".to_string()));
        }

        let values = ss.column_values(col);
        let checked = match ss.column_filter(col) {
            Some(allowed) => values.iter().map(|(v, _)| allowed.contains(v)).collect(),
            None => vec![true; values.len()],
        };
        Self::new(format!("Filter: {} (column {})", name, letter), commands, values, checked)
    }

    fn new(title: String, commands: Vec<(Command, String)>, values: Vec<(String, usize)>, checked: Vec<bool>) -> Self {
        let lower = values.iter().map(|(v, _)| value_label(v).to_lowercase()).collect();
        let matches = (0..values.len()).collect();
        let search_row = commands.len();
        let mut menu = FilterMenu {
            title,
            commands,
            values,
            lower,
            initial_checked: checked.clone(),
            checked,
            search: String::new(),
            matches,
            search_checked: Vec::new(),
            highlight: search_row,
            scroll: 0,
            list_rows: 10,
            box_width: 48,
            message: None,
            layout: Layout::default(),
        };
        menu.box_width = menu.natural_width();
        menu
    }

    pub fn run<W: Write>(&mut self, writer: &mut W) -> io::Result<Option<FilterAction>> {
        let (term_w, term_h) = terminal::size()?;
        self.fit(term_w as usize, term_h as usize);
        let mut dirty = true;
        loop {
            if dirty {
                self.draw(writer)?;
            }
            dirty = true;
            match event::read()? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Release {
                        dirty = false;
                        continue;
                    }
                    if let Step::Close(action) = self.handle_key(&key) {
                        return Ok(action);
                    }
                }
                Event::Mouse(me) => {
                    if matches!(me.kind, MouseEventKind::Moved | MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
                        dirty = false;
                        continue;
                    }
                    if let Step::Close(action) = self.handle_mouse(&me) {
                        return Ok(action);
                    }
                }
                Event::Resize(w, h) => {
                    // Refit to the new size and wipe the old box; the grid
                    // behind repaints when the menu closes.
                    self.fit(w as usize, h as usize);
                    clear_to_grid_bg(writer)?;
                }
                _ => {}
            }
        }
    }

    // --- state ---------------------------------------------------------------

    fn search_row(&self) -> usize {
        self.commands.len()
    }

    fn select_all_row(&self) -> usize {
        self.commands.len() + 1
    }

    fn first_value_row(&self) -> usize {
        self.commands.len() + 2
    }

    fn row_count(&self) -> usize {
        self.first_value_row() + self.matches.len()
    }

    fn searching(&self) -> bool {
        !self.search.is_empty()
    }

    /// Tick state of listed value `k` (an index into `matches`).
    fn is_checked(&self, k: usize) -> bool {
        if self.searching() {
            self.search_checked.get(k).copied().unwrap_or(false)
        } else {
            self.matches.get(k).map_or(false, |&i| self.checked[i])
        }
    }

    fn set_checked(&mut self, k: usize, on: bool) {
        if self.searching() {
            if let Some(c) = self.search_checked.get_mut(k) {
                *c = on;
            }
        } else if let Some(&i) = self.matches.get(k) {
            self.checked[i] = on;
        }
    }

    fn ticked_count(&self) -> usize {
        (0..self.matches.len()).filter(|&k| self.is_checked(k)).count()
    }

    fn toggle_row(&mut self, row: usize) {
        if row == self.select_all_row() {
            let all = self.ticked_count() == self.matches.len();
            for k in 0..self.matches.len() {
                self.set_checked(k, !all);
            }
        } else if row >= self.first_value_row() {
            let k = row - self.first_value_row();
            let on = !self.is_checked(k);
            self.set_checked(k, on);
        }
    }

    fn update_matches(&mut self) {
        if self.searching() {
            let q = self.search.to_lowercase();
            self.matches = (0..self.values.len()).filter(|&i| self.lower[i].contains(&q)).collect();
            self.search_checked = vec![true; self.matches.len()];
        } else {
            self.matches = (0..self.values.len()).collect();
            self.search_checked.clear();
        }
        self.scroll = 0;
        self.highlight = self.highlight.min(self.row_count().saturating_sub(1));
    }

    fn move_highlight(&mut self, delta: isize) {
        let last = self.row_count().saturating_sub(1) as isize;
        self.highlight = (self.highlight as isize + delta).clamp(0, last) as usize;
        if self.highlight >= self.first_value_row() {
            let k = self.highlight - self.first_value_row();
            if k < self.scroll {
                self.scroll = k;
            } else if k >= self.scroll + self.list_rows {
                self.scroll = k + 1 - self.list_rows;
            }
        }
    }

    fn scroll_list(&mut self, delta: isize) {
        let max = self.matches.len().saturating_sub(self.list_rows) as isize;
        self.scroll = (self.scroll as isize + delta).clamp(0, max.max(0)) as usize;
    }

    fn command_action(command: Command) -> FilterAction {
        match command {
            Command::SortAscending => FilterAction::SortAscending,
            Command::SortDescending => FilterAction::SortDescending,
            Command::ClearSort => FilterAction::ClearSort,
            Command::ClearFilter => FilterAction::ClearFilter,
            Command::ConvertDates => FilterAction::ConvertDates,
        }
    }

    /// OK: filter to the ticked values (the ticked search results while searching).
    fn ok(&mut self) -> Step {
        // Nothing changed: leave the column's filter exactly as it was. The list
        // only holds values the other columns' filters leave, so rebuilding the
        // filter from it could drop values this column was hiding.
        if !self.searching() && self.checked == self.initial_checked {
            return Step::Close(None);
        }
        let allowed: HashSet<String> = (0..self.matches.len())
            .filter(|&k| self.is_checked(k))
            .map(|k| self.values[self.matches[k]].0.clone())
            .collect();
        if allowed.is_empty() {
            self.message = Some("Tick at least one value");
            return Step::Stay;
        }
        if allowed.len() == self.values.len() {
            return Step::Close(Some(FilterAction::Filter(None)));
        }
        Step::Close(Some(FilterAction::Filter(Some(allowed))))
    }

    fn handle_key(&mut self, key: &KeyEvent) -> Step {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        self.message = None;
        let page = self.list_rows.max(1) as isize;
        match key.code {
            KeyCode::Esc => Step::Close(None),
            KeyCode::Char('c') | KeyCode::Char('C') if ctrl => Step::Close(None),
            KeyCode::Up => {
                self.move_highlight(-1);
                Step::Stay
            }
            KeyCode::Down => {
                self.move_highlight(1);
                Step::Stay
            }
            // Paging stays inside the value list once there, so a PageUp can't
            // land on a command that the next Enter would run.
            KeyCode::PageUp => {
                let floor = self.first_value_row();
                if self.highlight >= floor {
                    let target = self.highlight.saturating_sub(page as usize).max(floor);
                    self.move_highlight(target as isize - self.highlight as isize);
                } else {
                    self.move_highlight(-page);
                }
                Step::Stay
            }
            KeyCode::PageDown => {
                self.move_highlight(page);
                Step::Stay
            }
            KeyCode::Enter => {
                if self.highlight < self.search_row() {
                    Step::Close(Some(Self::command_action(self.commands[self.highlight].0)))
                } else {
                    self.ok()
                }
            }
            KeyCode::Char(' ') if self.highlight >= self.select_all_row() => {
                self.toggle_row(self.highlight);
                Step::Stay
            }
            KeyCode::Backspace => {
                if self.search.pop().is_some() {
                    self.highlight = self.search_row();
                    self.update_matches();
                }
                Step::Stay
            }
            // Anything typed goes to the search box. Ctrl+Alt together is how
            // Windows reports AltGr, which types characters like @ and € on
            // non-US layouts.
            KeyCode::Char(c) if ctrl == alt => {
                self.search.push(c);
                self.highlight = self.search_row();
                self.update_matches();
                Step::Stay
            }
            _ => Step::Stay,
        }
    }

    fn handle_mouse(&mut self, me: &MouseEvent) -> Step {
        let l = self.layout;
        let inside = me.column >= l.left
            && me.column < l.left + l.width
            && me.row >= l.top
            && me.row < l.top + l.height;
        match me.kind {
            MouseEventKind::ScrollDown if inside => {
                self.scroll_list(3);
                Step::Stay
            }
            MouseEventKind::ScrollUp if inside => {
                self.scroll_list(-3);
                Step::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if !inside {
                    return Step::Close(None);
                }
                self.message = None;
                let r = (me.row - l.top) as usize; // 0 = top border
                let n = self.commands.len();
                if (1..=n).contains(&r) {
                    return Step::Close(Some(Self::command_action(self.commands[r - 1].0)));
                }
                let search_line = n + 2;
                let list_top = search_line + 2;
                if r == search_line {
                    self.highlight = self.search_row();
                } else if r == search_line + 1 {
                    self.highlight = self.select_all_row();
                    self.toggle_row(self.highlight);
                } else if r >= list_top && r < list_top + self.list_rows {
                    let k = self.scroll + (r - list_top);
                    if k < self.matches.len() {
                        self.highlight = self.first_value_row() + k;
                        self.toggle_row(self.highlight);
                    }
                } else if r == list_top + self.list_rows + 1 {
                    // Footer: [ OK ]  [ Cancel ]
                    let x = (me.column - l.left) as usize; // 0 = left border
                    let ok = 2..2 + OK_LABEL.len();
                    let cancel_start = 2 + OK_LABEL.len() + 2;
                    let cancel = cancel_start..cancel_start + CANCEL_LABEL.len();
                    if ok.contains(&x) {
                        return self.ok();
                    }
                    if cancel.contains(&x) {
                        return Step::Close(None);
                    }
                }
                Step::Stay
            }
            _ => Step::Stay,
        }
    }

    // --- drawing ---------------------------------------------------------------

    fn natural_width(&self) -> usize {
        let longest_value = self
            .values
            .iter()
            .map(|(v, n)| text_width(&value_label(v)) + group_thousands(*n).len())
            .max()
            .unwrap_or(0);
        let longest_command = self.commands.iter().map(|(_, s)| text_width(s)).max().unwrap_or(0);
        // "│ [x] " + value + "  " + count + " │"
        (longest_value + 10)
            .max(longest_command + 4)
            .max(text_width(&self.title) + 6)
            .max(44)
            .min(80)
    }

    /// Size the box to the terminal: value rows fill what's left after the
    /// commands, search, select-all, borders and footer (`commands + 7` rows).
    fn fit(&mut self, term_w: usize, term_h: usize) {
        self.box_width = self.natural_width().min(term_w.saturating_sub(2)).max(24);
        let chrome = self.commands.len() + 7;
        let room = term_h.saturating_sub(chrome + 2).max(1);
        self.list_rows = self.values.len().clamp(1, room.min(20));
    }

    fn draw<W: Write>(&mut self, w: &mut W) -> io::Result<()> {
        let (term_w, term_h) = terminal::size()?;
        let width = self.box_width.min(term_w as usize);
        let inner = width.saturating_sub(2);
        let height = self.commands.len() + self.list_rows + 7;
        let left = ((term_w as usize).saturating_sub(width) / 2) as u16;
        let top = ((term_h as usize).saturating_sub(height) / 2) as u16;
        self.layout = Layout { left, top, width: width as u16, height: height as u16 };

        let mut row = top;
        draw_title(w, left, row, inner, &self.title)?;
        row += 1;

        for i in 0..self.commands.len() {
            let text = format!(" {}", self.commands[i].1);
            draw_line(w,left, row, inner, &text, i == self.highlight, TEXT)?;
            row += 1;
        }
        draw_rule(w,left, row, inner, '\u{251c}', '\u{2524}')?;
        row += 1;

        // Search line
        let search_text = format!(" Search: {}", self.search);
        let on_search = self.highlight == self.search_row();
        draw_line(w,left, row, inner, &search_text, on_search, TEXT)?;
        let caret = (left as usize + 1 + text_width(&search_text)).min(left as usize + inner) as u16;
        let search_screen_row = row;
        row += 1;

        // Select all
        let ticked = self.ticked_count();
        let mark = if ticked == self.matches.len() && ticked > 0 {
            "[x]"
        } else if ticked == 0 {
            "[ ]"
        } else {
            "[-]"
        };
        let label = if self.searching() { "(Select all search results)" } else { "(Select all)" };
        let text = format!(" {} {}", mark, label);
        draw_line(w,left, row, inner, &text, self.highlight == self.select_all_row(), TEXT)?;
        row += 1;

        // Values
        for j in 0..self.list_rows {
            let k = self.scroll + j;
            if k >= self.matches.len() {
                let text = if k == 0 { "   (no matching values)" } else { "" };
                draw_line(w,left, row, inner, text, false, DIM)?;
            } else {
                let (value, count) = &self.values[self.matches[k]];
                let mark = if self.is_checked(k) { "[x]" } else { "[ ]" };
                let count = group_thousands(*count);
                let label_room = inner.saturating_sub(6 + count.len() + 2);
                let label = fit_text(&value_label(value), label_room);
                let pad = inner.saturating_sub(5 + text_width(&label) + count.len() + 1);
                let text = format!(" {} {}{}{} ", mark, label, " ".repeat(pad), count);
                let color = if value.is_empty() { DIM } else { TEXT };
                draw_line(w,left, row, inner, &text, self.first_value_row() + k == self.highlight, color)?;
            }
            row += 1;
        }
        draw_rule(w,left, row, inner, '\u{251c}', '\u{2524}')?;
        row += 1;

        // Footer: buttons, then a hint or message
        let hint = match self.message {
            Some(m) => m.to_string(),
            None => format!("{} of {} ticked \u{00b7} Space tick \u{00b7} Enter OK", ticked, self.matches.len()),
        };
        let footer = format!(" {}  {}  {}", OK_LABEL, CANCEL_LABEL, hint);
        let color = if self.message.is_some() { Color::Yellow } else { DIM };
        draw_line(w,left, row, inner, &footer, false, color)?;
        row += 1;
        queue!(
            w,
            cursor::MoveTo(left, row),
            SetBackgroundColor(BOX_BG),
            SetForegroundColor(BORDER),
            Print(format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(inner))),
            ResetColor,
        )?;

        if on_search {
            queue!(w, cursor::MoveTo(caret, search_screen_row), cursor::Show)?;
        } else {
            queue!(w, cursor::Hide)?;
        }
        w.flush()
    }
}

// --- date conversion dialog --------------------------------------------------

/// What the date dialog's buttons and keys do.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DateChoice {
    /// Convert, reading the column's dates with this plan.
    Convert(Plan),
    /// Convert with the highlighted option (the day-or-month list).
    Selected,
    Cancel,
}

/// Confirmation for "Convert dates to ISO 8601": what the column holds, what
/// it will become, and (for a column the data can't settle) a list to pick
/// day or month first from.
pub struct DateDialog {
    title: String,
    lines: Vec<(String, Color)>,
    /// Pickable readings, drawn as a list after `lines[..options_at]`;
    /// ↑/↓ move `selected`, Enter converts with it. Nothing is highlighted at
    /// first: the data couldn't tell, so Enter must not pick a reading unseen.
    options: Vec<(String, DateChoice)>,
    options_at: usize,
    selected: Option<usize>,
    buttons: Vec<(String, DateChoice)>,
    /// Keys that pick a button: Enter, and D / M for day or month first.
    enter: Option<DateChoice>,
    letter_keys: Vec<(char, DateChoice)>,
    /// Screen rows of the options and of the buttons, and the buttons' column
    /// ranges, from the last draw (for mouse clicks).
    option_rows: Vec<u16>,
    button_row: u16,
    button_x: Vec<std::ops::Range<u16>>,
    layout: Layout,
}

impl DateDialog {
    pub fn for_column(ss: &Spreadsheet, col: usize) -> Self {
        let (name, letter) = column_name(ss, col);
        let title = format!("Convert dates: {} (column {})", name, letter);
        let a = ss.analyze_dates(col);
        let show = |v: &str| value_label(v);
        let plural = |n: usize| if n == 1 { "value" } else { "values" };
        let mut lines: Vec<(String, Color)> = Vec::new();
        let close = vec![("[ Close ]".to_string(), DateChoice::Cancel)];
        let not_dates = |lines: &mut Vec<(String, Color)>| {
            if a.impossible > 0 {
                let ex = a.impossible_example.as_deref().map(|v| format!(", e.g. {}", show(v))).unwrap_or_default();
                lines.push((
                    format!("{} can't be real dates in any reading, left as they are{}", group_thousands(a.impossible), ex),
                    Color::Yellow,
                ));
            }
            if a.left > 0 {
                let ex = a.left_example.as_deref().map(|v| format!(", e.g. \"{}\"", show(v))).unwrap_or_default();
                lines.push((format!("{} not dates, left as they are{}", group_thousands(a.left), ex), DIM));
            }
        };

        // Refused: an order question the data answers both ways.
        if a.is_mixed() {
            if let Verdict::Mixed { dayfirst, monthfirst } = &a.verdict {
                lines.push(("This column mixes day-first and month-first dates:".into(), TEXT));
                lines.push((format!("  {}   (first part over 12)", show(dayfirst)), TEXT));
                lines.push((format!("  {}   (second part over 12)", show(monthfirst)), TEXT));
            }
            if let YearFirstVerdict::Mixed { ymd, ydm } = &a.year_first {
                lines.push(("This column mixes year-month-day and year-day-month dates:".into(), TEXT));
                lines.push((format!("  {}   (last part over 12)", show(ymd)), TEXT));
                lines.push((format!("  {}   (middle part over 12)", show(ydm)), TEXT));
            }
            lines.push(("Converting would mean guessing. Re-export the file.".into(), Color::Yellow));
            return Self::build(title, lines, close, Some(DateChoice::Cancel), Vec::new(), Vec::new(), 0);
        }

        // Nothing to convert: say why instead of doing nothing.
        if a.convert == 0 {
            lines.push(("Nothing to convert.".into(), TEXT));
            not_dates(&mut lines);
            return Self::build(title, lines, close, Some(DateChoice::Cancel), Vec::new(), Vec::new(), 0);
        }

        let ydm = matches!(a.year_first, YearFirstVerdict::Ydm { .. });
        let mut plan = Plan { dmy: None, ydm };
        let mut options: Vec<(String, DateChoice)> = Vec::new();
        let mut options_at = 0;
        match &a.verdict {
            Verdict::Ambiguous { example } => {
                lines.push(("Day or month first? Every d/m/y date here has both parts 12".into(), TEXT));
                lines.push(("or less, so the data can't tell. Check with whoever sent it.".into(), TEXT));
                options_at = lines.len();
                for (label, order) in [("Day first  ", Order::DayFirst), ("Month first", Order::MonthFirst)] {
                    let choice = Plan { dmy: Some(order), ydm };
                    let after = dates::to_iso(example, choice).unwrap_or_default();
                    options.push((
                        format!("{}   {}  \u{2192}  {}", label, show(example), after),
                        DateChoice::Convert(choice),
                    ));
                }
            }
            Verdict::Proven { order, evidence, count } => {
                plan.dmy = Some(*order);
                let (which, part) = match order {
                    Order::DayFirst => ("Day first", "first"),
                    Order::MonthFirst => ("Month first", "second"),
                };
                lines.push((
                    format!("{}: {} {} like {} ({} part over 12)", which, group_thousands(*count), plural(*count), show(evidence), part),
                    TEXT,
                ));
            }
            _ => {}
        }
        if let YearFirstVerdict::Ydm { evidence, count } = &a.year_first {
            lines.push((
                format!("Year-day-month: {} {} like {} (middle part over 12)", group_thousands(*count), plural(*count), show(evidence)),
                TEXT,
            ));
        }
        if lines.is_empty() {
            lines.push(("No day/month question: these formats read only one way.".into(), TEXT));
        }

        // What converts. While the order is still to be chosen, d/m/y examples
        // are the ones shown above; the rest read the same either way.
        let examples: Vec<(String, String)> = a
            .examples
            .iter()
            .filter_map(|v| dates::to_iso(v, plan).map(|after| (show(v), after)))
            .collect();
        let colon = if examples.is_empty() { "" } else { ", e.g.:" };
        lines.push((format!("{} to convert{}", group_thousands(a.convert), colon), TEXT));
        for (before, after) in examples {
            lines.push((format!("  {}  \u{2192}  {}", before, after), TEXT));
        }
        if a.already > 0 {
            lines.push((format!("{} already ISO", group_thousands(a.already)), DIM));
        }
        not_dates(&mut lines);
        lines.push(("Rows hidden by a filter convert too. Ctrl+Z undoes it.".into(), DIM));

        if options.is_empty() {
            let convert = DateChoice::Convert(plan);
            let buttons = vec![("[ Convert ]".to_string(), convert), ("[ Cancel ]".to_string(), DateChoice::Cancel)];
            Self::build(title, lines, buttons, Some(convert), Vec::new(), Vec::new(), 0)
        } else {
            // D and M still pick directly; the list is the main way.
            let keys = vec![('d', options[0].1), ('m', options[1].1)];
            let buttons = vec![("[ Convert ]".to_string(), DateChoice::Selected), ("[ Cancel ]".to_string(), DateChoice::Cancel)];
            Self::build(title, lines, buttons, Some(DateChoice::Selected), keys, options, options_at)
        }
    }

    fn build(
        title: String,
        lines: Vec<(String, Color)>,
        buttons: Vec<(String, DateChoice)>,
        enter: Option<DateChoice>,
        letter_keys: Vec<(char, DateChoice)>,
        options: Vec<(String, DateChoice)>,
        options_at: usize,
    ) -> Self {
        DateDialog {
            title,
            lines,
            options,
            options_at,
            selected: None,
            buttons,
            enter,
            letter_keys,
            option_rows: Vec::new(),
            button_row: 0,
            button_x: Vec::new(),
            layout: Layout::default(),
        }
    }

    /// A `Selected` choice becomes the highlighted option's; `None` while
    /// nothing is highlighted yet.
    fn resolve(&self, choice: DateChoice) -> Option<DateChoice> {
        match choice {
            DateChoice::Selected => self.selected.and_then(|i| self.options.get(i)).map(|(_, c)| *c),
            other => Some(other),
        }
    }

    /// Show the dialog. `Some(plan)` means convert with that plan; `None`
    /// means cancelled, refused, or nothing to convert.
    pub fn run<W: Write>(&mut self, writer: &mut W) -> io::Result<Option<Plan>> {
        let mut dirty = true;
        loop {
            if dirty {
                self.draw(writer)?;
            }
            dirty = true;
            let choice = match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => self.key_choice(&key),
                Event::Mouse(me) if matches!(me.kind, MouseEventKind::Down(MouseButton::Left)) => self.mouse_choice(&me),
                Event::Resize(..) => {
                    clear_to_grid_bg(writer)?;
                    None
                }
                _ => {
                    dirty = false;
                    None
                }
            };
            match choice {
                Some(DateChoice::Convert(plan)) => return Ok(Some(plan)),
                Some(DateChoice::Cancel) | Some(DateChoice::Selected) => return Ok(None),
                None => {}
            }
        }
    }

    /// A key's effect: a final choice, or `None` to stay (↑/↓ just move the
    /// highlight in the list).
    fn key_choice(&mut self, key: &KeyEvent) -> Option<DateChoice> {
        match key.code {
            KeyCode::Esc => Some(DateChoice::Cancel),
            KeyCode::Char('c') | KeyCode::Char('C') if key.modifiers.contains(KeyModifiers::CONTROL) => Some(DateChoice::Cancel),
            KeyCode::Up if !self.options.is_empty() => {
                let last = self.options.len() - 1;
                self.selected = Some(self.selected.map_or(last, |i| i.saturating_sub(1)));
                None
            }
            KeyCode::Down if !self.options.is_empty() => {
                let last = self.options.len() - 1;
                self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(last)));
                None
            }
            KeyCode::Enter => self.enter.and_then(|c| self.resolve(c)),
            KeyCode::Char(c) => {
                let c = c.to_ascii_lowercase();
                self.letter_keys.iter().find(|(k, _)| *k == c).map(|(_, choice)| *choice)
            }
            _ => None,
        }
    }

    fn mouse_choice(&mut self, me: &MouseEvent) -> Option<DateChoice> {
        let l = self.layout;
        let inside = me.column >= l.left && me.column < l.left + l.width && me.row >= l.top && me.row < l.top + l.height;
        if !inside {
            return Some(DateChoice::Cancel);
        }
        if let Some(i) = self.option_rows.iter().position(|&r| r == me.row) {
            self.selected = Some(i); // a click picks the reading; Convert (or Enter) applies it
            return None;
        }
        if me.row == self.button_row {
            for (range, (_, choice)) in self.button_x.iter().zip(&self.buttons) {
                if range.contains(&me.column) {
                    return self.resolve(*choice);
                }
            }
        }
        None
    }

    fn draw<W: Write>(&mut self, w: &mut W) -> io::Result<()> {
        let (term_w, term_h) = terminal::size()?;
        let buttons_text: String = self.buttons.iter().map(|(label, _)| format!(" {}", label)).collect();
        let hint = match self.enter {
            Some(DateChoice::Convert(_)) => "   Enter convert \u{00b7} Esc cancel",
            Some(DateChoice::Selected) => "   \u{2191}\u{2193} choose \u{00b7} Enter convert \u{00b7} Esc cancel",
            _ => "   Esc close",
        };
        let option_text = |i: usize, text: &str| format!(" {} {}", if Some(i) == self.selected { ">" } else { " " }, text);
        let longest = self
            .lines
            .iter()
            .map(|(s, _)| text_width(s) + 2)
            .chain(self.options.iter().map(|(s, _)| text_width(s) + 4))
            .chain([text_width(&buttons_text) + text_width(hint) + 1, text_width(&self.title) + 6])
            .max()
            .unwrap_or(40);
        let width = (longest + 2).clamp(40, 96).min(term_w as usize);
        let inner = width.saturating_sub(2);
        let height = self.lines.len() + self.options.len() + 4;
        let left = ((term_w as usize).saturating_sub(width) / 2) as u16;
        let top = ((term_h as usize).saturating_sub(height) / 2) as u16;
        self.layout = Layout { left, top, width: width as u16, height: height as u16 };

        let mut row = top;
        draw_title(w, left, row, inner, &self.title)?;
        row += 1;
        self.option_rows.clear();
        for i in 0..=self.lines.len() {
            if i == self.options_at {
                // The pickable readings, the highlighted one marked and lit.
                for (k, (text, _)) in self.options.iter().enumerate() {
                    draw_line(w, left, row, inner, &option_text(k, text), Some(k) == self.selected, TEXT)?;
                    self.option_rows.push(row);
                    row += 1;
                }
            }
            if let Some((text, color)) = self.lines.get(i) {
                draw_line(w, left, row, inner, &format!(" {}", text), false, *color)?;
                row += 1;
            }
        }
        draw_rule(w, left, row, inner, '\u{251c}', '\u{2524}')?;
        row += 1;

        // Buttons row: remember where each button sits for mouse clicks.
        self.button_row = row;
        self.button_x.clear();
        let mut x = left + 1;
        for (label, _) in &self.buttons {
            x += 1; // the space before each button
            let start = x;
            x += text_width(label) as u16;
            self.button_x.push(start..x);
        }
        draw_line(w, left, row, inner, &format!("{}{}", buttons_text, hint), false, TEXT)?;
        row += 1;
        queue!(
            w,
            cursor::MoveTo(left, row),
            SetBackgroundColor(BOX_BG),
            SetForegroundColor(BORDER),
            Print(format!("\u{2514}{}\u{2518}", "\u{2500}".repeat(inner))),
            ResetColor,
            cursor::Hide,
        )?;
        w.flush()
    }
}

// --- drawing helpers shared by the menu and the date dialog -------------------

/// One boxed row: `│text…padding│`, highlighted or in `fg`.
fn draw_line<W: Write>(w: &mut W, left: u16, row: u16, inner: usize, text: &str, highlighted: bool, fg: Color) -> io::Result<()> {
    let body = fit_text(text, inner);
    let pad = inner.saturating_sub(text_width(&body));
    let bg = if highlighted { HIGHLIGHT_BG } else { BOX_BG };
    queue!(
        w,
        cursor::MoveTo(left, row),
        SetBackgroundColor(BOX_BG),
        SetForegroundColor(BORDER),
        Print("\u{2502}"),
        SetBackgroundColor(bg),
        SetForegroundColor(if highlighted { Color::White } else { fg }),
        Print(format!("{}{}", body, " ".repeat(pad))),
        SetBackgroundColor(BOX_BG),
        SetForegroundColor(BORDER),
        Print("\u{2502}"),
        ResetColor,
    )
}

/// A horizontal rule across the box, `l` and `r` at its ends.
fn draw_rule<W: Write>(w: &mut W, left: u16, row: u16, inner: usize, l: char, r: char) -> io::Result<()> {
    queue!(
        w,
        cursor::MoveTo(left, row),
        SetBackgroundColor(BOX_BG),
        SetForegroundColor(BORDER),
        Print(format!("{}{}{}", l, "\u{2500}".repeat(inner), r)),
        ResetColor,
    )
}

/// The box's top border with `title` set into it.
fn draw_title<W: Write>(w: &mut W, left: u16, row: u16, inner: usize, title: &str) -> io::Result<()> {
    let title = fit_text(&format!(" {} ", title), inner.saturating_sub(1));
    let rule = "\u{2500}".repeat(inner.saturating_sub(1 + text_width(&title)));
    queue!(
        w,
        cursor::MoveTo(left, row),
        SetBackgroundColor(BOX_BG),
        SetForegroundColor(BORDER),
        Print("\u{250c}\u{2500}"),
        SetForegroundColor(TEXT),
        Print(&title),
        SetForegroundColor(BORDER),
        Print(format!("{}\u{2510}", rule)),
    )
}

/// Wipe the screen to the grid's background (after a resize under a pop-up).
fn clear_to_grid_bg<W: Write>(w: &mut W) -> io::Result<()> {
    queue!(
        w,
        SetBackgroundColor(Color::AnsiValue(234)),
        terminal::Clear(terminal::ClearType::All),
        ResetColor,
    )
}

/// How a value shows in the list: blanks as "(Blanks)", line breaks and tabs as spaces.
fn value_label(value: &str) -> String {
    if value.is_empty() {
        "(Blanks)".to_string()
    } else {
        value.replace(['\n', '\r', '\t'], " ")
    }
}

fn text_width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(0)).sum()
}

/// Truncate `s` to `max` display columns, ending in "…" when cut.
fn fit_text(s: &str, max: usize) -> String {
    if text_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if used + cw + 1 > max {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('\u{2026}');
    out
}

fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn menu(values: &[(&str, usize)]) -> FilterMenu {
        let values: Vec<(String, usize)> = values.iter().map(|(v, n)| (v.to_string(), *n)).collect();
        let checked = vec![true; values.len()];
        let commands = vec![
            (Command::SortAscending, "Sort ascending".to_string()),
            (Command::SortDescending, "Sort descending".to_string()),
        ];
        FilterMenu::new("Filter: GTWY (column A)".to_string(), commands, values, checked)
    }

    fn close(step: Step) -> Option<FilterAction> {
        match step {
            Step::Close(a) => a,
            Step::Stay => panic!("menu stayed open"),
        }
    }

    #[test]
    fn unticking_a_value_filters_to_the_rest() {
        let mut m = menu(&[("YYC", 3), ("YYZ", 2), ("", 1)]);
        m.handle_key(&key(KeyCode::Down)); // (Select all)
        m.handle_key(&key(KeyCode::Down)); // YYC
        m.handle_key(&key(KeyCode::Down)); // YYZ
        m.handle_key(&key(KeyCode::Char(' ')));
        let action = close(m.handle_key(&key(KeyCode::Enter)));
        let expected: HashSet<String> = ["YYC".to_string(), String::new()].into_iter().collect();
        assert_eq!(action, Some(FilterAction::Filter(Some(expected))));
    }

    #[test]
    fn everything_ticked_clears_the_filter() {
        let mut m = menu(&[("YYC", 3), ("YYZ", 2)]);
        m.checked = vec![true, false];
        m.initial_checked = m.checked.clone(); // opened with a filter on YYC
        m.handle_key(&key(KeyCode::Down)); // (Select all)
        m.handle_key(&key(KeyCode::Char(' ')));
        assert_eq!(close(m.handle_key(&key(KeyCode::Enter))), Some(FilterAction::Filter(None)));
    }

    #[test]
    fn enter_with_nothing_changed_leaves_the_filter_alone() {
        let mut m = menu(&[("YYC", 3), ("YYZ", 2)]);
        m.checked = vec![true, false];
        m.initial_checked = m.checked.clone();
        assert_eq!(close(m.handle_key(&key(KeyCode::Enter))), None);
    }

    #[test]
    fn page_up_stays_in_the_value_list() {
        let values: Vec<(String, usize)> = (0..30).map(|i| (format!("v{i:02}"), 1)).collect();
        let pairs: Vec<(&str, usize)> = values.iter().map(|(v, n)| (v.as_str(), *n)).collect();
        let mut m = menu(&pairs);
        for _ in 0..8 {
            m.handle_key(&key(KeyCode::Down)); // into the values
        }
        m.handle_key(&key(KeyCode::PageUp));
        assert_eq!(m.highlight, m.first_value_row());
    }

    #[test]
    fn altgr_characters_reach_the_search() {
        let mut m = menu(&[("a@b", 1), ("c", 1)]);
        m.handle_key(&KeyEvent::new(KeyCode::Char('@'), KeyModifiers::CONTROL | KeyModifiers::ALT));
        assert_eq!(m.search, "@");
        assert_eq!(m.matches, vec![0]);
    }

    #[test]
    fn nothing_ticked_keeps_the_menu_open() {
        let mut m = menu(&[("YYC", 3), ("YYZ", 2)]);
        m.handle_key(&key(KeyCode::Down)); // (Select all)
        m.handle_key(&key(KeyCode::Char(' '))); // untick all
        assert!(matches!(m.handle_key(&key(KeyCode::Enter)), Step::Stay));
        assert!(m.message.is_some());
    }

    #[test]
    fn search_keeps_only_the_matching_values() {
        let mut m = menu(&[("YYC", 3), ("YYZ", 2), ("YEG", 4)]);
        m.handle_key(&key(KeyCode::Char('y')));
        m.handle_key(&key(KeyCode::Char('y')));
        assert_eq!(m.matches.len(), 2);
        let action = close(m.handle_key(&key(KeyCode::Enter)));
        let expected: HashSet<String> = ["YYC".to_string(), "YYZ".to_string()].into_iter().collect();
        assert_eq!(action, Some(FilterAction::Filter(Some(expected))));
    }

    #[test]
    fn space_types_into_search_but_ticks_in_the_list() {
        let mut m = menu(&[("New York", 3), ("Newark", 2)]);
        for c in "new y".chars() {
            m.handle_key(&key(KeyCode::Char(c)));
        }
        assert_eq!(m.search, "new y");
        assert_eq!(m.matches, vec![0]);
    }

    #[test]
    fn enter_on_a_command_returns_it() {
        let mut m = menu(&[("a", 1)]);
        m.handle_key(&key(KeyCode::Up));
        m.handle_key(&key(KeyCode::Up)); // Sort ascending
        assert_eq!(close(m.handle_key(&key(KeyCode::Enter))), Some(FilterAction::SortAscending));
    }

    fn one_column(values: &[&str]) -> Spreadsheet {
        let mut ss = Spreadsheet::new_empty(b',');
        ss.rows = std::iter::once("when")
            .chain(values.iter().copied())
            .map(|v| vec![v.to_string(), "x".to_string()])
            .collect();
        ss.null_mask = ss.rows.iter().map(|r| vec![false; r.len()]).collect();
        ss
    }

    #[test]
    fn the_menu_offers_date_conversion_only_on_date_columns() {
        let ss = one_column(&["25/04/2026", "03/04/2026"]);
        let offers = |ss: &Spreadsheet, col| {
            FilterMenu::for_column(ss, col).commands.iter().any(|(c, _)| *c == Command::ConvertDates)
        };
        assert!(offers(&ss, 0));
        assert!(!offers(&ss, 1)); // the "x" column
        // Year-day-month and impossible dates are offered too, so the dialog can say what it sees.
        assert!(offers(&one_column(&["2023-31-12"]), 0));
        assert!(offers(&one_column(&["20/20/2000"]), 0));
        assert!(!offers(&one_column(&["2023-12-31"]), 0)); // already ISO
    }

    #[test]
    fn the_date_dialog_follows_the_verdict() {
        let enter = key(KeyCode::Enter);
        let plan = |dmy, ydm| DateChoice::Convert(Plan { dmy, ydm });

        let mut proven = DateDialog::for_column(&one_column(&["25/04/2026", "03/04/2026"]), 0);
        assert_eq!(proven.key_choice(&enter), Some(plan(Some(Order::DayFirst), false)));
        assert!(proven.lines.iter().any(|(l, _)| l.contains("25/04/2026  \u{2192}  2026-04-25")));

        let mut mixed = DateDialog::for_column(&one_column(&["25/04/2026", "04/25/2026"]), 0);
        assert_eq!(mixed.key_choice(&enter), Some(DateChoice::Cancel));
        assert_eq!(mixed.key_choice(&key(KeyCode::Char('d'))), None);

        let mut named = DateDialog::for_column(&one_column(&["25-Apr-2026"]), 0);
        assert_eq!(named.key_choice(&enter), Some(plan(None, false)));

        let mut ydm = DateDialog::for_column(&one_column(&["2023-31-12"]), 0);
        assert_eq!(ydm.key_choice(&enter), Some(plan(None, true)));
        assert!(ydm.lines.iter().any(|(l, _)| l.contains("2023-31-12  \u{2192}  2023-12-31")));

        let mut impossible = DateDialog::for_column(&one_column(&["20/20/2000"]), 0);
        assert_eq!(impossible.key_choice(&enter), Some(DateChoice::Cancel));
        assert!(impossible.lines.iter().any(|(l, _)| l.contains("can't be real dates") && l.contains("20/20/2000")));

        let mut year_mixed = DateDialog::for_column(&one_column(&["2023-31-12", "2023-12-31"]), 0);
        assert_eq!(year_mixed.key_choice(&enter), Some(DateChoice::Cancel));
    }

    #[test]
    fn the_day_or_month_question_is_a_list_picked_with_the_arrows() {
        let enter = key(KeyCode::Enter);
        let day = DateChoice::Convert(Plan { dmy: Some(Order::DayFirst), ydm: false });
        let month = DateChoice::Convert(Plan { dmy: Some(Order::MonthFirst), ydm: false });
        let ask = || DateDialog::for_column(&one_column(&["03/04/2026"]), 0);

        let mut d = ask();
        assert_eq!(d.options.len(), 2);
        assert!(d.options[0].0.contains("03/04/2026  \u{2192}  2026-04-03"));
        assert!(d.options[1].0.contains("03/04/2026  \u{2192}  2026-03-04"));
        // Nothing is highlighted yet, so Enter can't pick a reading unseen.
        assert_eq!(d.key_choice(&enter), None);
        assert_eq!(d.key_choice(&key(KeyCode::Down)), None);
        assert_eq!(d.selected, Some(0));
        assert_eq!(d.key_choice(&enter), Some(day));

        let mut d = ask();
        d.key_choice(&key(KeyCode::Down));
        d.key_choice(&key(KeyCode::Down));
        d.key_choice(&key(KeyCode::Down)); // stays on the last
        assert_eq!(d.key_choice(&enter), Some(month));

        let mut d = ask();
        d.key_choice(&key(KeyCode::Up)); // from nothing, Up starts at the bottom
        assert_eq!(d.selected, Some(1));
        d.key_choice(&key(KeyCode::Up));
        assert_eq!(d.key_choice(&enter), Some(day));

        // D and M still pick directly.
        assert_eq!(ask().key_choice(&key(KeyCode::Char('M'))), Some(month));
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(7), "7");
        assert_eq!(group_thousands(12345), "12,345");
        assert_eq!(group_thousands(1234567), "1,234,567");
    }
}
