//! SQL statement splitter. Splits a SQL buffer into one `Cell` per statement
//! on uncommented, unquoted semicolons. Used as the cell parser when the
//! editor language is SQL — analogous to `cell::parse_cells` for Python's
//! `##$$` delimiters.
//!
//! Handles Snowflake-flavored SQL quoting:
//! - `-- line comments` (terminated by newline)
//! - `/* block comments */` with nesting (Snowflake allows nested)
//! - `'single quoted strings'` with doubled-quote escape (`'can''t'`)
//! - `"double quoted identifiers"` with doubled-quote escape (`"id""ent"`)
//! - `$$dollar quoted$$` and `$tag$dollar quoted$tag$` (alphanumeric+`_` tag)
//!
//! Byte-indexed. UTF-8 is preserved because every state transition is on an
//! ASCII byte, and the only positions we record as cell boundaries (`cell_start`
//! and the byte right after a `;`) land at ASCII bytes.

use crate::cell::Cell;
use ropey::Rope;

enum State {
    Normal,
    LineComment,
    BlockComment(usize),
    SingleString,
    DoubleString,
    DollarString(String),
}

pub fn parse_sql_cells(buffer: &Rope) -> Vec<Cell> {
    let text = buffer.to_string();
    let bytes = text.as_bytes();
    let mut cells = Vec::new();
    let mut cell_start: usize = 0;
    let mut state = State::Normal;
    let mut i: usize = 0;

    while i < bytes.len() {
        let b = bytes[i];
        let next = bytes.get(i + 1).copied();

        match &state {
            State::Normal => match b {
                b';' => {
                    // End-of-statement: cell spans [cell_start, i+1) (semicolon included).
                    cells.push(Cell {
                        start: cell_start,
                        end: i + 1,
                    });
                    cell_start = i + 1;
                    i += 1;
                }
                b'-' if next == Some(b'-') => {
                    state = State::LineComment;
                    i += 2;
                }
                b'/' if next == Some(b'*') => {
                    state = State::BlockComment(1);
                    i += 2;
                }
                b'\'' => {
                    state = State::SingleString;
                    i += 1;
                }
                b'"' => {
                    state = State::DoubleString;
                    i += 1;
                }
                b'$' => {
                    if let Some(end_tag_idx) = find_dollar_tag_end(bytes, i + 1) {
                        // Tag bytes are i+1 .. end_tag_idx (exclusive), then a `$`.
                        let tag =
                            std::str::from_utf8(&bytes[i + 1..end_tag_idx])
                                .unwrap_or("")
                                .to_string();
                        state = State::DollarString(tag);
                        i = end_tag_idx + 1;
                    } else {
                        i += 1;
                    }
                }
                _ => i += 1,
            },
            State::LineComment => {
                if b == b'\n' {
                    state = State::Normal;
                }
                i += 1;
            }
            State::BlockComment(depth) => {
                if b == b'/' && next == Some(b'*') {
                    state = State::BlockComment(depth + 1);
                    i += 2;
                } else if b == b'*' && next == Some(b'/') {
                    state = if *depth == 1 {
                        State::Normal
                    } else {
                        State::BlockComment(depth - 1)
                    };
                    i += 2;
                } else {
                    i += 1;
                }
            }
            State::SingleString => {
                if b == b'\'' {
                    if next == Some(b'\'') {
                        i += 2; // escaped ''
                    } else {
                        state = State::Normal;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            State::DoubleString => {
                if b == b'"' {
                    if next == Some(b'"') {
                        i += 2; // escaped ""
                    } else {
                        state = State::Normal;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            State::DollarString(tag) => {
                // Look for `$tag$` closer at this position.
                let close = format!("${}$", tag);
                if bytes[i..].starts_with(close.as_bytes()) {
                    state = State::Normal;
                    i += close.len();
                } else {
                    i += 1;
                }
            }
        }
    }

    // Trailing content after the last `;` (or the whole buffer if no `;`).
    if cell_start < bytes.len() {
        let remaining = &text[cell_start..];
        if !remaining.trim().is_empty() {
            cells.push(Cell {
                start: cell_start,
                end: bytes.len(),
            });
        }
    }

    // Empty buffer / whitespace-only — keep editor invariants by returning
    // a single zero-length cell rather than nothing.
    if cells.is_empty() {
        cells.push(Cell {
            start: 0,
            end: bytes.len(),
        });
    }

    cells
}

/// Find the closing `$` of a dollar-quote opening tag starting at `start`.
/// Returns the byte index of the closing `$`, or `None` if the bytes between
/// `start` and the next `$` aren't a valid tag (only alphanumeric and `_`).
/// Empty tag (`$$`) is valid — `start` points at the second `$` in that case.
fn find_dollar_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'$' {
            return Some(i);
        }
        if !(b.is_ascii_alphanumeric() || b == b'_') {
            return None;
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(s: &str) -> Vec<String> {
        let rope = Rope::from_str(s);
        parse_sql_cells(&rope)
            .into_iter()
            .map(|c| s[c.start..c.end].to_string())
            .collect()
    }

    #[test]
    fn single_statement_no_semicolon() {
        assert_eq!(split("SELECT 1"), vec!["SELECT 1"]);
    }

    #[test]
    fn two_statements() {
        assert_eq!(split("SELECT 1;\nSELECT 2;"), vec!["SELECT 1;", "\nSELECT 2;"]);
    }

    #[test]
    fn semicolon_in_single_string() {
        assert_eq!(split("SELECT ';' AS a; SELECT 2;"), vec!["SELECT ';' AS a;", " SELECT 2;"]);
    }

    #[test]
    fn semicolon_in_line_comment() {
        assert_eq!(
            split("SELECT 1; -- next;\nSELECT 2;"),
            vec!["SELECT 1;", " -- next;\nSELECT 2;"]
        );
    }

    #[test]
    fn semicolon_in_block_comment() {
        assert_eq!(
            split("SELECT /* a; b */ 1; SELECT 2;"),
            vec!["SELECT /* a; b */ 1;", " SELECT 2;"]
        );
    }

    #[test]
    fn nested_block_comment() {
        assert_eq!(
            split("SELECT /* a /* nested; */ rest; */ 1;"),
            vec!["SELECT /* a /* nested; */ rest; */ 1;"]
        );
    }

    #[test]
    fn doubled_single_quote_escape() {
        assert_eq!(
            split("SELECT 'can''t; stop'; SELECT 2;"),
            vec!["SELECT 'can''t; stop';", " SELECT 2;"]
        );
    }

    #[test]
    fn dollar_quoted() {
        assert_eq!(
            split("SELECT $$a;b$$; SELECT $tag$x;$tag$;"),
            vec!["SELECT $$a;b$$;", " SELECT $tag$x;$tag$;"]
        );
    }

    #[test]
    fn whitespace_only() {
        assert_eq!(split("   \n  "), vec!["   \n  "]);
    }

    #[test]
    fn empty() {
        assert_eq!(split(""), vec![""]);
    }
}
