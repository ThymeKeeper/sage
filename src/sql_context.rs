use ropey::Rope;

/// Detect if the cursor is inside a SQL string context
/// Returns true if we're inside a string that's an argument to a SQL function
pub fn is_in_sql_context(rope: &Rope, cursor_pos: usize) -> bool {
    // Common SQL function patterns to detect
    const SQL_PATTERNS: &[&str] = &[
        ".sql(",
        ".execute(",
        ".query(",
        ".read_sql(",
        ".read_sql_query(",
        ".read_sql_table(",
        "spark.sql(",
    ];

    // First, check if we're inside a string at all
    if !is_in_string(rope, cursor_pos) {
        return false;
    }

    // Look backwards from cursor to find the opening quote of the string
    let mut pos = cursor_pos;
    let mut in_string = false;
    let mut string_start = cursor_pos;
    let mut is_triple_quote = false;

    while pos > 0 {
        pos -= 1;
        let char_idx = rope.byte_to_char(pos);
        if let Some(ch) = rope.get_char(char_idx) {
            if ch == '"' || ch == '\'' {
                // Check if it's escaped
                let mut escape_count = 0;
                let mut check_pos = pos;
                while check_pos > 0 {
                    check_pos -= 1;
                    let check_idx = rope.byte_to_char(check_pos);
                    if let Some(check_ch) = rope.get_char(check_idx) {
                        if check_ch == '\\' {
                            escape_count += 1;
                        } else {
                            break;
                        }
                    }
                }

                // If even number of escapes, this quote is not escaped
                if escape_count % 2 == 0 {
                    // Check for triple quotes
                    if pos >= 2 {
                        let idx1 = rope.byte_to_char(pos.saturating_sub(1));
                        let idx2 = rope.byte_to_char(pos.saturating_sub(2));
                        if let (Some(ch1), Some(ch2)) = (rope.get_char(idx1), rope.get_char(idx2)) {
                            if ch1 == ch && ch2 == ch {
                                // Found triple quote
                                is_triple_quote = true;
                                in_string = !in_string;
                                if in_string {
                                    string_start = pos.saturating_sub(2);
                                    break;
                                }
                            }
                        }
                    }

                    // Regular single/double quote
                    if !is_triple_quote {
                        in_string = !in_string;
                        if in_string {
                            string_start = pos;
                            break;
                        }
                    }
                }
            }
        }
    }

    if !in_string {
        return false;
    }

    // Check if this is an f-string (f"..." or F"...")
    let mut check_start = string_start;
    if string_start > 0 {
        let char_before_quote_idx = rope.byte_to_char(string_start.saturating_sub(1));
        if let Some(ch_before) = rope.get_char(char_before_quote_idx) {
            if ch_before == 'f' || ch_before == 'F' {
                // This is an f-string, adjust search start to before the 'f'
                check_start = string_start.saturating_sub(1);
            }
        }
    }

    // Now look backwards from check_start to find if there's a SQL function call
    // We need to look for patterns like: .sql( or .execute( etc.
    // Increased from 200 to 1000 bytes to handle longer multiline strings
    let search_start = check_start.saturating_sub(1000);
    let search_text = rope.slice(search_start..check_start).to_string();

    // Check if any SQL pattern appears near the string start
    for pattern in SQL_PATTERNS {
        if search_text.ends_with(pattern) {
            return true;
        }

        // Also check with whitespace between pattern and quote
        if let Some(trimmed_pos) = search_text.trim_end().rfind(pattern) {
            let after_pattern = &search_text[trimmed_pos + pattern.len()..];
            if after_pattern.trim().is_empty() {
                return true;
            }
        }
    }

    false
}

/// Check if cursor is inside any string (helper function)
fn is_in_string(rope: &Rope, cursor_pos: usize) -> bool {
    let mut pos = 0;
    let mut in_double_quote = false;
    let mut in_single_quote = false;

    while pos < cursor_pos && pos < rope.len_bytes() {
        let char_idx = rope.byte_to_char(pos);
        if let Some(ch) = rope.get_char(char_idx) {
            // Check for escape sequences
            if ch == '\\' && pos + 1 < rope.len_bytes() {
                pos += ch.len_utf8();
                if let Ok(next_char_idx) = rope.try_byte_to_char(pos) {
                    if let Some(next_ch) = rope.get_char(next_char_idx) {
                        pos += next_ch.len_utf8();
                    }
                }
                continue;
            }

            if ch == '"' && !in_single_quote {
                in_double_quote = !in_double_quote;
            } else if ch == '\'' && !in_double_quote {
                in_single_quote = !in_single_quote;
            }

            pos += ch.len_utf8();
        } else {
            break;
        }
    }

    in_double_quote || in_single_quote
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sql_context_detection() {
        // Test basic SQL context
        let rope = Rope::from_str("db.sql(\"SELECT * FROM \")");
        assert!(is_in_sql_context(&rope, 20)); // Inside the SQL string

        // Test not in SQL context
        let rope = Rope::from_str("print(\"hello\")");
        assert!(!is_in_sql_context(&rope, 8)); // Inside a regular string

        // Test Spark SQL context
        let rope = Rope::from_str("spark.sql(\"SELECT \")");
        assert!(is_in_sql_context(&rope, 15));
    }
}
