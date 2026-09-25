//! Excel-style filter & sort menu for one spreadsheet column, opened with
//! Alt+Down or a right-click. Sorting and filtering only change what the grid
//! shows (see `Spreadsheet`): the file keeps its rows and their order.

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
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Command {
    SortAscending,
    SortDescending,
    ClearSort,
    ClearFilter,
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
        let letter = col_letter(col);
        let header = ss.cell(0, col).trim().replace(['\n', '\r', '\t'], " ");
        let name = if header.is_empty() { letter.clone() } else { header };

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
                    queue!(
                        writer,
                        SetBackgroundColor(Color::AnsiValue(234)),
                        terminal::Clear(terminal::ClearType::All),
                        ResetColor,
                    )?;
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
        // Top border with the title
        let title = fit_text(&format!(" {} ", self.title), inner.saturating_sub(1));
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
        )?;
        row += 1;

        for i in 0..self.commands.len() {
            let text = format!(" {}", self.commands[i].1);
            self.line(w, left, row, inner, &text, i == self.highlight, TEXT)?;
            row += 1;
        }
        self.rule(w, left, row, inner, '\u{251c}', '\u{2524}')?;
        row += 1;

        // Search line
        let search_text = format!(" Search: {}", self.search);
        let on_search = self.highlight == self.search_row();
        self.line(w, left, row, inner, &search_text, on_search, TEXT)?;
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
        self.line(w, left, row, inner, &text, self.highlight == self.select_all_row(), TEXT)?;
        row += 1;

        // Values
        for j in 0..self.list_rows {
            let k = self.scroll + j;
            if k >= self.matches.len() {
                let text = if k == 0 { "   (no matching values)" } else { "" };
                self.line(w, left, row, inner, text, false, DIM)?;
            } else {
                let (value, count) = &self.values[self.matches[k]];
                let mark = if self.is_checked(k) { "[x]" } else { "[ ]" };
                let count = group_thousands(*count);
                let label_room = inner.saturating_sub(6 + count.len() + 2);
                let label = fit_text(&value_label(value), label_room);
                let pad = inner.saturating_sub(5 + text_width(&label) + count.len() + 1);
                let text = format!(" {} {}{}{} ", mark, label, " ".repeat(pad), count);
                let color = if value.is_empty() { DIM } else { TEXT };
                self.line(w, left, row, inner, &text, self.first_value_row() + k == self.highlight, color)?;
            }
            row += 1;
        }
        self.rule(w, left, row, inner, '\u{251c}', '\u{2524}')?;
        row += 1;

        // Footer: buttons, then a hint or message
        let hint = match self.message {
            Some(m) => m.to_string(),
            None => format!("{} of {} ticked \u{00b7} Space tick \u{00b7} Enter OK", ticked, self.matches.len()),
        };
        let footer = format!(" {}  {}  {}", OK_LABEL, CANCEL_LABEL, hint);
        let color = if self.message.is_some() { Color::Yellow } else { DIM };
        self.line(w, left, row, inner, &footer, false, color)?;
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

    fn line<W: Write>(&self, w: &mut W, left: u16, row: u16, inner: usize, text: &str, highlighted: bool, fg: Color) -> io::Result<()> {
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

    fn rule<W: Write>(&self, w: &mut W, left: u16, row: u16, inner: usize, l: char, r: char) -> io::Result<()> {
        queue!(
            w,
            cursor::MoveTo(left, row),
            SetBackgroundColor(BOX_BG),
            SetForegroundColor(BORDER),
            Print(format!("{}{}{}", l, "\u{2500}".repeat(inner), r)),
            ResetColor,
        )
    }
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

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(7), "7");
        assert_eq!(group_thousands(12345), "12,345");
        assert_eq!(group_thousands(1234567), "1,234,567");
    }
}
