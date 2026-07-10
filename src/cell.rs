use crate::kernel::ExecutionResult;
use ropey::Rope;

/// Represents a cell in the notebook
#[derive(Debug, Clone)]
pub struct Cell {
    /// Start byte position in the buffer
    pub start: usize,
    /// End byte position in the buffer (exclusive)
    pub end: usize,
}

/// Cell delimiter marker for non-SQL (e.g. Python) buffers. Text after the
/// marker on the delimiter line becomes the cell's output title. It's a valid
/// `#` comment, so it has no effect on execution.
pub const CELL_DELIMITER: &str = "##--";

/// Parse buffer into cells
pub fn parse_cells(buffer: &Rope) -> Vec<Cell> {
    let mut cells = Vec::new();
    let text = buffer.to_string();

    // Find all cell delimiters
    let mut delimiter_positions = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        if line.trim_start().starts_with(CELL_DELIMITER) {
            // Calculate byte position of this line
            let byte_pos = buffer.line_to_byte(line_idx);
            delimiter_positions.push(byte_pos);
        }
    }

    // If no delimiters found, treat entire buffer as one cell
    if delimiter_positions.is_empty() {
        cells.push(Cell {
            start: 0,
            end: buffer.len_bytes(),
        });
        return cells;
    }

    // If there's content before the first delimiter, create a cell for it
    if delimiter_positions[0] > 0 {
        cells.push(Cell {
            start: 0,
            end: delimiter_positions[0],
        });
    }

    // Create cells between delimiters
    for (i, &delimiter_pos) in delimiter_positions.iter().enumerate() {
        // Get the end position (either next delimiter or end of buffer)
        let end_pos = if i + 1 < delimiter_positions.len() {
            delimiter_positions[i + 1]
        } else {
            buffer.len_bytes()
        };

        cells.push(Cell {
            start: delimiter_pos,
            end: end_pos,
        });
    }

    cells
}

/// Get the cell at a given byte position
pub fn get_cell_at_position(cells: &[Cell], position: usize) -> Option<usize> {
    cells
        .iter()
        .position(|cell| position >= cell.start && position <= cell.end)
}

/// Get the content of a cell (excluding the delimiter line)
pub fn get_cell_content(buffer: &Rope, cell: &Cell) -> String {
    let start_line = buffer.byte_to_line(cell.start);

    // Check if the first line is actually a delimiter
    let line_start = buffer.line_to_byte(start_line);
    let line_end = if start_line + 1 < buffer.len_lines() {
        buffer.line_to_byte(start_line + 1)
    } else {
        buffer.len_bytes()
    };
    let first_line = buffer.byte_slice(line_start..line_end).to_string();
    let is_delimiter_line = first_line.trim_start().starts_with(CELL_DELIMITER);

    // Only skip the first line if it's a delimiter
    let content_start = if is_delimiter_line && start_line + 1 < buffer.len_lines() {
        buffer.line_to_byte(start_line + 1)
    } else {
        cell.start
    };

    if content_start >= cell.end {
        return String::new();
    }

    buffer.byte_slice(content_start..cell.end).to_string()
}

/// Split an explicit selection into the statements to execute, as
/// `(title, code)` pairs where `title` comes from the cell's marker line (see
/// [`cell_title`]).
///
/// The selected text is treated as a standalone buffer: SQL is split on
/// semicolons (via the SQL splitter), every other language on `##--` cell
/// delimiters. Non-executable fragments (blank / comment-only) are dropped.
/// This is what makes a selection run *exactly* what was selected — e.g.
/// selecting `select 1;` plus the blank line after it yields just `select 1;`,
/// never the next statement, because nothing past the selection's end is parsed.
pub fn statements_in_text(text: &str, is_sql: bool) -> Vec<(Option<String>, String)> {
    let rope = Rope::from_str(text);
    let cells = if is_sql {
        crate::sql_split::parse_sql_cells(&rope)
    } else {
        parse_cells(&rope)
    };
    cells
        .iter()
        .filter_map(|cell| {
            let code = get_cell_content(&rope, cell);
            if !has_executable_content(&code, is_sql) {
                return None;
            }
            // Title is derived from the cell's full text — for non-SQL the
            // marker line is stripped out of `code` by get_cell_content.
            let full = rope.byte_slice(cell.start..cell.end).to_string();
            Some((cell_title(&full, is_sql), code))
        })
        .collect()
}

/// True if `code` is worth sending to the kernel. For SQL this means it
/// contains something other than whitespace, `;`, and comments (see
/// [`crate::sql_split::has_executable_sql`]); for other languages, any
/// non-whitespace text. Callers use it to skip blank/comment-only cells.
pub fn has_executable_content(code: &str, is_sql: bool) -> bool {
    if is_sql {
        crate::sql_split::has_executable_sql(code)
    } else {
        !code.trim().is_empty()
    }
}

/// Heuristic: does this Python code look like a standalone application/game
/// meant to run as its own program, rather than a snippet to evaluate in the
/// persistent kernel?
///
/// Such code takes over its own event loop (`tkinter.mainloop()`, a `pygame`
/// game loop, a Qt `app.exec()`), which never returns until the window closes.
/// Run inside the shared kernel it would block the single REPL thread for the
/// app's whole lifetime, freezing the notebook — so the host runs it as a
/// detached process with its own window instead.
///
/// Any one of these trips it:
/// - an `if __name__ == "__main__":` entry guard (the mark of a runnable file),
/// - an import of a blocking-UI / game framework, or
/// - a `.mainloop(` call (tkinter, which is stdlib and so not import-detected).
///
/// matplotlib / seaborn / PIL are deliberately *not* signals: those are display
/// side-effects the session lane renders into orphan viewer windows. Full-line
/// comments are ignored; we don't parse string literals, so a framework name
/// buried in a string is a rare, harmless false positive.
pub fn is_standalone_program(code: &str) -> bool {
    /// Frameworks whose whole purpose is a blocking, windowed run loop.
    const APP_FRAMEWORKS: &[&str] = &[
        "pygame", "pyglet", "arcade", "kivy", "turtle", "ursina", "panda3d",
        "PyQt5", "PyQt6", "PySide2", "PySide6",
    ];

    for raw in code.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("if __name__") && line.contains("__main__") {
            return true;
        }
        if (line.starts_with("import ") || line.starts_with("from "))
            && APP_FRAMEWORKS.iter().any(|fw| line.contains(fw))
        {
            return true;
        }
        if line.contains(".mainloop(") {
            return true;
        }
    }
    false
}

/// SQL title marker: a line comment beginning with `--##`. It's a valid SQL
/// line comment, so it has no effect on execution.
pub const TITLE_MARKER: &str = "--##";

/// Derive a display title for a cell from `cell_text` (the cell's full text,
/// including any delimiter line). The marker depends on the language:
/// - SQL: a leading `--##` comment line (the `TITLE_MARKER`).
/// - other (e.g. Python): the `##--` cell-delimiter line (`CELL_DELIMITER`).
///
/// The title is the first few words after the marker. Returns `None` when the
/// first non-empty line doesn't start with the marker, so callers fall back to
/// a numbered label like "Cell 3".
pub fn cell_title(cell_text: &str, is_sql: bool) -> Option<String> {
    const MAX_WORDS: usize = 6;
    let marker = if is_sql { TITLE_MARKER } else { CELL_DELIMITER };
    let first = cell_text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let title_text = first.strip_prefix(marker)?.trim();
    if title_text.is_empty() {
        return None;
    }
    let title = title_text
        .split_whitespace()
        .take(MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    Some(title)
}

/// Format output for display (Jupyter-style)
pub fn format_output(result: &ExecutionResult) -> String {
    let mut output = String::new();
    let _exec_count = result.execution_count.unwrap_or(0);

    for exec_output in &result.outputs {
        match exec_output {
            crate::kernel::ExecutionOutput::Stdout(text) => {
                // Stdout is displayed as-is (from print statements)
                if !text.is_empty() {
                    output.push_str(text);
                    if !text.ends_with('\n') {
                        output.push('\n');
                    }
                }
            }
            crate::kernel::ExecutionOutput::Stderr(text) => {
                // Stderr with clear prefix
                if !text.is_empty() {
                    output.push_str("stderr: ");
                    output.push_str(text);
                    if !text.ends_with('\n') {
                        output.push('\n');
                    }
                }
            }
            crate::kernel::ExecutionOutput::Result(text) => {
                // Display result without prefix
                if !text.is_empty() {
                    output.push_str(text);
                    if !text.ends_with('\n') {
                        output.push('\n');
                    }
                }
            }
            crate::kernel::ExecutionOutput::Error {
                ename,
                evalue,
                traceback,
            } => {
                // Error with formatted traceback
                output.push_str(&format!("\x1b[31m{}\x1b[0m: {}\n", ename, evalue));

                // Filter and format traceback to be more concise
                let mut skip_internal = false;
                for line in traceback {
                    let trimmed = line.trim();

                    // Skip empty lines
                    if trimmed.is_empty() {
                        continue;
                    }

                    // Skip internal REPL lines (SAGE_EXEC markers)
                    if trimmed.contains("SAGE_EXEC") || trimmed.contains("<string>") {
                        skip_internal = true;
                        continue;
                    }

                    // Skip the "during handling" lines that are noise
                    if trimmed.starts_with("During handling of") {
                        continue;
                    }

                    // Show file lines and the actual error lines
                    if trimmed.starts_with("File") || trimmed.starts_with("Traceback") || !skip_internal {
                        output.push_str(line);
                        output.push('\n');
                    }
                }
            }
            crate::kernel::ExecutionOutput::Display { data: _data, mime_type } => {
                // The chart/image is shown in its own OS window by the host; the
                // pane just gets a muted breadcrumb rather than the temp path.
                let label = match mime_type.as_str() {
                    "figure" => "chart",
                    "image" => "image",
                    other => other,
                };
                output.push_str(&format!("\x1b[90m▸ {} opened in a window\x1b[0m\n", label));
            }
        }
    }

    // Remove trailing newline for cleaner display
    let formatted = output.trim_end().to_string();

    // If no output and execution was successful, show a message
    if formatted.is_empty() && result.success {
        return "\x1b[90m(executed successfully)\x1b[0m".to_string();
    }

    formatted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_cell_does_not_drag_in_sibling_cells() {
        // Regression: running an app cell must run ONLY that cell, not the whole
        // buffer — otherwise a following seaborn/plot cell (separated by `##--`,
        // which is just a `#` comment to Python) would execute too.
        let buffer = "import tkinter as tk\nroot = tk.Tk()\nif __name__ == \"__main__\":\n    root.mainloop()\n\n##--\n\nimport seaborn as sns\nsns.scatterplot(x=[1], y=[1])\nplt.show()\n";
        let rope = Rope::from_str(buffer);
        let cells = parse_cells(&rope);
        assert_eq!(cells.len(), 2, "expected two cells split on ##--");

        // Cursor in the app cell → its content is the app alone, and standalone.
        let app_idx = get_cell_at_position(&cells, 5).unwrap();
        let app_code = get_cell_content(&rope, &cells[app_idx]);
        assert!(is_standalone_program(&app_code));
        assert!(!app_code.contains("seaborn"), "app cell leaked the plot cell: {app_code:?}");
        assert!(!app_code.contains("plt.show"));

        // Cursor in the plot cell → not standalone (runs in the kernel/chart lane).
        let plot_idx = get_cell_at_position(&cells, buffer.find("seaborn").unwrap()).unwrap();
        let plot_code = get_cell_content(&rope, &cells[plot_idx]);
        assert!(plot_code.contains("seaborn"));
        assert!(!is_standalone_program(&plot_code));
    }

    #[test]
    fn standalone_program_detection() {
        // Apps / games → standalone lane.
        assert!(is_standalone_program("import pygame\npygame.init()\n"));
        assert!(is_standalone_program("from kivy.app import App\n"));
        assert!(is_standalone_program(
            "import tkinter as tk\nroot = tk.Tk()\nroot.mainloop()\n"
        ));
        assert!(is_standalone_program(
            "def main():\n    pass\n\nif __name__ == \"__main__\":\n    main()\n"
        ));

        // Ordinary notebook analysis → stays in-session.
        assert!(!is_standalone_program(
            "import pandas as pd\ndf = pd.read_csv('x.csv')\ndf.head()\n"
        ));
        // Charts are display side-effects, NOT standalone programs.
        assert!(!is_standalone_program(
            "import matplotlib.pyplot as plt\nplt.plot([1, 2, 3])\nplt.show()\n"
        ));
        assert!(!is_standalone_program("import seaborn as sns\nsns.histplot(x)\n"));
        // A framework name only inside a comment must not trip it.
        assert!(!is_standalone_program("# import pygame would be an app\nx = 1\n"));
    }

    /// Just the code strings a selection would run (drops the titles).
    fn codes(text: &str, is_sql: bool) -> Vec<String> {
        statements_in_text(text, is_sql)
            .into_iter()
            .map(|(_, code)| code)
            .collect()
    }

    #[test]
    fn selection_of_statement_plus_trailing_blank_line_runs_only_that_statement() {
        // The reported bug: selecting "select 1;" and the empty line after it
        // must NOT pull in "select 2;".
        let buffer = "select 1;\n\nselect 2;\n";
        let sel_end = buffer.find("select 2").unwrap(); // selection stops before stmt 2
        let selected = &buffer[0..sel_end]; // "select 1;\n\n"
        assert_eq!(codes(selected, true), vec!["select 1;"]);
    }

    #[test]
    fn selection_spanning_two_statements_runs_both() {
        let buffer = "select 1;\n\nselect 2;";
        assert_eq!(codes(buffer, true), vec!["select 1;", "\n\nselect 2;"]);
    }

    #[test]
    fn selection_of_a_partial_statement_runs_that_partial_text() {
        // Selecting exactly what the user dragged over, even if incomplete.
        assert_eq!(codes("select 2", true), vec!["select 2"]);
    }

    #[test]
    fn whitespace_only_selection_runs_nothing() {
        assert!(statements_in_text("  \n\n ", true).is_empty());
    }

    #[test]
    fn comment_only_selection_runs_nothing() {
        assert!(statements_in_text("-- just a note", true).is_empty());
        assert!(statements_in_text("-- note\n/* block */\n;", true).is_empty());
    }

    #[test]
    fn trailing_comment_after_statement_is_dropped() {
        // The statement runs; the comment-only trailing fragment is skipped.
        assert_eq!(codes("select 1;\n-- trailing note", true), vec!["select 1;"]);
    }

    #[test]
    fn non_sql_selection_runs_as_a_single_statement() {
        // No `##--` delimiters → the whole selection is one statement.
        assert_eq!(codes("print(1)\nprint(2)", false), vec!["print(1)\nprint(2)"]);
    }

    #[test]
    fn non_sql_selection_titles_and_splits_on_the_delimiter() {
        let text = "##-- load data\nimport pandas\n\n##-- analyze\ndf.describe()\n";
        let stmts = statements_in_text(text, false);
        let titles: Vec<_> = stmts.iter().map(|(t, _)| t.clone()).collect();
        let bodies: Vec<_> = stmts.iter().map(|(_, c)| c.clone()).collect();
        assert_eq!(
            titles,
            vec![Some("load data".to_string()), Some("analyze".to_string())]
        );
        assert_eq!(bodies, vec!["import pandas\n\n", "df.describe()\n"]);
    }

    // --- SQL titles: `--##` comment marker ---

    #[test]
    fn sql_cell_title_from_title_marker() {
        assert_eq!(
            cell_title("--## active users\nselect 1;", true).as_deref(),
            Some("active users")
        );
    }

    #[test]
    fn sql_cell_title_marker_without_a_space() {
        assert_eq!(
            cell_title("--##active users\nselect 1;", true).as_deref(),
            Some("active users")
        );
    }

    #[test]
    fn sql_cell_title_skips_leading_blank_lines_and_indent() {
        // Cells after the first carry the blank line(s) that precede them.
        assert_eq!(
            cell_title("\n\n   --## monthly revenue\nselect 1;", true).as_deref(),
            Some("monthly revenue")
        );
    }

    #[test]
    fn sql_cell_title_caps_at_a_few_words() {
        assert_eq!(
            cell_title("--## one two three four five six seven eight", true).as_deref(),
            Some("one two three four five six")
        );
    }

    #[test]
    fn sql_cell_title_none_for_plain_comment() {
        // A regular `--` comment is not a title marker.
        assert_eq!(cell_title("-- active users\nselect 1;", true), None);
        assert_eq!(cell_title("---- section\nselect 1;", true), None);
    }

    #[test]
    fn sql_cell_title_none_without_leading_marker() {
        assert_eq!(cell_title("select 1; --## trailing", true), None);
        assert_eq!(cell_title("select 1;", true), None);
    }

    #[test]
    fn sql_cell_title_none_for_empty_marker() {
        assert_eq!(cell_title("--##", true), None);
        assert_eq!(cell_title("--##   \nselect 1;", true), None);
    }

    // --- Python titles: `##--` delimiter line ---

    #[test]
    fn python_cell_title_from_delimiter_line() {
        assert_eq!(
            cell_title("##-- load data\nimport pandas", false).as_deref(),
            Some("load data")
        );
        assert_eq!(
            cell_title("##--load data\nimport pandas", false).as_deref(),
            Some("load data")
        );
    }

    #[test]
    fn python_cell_title_none_without_delimiter() {
        assert_eq!(cell_title("import pandas", false), None);
        // The SQL marker is not a Python title.
        assert_eq!(cell_title("--## not python\nx = 1", false), None);
    }

    #[test]
    fn python_cell_title_none_for_bare_delimiter() {
        assert_eq!(cell_title("##--\nx = 1", false), None);
        assert_eq!(cell_title("##--   ", false), None);
    }
}
