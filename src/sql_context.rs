//! SQL written inside Python: whether the caret is in the SQL string of a
//! call such as `sf.sql("...")` or `db.sql("...")`, which dialect it is, and
//! the Snowflake SQL a cell sends through abp (`sf.sql`, `sf.submit`).
//!
//! Python is scanned from the top, so comments (`# don't`), string prefixes
//! (`f`, `r`, `rb`), triple quotes and escapes are read as Python reads them.

use crate::sql_words::SqlCompletion;

/// Which SQL the string holds, from the call it is passed to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SqlDialect {
    /// abp's `sf.sql(...)` or `sf.submit(...)`: Snowflake.
    Snowflake,
    /// `db.sql`, `.execute`, `.query`, `read_sql*` and the like: DuckDB, Spark
    /// or another database.
    Other,
}

/// The caret inside an SQL string in Python: the dialect, and the name being
/// typed there.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddedSql {
    pub dialect: SqlDialect,
    pub completion: SqlCompletion,
}

/// One string literal in Python source.
struct PyString {
    /// Where the literal starts, prefix included (`f` of `f"..."`).
    start: usize,
    /// Where its text starts and ends (end is None when the source ends inside it).
    text_start: usize,
    text_end: Option<usize>,
    is_f: bool,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Walk Python source, calling `on_string` for each string literal outside
/// comments. A literal the source ends inside comes last, with no end.
fn python_strings(code: &str, mut on_string: impl FnMut(&PyString)) {
    let b = code.as_bytes();
    let n = b.len();
    let mut i = 0;
    while i < n {
        match b[i] {
            b'#' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            q @ (b'"' | b'\'') => {
                // A prefix: up to two of r, b, u, f right before the quote,
                // not the tail of a longer name.
                let mut start = i;
                while start > 0 && i - start < 2 && b"rRbBuUfF".contains(&b[start - 1]) {
                    start -= 1;
                }
                if start > 0 && is_ident_byte(b[start - 1]) {
                    start = i; // e.g. `elif'...'` can't happen; a name ending in f isn't a prefix
                }
                let is_f = b[start..i].iter().any(|&c| c == b'f' || c == b'F');
                let triple = i + 2 < n && b[i + 1] == q && b[i + 2] == q;
                let text_start = if triple { i + 3 } else { i + 1 };
                let mut j = text_start;
                let text_end = loop {
                    if j >= n {
                        break None;
                    }
                    match b[j] {
                        b'\\' => j += 2,
                        c if c == q && (!triple || (j + 2 < n && b[j + 1] == q && b[j + 2] == q)) => {
                            break Some(j);
                        }
                        b'\n' if !triple => break Some(j), // an unclosed one-line string ends here
                        _ => j += 1,
                    }
                };
                on_string(&PyString { start, text_start, text_end, is_f });
                match text_end {
                    None => return,
                    Some(e) if b[e] == b'\n' => i = e,
                    Some(e) => i = e + if triple { 3 } else { 1 },
                }
            }
            _ => i += 1,
        }
    }
}

/// The dotted name of the call whose first argument starts at `start`:
/// `sf.sql` for `sf.sql(  "...`. None when the literal isn't right after `(`.
fn callee_before(code: &str, start: usize) -> Option<&str> {
    let before = code[..start].trim_end();
    let before = before.strip_suffix('(')?.trim_end();
    let name_start = before
        .bytes()
        .rev()
        .take_while(|&c| is_ident_byte(c) || c == b'.')
        .count();
    let name = &before[before.len() - name_start..];
    (!name.is_empty()).then_some(name)
}

/// The SQL dialect a call takes, or None when it isn't an SQL call.
fn sql_call(callee: &str) -> Option<SqlDialect> {
    let (object, method) = callee.rsplit_once('.')?;
    match method {
        "sql" | "submit" if object == "sf" => Some(SqlDialect::Snowflake),
        "sql" | "execute" | "query" | "read_sql" | "read_sql_query" | "read_sql_table" => {
            Some(SqlDialect::Other)
        }
        _ => None,
    }
}

/// An f-string's text with its `{...}` fields blanked, as SQL to read.
/// None when the text ends inside a field (that part is Python, not SQL).
fn without_fields(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if depth == 0 && chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if depth == 0 && chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => depth += 1,
            '}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push(' ');
                }
            }
            _ if depth > 0 => {}
            _ => out.push(c),
        }
    }
    (depth == 0).then_some(out)
}

/// The caret at the end of `before` (the source up to it), inside the SQL
/// string passed to an SQL call: its dialect and the name being typed. None
/// anywhere else, including inside an SQL string literal or comment within
/// the SQL, and inside an f-string's `{...}` field.
pub fn embedded_sql_at(before: &str) -> Option<EmbeddedSql> {
    let mut last: Option<(usize, usize, bool, bool)> = None;
    python_strings(before, |s| last = Some((s.start, s.text_start, s.is_f, s.text_end.is_none())));
    let (start, text_start, is_f, open) = last?;
    if !open {
        return None;
    }
    let dialect = sql_call(callee_before(before, start)?)?;
    let text = &before[text_start..];
    let sql = if is_f { without_fields(text)? } else { text.to_string() };
    let completion = crate::sql_words::completion_context(&sql)?;
    Some(EmbeddedSql { dialect, completion })
}

/// The Snowflake SQL a Python cell sends through abp: the text of each
/// string literal passed to `sf.sql(...)` or `sf.submit(...)`, with an
/// f-string's `{...}` fields blanked.
pub fn snowflake_sql_in(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    python_strings(code, |s| {
        let Some(end) = s.text_end else { return };
        let is_snowflake = callee_before(code, s.start)
            .and_then(sql_call)
            .map_or(false, |d| d == SqlDialect::Snowflake);
        if !is_snowflake {
            return;
        }
        let text = &code[s.text_start..end];
        let sql = if s.is_f { without_fields(text) } else { Some(text.to_string()) };
        if let Some(sql) = sql {
            out.push(sql);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(code: &str) -> Option<(SqlDialect, Option<String>, String)> {
        embedded_sql_at(code).map(|e| (e.dialect, e.completion.qualifier, e.completion.prefix))
    }

    #[test]
    fn the_call_decides_the_dialect() {
        assert_eq!(at("sf.sql(\"SELECT pa"), Some((SqlDialect::Snowflake, None, "pa".into())));
        assert_eq!(at("qid = sf.submit('SELECT sc.pa"), Some((SqlDialect::Snowflake, Some("sc".into()), "pa".into())));
        assert_eq!(at("db.sql(\"SELECT pa"), Some((SqlDialect::Other, None, "pa".into())));
        assert_eq!(at("spark.sql(\"SELECT pa"), Some((SqlDialect::Other, None, "pa".into())));
        assert_eq!(at("con.execute(\"\"\"\nSELECT pa"), Some((SqlDialect::Other, None, "pa".into())));
        assert_eq!(at("print(\"SELECT pa"), None);
        assert_eq!(at("x = \"SELECT pa"), None);
    }

    #[test]
    fn python_is_read_as_python() {
        // An apostrophe in a comment doesn't open a string.
        assert_eq!(at("# don't\nsf.sql(\"SELECT pa"), Some((SqlDialect::Snowflake, None, "pa".into())));
        // A closed string before the caret: not inside SQL.
        assert_eq!(at("sf.sql(\"SELECT 1\")\nx = pa"), None);
        // Triple quotes span lines and hold quotes of the other kind.
        assert_eq!(at("sf.sql(f\"\"\"\nSELECT \"A\" FROM {tbl} t\nWHERE t.co"),
                   Some((SqlDialect::Snowflake, Some("t".into()), "co".into())));
        // Inside an f-string field is Python.
        assert_eq!(at("sf.sql(f\"SELECT * FROM {tab"), None);
        // Inside an SQL literal or comment within the SQL.
        assert_eq!(at("sf.sql(\"SELECT * FROM t WHERE a = 'x"), None);
        assert_eq!(at("sf.sql(\"\"\"SELECT 1 -- pa"), None);
        // Multi-byte text before the call.
        assert_eq!(at("x = 'ééé'\nsf.sql(\"SELECT pa"), Some((SqlDialect::Snowflake, None, "pa".into())));
    }

    #[test]
    fn a_cells_snowflake_sql_is_found() {
        let code = "sf.sql(\"ALTER SESSION SET QUERY_TAG = 'x'\", show=False)\n\
                    db.sql(\"SELECT * FROM result\")\n\
                    # sf.sql(\"SELECT dead FROM comment\")\n\
                    qid = sf.submit(f\"\"\"SELECT a FROM {src} s\n  WHERE s.b = 1\"\"\")\n";
        assert_eq!(
            snowflake_sql_in(code),
            vec!["ALTER SESSION SET QUERY_TAG = 'x'".to_string(), "SELECT a FROM   s\n  WHERE s.b = 1".to_string()]
        );
    }
}
