//! Word-wrap layout for plain-text and Markdown modes.
//!
//! When wrap is enabled, a single buffer line is split into one or more *visual
//! segments*, each of which fits within the terminal width. Continuation
//! segments are rendered with a "hanging indent" that aligns them with the start
//! of the text on the first row — after any leading whitespace and any list or
//! blockquote marker — so wrapped bullet points and indented paragraphs stay
//! visually aligned.
//!
//! The buffer, cursor (a byte offset), and selection model are all untouched by
//! wrapping. Only the *display* mapping changes: rendering, vertical cursor
//! movement, viewport scrolling, and mouse hit-testing operate on visual rows
//! instead of buffer lines. All of that is derived on demand from these pure
//! functions plus the `Editor` helpers at the bottom of this file — nothing is
//! cached, so there is no wrap state to invalidate on edits.

use super::Editor;
use crate::syntax::Language;
use unicode_width::UnicodeWidthChar;

/// One visual row's worth of a logical line.
#[derive(Debug, Clone, Copy)]
pub struct WrapSegment {
    /// Byte offset within the line where this segment's text begins.
    pub start: usize,
    /// Byte offset within the line where this segment's text ends (exclusive),
    /// never including the trailing newline.
    pub end: usize,
    /// Hanging-indent width (display columns of leading padding) rendered before
    /// the text on this segment. 0 for the first segment of a line.
    pub indent: usize,
}

/// Display width of a string in terminal columns.
fn str_width(s: &str) -> usize {
    s.chars().map(|c| c.width().unwrap_or(1)).sum()
}

/// Value of a single Roman-numeral letter (0 if the byte is not one).
fn roman_value(b: u8) -> i64 {
    match b {
        b'I' => 1,
        b'V' => 5,
        b'X' => 10,
        b'L' => 50,
        b'C' => 100,
        b'D' => 500,
        b'M' => 1000,
        _ => 0,
    }
}

/// Canonical Roman-numeral spelling of a positive integer.
fn int_to_roman(mut n: u32) -> String {
    const TABLE: [(u32, &str); 13] = [
        (1000, "M"), (900, "CM"), (500, "D"), (400, "CD"),
        (100, "C"), (90, "XC"), (50, "L"), (40, "XL"),
        (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I"),
    ];
    let mut out = String::new();
    for (val, sym) in TABLE {
        while n >= val {
            out.push_str(sym);
            n -= val;
        }
    }
    out
}

/// Whether `s` is a syntactically valid Roman numeral (case-insensitive). Uses a
/// round-trip through the canonical spelling, so non-standard forms ("IIII",
/// "VX") and English words that merely reuse Roman letters ("did", "mid") are
/// rejected — only genuine numerals like "iv" or "XI" qualify.
fn is_roman_numeral(s: &str) -> bool {
    if s.is_empty() || s.len() > 15 {
        return false;
    }
    let upper = s.to_ascii_uppercase();
    if !upper.bytes().all(|b| roman_value(b) != 0) {
        return false;
    }
    let mut total = 0i64;
    let mut prev = 0i64;
    for &b in upper.as_bytes().iter().rev() {
        let v = roman_value(b);
        if v < prev {
            total -= v;
        } else {
            total += v;
        }
        prev = v;
    }
    total > 0 && int_to_roman(total as u32) == upper
}

/// Compute the hanging-indent width (display columns) for continuation rows of
/// `line`. This is the leading whitespace plus the width of any list/blockquote
/// marker, so wrapped text lines up under the first word after the marker.
///
/// Recognized markers: leading spaces, one or more `>` blockquote markers, and a
/// single unordered (`-`/`*`/`+`) or ordered list marker — each requiring a
/// trailing space to count. Ordered enumerators may be Arabic numerals (`12`), a
/// single letter (`a`), or a Roman numeral (`iv`, `XI`), each followed by `.`,
/// `:`, or `)`.
pub fn hanging_indent(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut i = 0usize; // byte index (all markers handled here are ASCII)
    let mut col = 0usize; // display column

    // 1. Leading whitespace.
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
        col += 1;
    }

    // 2. Blockquote markers (may nest: "> > ").
    while i < bytes.len() && bytes[i] == b'>' {
        i += 1;
        col += 1;
        while i < bytes.len() && bytes[i] == b' ' {
            i += 1;
            col += 1;
        }
    }

    // 3. An optional single list marker, which must be followed by a space.
    if i < bytes.len() {
        let c = bytes[i];
        if (c == b'-' || c == b'*' || c == b'+') && i + 1 < bytes.len() && bytes[i + 1] == b' ' {
            // Unordered bullet.
            i += 1;
            col += 1;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
                col += 1;
            }
        } else if c.is_ascii_digit() || c.is_ascii_alphabetic() {
            // Ordered/enumerated marker: <enumerator><term><space>, where the
            // enumerator is a run of digits (1, 2, ...), a single letter (a, b, ...),
            // or a Roman numeral (i, iv, XI, ...), and the terminator is '.', ':' or
            // ')'. The required trailing space keeps abbreviations like "e.g." and
            // clock times like "12:30" from matching, and only single letters or
            // valid Roman numerals qualify so prose such as "Note:" stays unindented.
            let start = i;
            let mut j = i;
            let digit_run = c.is_ascii_digit();
            if digit_run {
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
            } else {
                while j < bytes.len() && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
            }
            let enumerator = &line[start..j];
            let valid_enum = digit_run || (j - start == 1) || is_roman_numeral(enumerator);
            if valid_enum
                && j < bytes.len()
                && matches!(bytes[j], b'.' | b':' | b')')
                && j + 1 < bytes.len()
                && bytes[j + 1] == b' '
            {
                col += (j - start) + 1; // enumerator + the terminator
                i = j + 1;
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                    col += 1;
                }
            }
        }
    }

    let _ = i;
    col
}

/// Split a line's content (without trailing newline) into visual segments that
/// each fit within `width` columns. Greedy word wrap: break at the last space
/// that fits, or hard-break inside a word that is longer than the available
/// width. Always returns at least one segment (empty for an empty line).
pub fn segments(line: &str, width: usize) -> Vec<WrapSegment> {
    let width = width.max(1);
    if line.is_empty() {
        return vec![WrapSegment { start: 0, end: 0, indent: 0 }];
    }

    // Continuation rows are inset by the hanging indent, capped so they always
    // keep at least half the width for text (guards against pathological lines
    // that are almost entirely indentation).
    let indent = hanging_indent(line).min(width / 2);
    let cont_avail = width.saturating_sub(indent).max(1);

    let mut segs: Vec<WrapSegment> = Vec::new();
    let mut seg_start = 0usize;
    let mut first = true;
    let mut col = 0usize; // display width used so far in the current segment's text
    let mut last_break: Option<usize> = None; // byte index just after a space in this segment
    let mut cur_avail = width; // first segment has the full width

    for (i, c) in line.char_indices() {
        let cw = c.width().unwrap_or(1);
        // Break before `c` if it would overflow — but only once at least one
        // character sits in the current segment (guarantees forward progress).
        if col + cw > cur_avail && i > seg_start {
            let brk = match last_break {
                Some(b) if b > seg_start => b, // soft break at last space
                _ => i,                        // hard break inside a long word
            };
            segs.push(WrapSegment {
                start: seg_start,
                end: brk,
                indent: if first { 0 } else { indent },
            });
            seg_start = brk;
            first = false;
            cur_avail = cont_avail;
            col = str_width(&line[seg_start..i]); // width of the carried-over fragment
            last_break = None;
        }
        col += cw;
        if c == ' ' {
            last_break = Some(i + c.len_utf8());
        }
    }

    segs.push(WrapSegment {
        start: seg_start,
        end: line.len(),
        indent: if first { 0 } else { indent },
    });
    segs
}

/// Locate a byte offset within a line: returns `(segment index, screen column)`.
/// A cursor sitting exactly on an interior wrap boundary resolves to the *start*
/// of the following segment (column == that segment's indent).
pub fn locate(segs: &[WrapSegment], line: &str, byte_in_line: usize) -> (usize, usize) {
    let n = segs.len();
    let (idx, seg) = segs
        .iter()
        .enumerate()
        .find(|(_, s)| byte_in_line >= s.start && byte_in_line < s.end)
        .map(|(k, s)| (k, *s))
        .unwrap_or((n - 1, segs[n - 1]));
    let end = byte_in_line.min(seg.end);
    let col = seg.indent + str_width(&line[seg.start..end]);
    (idx, col)
}

/// Map a target screen column on a given segment back to a byte offset within
/// the line. For interior segments the result never lands on the exclusive
/// boundary (which visually belongs to the next row); it clamps to the last
/// character of the row instead, so vertical movement and clicks stay put.
pub fn column_to_byte(segs: &[WrapSegment], line: &str, seg_idx: usize, screen_col: usize) -> usize {
    let seg = segs[seg_idx];
    if screen_col <= seg.indent {
        return seg.start;
    }
    let target = screen_col - seg.indent;
    let mut col = 0usize;
    for (i, c) in line[seg.start..seg.end].char_indices() {
        let cw = c.width().unwrap_or(1);
        if col + cw > target {
            let without = target - col;
            let with = (col + cw) - target;
            return if with < without {
                seg.start + i + c.len_utf8()
            } else {
                seg.start + i
            };
        }
        col += cw;
        if col == target {
            return seg.start + i + c.len_utf8();
        }
    }
    // Target is beyond the segment's text.
    if seg_idx + 1 < segs.len() {
        // Interior row: stay on it by landing on the last character start.
        line[seg.start..seg.end]
            .char_indices()
            .next_back()
            .map(|(i, _)| seg.start + i)
            .unwrap_or(seg.start)
    } else {
        seg.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line must be partitioned into contiguous, gap-free segments that
    /// cover it exactly, each fitting within `width` (bar a single overlong char).
    fn assert_invariants(line: &str, width: usize) {
        let segs = segments(line, width);
        assert!(!segs.is_empty(), "no segments for {:?}", line);
        assert_eq!(segs[0].start, 0, "first seg start for {:?}", line);
        assert_eq!(segs[0].indent, 0, "first seg indent for {:?}", line);
        assert_eq!(segs.last().unwrap().end, line.len(), "coverage for {:?}", line);
        for w in segs.windows(2) {
            assert_eq!(w[0].end, w[1].start, "contiguity for {:?}", line);
        }
        // Concatenating the segment texts reproduces the line.
        let mut rebuilt = String::new();
        for s in &segs {
            rebuilt.push_str(&line[s.start..s.end]);
        }
        assert_eq!(rebuilt, line, "rebuild for {:?}", line);
        for (k, s) in segs.iter().enumerate() {
            if !line.is_empty() {
                assert!(s.start < s.end, "empty seg {} for non-empty {:?}", k, line);
            }
            let rendered = s.indent + str_width(&line[s.start..s.end]);
            let single_char = line[s.start..s.end].chars().count() <= 1;
            assert!(
                rendered <= width || single_char,
                "seg {} rendered {} > width {} for {:?}",
                k, rendered, width, line
            );
        }
    }

    #[test]
    fn empty_line_has_one_empty_segment() {
        let segs = segments("", 10);
        assert_eq!(segs.len(), 1);
        assert_eq!((segs[0].start, segs[0].end, segs[0].indent), (0, 0, 0));
    }

    #[test]
    fn hanging_indent_markers() {
        assert_eq!(hanging_indent("no marker here"), 0);
        assert_eq!(hanging_indent("    indented text"), 4);
        assert_eq!(hanging_indent("- bullet"), 2);
        assert_eq!(hanging_indent("* bullet"), 2);
        assert_eq!(hanging_indent("+ bullet"), 2);
        assert_eq!(hanging_indent("  - nested bullet"), 4);
        assert_eq!(hanging_indent("12. ordered"), 4);
        assert_eq!(hanging_indent("3) ordered"), 3);
        assert_eq!(hanging_indent("> quote"), 2);
        assert_eq!(hanging_indent("> > nested"), 4);
        assert_eq!(hanging_indent("> - quoted bullet"), 4);
        // A hyphen with no trailing space is not a list marker.
        assert_eq!(hanging_indent("-nolist"), 0);
    }

    #[test]
    fn hanging_indent_enumerated_markers() {
        // Numeric with each terminator.
        assert_eq!(hanging_indent("1. item"), 3);
        assert_eq!(hanging_indent("1) item"), 3);
        assert_eq!(hanging_indent("1: item"), 3);
        // Single-letter enumerators.
        assert_eq!(hanging_indent("a. item"), 3);
        assert_eq!(hanging_indent("a) item"), 3);
        assert_eq!(hanging_indent("a: item"), 3);
        assert_eq!(hanging_indent("A. Section"), 3);
        // Roman numerals (upper and lower), each terminator.
        assert_eq!(hanging_indent("XI. item"), 4);
        assert_eq!(hanging_indent("XI) item"), 4);
        assert_eq!(hanging_indent("XI: item"), 4);
        assert_eq!(hanging_indent("iv. item"), 4);
        assert_eq!(hanging_indent("ix: item"), 4);
        assert_eq!(hanging_indent("iii. item"), 5);
        // Combined with leading indent / blockquote.
        assert_eq!(hanging_indent("  b. nested"), 5);
        assert_eq!(hanging_indent("> 2. quoted ordered"), 5);

        // Non-markers: prose and look-alikes must stay unindented.
        assert_eq!(hanging_indent("Note: some text"), 0); // multi-letter, not Roman
        assert_eq!(hanging_indent("e.g. example"), 0); // no space after first '.'
        assert_eq!(hanging_indent("i.e. example"), 0); // no space after first '.'
        assert_eq!(hanging_indent("I am tall"), 0); // no terminator
        assert_eq!(hanging_indent("A dog barks"), 0); // no terminator
        assert_eq!(hanging_indent("12:30 is lunch"), 0); // no space after ':'
        assert_eq!(hanging_indent("Introduction: text"), 0); // not single/Roman
    }

    #[test]
    fn roman_numeral_validation() {
        assert!(is_roman_numeral("I"));
        assert!(is_roman_numeral("iv"));
        assert!(is_roman_numeral("XI"));
        assert!(is_roman_numeral("mcmxciv"));
        assert!(!is_roman_numeral("IIII")); // non-canonical
        assert!(!is_roman_numeral("VX")); // non-canonical
        assert!(!is_roman_numeral("did")); // English word, not a numeral
        assert!(!is_roman_numeral("note")); // contains non-Roman letters
        assert!(!is_roman_numeral(""));
    }

    #[test]
    fn wraps_at_word_boundary() {
        let segs = segments("hello world", 6);
        assert_eq!(segs.len(), 2);
        assert_eq!(&"hello world"[segs[0].start..segs[0].end], "hello ");
        assert_eq!(&"hello world"[segs[1].start..segs[1].end], "world");
    }

    #[test]
    fn bullet_continuation_is_indented() {
        let line = "- one two three four five";
        let segs = segments(line, 12);
        // Every continuation row is inset to align under the bullet's text.
        for s in &segs[1..] {
            assert_eq!(s.indent, 2);
        }
        assert_eq!(segs[0].indent, 0);
        assert_invariants(line, 12);
    }

    #[test]
    fn hard_breaks_overlong_word() {
        let line = "abcdefghijklmnop"; // no spaces, longer than width
        let segs = segments(line, 5);
        assert!(segs.len() >= 4);
        assert_invariants(line, 5);
    }

    #[test]
    fn handles_wide_characters() {
        let line = "日本語ですよ"; // each char is 2 columns wide
        assert_invariants(line, 5);
        let segs = segments(line, 5);
        for s in &segs {
            assert!(str_width(&line[s.start..s.end]) <= 5);
        }
    }

    #[test]
    fn cursor_at_wrap_boundary_maps_to_next_row() {
        let line = "hello world";
        let segs = segments(line, 6); // ["hello ", "world"]
        // Byte 6 is the boundary (seg0.end == seg1.start): resolves to row 1, col 0.
        let (seg_idx, col) = locate(&segs, line, 6);
        assert_eq!((seg_idx, col), (1, 0));
        // Byte 5 (the space) stays at the end of row 0.
        let (seg_idx0, col0) = locate(&segs, line, 5);
        assert_eq!((seg_idx0, col0), (0, 5));
    }

    #[test]
    fn column_to_byte_clamps_interior_rows() {
        let line = "aaaa bbbb cccc";
        let segs = segments(line, 6); // ["aaaa ", "bbbb ", "cccc"]
        assert_eq!(segs.len(), 3);
        // A column past the end of an interior row lands on that row's last char,
        // never on the exclusive boundary (which would jump to the next row).
        let byte = column_to_byte(&segs, line, 1, 99);
        let (seg_idx, _) = locate(&segs, line, byte);
        assert_eq!(seg_idx, 1, "clamped position must stay on the interior row");
        // The last row may sit at the true end of the line.
        let last = segs.len() - 1;
        let byte_last = column_to_byte(&segs, line, last, 99);
        assert_eq!(byte_last, line.len());
    }

    #[test]
    fn locate_and_column_to_byte_round_trip_first_columns() {
        let line = "the quick brown fox jumps";
        let width = 10;
        let segs = segments(line, width);
        // For each char boundary, locating then mapping the column back yields a
        // position on the same (or, at an interior boundary, the following) row.
        let mut b = 0;
        for ch in line.chars() {
            let (seg_idx, col) = locate(&segs, line, b);
            let back = column_to_byte(&segs, line, seg_idx, col);
            assert_eq!(back, b, "round trip at byte {} of {:?}", b, line);
            b += ch.len_utf8();
        }
    }

    #[test]
    fn various_lines_satisfy_invariants() {
        let cases = [
            ("", 10),
            ("short", 80),
            ("a b c d e f g h i j k l m n o p", 8),
            ("    deeply indented paragraph that keeps going and going", 20),
            ("1. an ordered list item with enough text to wrap several times over", 15),
            ("> a blockquote that is long enough to require wrapping onto more rows", 18),
            ("mixed 日本 text with わ wide chars sprinkled in between the words", 12),
            ("supercalifragilisticexpialidocious", 7),
        ];
        for (line, width) in cases {
            assert_invariants(line, width);
        }
    }
}

/// A position in the visual-row space. `vline` uses the same "+2 virtual lines"
/// convention as the non-wrap renderer: vline 0 and 1 are the leading `~` rows,
/// vline `n+2` is buffer line `n`. `seg` is the segment index within that line.
type VPos = (usize, usize);

impl Editor {
    /// Whether the current language supports wrapping (plain text / Markdown).
    pub fn is_wrappable_language(&self) -> bool {
        matches!(*self.syntax.get_language(), Language::PlainText | Language::Markdown)
    }

    /// Whether wrapping is both enabled and applicable to the current language.
    pub fn is_wrap_active(&self) -> bool {
        self.word_wrap && self.is_wrappable_language()
    }

    /// The user's wrap preference, independent of the current language.
    pub fn word_wrap_enabled(&self) -> bool {
        self.word_wrap
    }

    pub fn set_word_wrap(&mut self, on: bool) {
        self.word_wrap = on;
    }

    /// Segment index of the buffer line shown at the top of the viewport.
    pub fn viewport_top_seg(&self) -> usize {
        self.viewport_top_seg
    }

    /// Reset wrap-specific view state (call when toggling wrap or changing view).
    pub fn reset_wrap_view(&mut self) {
        self.viewport_top_seg = 0;
        self.preferred_column = None;
        if self.is_wrap_active() {
            self.viewport_offset.1 = 0; // no horizontal scrolling while wrapped
        }
    }

    /// Current terminal width (columns) used for wrapping; matches the renderer.
    pub(super) fn wrap_width(&self) -> usize {
        crossterm::terminal::size()
            .map(|(w, _)| w as usize)
            .unwrap_or(80)
            .max(1)
    }

    /// Segments for a buffer line at the given width (content only, no newline).
    pub fn line_segments(&self, line_idx: usize, width: usize) -> Vec<WrapSegment> {
        let line = self.buffer.line(line_idx);
        let content = line.strip_suffix('\n').unwrap_or(&line);
        segments(content, width)
    }

    /// Number of visual rows a buffer line occupies.
    fn segcount(&self, file_row: usize, width: usize) -> usize {
        self.line_segments(file_row, width).len()
    }

    /// Advance one visual row.
    pub(super) fn vpos_advance(&self, pos: VPos, width: usize) -> VPos {
        let (vline, seg) = pos;
        if vline < 2 {
            return (vline + 1, 0);
        }
        let file = vline - 2;
        if file >= self.buffer.len_lines() {
            return (vline + 1, 0);
        }
        let count = self.segcount(file, width);
        if seg + 1 < count {
            (vline, seg + 1)
        } else {
            (vline + 1, 0)
        }
    }

    /// Clamp a stored viewport-top position so its segment index is valid for
    /// the current width and buffer. Segment counts change on edits and resizes,
    /// so the stored top can go stale between the update that set it and the next
    /// render; clamping here keeps rendering, cursor placement, and hit-testing
    /// consistent without needing to eagerly fix the stored value.
    fn clamp_top(&self, top: VPos, width: usize) -> VPos {
        let (vline, seg) = top;
        if vline < 2 {
            return (vline, 0);
        }
        let file = vline - 2;
        if file >= self.buffer.len_lines() {
            return (vline, 0);
        }
        let count = self.segcount(file, width).max(1);
        (vline, seg.min(count - 1))
    }

    /// Retreat one visual row, or `None` at the very top of the document.
    pub(super) fn vpos_retreat(&self, pos: VPos, width: usize) -> Option<VPos> {
        let (vline, seg) = pos;
        if seg > 0 {
            return Some((vline, seg - 1));
        }
        if vline == 0 {
            return None;
        }
        let pv = vline - 1;
        if pv < 2 {
            return Some((pv, 0));
        }
        let file = pv - 2;
        if file >= self.buffer.len_lines() {
            return Some((pv, 0));
        }
        let count = self.segcount(file, width).max(1);
        Some((pv, count - 1))
    }

    /// The cursor's visual position `(vline, seg)`.
    fn wrap_cursor_vpos(&self, width: usize) -> VPos {
        let line = self.buffer.byte_to_line(self.cursor);
        let line_start = self.buffer.line_to_byte(line);
        let s = self.buffer.line(line);
        let content = s.strip_suffix('\n').unwrap_or(&s);
        let segs = segments(content, width);
        let (seg_idx, _) = locate(&segs, content, (self.cursor - line_start).min(content.len()));
        (line + 2, seg_idx)
    }

    /// Move the cursor one visual row up (`down = false`) or down, preserving the
    /// preferred screen column. Callers manage selection anchoring.
    pub(super) fn visual_move_vertical(&mut self, down: bool) {
        let width = self.wrap_width();
        let line = self.buffer.byte_to_line(self.cursor);
        let line_start = self.buffer.line_to_byte(line);
        let s = self.buffer.line(line);
        let content = s.strip_suffix('\n').unwrap_or(&s).to_string();
        let segs = segments(&content, width);
        let byte_in_line = (self.cursor - line_start).min(content.len());
        let (seg_idx, cur_col) = locate(&segs, &content, byte_in_line);

        if self.preferred_column.is_none() {
            self.preferred_column = Some(cur_col);
        }
        let target_col = self.preferred_column.unwrap();

        if !down {
            if seg_idx > 0 {
                let byte = column_to_byte(&segs, &content, seg_idx - 1, target_col);
                self.cursor = line_start + byte;
            } else if line > 0 {
                let prev = line - 1;
                let ps = self.buffer.line(prev);
                let pcontent = ps.strip_suffix('\n').unwrap_or(&ps).to_string();
                let psegs = segments(&pcontent, width);
                let pstart = self.buffer.line_to_byte(prev);
                let byte = column_to_byte(&psegs, &pcontent, psegs.len() - 1, target_col);
                self.cursor = pstart + byte;
            } else {
                self.cursor = 0;
                self.preferred_column = Some(0);
            }
        } else {
            if seg_idx + 1 < segs.len() {
                let byte = column_to_byte(&segs, &content, seg_idx + 1, target_col);
                self.cursor = line_start + byte;
            } else if line + 1 < self.buffer.len_lines() {
                let next = line + 1;
                let ns = self.buffer.line(next);
                let ncontent = ns.strip_suffix('\n').unwrap_or(&ns).to_string();
                let nsegs = segments(&ncontent, width);
                let nstart = self.buffer.line_to_byte(next);
                let byte = column_to_byte(&nsegs, &ncontent, 0, target_col);
                self.cursor = nstart + byte;
            } else {
                self.cursor = self.buffer.len_bytes();
            }
        }
    }

    /// Wrap-aware viewport follow: keep the cursor within `scrolloff` visual rows
    /// of the top/bottom edges. Only walks a bounded (≈viewport-height) window.
    pub(super) fn update_viewport_wrap(&mut self, viewport_height: usize, width: usize) {
        let vh = viewport_height.max(1);
        let scrolloff = 3.min(vh.saturating_sub(1) / 2);
        let cursor = self.wrap_cursor_vpos(width);
        let top: VPos = self.clamp_top((self.viewport_offset.0, self.viewport_top_seg), width);
        self.viewport_top_seg = top.1;

        // Find the cursor's row offset relative to the current top, searching a
        // bounded window downward then upward. `None` means it's far off-screen.
        let mut found: Option<isize> = None;
        {
            let mut p = top;
            for r in 0..vh {
                if p == cursor {
                    found = Some(r as isize);
                    break;
                }
                p = self.vpos_advance(p, width);
            }
        }
        if found.is_none() {
            let mut p = top;
            for r in 1..vh {
                match self.vpos_retreat(p, width) {
                    Some(q) => {
                        p = q;
                        if p == cursor {
                            found = Some(-(r as isize));
                            break;
                        }
                    }
                    None => break,
                }
            }
        }

        let bottom_margin = vh as isize - 1 - scrolloff as isize;
        let needs_scroll = match found {
            Some(r) => r < scrolloff as isize || r > bottom_margin,
            None => true,
        };
        if !needs_scroll {
            return;
        }

        // Re-derive the top by retreating from the cursor by `k` rows so it lands
        // on the appropriate margin (or centered for a far jump).
        let k = match found {
            Some(r) if r > bottom_margin => vh.saturating_sub(1 + scrolloff),
            Some(_) => scrolloff,
            None => vh / 2,
        };
        let mut p = cursor;
        for _ in 0..k {
            match self.vpos_retreat(p, width) {
                Some(q) => p = q,
                None => break,
            }
        }
        self.viewport_offset.0 = p.0;
        self.viewport_top_seg = p.1;
    }

    /// Scroll the viewport by `lines` visual rows (mouse wheel), clamped so the
    /// top never moves past the last buffer line.
    pub(super) fn scroll_wrap_vertical(&mut self, lines: i32, width: usize) {
        let len = self.buffer.len_lines();
        let mut pos: VPos = self.clamp_top((self.viewport_offset.0, self.viewport_top_seg), width);
        if lines > 0 {
            for _ in 0..lines {
                let next = self.vpos_advance(pos, width);
                if next.0 >= 2 && (next.0 - 2) >= len {
                    break; // don't scroll the last line off the top
                }
                pos = next;
            }
        } else {
            for _ in 0..(-lines) {
                match self.vpos_retreat(pos, width) {
                    Some(q) => pos = q,
                    None => break,
                }
            }
        }
        self.viewport_offset.0 = pos.0;
        self.viewport_top_seg = pos.1;
    }

    /// The `(vline, seg)` visual position for each of `count` screen rows,
    /// starting at `top`. Used by the renderer to walk the visible rows.
    pub fn visual_rows(&self, top: (usize, usize), count: usize, width: usize) -> Vec<(usize, usize)> {
        let mut rows = Vec::with_capacity(count);
        let mut p = self.clamp_top(top, width);
        for _ in 0..count {
            rows.push(p);
            p = self.vpos_advance(p, width);
        }
        rows
    }

    /// Screen position `(row, col)` of the cursor within a `content_height`-tall
    /// content area, or `None` if it's outside the viewport.
    pub fn wrap_screen_position(
        &self,
        content_height: usize,
        width: usize,
    ) -> Option<(usize, usize)> {
        let cursor = self.wrap_cursor_vpos(width);
        let mut p: VPos = self.clamp_top((self.viewport_offset.0, self.viewport_top_seg), width);
        for r in 0..content_height {
            if p == cursor {
                let (vline, _) = cursor;
                let col = if vline < 2 {
                    0
                } else {
                    let file = vline - 2;
                    let line_start = self.buffer.line_to_byte(file);
                    let s = self.buffer.line(file);
                    let content = s.strip_suffix('\n').unwrap_or(&s);
                    let segs = segments(content, width);
                    let (_, c) =
                        locate(&segs, content, (self.cursor - line_start).min(content.len()));
                    c
                };
                return Some((r, col));
            }
            p = self.vpos_advance(p, width);
        }
        None
    }

    /// Map screen coordinates to a buffer byte offset in wrap mode.
    pub(super) fn wrap_screen_to_buffer(
        &self,
        screen_col: usize,
        screen_row: usize,
        width: usize,
    ) -> Option<usize> {
        let mut p: VPos = self.clamp_top((self.viewport_offset.0, self.viewport_top_seg), width);
        for _ in 0..screen_row {
            p = self.vpos_advance(p, width);
        }
        let (vline, seg) = p;
        if vline < 2 {
            return Some(0);
        }
        let file = vline - 2;
        if file >= self.buffer.len_lines() {
            return Some(self.buffer.len_bytes());
        }
        let line_start = self.buffer.line_to_byte(file);
        let s = self.buffer.line(file);
        let content = s.strip_suffix('\n').unwrap_or(&s);
        let segs = segments(content, width);
        if seg >= segs.len() {
            return Some(line_start);
        }
        let byte = column_to_byte(&segs, content, seg, screen_col);
        Some(line_start + byte)
    }
}
