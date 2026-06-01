//! Delimiter-separated values (CSV/TSV) with SQL-style null support.
//!
//! Standard CSV libraries — including the `csv` crate sage uses elsewhere —
//! follow RFC 4180, where an unquoted empty field and a quoted empty field
//! (`""`) are both just the empty string. Several database tools (e.g.
//! PostgreSQL's `COPY ... CSV`) instead use that distinction to encode NULL: an
//! *unquoted* empty field is NULL, while `""` is a zero-length string. Sage
//! adopts the same convention so query-result NULLs stay visually distinct from
//! empty strings (NULLs render as `∅`). This module is the single place that
//! parses and serializes with that distinction.
//!
//! A field is represented as `Option<String>`: `None` is NULL, `Some(s)` is a
//! value (and `Some(String::new())` is an explicit empty string).

/// One parsed field. `None` = SQL NULL (an unquoted empty field); `Some` is a
/// value, where `Some("")` is an explicitly-quoted empty string.
pub type Field = Option<String>;

/// Parse `input` into delimiter-separated rows, distinguishing NULL (unquoted
/// empty) from the empty string (`""`).
///
/// Record/field splitting mirrors the `csv` crate's defaults — `""` escapes a
/// quote inside a quoted field, delimiters/newlines inside quotes are literal,
/// `\n`/`\r\n`/lone `\r` terminate records, and a single trailing newline does
/// not yield an extra empty record — so this can stand in for a plain CSV read
/// without changing how any well-formed file is laid out; it only adds the
/// null/empty distinction on top.
pub fn parse(input: &str, delim: u8) -> Vec<Vec<Field>> {
    let dc = delim as char;
    let mut rows: Vec<Vec<Field>> = Vec::new();
    let mut row: Vec<Field> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut quoted = false; // this field contained a quoted section

    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        // CRLF: the CR is swallowed and the LF terminates; a lone CR terminates.
        let terminate = match ch {
            '"' => {
                in_quotes = true;
                quoted = true;
                false
            }
            c if c == dc => {
                row.push(make_field(&field, quoted));
                field.clear();
                quoted = false;
                false
            }
            '\r' => chars.peek() != Some(&'\n'),
            '\n' => true,
            _ => {
                field.push(ch);
                false
            }
        };
        if terminate {
            // Skip completely empty lines, matching the csv crate (a line with
            // even a single delimiter or a quoted "" is a real record).
            if !(row.is_empty() && field.is_empty() && !quoted) {
                row.push(make_field(&field, quoted));
                rows.push(std::mem::take(&mut row));
            }
            field.clear();
            quoted = false;
        }
    }

    // Flush a trailing record that wasn't newline-terminated. The `quoted` and
    // `!row.is_empty()` checks capture a final `""` or trailing-delimiter field
    // (e.g. `a,`) that would otherwise be dropped.
    if !field.is_empty() || quoted || !row.is_empty() {
        row.push(make_field(&field, quoted));
        rows.push(row);
    }
    rows
}

fn make_field(content: &str, quoted: bool) -> Field {
    if !quoted && content.is_empty() {
        None
    } else {
        Some(content.to_string())
    }
}

/// Append one serialized field to `out`. NULL is written as an unquoted empty
/// field (nothing); a value is quoted only when necessary, with the empty
/// string always quoted (`""`) so it round-trips as a value rather than a NULL.
pub fn serialize_field(out: &mut String, field: Option<&str>, delim: u8) {
    let Some(s) = field else { return };
    let dc = delim as char;
    let needs_quote =
        s.is_empty() || s.contains(dc) || s.contains('"') || s.contains('\n') || s.contains('\r');
    if needs_quote {
        out.push('"');
        for ch in s.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
    } else {
        out.push_str(s);
    }
}

/// Serialize whole rows back to a `String`, newline-terminated. Hot paths
/// (spreadsheet save, the Snowflake spool) serialize field-by-field instead, so
/// this is currently used only for round-trip tests — kept as the natural
/// counterpart to [`parse`].
#[allow(dead_code)]
pub fn serialize(rows: &[Vec<Field>], delim: u8) -> String {
    let mut out = String::new();
    for row in rows {
        for (i, f) in row.iter().enumerate() {
            if i > 0 {
                out.push(delim as char);
            }
            serialize_field(&mut out, f.as_deref(), delim);
        }
        out.push('\n');
    }
    out
}

/// Copy the first `max_records` records from `reader` to `writer` verbatim,
/// preserving the exact bytes (and therefore the null/empty encoding and any
/// quoting). Record boundaries are newlines that fall outside quoted fields, so
/// a quoted value containing embedded newlines counts as a single record.
/// Streaming and byte-accurate — used to bound a huge result spool to its first
/// N rows without parsing or re-encoding it. Returns the number of records
/// copied (each ends in a newline; sage's spools are always newline-terminated).
pub fn copy_first_records<R: std::io::Read, W: std::io::Write>(
    mut reader: R,
    mut writer: W,
    max_records: usize,
) -> std::io::Result<usize> {
    if max_records == 0 {
        return Ok(0);
    }
    let mut in_quotes = false;
    let mut records = 0usize;
    let mut buf = [0u8; 16 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        for i in 0..n {
            match buf[i] {
                b'"' => in_quotes = !in_quotes,
                b'\n' if !in_quotes => {
                    records += 1;
                    if records >= max_records {
                        writer.write_all(&buf[..=i])?;
                        return Ok(records);
                    }
                }
                _ => {}
            }
        }
        writer.write_all(&buf[..n])?;
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Field contents only (NULL and "" both collapse to ""), to compare against
    // the csv crate's RFC 4180 view.
    fn contents(input: &str, delim: u8) -> Vec<Vec<String>> {
        parse(input, delim)
            .into_iter()
            .map(|row| row.into_iter().map(|f| f.unwrap_or_default()).collect())
            .collect()
    }

    fn csv_contents(input: &str, delim: u8) -> Vec<Vec<String>> {
        let mut rdr = csv::ReaderBuilder::new()
            .delimiter(delim)
            .has_headers(false)
            .flexible(true)
            .from_reader(input.as_bytes());
        rdr.records()
            .map(|r| r.unwrap().iter().map(|s| s.to_string()).collect())
            .collect()
    }

    #[test]
    fn distinguishes_null_from_empty_string() {
        assert_eq!(
            parse("a,,b", b','),
            vec![vec![Some("a".into()), None, Some("b".into())]]
        );
        assert_eq!(
            parse("a,\"\",b", b','),
            vec![vec![Some("a".into()), Some(String::new()), Some("b".into())]]
        );
        // Trailing unquoted-empty is a null; trailing quoted-empty is "".
        assert_eq!(parse("a,", b','), vec![vec![Some("a".into()), None]]);
        assert_eq!(
            parse("a,\"\"", b','),
            vec![vec![Some("a".into()), Some(String::new())]]
        );
    }

    #[test]
    fn matches_csv_crate_field_contents() {
        let cases = [
            "a,b,c\n1,2,3\n",
            "a,,c\n",
            "a,\"\",c\n",
            "\"x,y\",z\n",
            "\"line1\nline2\",b\n",
            "a,b\r\nc,d\r\n",
            "",
            "a",
            "a,",
            ",",
            "a,b\n\n",
            "a,b\n\nc,d\n",
            "x,y",
            "\"he said \"\"hi\"\"\",end\n",
        ];
        for input in cases {
            assert_eq!(
                contents(input, b','),
                csv_contents(input, b','),
                "content mismatch for {input:?}"
            );
        }
    }

    #[test]
    fn tsv_delimiter() {
        assert_eq!(
            parse("a\t\tb", b'\t'),
            vec![vec![Some("a".into()), None, Some("b".into())]]
        );
    }

    #[test]
    fn round_trips_null_and_empty() {
        let input = "a,,c\nd,\"\",f\n";
        let parsed = parse(input, b',');
        let serialized = serialize(&parsed, b',');
        // Re-parsing the serialized form preserves the null/empty distinction.
        assert_eq!(parse(&serialized, b','), parsed);
        assert_eq!(serialized, "a,,c\nd,\"\",f\n");
    }

    #[test]
    fn serialize_quotes_only_when_needed() {
        let mut out = String::new();
        serialize_field(&mut out, None, b','); // null -> nothing
        assert_eq!(out, "");
        out.clear();
        serialize_field(&mut out, Some(""), b','); // empty string -> ""
        assert_eq!(out, "\"\"");
        out.clear();
        serialize_field(&mut out, Some("a,b"), b','); // delimiter -> quoted
        assert_eq!(out, "\"a,b\"");
        out.clear();
        serialize_field(&mut out, Some("plain"), b',');
        assert_eq!(out, "plain");
    }

    #[test]
    fn copy_first_records_bounds_and_respects_quoted_newlines() {
        // Row 2's first field has an embedded newline; it must not count as a
        // record boundary, so 2 records = header + the multiline row.
        let input = "h1,h2\n\"line1\nline2\",x\nr3a,r3b\nr4a,r4b\n";
        let mut out = Vec::new();
        let copied = copy_first_records(input.as_bytes(), &mut out, 2).unwrap();
        assert_eq!(copied, 2);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "h1,h2\n\"line1\nline2\",x\n"
        );
    }

    #[test]
    fn copy_first_records_handles_fewer_than_requested() {
        let input = "a,b\nc,d\n";
        let mut out = Vec::new();
        let copied = copy_first_records(input.as_bytes(), &mut out, 100).unwrap();
        assert_eq!(copied, 2);
        assert_eq!(String::from_utf8(out).unwrap(), input);
    }
}
