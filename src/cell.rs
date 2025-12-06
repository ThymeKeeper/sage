use crate::kernel::ExecutionResult;
use ropey::Rope;

/// Represents a cell in the notebook
#[derive(Debug, Clone)]
pub struct Cell {
    /// Start byte position in the buffer
    pub start: usize,
    /// End byte position in the buffer (exclusive)
    pub end: usize,
    /// Type of cell
    pub cell_type: CellType,
    /// Execution output (if executed)
    pub output: Option<ExecutionResult>,
    /// Execution count
    pub execution_count: Option<usize>,
}

/// Type of cell
#[derive(Debug, Clone, PartialEq)]
pub enum CellType {
    /// Python code cell
    Code,
    /// Markdown/text cell
    Markdown,
}

/// Cell delimiter marker
pub const CELL_DELIMITER: &str = "# %%";

/// Parse buffer into cells
pub fn parse_cells(buffer: &Rope) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut current_start = 0;
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
            cell_type: CellType::Code,
            output: None,
            execution_count: None,
        });
        return cells;
    }

    // Create cells between delimiters
    for (i, &delimiter_pos) in delimiter_positions.iter().enumerate() {
        // Get the end position (either next delimiter or end of buffer)
        let end_pos = if i + 1 < delimiter_positions.len() {
            delimiter_positions[i + 1]
        } else {
            buffer.len_bytes()
        };

        // Determine cell type from delimiter line
        let delimiter_line_idx = buffer.byte_to_line(delimiter_pos);
        let line_start = buffer.line_to_byte(delimiter_line_idx);
        let line_end = if delimiter_line_idx + 1 < buffer.len_lines() {
            buffer.line_to_byte(delimiter_line_idx + 1)
        } else {
            buffer.len_bytes()
        };

        let line_text = buffer.slice(line_start..line_end).to_string();
        let cell_type = if line_text.to_lowercase().contains("markdown") {
            CellType::Markdown
        } else {
            CellType::Code
        };

        // Cell starts after the delimiter line
        let cell_start = if delimiter_line_idx + 1 < buffer.len_lines() {
            buffer.line_to_byte(delimiter_line_idx + 1)
        } else {
            buffer.len_bytes()
        };

        cells.push(Cell {
            start: delimiter_pos,
            end: end_pos,
            cell_type,
            output: None,
            execution_count: None,
        });
    }

    cells
}

/// Get the cell at a given byte position
pub fn get_cell_at_position(cells: &[Cell], position: usize) -> Option<usize> {
    cells
        .iter()
        .position(|cell| position >= cell.start && position < cell.end)
}

/// Get the content of a cell (excluding the delimiter line)
pub fn get_cell_content(buffer: &Rope, cell: &Cell) -> String {
    // Find the first non-delimiter line
    let start_line = buffer.byte_to_line(cell.start);
    let content_start = if start_line + 1 < buffer.len_lines() {
        buffer.line_to_byte(start_line + 1)
    } else {
        cell.end
    };

    if content_start >= cell.end {
        return String::new();
    }

    buffer.slice(content_start..cell.end).to_string()
}

/// Format output for display
pub fn format_output(result: &ExecutionResult) -> String {
    let mut output = String::new();

    for exec_output in &result.outputs {
        match exec_output {
            crate::kernel::ExecutionOutput::Stdout(text) => {
                output.push_str(text);
                output.push('\n');
            }
            crate::kernel::ExecutionOutput::Stderr(text) => {
                output.push_str("stderr: ");
                output.push_str(text);
                output.push('\n');
            }
            crate::kernel::ExecutionOutput::Result(text) => {
                output.push_str(text);
                output.push('\n');
            }
            crate::kernel::ExecutionOutput::Error {
                ename,
                evalue,
                traceback,
            } => {
                output.push_str(&format!("{}: {}\n", ename, evalue));
                for line in traceback {
                    if !line.is_empty() {
                        output.push_str(line);
                        output.push('\n');
                    }
                }
            }
            crate::kernel::ExecutionOutput::Display { data, mime_type } => {
                output.push_str(&format!("[{}] {}\n", mime_type, data));
            }
        }
    }

    output
}
