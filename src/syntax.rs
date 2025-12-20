/// Represents the syntactic state at a point in the text
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntaxState {
    Normal,
    StringDouble,       // Inside a double-quoted string "..."
    StringSingle,       // Inside a single-quoted string '...'
    StringTriple,       // Inside a triple-quoted string (Python) - """
    StringTripleSingle, // Inside a triple-quoted string (Python) - '''
    LineComment,        // Inside a single-line comment // or -- or #
    BlockComment,       // Inside a multi-line/block comment /* */
    Keyword,            // Language keywords (if, for, while, etc.)
    Type,               // Type names (int, str, bool, etc.)
    Function,           // Function/method names
    Number,             // Numeric literals
    Operator,           // Operators (+, -, *, etc.)
    Punctuation,        // Punctuation (brackets, parens, etc.)
    MacroOrDecorator,   // Rust macros or Python decorators
    SqlKeyword,         // SQL keywords within strings (SELECT, FROM, etc.)
    SqlFunction,        // SQL functions within strings (COUNT, SUM, etc.)
    SqlNumber,          // SQL numeric literals within strings
    SqlText,            // SQL string content (non-keyword text, spaces, punctuation)
}

/// Language type for syntax highlighting
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    PlainText,
    Python,
    Sql,
    Rust,
    R,
    Yaml,
    Markdown,
    Json,
    Shell,
    Toml,
}

/// Represents a highlighted span within a line
#[derive(Debug, Clone)]
pub struct HighlightSpan {
    pub start: usize,      // Byte offset within the line
    pub end: usize,        // Byte offset within the line
    pub state: SyntaxState,
}

/// Tracks the syntax state for a single line
#[derive(Debug, Clone)]
pub struct LineState {
    /// The state we're in when we start processing this line (from previous line)
    pub entry_state: SyntaxState,
    
    /// The state we're in when we finish processing this line (for next line)
    pub exit_state: SyntaxState,
    
    /// All the highlight spans in this line
    pub spans: Vec<HighlightSpan>,
    
    /// Hash of the line content for change detection
    pub content_hash: u64,
}

impl LineState {
    fn new() -> Self {
        Self {
            entry_state: SyntaxState::Normal,
            exit_state: SyntaxState::Normal,
            spans: Vec::new(),
            content_hash: 0,
        }
    }
}

/// Manages syntax highlighting state for the entire buffer
pub struct SyntaxHighlighter {
    /// State for each line in the buffer
    line_states: Vec<LineState>,

    /// Lines that need to be re-highlighted (indices)
    dirty_lines: Vec<usize>,

    /// Current viewport range for large files
    viewport_start: usize,
    viewport_end: usize,

    /// Whether we're in viewport-only mode (for large files)
    viewport_mode: bool,

    /// Buffer size around viewport (lines before/after to process)
    viewport_buffer: usize,

    /// Current language for syntax highlighting
    language: Language,
}

impl Language {
    /// Detect language from file extension
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "py" | "pyw" => Language::Python,
            "sql" | "mysql" | "psql" => Language::Sql,
            "rs" => Language::Rust,
            "r" | "rdata" | "rds" => Language::R,
            "yaml" | "yml" => Language::Yaml,
            "md" | "markdown" => Language::Markdown,
            "json" | "jsonl" | "jsonc" => Language::Json,
            "sh" | "bash" | "zsh" => Language::Shell,
            "toml" => Language::Toml,
            _ => Language::PlainText,
        }
    }
}

impl SyntaxHighlighter {
    pub fn new() -> Self {
        Self {
            line_states: Vec::new(),
            dirty_lines: Vec::new(),
            viewport_start: 0,
            viewport_end: 0,
            viewport_mode: false,
            viewport_buffer: 500,
            language: Language::Python,
        }
    }

    /// Set the language for syntax highlighting
    pub fn set_language(&mut self, language: Language) {
        if self.language != language {
            self.language = language;
            // Mark all lines as dirty when language changes
            for i in 0..self.line_states.len() {
                self.mark_dirty(i);
            }
        }
    }

    /// Set language from file path
    pub fn set_language_from_path(&mut self, path: &str) {
        if let Some(ext) = path.rsplit('.').next() {
            let language = Language::from_extension(ext);
            self.set_language(language);
        }
    }

    /// Set viewport for large file mode
    pub fn set_viewport(&mut self, start: usize, end: usize, total_lines: usize) {
        // Check if we should be in viewport mode
        if total_lines > 50_000 && !self.viewport_mode {
            // Switch to viewport mode
            self.viewport_mode = true;
            self.line_states.clear(); // Clear full buffer
            self.dirty_lines.clear();
        } else if total_lines <= 50_000 && self.viewport_mode {
            // Switch back to full mode
            self.viewport_mode = false;
            self.init_all_lines(total_lines);
            return;
        }
        
        if !self.viewport_mode {
            return; // Not in viewport mode, nothing to do
        }
        
        // Calculate the range with buffer
        let buffer_start = start.saturating_sub(self.viewport_buffer);
        let buffer_end = (end + self.viewport_buffer).min(total_lines);
        
        // Check if viewport has moved significantly (more than 100 lines)
        let viewport_changed = 
            buffer_start < self.viewport_start.saturating_sub(100) ||
            buffer_end > self.viewport_end.saturating_add(100);
        
        if viewport_changed {
            self.viewport_start = buffer_start;
            self.viewport_end = buffer_end;
            
            // Mark new lines in range as dirty if they're not already processed
            for line in buffer_start..=buffer_end {
                if line >= self.line_states.len() {
                    // Extend the vector if needed
                    while self.line_states.len() <= line {
                        self.line_states.push(LineState::new());
                    }
                    self.mark_dirty(line);
                } else if self.line_states[line].content_hash == 0 {
                    // Line exists but hasn't been processed yet
                    self.mark_dirty(line);
                }
            }
        }
    }
    
    /// Check if we're in viewport mode
    pub fn is_viewport_mode(&self) -> bool {
        self.viewport_mode
    }
    
    /// Mark a line as needing re-highlighting
    pub fn mark_dirty(&mut self, line_index: usize) {
        if !self.dirty_lines.contains(&line_index) {
            self.dirty_lines.push(line_index);
        }
    }
    
    /// Check if there are any dirty lines to process
    pub fn has_dirty_lines(&self) -> bool {
        !self.dirty_lines.is_empty()
    }
    
    /// Mark a range of lines as dirty
    pub fn mark_range_dirty(&mut self, start: usize, end: usize) {
        for line in start..=end {
            self.mark_dirty(line);
        }
    }

    /// Check if a word is a keyword for the current language
    fn is_keyword(&self, word: &str) -> bool {
        match self.language {
            Language::Python => matches!(word,
                "False" | "None" | "True" | "and" | "as" | "assert" | "async" | "await" |
                "break" | "class" | "continue" | "def" | "del" | "elif" | "else" | "except" |
                "finally" | "for" | "from" | "global" | "if" | "import" | "in" | "is" |
                "lambda" | "nonlocal" | "not" | "or" | "pass" | "raise" | "return" |
                "try" | "while" | "with" | "yield" | "match" | "case"
            ),
            Language::Sql => matches!(word.to_uppercase().as_str(),
                "SELECT" | "FROM" | "WHERE" | "INSERT" | "UPDATE" | "DELETE" | "CREATE" |
                "DROP" | "ALTER" | "TABLE" | "INDEX" | "VIEW" | "JOIN" | "LEFT" | "RIGHT" |
                "INNER" | "OUTER" | "ON" | "AS" | "ORDER" | "BY" | "GROUP" | "HAVING" |
                "UNION" | "AND" | "OR" | "NOT" | "IN" | "EXISTS" | "BETWEEN" | "LIKE" |
                "IS" | "NULL" | "PRIMARY" | "KEY" | "FOREIGN" | "REFERENCES" | "CONSTRAINT" |
                "DISTINCT" | "ALL" | "ASC" | "DESC" | "LIMIT" | "OFFSET" | "CASE" | "WHEN" |
                "THEN" | "ELSE" | "END" | "BEGIN" | "COMMIT" | "ROLLBACK" | "TRANSACTION" |
                "INTO" | "VALUES" | "DEFAULT" | "SET" | "OVER" | "WITH" | "PARTITION" |
                "ROWS" | "RANGE" | "UNBOUNDED" | "PRECEDING" | "FOLLOWING" | "CURRENT" |
                "ROW" | "WINDOW" | "RECURSIVE" | "RETURNING" | "CROSS" | "NATURAL" |
                "USING" | "FULL" | "GRANT" | "REVOKE" | "TO" | "CASCADE" | "RESTRICT" |
                "CHECK" | "UNIQUE" | "AUTO_INCREMENT" | "COLLATE" | "IF" | "IFNULL" |
                "NULLIF" | "COALESCE" | "CAST" | "CONVERT" | "EXTRACT" | "SUBSTRING" |
                "TRIM" | "UPPER" | "LOWER" | "LENGTH" | "CONCAT" | "REPLACE" | "ROUND" |
                "FLOOR" | "CEIL" | "ABS" | "MOD" | "POWER" | "SQRT" | "NOW" | "CURRENT_DATE" |
                "CURRENT_TIME" | "CURRENT_TIMESTAMP" | "DATEADD" | "DATEDIFF" | "DATEPART" |
                "YEAR" | "MONTH" | "DAY" | "HOUR" | "MINUTE" | "SECOND" | "COUNT" | "SUM" |
                "AVG" | "MIN" | "MAX" | "FIRST" | "LAST" | "RANK" | "DENSE_RANK" | "ROW_NUMBER" |
                "LEAD" | "LAG" | "NTILE" | "CUME_DIST" | "PERCENT_RANK" | "FIRST_VALUE" |
                "LAST_VALUE" | "NTH_VALUE" | "ANY" | "SOME" | "INTERSECT" | "EXCEPT" | "MINUS" |
                "FOR" | "PROCEDURE" | "FUNCTION" | "TRIGGER" | "DECLARE" | "CURSOR" | "OPEN" |
                "FETCH" | "CLOSE" | "DEALLOCATE" | "EXEC" | "EXECUTE" | "RETURNS" | "RETURN" |
                "WHILE" | "LOOP" | "REPEAT" | "UNTIL" | "CONTINUE" | "BREAK" | "GOTO" | "LABEL" |
                "TRY" | "CATCH" | "THROW" | "RAISERROR" | "PRINT" | "TRUNCATE" | "MERGE" |
                "MATERIALIZED" | "TEMPORARY" | "TEMP" | "VOLATILE" | "IMMUTABLE" | "STABLE"
            ),
            Language::Rust => matches!(word,
                "as" | "break" | "const" | "continue" | "crate" | "else" | "enum" | "extern" |
                "false" | "fn" | "for" | "if" | "impl" | "in" | "let" | "loop" | "match" |
                "mod" | "move" | "mut" | "pub" | "ref" | "return" | "self" | "Self" | "static" |
                "struct" | "super" | "trait" | "true" | "type" | "unsafe" | "use" | "where" |
                "while" | "async" | "await" | "dyn"
            ),
            Language::R => matches!(word,
                "if" | "else" | "repeat" | "while" | "function" | "for" | "in" | "next" |
                "break" | "TRUE" | "FALSE" | "NULL" | "Inf" | "NaN" | "NA" | "NA_integer_" |
                "NA_real_" | "NA_complex_" | "NA_character_"
            ),
            Language::Yaml => matches!(word,
                "true" | "false" | "yes" | "no" | "null" | "True" | "False" | "YES" | "NO" | "Null"
            ),
            Language::Json => matches!(word,
                "true" | "false" | "null"
            ),
            Language::Toml => matches!(word,
                "true" | "false"
            ),
            Language::Shell => matches!(word,
                "if" | "then" | "else" | "elif" | "fi" | "case" | "esac" | "for" | "select" |
                "while" | "until" | "do" | "done" | "in" | "function" | "time" | "coproc" |
                "break" | "continue" | "return" | "exit" | "export" | "readonly" | "local" |
                "declare" | "typeset" | "unset" | "shift" | "source" | "eval" | "exec" |
                "trap" | "wait" | "jobs" | "bg" | "fg" | "kill" | "disown" | "suspend" |
                "alias" | "unalias" | "set" | "shopt" | "test" | "true" | "false"
            ),
            Language::Markdown => false, // Markdown doesn't have traditional keywords
            Language::PlainText => false,
        }
    }

    /// Check if a word is a type for the current language
    fn is_type(&self, word: &str) -> bool {
        match self.language {
            Language::Python => matches!(word,
                "int" | "float" | "str" | "bool" | "list" | "dict" | "set" | "tuple" |
                "bytes" | "bytearray" | "complex" | "frozenset" | "object" | "type" |
                "range" | "slice" | "memoryview" | "property" | "classmethod" | "staticmethod" |
                "super" | "enumerate" | "zip" | "map" | "filter" | "reversed" | "sorted" |
                "len" | "abs" | "all" | "any" | "min" | "max" | "sum" | "round" | "pow" |
                "iter" | "next" | "open" | "print" | "input" | "isinstance" | "issubclass" |
                "hasattr" | "getattr" | "setattr" | "delattr" | "callable" | "dir" | "vars" |
                "help" | "id" | "hash" | "hex" | "oct" | "bin" | "ord" | "chr" | "repr" | "ascii"
            ),
            Language::Sql => matches!(word.to_uppercase().as_str(),
                "INT" | "INTEGER" | "VARCHAR" | "CHAR" | "TEXT" | "BOOLEAN" | "BOOL" |
                "FLOAT" | "DOUBLE" | "DECIMAL" | "NUMERIC" | "REAL" | "DATE" | "TIME" |
                "TIMESTAMP" | "DATETIME" | "INTERVAL" | "BLOB" | "CLOB" | "JSON" | "JSONB" |
                "UUID" | "SERIAL" | "BIGSERIAL" | "BIGINT" | "SMALLINT" | "TINYINT" | "MEDIUMINT" |
                "LONGTEXT" | "MEDIUMTEXT" | "TINYTEXT" | "BINARY" | "VARBINARY" | "BIT" | "ENUM" |
                "ARRAY" | "MONEY" | "XML" | "GEOMETRY" | "POINT" | "LINESTRING" | "POLYGON"
            ),
            Language::Rust => matches!(word,
                "i8" | "i16" | "i32" | "i64" | "i128" | "isize" |
                "u8" | "u16" | "u32" | "u64" | "u128" | "usize" |
                "f32" | "f64" | "bool" | "char" | "str" | "String" |
                "Vec" | "Option" | "Result" | "Box" | "Rc" | "Arc" |
                "HashMap" | "HashSet" | "BTreeMap" | "BTreeSet"
            ),
            Language::R => matches!(word,
                "numeric" | "integer" | "logical" | "character" | "complex" | "raw" |
                "vector" | "matrix" | "array" | "list" | "data.frame" | "factor"
            ),
            Language::Shell => matches!(word,
                "echo" | "printf" | "read" | "cd" | "pwd" | "ls" | "cp" | "mv" | "rm" |
                "mkdir" | "rmdir" | "cat" | "grep" | "sed" | "awk" | "find" | "sort" |
                "uniq" | "cut" | "tr" | "head" | "tail" | "wc" | "diff" | "chmod" | "chown" |
                "tar" | "gzip" | "gunzip" | "zip" | "unzip" | "curl" | "wget" | "ssh" | "scp"
            ),
            Language::Json => false, // JSON doesn't have type keywords
            _ => false,
        }
    }

    /// Check if a character can be part of an identifier
    fn is_ident_char(&self, ch: char) -> bool {
        ch.is_alphanumeric() || ch == '_' || (self.language == Language::Python && ch == '@')
    }

    /// Check if a character can start an identifier
    fn is_ident_start(&self, ch: char) -> bool {
        ch.is_alphabetic() || ch == '_' || (self.language == Language::Python && ch == '@')
    }

    /// Check if a SQL word is a function (not a structural keyword)
    fn is_sql_function(&self, word: &str) -> bool {
        matches!(word.to_uppercase().as_str(),
            // Aggregate functions
            "COUNT" | "SUM" | "AVG" | "MIN" | "MAX" | "FIRST" | "LAST" |
            // String functions (REPLACE removed - it's a keyword in CREATE OR REPLACE context)
            "UPPER" | "LOWER" | "LENGTH" | "CONCAT" | "SUBSTRING" | "TRIM" |
            "LTRIM" | "RTRIM" | "LPAD" | "RPAD" |
            // Math functions
            "ROUND" | "FLOOR" | "CEIL" | "ABS" | "MOD" | "POWER" | "SQRT" | "EXP" | "LOG" |
            // Date/time functions
            "NOW" | "CURRENT_DATE" | "CURRENT_TIME" | "CURRENT_TIMESTAMP" |
            "DATEADD" | "DATEDIFF" | "DATEPART" | "YEAR" | "MONTH" | "DAY" |
            "HOUR" | "MINUTE" | "SECOND" | "EXTRACT" |
            // Conditional functions
            "IFNULL" | "NULLIF" | "COALESCE" | "CASE" |
            // Conversion functions
            "CAST" | "CONVERT" |
            // Window functions
            "RANK" | "DENSE_RANK" | "ROW_NUMBER" | "LEAD" | "LAG" | "NTILE" |
            "CUME_DIST" | "PERCENT_RANK" | "FIRST_VALUE" | "LAST_VALUE" | "NTH_VALUE"
        )
    }

    /// Check if a SQL word is a keyword (for highlighting within strings)
    fn is_sql_keyword(&self, word: &str) -> bool {
        matches!(word.to_uppercase().as_str(),
            "SELECT" | "FROM" | "WHERE" | "INSERT" | "UPDATE" | "DELETE" | "CREATE" |
            "DROP" | "ALTER" | "REPLACE" | "TABLE" | "INDEX" | "VIEW" | "JOIN" | "LEFT" | "RIGHT" |
            "INNER" | "OUTER" | "ON" | "AS" | "ORDER" | "BY" | "GROUP" | "HAVING" |
            "UNION" | "AND" | "OR" | "NOT" | "IN" | "EXISTS" | "BETWEEN" | "LIKE" |
            "IS" | "NULL" | "PRIMARY" | "KEY" | "FOREIGN" | "REFERENCES" | "CONSTRAINT" |
            "DISTINCT" | "ALL" | "ASC" | "DESC" | "LIMIT" | "OFFSET" | "WHEN" |
            "THEN" | "ELSE" | "END" | "BEGIN" | "COMMIT" | "ROLLBACK" | "TRANSACTION" |
            "INTO" | "VALUES" | "DEFAULT" | "SET" | "OVER" | "WITH" | "PARTITION" |
            "ROWS" | "RANGE" | "UNBOUNDED" | "PRECEDING" | "FOLLOWING" | "CURRENT" |
            "ROW" | "WINDOW" | "RECURSIVE" | "RETURNING" | "CROSS" | "NATURAL" |
            "USING" | "FULL" | "GRANT" | "REVOKE" | "TO" | "CASCADE" | "RESTRICT" |
            "CHECK" | "UNIQUE" | "AUTO_INCREMENT" | "COLLATE" | "IF" |
            "FOR" | "PROCEDURE" | "FUNCTION" | "TRIGGER" | "DECLARE" | "CURSOR" | "OPEN" |
            "FETCH" | "CLOSE" | "DEALLOCATE" | "EXEC" | "EXECUTE" | "RETURNS" | "RETURN" |
            "WHILE" | "LOOP" | "REPEAT" | "UNTIL" | "CONTINUE" | "BREAK" | "GOTO" | "LABEL" |
            "TRY" | "CATCH" | "THROW" | "RAISERROR" | "PRINT" | "TRUNCATE" | "MERGE" |
            "MATERIALIZED" | "TEMPORARY" | "TEMP" | "VOLATILE" | "IMMUTABLE" | "STABLE" |
            "ANY" | "SOME" | "INTERSECT" | "EXCEPT" | "MINUS"
        )
    }

    /// Tokenize SQL content within a string
    /// Returns spans for SQL keywords, functions, and numbers
    fn tokenize_sql_content(&self, content: &str, string_start: usize) -> Vec<HighlightSpan> {
        let mut spans = Vec::new();
        let mut char_indices = content.char_indices().peekable();

        while let Some((byte_pos, ch)) = char_indices.next() {
            // Skip whitespace
            if ch.is_whitespace() {
                continue;
            }

            // Check for numbers
            if ch.is_numeric() {
                let start = byte_pos;
                let mut end = byte_pos + ch.len_utf8();

                // Consume all numeric characters and dots
                while let Some(&(next_pos, next_ch)) = char_indices.peek() {
                    if next_ch.is_numeric() || next_ch == '.' {
                        end = next_pos + next_ch.len_utf8();
                        char_indices.next();
                    } else {
                        break;
                    }
                }

                spans.push(HighlightSpan {
                    start: string_start + start,
                    end: string_start + end,
                    state: SyntaxState::SqlNumber,
                });
                continue;
            }

            // Check for identifiers/keywords/functions
            if ch.is_alphabetic() || ch == '_' {
                let start = byte_pos;
                let mut end = byte_pos + ch.len_utf8();

                // Consume all alphanumeric characters and underscores
                while let Some(&(next_pos, next_ch)) = char_indices.peek() {
                    if next_ch.is_alphanumeric() || next_ch == '_' {
                        end = next_pos + next_ch.len_utf8();
                        char_indices.next();
                    } else {
                        break;
                    }
                }

                let word = &content[start..end];

                // Check if it's a SQL keyword or function
                if self.is_sql_function(word) {
                    spans.push(HighlightSpan {
                        start: string_start + start,
                        end: string_start + end,
                        state: SyntaxState::SqlFunction,
                    });
                } else if self.is_sql_keyword(word) {
                    spans.push(HighlightSpan {
                        start: string_start + start,
                        end: string_start + end,
                        state: SyntaxState::SqlKeyword,
                    });
                }
            }

            // Skip other characters (operators, punctuation, etc.) - iterator advances automatically
        }

        spans
    }

    /// Check if a line contains SQL context patterns
    fn line_has_sql_context(&self, line_content: &str) -> bool {
        const SQL_PATTERNS: &[&str] = &[
            ".sql(",
            ".execute(",
            ".query(",
            ".read_sql(",
            ".read_sql_query(",
            ".read_sql_table(",
            "spark.sql(",
        ];

        for pattern in SQL_PATTERNS {
            if line_content.contains(pattern) {
                return true;
            }
        }
        false
    }

    /// Check if we're in an SQL context, looking back at recent lines for multiline strings
    fn check_sql_context<F>(&self, line_index: usize, line_content: &str, entry_state: SyntaxState, get_line: &F) -> bool
    where
        F: Fn(usize) -> Option<String>
    {
        // First check current line
        if self.line_has_sql_context(line_content) {
            return true;
        }

        // If we're in a string continuation (entry_state is a string state),
        // look back at recent lines to see if the string started with SQL context
        if matches!(entry_state, SyntaxState::StringTriple | SyntaxState::StringTripleSingle |
                                 SyntaxState::StringDouble | SyntaxState::StringSingle) {
            // Look back up to 100 lines to find SQL context pattern (increased for longer multiline strings)
            let start = line_index.saturating_sub(100);
            for i in (start..line_index).rev() {
                if let Some(prev_line) = get_line(i) {
                    if self.line_has_sql_context(&prev_line) {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Process a single line and update its state
    pub fn process_line<F>(&mut self, line_index: usize, line_content: &str, get_line: &F)
    where
        F: Fn(usize) -> Option<String>
    {
        // Ensure we have enough line states
        while self.line_states.len() <= line_index {
            self.line_states.push(LineState::new());
        }

        // Get the entry state from the previous line
        let entry_state = if line_index > 0 && line_index - 1 < self.line_states.len() {
            self.line_states[line_index - 1].exit_state
        } else {
            SyntaxState::Normal
        };

        // Parse the line and collect all the data we need
        let content_hash = calculate_hash(line_content);
        let bytes = line_content.as_bytes();

        // Use the enhanced tokenizer if we're processing a programming language
        let (new_spans, final_state) = if self.language != Language::PlainText {
            self.tokenize_line_enhanced(line_content, entry_state, bytes, line_index, get_line)
        } else {
            self.tokenize_line_simple(line_content, entry_state, bytes)
        };

        // Set exit state (line comments don't carry over)
        let new_exit_state = match final_state {
            SyntaxState::LineComment => SyntaxState::Normal,
            other => other,
        };

        // Check if we need to mark the next line as dirty before updating
        let should_mark_next = if line_index + 1 < self.line_states.len() {
            self.line_states[line_index + 1].entry_state != new_exit_state
        } else {
            false
        };

        // Now update the line state
        let line_state = &mut self.line_states[line_index];
        line_state.entry_state = entry_state;
        line_state.exit_state = new_exit_state;
        line_state.spans = new_spans;
        line_state.content_hash = content_hash;

        // Mark next line as dirty if needed
        if should_mark_next {
            self.mark_dirty(line_index + 1);
        }
    }

    /// Simple tokenizer for plain text (existing logic)
    fn tokenize_line_simple(&self, _line_content: &str, entry_state: SyntaxState, bytes: &[u8]) -> (Vec<HighlightSpan>, SyntaxState) {
        let mut new_spans = Vec::new();
        let mut current_state = entry_state;
        let mut current_pos = 0;
        let mut span_start = 0;

        while current_pos < bytes.len() {
            match current_state {
                SyntaxState::Normal => {
                    // Check for comment starts
                    if current_pos + 1 < bytes.len() {
                        if bytes[current_pos] == b'/' && bytes[current_pos + 1] == b'/' {
                            if current_pos > span_start {
                                new_spans.push(HighlightSpan {
                                    start: span_start,
                                    end: current_pos,
                                    state: SyntaxState::Normal,
                                });
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::LineComment;
                            current_pos += 2;
                            continue;
                        } else if bytes[current_pos] == b'-' && bytes[current_pos + 1] == b'-' {
                            if current_pos > span_start {
                                new_spans.push(HighlightSpan {
                                    start: span_start,
                                    end: current_pos,
                                    state: SyntaxState::Normal,
                                });
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::LineComment;
                            current_pos += 2;
                            continue;
                        } else if bytes[current_pos] == b'/' && bytes[current_pos + 1] == b'*' {
                            if current_pos > span_start {
                                new_spans.push(HighlightSpan {
                                    start: span_start,
                                    end: current_pos,
                                    state: SyntaxState::Normal,
                                });
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::BlockComment;
                            current_pos += 2;
                            continue;
                        }
                    }

                    // Check for string starts
                    if bytes[current_pos] == b'"' {
                        if current_pos > span_start {
                            new_spans.push(HighlightSpan {
                                start: span_start,
                                end: current_pos,
                                state: SyntaxState::Normal,
                            });
                        }
                        span_start = current_pos;
                        current_state = SyntaxState::StringDouble;
                        current_pos += 1;
                    } else {
                        current_pos += 1;
                    }
                }

                SyntaxState::StringDouble => {
                    if bytes[current_pos] == b'\\' && current_pos + 1 < bytes.len() {
                        current_pos += 2;
                    } else if bytes[current_pos] == b'"' {
                        current_pos += 1;
                        new_spans.push(HighlightSpan {
                            start: span_start,
                            end: current_pos,
                            state: SyntaxState::StringDouble,
                        });
                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                    } else {
                        current_pos += 1;
                    }
                }

                SyntaxState::LineComment => {
                    current_pos = bytes.len();
                }

                SyntaxState::BlockComment => {
                    if current_pos + 1 < bytes.len() &&
                       bytes[current_pos] == b'*' && bytes[current_pos + 1] == b'/' {
                        current_pos += 2;
                        new_spans.push(HighlightSpan {
                            start: span_start,
                            end: current_pos,
                            state: SyntaxState::BlockComment,
                        });
                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                    } else {
                        current_pos += 1;
                    }
                }

                _ => {
                    current_pos += 1;
                }
            }
        }

        // Add final span if needed
        if span_start < bytes.len() || (span_start == 0 && bytes.len() == 0) {
            new_spans.push(HighlightSpan {
                start: span_start,
                end: bytes.len(),
                state: current_state,
            });
        }

        (new_spans, current_state)
    }

    /// Tokenize normal code content (not inside strings/comments)
    fn tokenize_normal_segment(&self, line_content: &str, start: usize, end: usize, spans: &mut Vec<HighlightSpan>) {
        if start >= end {
            return;
        }

        // PlainText doesn't need syntax highlighting
        if self.language == Language::PlainText {
            return;
        }

        let segment = &line_content[start..end];
        let segment_bytes = segment.as_bytes();
        let mut pos = 0;

        while pos < segment_bytes.len() {
            let ch = segment_bytes[pos] as char;
            let abs_pos = start + pos;

            // Skip whitespace
            if ch.is_whitespace() {
                pos += 1;
                continue;
            }

            // Check for numbers
            if ch.is_numeric() {
                let token_start = pos;
                while pos < segment_bytes.len() {
                    let c = segment_bytes[pos] as char;
                    if !c.is_numeric() && c != '.' && c != '_' {
                        break;
                    }
                    pos += 1;
                }
                spans.push(HighlightSpan {
                    start: start + token_start,
                    end: start + pos,
                    state: SyntaxState::Number,
                });
                continue;
            }

            // Check for identifiers/keywords
            if ch.is_alphabetic() || ch == '_' || ch == '@' {
                let token_start = pos;
                while pos < segment_bytes.len() {
                    let c = segment_bytes[pos] as char;
                    if !c.is_alphanumeric() && c != '_' {
                        break;
                    }
                    pos += 1;
                }

                let word = &segment[token_start..pos];

                // Determine token type
                let token_state = if self.is_keyword(word) {
                    SyntaxState::Keyword
                } else if self.is_type(word) {
                    SyntaxState::Type
                } else {
                    // Check if it's a function call (followed by '(')
                    let mut check_pos = pos;
                    while check_pos < segment_bytes.len() && (segment_bytes[check_pos] as char).is_whitespace() {
                        check_pos += 1;
                    }
                    if check_pos < segment_bytes.len() && segment_bytes[check_pos] == b'(' {
                        SyntaxState::Function
                    } else {
                        SyntaxState::Normal
                    }
                };

                spans.push(HighlightSpan {
                    start: start + token_start,
                    end: start + pos,
                    state: token_state,
                });
                continue;
            }

            // Check for operators
            if "+-*/%=<>!&|^~".contains(ch) {
                spans.push(HighlightSpan {
                    start: abs_pos,
                    end: abs_pos + 1,
                    state: SyntaxState::Operator,
                });
                pos += 1;
                continue;
            }

            // Check for punctuation
            if "()[]{},.;:".contains(ch) {
                spans.push(HighlightSpan {
                    start: abs_pos,
                    end: abs_pos + 1,
                    state: SyntaxState::Punctuation,
                });
                pos += 1;
                continue;
            }

            // Other characters - skip
            pos += 1;
        }
    }

    /// Tokenize markdown content
    fn tokenize_markdown_line(&self, _line_content: &str, entry_state: SyntaxState, bytes: &[u8]) -> (Vec<HighlightSpan>, SyntaxState) {
        let mut new_spans = Vec::new();
        let current_state = entry_state;
        let mut current_pos = 0;

        // Handle code blocks (```)
        if current_state == SyntaxState::StringTriple || current_state == SyntaxState::StringTripleSingle {
            // We're inside a code block, look for closing ```
            if bytes.len() >= 3 && bytes[0] == b'`' && bytes[1] == b'`' && bytes[2] == b'`' {
                // Found closing ```, highlight the line and exit code block
                new_spans.push(HighlightSpan {
                    start: 0,
                    end: bytes.len(),
                    state: current_state,
                });
                return (new_spans, SyntaxState::Normal);
            } else {
                // Still inside code block, highlight entire line
                new_spans.push(HighlightSpan {
                    start: 0,
                    end: bytes.len(),
                    state: current_state,
                });
                return (new_spans, current_state);
            }
        }

        // Check if line starts with ``` (code block start)
        if bytes.len() >= 3 && bytes[0] == b'`' && bytes[1] == b'`' && bytes[2] == b'`' {
            new_spans.push(HighlightSpan {
                start: 0,
                end: bytes.len(),
                state: SyntaxState::StringTriple,
            });
            return (new_spans, SyntaxState::StringTriple);
        }

        // Check for headers (# ## ### etc at start of line)
        if bytes.len() > 0 && bytes[0] == b'#' {
            let mut hash_count = 0;
            while hash_count < bytes.len() && bytes[hash_count] == b'#' {
                hash_count += 1;
            }
            if hash_count <= 6 && (hash_count == bytes.len() || bytes[hash_count] == b' ') {
                // Valid header
                new_spans.push(HighlightSpan {
                    start: 0,
                    end: bytes.len(),
                    state: SyntaxState::Keyword,
                });
                return (new_spans, SyntaxState::Normal);
            }
        }

        // Process inline formatting (bold, italic, inline code)
        while current_pos < bytes.len() {
            let ch = bytes[current_pos];

            // Inline code (`code`)
            if ch == b'`' {
                let code_start = current_pos;
                current_pos += 1;
                // Find closing `
                while current_pos < bytes.len() && bytes[current_pos] != b'`' {
                    current_pos += 1;
                }
                if current_pos < bytes.len() {
                    current_pos += 1; // Include closing `
                }
                new_spans.push(HighlightSpan {
                    start: code_start,
                    end: current_pos,
                    state: SyntaxState::StringDouble,
                });
                continue;
            }

            // Bold (**text** or __text__)
            if (ch == b'*' && current_pos + 1 < bytes.len() && bytes[current_pos + 1] == b'*') ||
               (ch == b'_' && current_pos + 1 < bytes.len() && bytes[current_pos + 1] == b'_') {
                let marker = ch;
                let bold_start = current_pos;
                current_pos += 2;
                // Find closing marker
                while current_pos + 1 < bytes.len() {
                    if bytes[current_pos] == marker && bytes[current_pos + 1] == marker {
                        current_pos += 2;
                        new_spans.push(HighlightSpan {
                            start: bold_start,
                            end: current_pos,
                            state: SyntaxState::Type,
                        });
                        break;
                    }
                    current_pos += 1;
                }
                continue;
            }

            // Italic (*text* or _text_)
            if (ch == b'*' || ch == b'_') &&
               (current_pos == 0 || bytes[current_pos - 1] == b' ') {
                let marker = ch;
                let italic_start = current_pos;
                current_pos += 1;
                // Find closing marker
                while current_pos < bytes.len() {
                    if bytes[current_pos] == marker &&
                       (current_pos + 1 >= bytes.len() || bytes[current_pos + 1] == b' ' ||
                        !bytes[current_pos + 1].is_ascii_alphanumeric()) {
                        current_pos += 1;
                        new_spans.push(HighlightSpan {
                            start: italic_start,
                            end: current_pos,
                            state: SyntaxState::Function,
                        });
                        break;
                    }
                    current_pos += 1;
                }
                continue;
            }

            // Links [text](url)
            if ch == b'[' {
                let link_start = current_pos;
                current_pos += 1;
                // Find ]
                while current_pos < bytes.len() && bytes[current_pos] != b']' {
                    current_pos += 1;
                }
                if current_pos < bytes.len() && current_pos + 1 < bytes.len() && bytes[current_pos + 1] == b'(' {
                    current_pos += 2; // Skip ](
                    // Find closing )
                    while current_pos < bytes.len() && bytes[current_pos] != b')' {
                        current_pos += 1;
                    }
                    if current_pos < bytes.len() {
                        current_pos += 1; // Include )
                        new_spans.push(HighlightSpan {
                            start: link_start,
                            end: current_pos,
                            state: SyntaxState::Number,
                        });
                        continue;
                    }
                }
            }

            current_pos += 1;
        }

        (new_spans, SyntaxState::Normal)
    }

    /// Enhanced tokenizer for programming languages
    fn tokenize_line_enhanced<F>(&self, line_content: &str, entry_state: SyntaxState, bytes: &[u8], line_index: usize, get_line: &F) -> (Vec<HighlightSpan>, SyntaxState)
    where
        F: Fn(usize) -> Option<String>
    {
        // Special handling for Markdown
        if self.language == Language::Markdown {
            return self.tokenize_markdown_line(line_content, entry_state, bytes);
        }

        let mut new_spans = Vec::new();
        let mut current_state = entry_state;
        let mut current_pos = 0;
        let mut span_start = 0;

        while current_pos < bytes.len() {
            match current_state {
                SyntaxState::Normal => {
                    // Check for Python triple-quoted strings
                    if self.language == Language::Python && current_pos + 2 < bytes.len() {
                        if bytes[current_pos] == b'"' && bytes[current_pos + 1] == b'"' && bytes[current_pos + 2] == b'"' {
                            if current_pos > span_start {
                                self.tokenize_normal_segment(line_content, span_start, current_pos, &mut new_spans);
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::StringTriple;
                            current_pos += 3;
                            continue;
                        } else if bytes[current_pos] == b'\'' && bytes[current_pos + 1] == b'\'' && bytes[current_pos + 2] == b'\'' {
                            if current_pos > span_start {
                                self.tokenize_normal_segment(line_content, span_start, current_pos, &mut new_spans);
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::StringTripleSingle;
                            current_pos += 3;
                            continue;
                        }
                    }

                    // Check for # comments (Python, R, Yaml, Shell, Toml)
                    if (self.language == Language::Python || self.language == Language::R ||
                        self.language == Language::Yaml || self.language == Language::Shell ||
                        self.language == Language::Toml) && bytes[current_pos] == b'#' {
                        if current_pos > span_start {
                            self.tokenize_normal_segment(line_content, span_start, current_pos, &mut new_spans);
                        }
                        span_start = current_pos;
                        current_state = SyntaxState::LineComment;
                        current_pos += 1;
                        continue;
                    }

                    // Check for // comments (Rust, C-style)
                    if (self.language == Language::Rust) && current_pos + 1 < bytes.len() {
                        if bytes[current_pos] == b'/' && bytes[current_pos + 1] == b'/' {
                            if current_pos > span_start {
                                self.tokenize_normal_segment(line_content, span_start, current_pos, &mut new_spans);
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::LineComment;
                            current_pos += 2;
                            continue;
                        }
                    }

                    // Check for -- comments (SQL)
                    if self.language == Language::Sql && current_pos + 1 < bytes.len() {
                        if bytes[current_pos] == b'-' && bytes[current_pos + 1] == b'-' {
                            if current_pos > span_start {
                                self.tokenize_normal_segment(line_content, span_start, current_pos, &mut new_spans);
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::LineComment;
                            current_pos += 2;
                            continue;
                        }
                    }

                    // Check for /* */ comments
                    if (self.language == Language::Rust || self.language == Language::Sql) &&
                       current_pos + 1 < bytes.len() {
                        if bytes[current_pos] == b'/' && bytes[current_pos + 1] == b'*' {
                            if current_pos > span_start {
                                self.tokenize_normal_segment(line_content, span_start, current_pos, &mut new_spans);
                            }
                            span_start = current_pos;
                            current_state = SyntaxState::BlockComment;
                            current_pos += 2;
                            continue;
                        }
                    }

                    // Check for string starts
                    if bytes[current_pos] == b'"' {
                        if current_pos > span_start {
                            self.tokenize_normal_segment(line_content, span_start, current_pos, &mut new_spans);
                        }
                        span_start = current_pos;
                        current_state = SyntaxState::StringDouble;
                        current_pos += 1;
                        continue;
                    }

                    // Single quotes for Python, R, Rust, SQL, Shell
                    if (self.language == Language::Python || self.language == Language::R ||
                        self.language == Language::Rust || self.language == Language::Sql ||
                        self.language == Language::Shell) && bytes[current_pos] == b'\'' {
                        if current_pos > span_start {
                            self.tokenize_normal_segment(line_content, span_start, current_pos, &mut new_spans);
                        }
                        span_start = current_pos;
                        current_state = SyntaxState::StringSingle;
                        current_pos += 1;
                        continue;
                    }

                    current_pos += 1;
                }

                SyntaxState::StringDouble => {
                    if bytes[current_pos] == b'\\' && current_pos + 1 < bytes.len() {
                        current_pos += 2;
                    } else if bytes[current_pos] == b'"' {
                        current_pos += 1;

                        // Check if this string should have SQL highlighting
                        let in_sql_context = self.language == Language::Python &&
                                             self.check_sql_context(line_index, line_content, entry_state, get_line);

                        if in_sql_context && span_start + 1 < current_pos - 1 {
                            // Extract string content (without quotes)
                            let string_content_start = span_start + 1;
                            let string_content_end = current_pos - 1;
                            let string_content = &line_content[string_content_start..string_content_end];

                            // Add opening quote span
                            new_spans.push(HighlightSpan {
                                start: span_start,
                                end: span_start + 1,
                                state: SyntaxState::StringDouble,
                            });

                            // Get SQL spans for content
                            let sql_spans = self.tokenize_sql_content(string_content, string_content_start);

                            // Fill gaps with string spans
                            let mut last_end = string_content_start;
                            for sql_span in &sql_spans {
                                // Add SQL text span before SQL span if there's a gap
                                if sql_span.start > last_end {
                                    new_spans.push(HighlightSpan {
                                        start: last_end,
                                        end: sql_span.start,
                                        state: SyntaxState::SqlText,
                                    });
                                }
                                new_spans.push(sql_span.clone());
                                last_end = sql_span.end;
                            }

                            // Add remaining SQL text after last SQL span
                            if last_end < string_content_end {
                                new_spans.push(HighlightSpan {
                                    start: last_end,
                                    end: string_content_end,
                                    state: SyntaxState::SqlText,
                                });
                            }

                            // Add closing quote span
                            new_spans.push(HighlightSpan {
                                start: string_content_end,
                                end: current_pos,
                                state: SyntaxState::StringDouble,
                            });
                        } else {
                            // Regular string without SQL highlighting
                            new_spans.push(HighlightSpan {
                                start: span_start,
                                end: current_pos,
                                state: SyntaxState::StringDouble,
                            });
                        }

                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                    } else {
                        current_pos += 1;
                    }
                }

                SyntaxState::StringSingle => {
                    if bytes[current_pos] == b'\\' && current_pos + 1 < bytes.len() {
                        current_pos += 2;
                    } else if bytes[current_pos] == b'\'' {
                        current_pos += 1;

                        // Check if this string should have SQL highlighting
                        let in_sql_context = self.language == Language::Python &&
                                             self.check_sql_context(line_index, line_content, entry_state, get_line);

                        if in_sql_context && span_start + 1 < current_pos - 1 {
                            // Extract string content (without quotes)
                            let string_content_start = span_start + 1;
                            let string_content_end = current_pos - 1;
                            let string_content = &line_content[string_content_start..string_content_end];

                            // Add opening quote span
                            new_spans.push(HighlightSpan {
                                start: span_start,
                                end: span_start + 1,
                                state: SyntaxState::StringSingle,
                            });

                            // Get SQL spans for content
                            let sql_spans = self.tokenize_sql_content(string_content, string_content_start);

                            // Fill gaps with string spans
                            let mut last_end = string_content_start;
                            for sql_span in &sql_spans {
                                // Add SQL text span before SQL span if there's a gap
                                if sql_span.start > last_end {
                                    new_spans.push(HighlightSpan {
                                        start: last_end,
                                        end: sql_span.start,
                                        state: SyntaxState::SqlText,
                                    });
                                }
                                new_spans.push(sql_span.clone());
                                last_end = sql_span.end;
                            }

                            // Add remaining SQL text after last SQL span
                            if last_end < string_content_end {
                                new_spans.push(HighlightSpan {
                                    start: last_end,
                                    end: string_content_end,
                                    state: SyntaxState::SqlText,
                                });
                            }

                            // Add closing quote span
                            new_spans.push(HighlightSpan {
                                start: string_content_end,
                                end: current_pos,
                                state: SyntaxState::StringSingle,
                            });
                        } else {
                            // Regular string without SQL highlighting
                            new_spans.push(HighlightSpan {
                                start: span_start,
                                end: current_pos,
                                state: SyntaxState::StringSingle,
                            });
                        }

                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                    } else {
                        current_pos += 1;
                    }
                }

                SyntaxState::StringTriple => {
                    // For continuation lines (span_start == 0 means we entered this state from previous line),
                    // tokenize the entire line as SQL if in SQL context
                    if span_start == 0 && current_pos == 0 {
                        let in_sql_context = self.language == Language::Python &&
                                             self.check_sql_context(line_index, line_content, entry_state, get_line);

                        if in_sql_context && !line_content.is_empty() {
                            // Check if this line has the closing """ by searching in bytes
                            let mut has_closing = false;
                            for i in 0..bytes.len().saturating_sub(2) {
                                if bytes[i] == b'"' && bytes[i+1] == b'"' && bytes[i+2] == b'"' {
                                    has_closing = true;
                                    break;
                                }
                            }

                            if !has_closing {
                                // This is a full continuation line - tokenize entire line as SQL
                                let sql_spans = self.tokenize_sql_content(line_content, 0);

                                // Fill gaps with SQL text spans
                                let mut last_end = 0;
                                for sql_span in &sql_spans {
                                    if sql_span.start > last_end {
                                        new_spans.push(HighlightSpan {
                                            start: last_end,
                                            end: sql_span.start,
                                            state: SyntaxState::SqlText,
                                        });
                                    }
                                    new_spans.push(sql_span.clone());
                                    last_end = sql_span.end;
                                }

                                // Add remaining SQL text
                                if last_end < bytes.len() {
                                    new_spans.push(HighlightSpan {
                                        start: last_end,
                                        end: bytes.len(),
                                        state: SyntaxState::SqlText,
                                    });
                                }

                                // Skip to end of line
                                current_pos = bytes.len();
                                continue;
                            }
                        }
                    }

                    // Look for closing """
                    if current_pos + 2 < bytes.len() &&
                       bytes[current_pos] == b'"' &&
                       bytes[current_pos + 1] == b'"' &&
                       bytes[current_pos + 2] == b'"' {
                        current_pos += 3;

                        // Check if this string should have SQL highlighting
                        let in_sql_context = self.language == Language::Python &&
                                             self.check_sql_context(line_index, line_content, entry_state, get_line);

                        // Check if this is a continuation line (span_start == 0)
                        let is_continuation = span_start == 0;

                        if in_sql_context {
                            // For continuation lines, content starts at 0, not span_start + 3
                            let string_content_start = if is_continuation { 0 } else { span_start + 3 };
                            let string_content_end = current_pos - 3;

                            if string_content_start < string_content_end {
                                let string_content = &line_content[string_content_start..string_content_end];

                                // Add opening quote span (only if not continuation line)
                                if !is_continuation {
                                    new_spans.push(HighlightSpan {
                                        start: span_start,
                                        end: span_start + 3,
                                        state: SyntaxState::StringTriple,
                                    });
                                }

                                // Get SQL spans for content
                                let sql_spans = self.tokenize_sql_content(string_content, string_content_start);

                            // Fill gaps with SQL text spans
                            let mut last_end = string_content_start;
                            for sql_span in &sql_spans {
                                // Add SQL text span before SQL span if there's a gap
                                if sql_span.start > last_end {
                                    new_spans.push(HighlightSpan {
                                        start: last_end,
                                        end: sql_span.start,
                                        state: SyntaxState::SqlText,
                                    });
                                }
                                new_spans.push(sql_span.clone());
                                last_end = sql_span.end;
                            }

                            // Add remaining SQL text after last SQL span
                            if last_end < string_content_end {
                                new_spans.push(HighlightSpan {
                                    start: last_end,
                                    end: string_content_end,
                                    state: SyntaxState::SqlText,
                                });
                            }

                            // Add closing quote span
                            new_spans.push(HighlightSpan {
                                start: string_content_end,
                                end: current_pos,
                                state: SyntaxState::StringTriple,
                            });
                            }
                        } else {
                            // Regular string without SQL highlighting
                            new_spans.push(HighlightSpan {
                                start: span_start,
                                end: current_pos,
                                state: SyntaxState::StringTriple,
                            });
                        }

                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                        continue;
                    }
                    current_pos += 1;
                }

                SyntaxState::StringTripleSingle => {
                    // For continuation lines (span_start == 0 means we entered this state from previous line),
                    // tokenize the entire line as SQL if in SQL context
                    if span_start == 0 && current_pos == 0 {
                        let in_sql_context = self.language == Language::Python &&
                                             self.check_sql_context(line_index, line_content, entry_state, get_line);

                        if in_sql_context && !line_content.is_empty() {
                            // Check if this line has the closing ''' by searching in bytes
                            let mut has_closing = false;
                            for i in 0..bytes.len().saturating_sub(2) {
                                if bytes[i] == b'\'' && bytes[i+1] == b'\'' && bytes[i+2] == b'\'' {
                                    has_closing = true;
                                    break;
                                }
                            }

                            if !has_closing {
                                // This is a full continuation line - tokenize entire line as SQL
                                let sql_spans = self.tokenize_sql_content(line_content, 0);

                                // Fill gaps with SQL text spans
                                let mut last_end = 0;
                                for sql_span in &sql_spans {
                                    if sql_span.start > last_end {
                                        new_spans.push(HighlightSpan {
                                            start: last_end,
                                            end: sql_span.start,
                                            state: SyntaxState::SqlText,
                                        });
                                    }
                                    new_spans.push(sql_span.clone());
                                    last_end = sql_span.end;
                                }

                                // Add remaining SQL text
                                if last_end < bytes.len() {
                                    new_spans.push(HighlightSpan {
                                        start: last_end,
                                        end: bytes.len(),
                                        state: SyntaxState::SqlText,
                                    });
                                }

                                // Skip to end of line
                                current_pos = bytes.len();
                                continue;
                            }
                        }
                    }

                    // Look for closing '''
                    if current_pos + 2 < bytes.len() &&
                       bytes[current_pos] == b'\'' &&
                       bytes[current_pos + 1] == b'\'' &&
                       bytes[current_pos + 2] == b'\'' {
                        current_pos += 3;

                        // Check if this string should have SQL highlighting
                        let in_sql_context = self.language == Language::Python &&
                                             self.check_sql_context(line_index, line_content, entry_state, get_line);

                        // Check if this is a continuation line (span_start == 0)
                        let is_continuation = span_start == 0;

                        if in_sql_context {
                            // For continuation lines, content starts at 0, not span_start + 3
                            let string_content_start = if is_continuation { 0 } else { span_start + 3 };
                            let string_content_end = current_pos - 3;

                            if string_content_start < string_content_end {
                                let string_content = &line_content[string_content_start..string_content_end];

                                // Add opening quote span (only if not continuation line)
                                if !is_continuation {
                                    new_spans.push(HighlightSpan {
                                        start: span_start,
                                        end: span_start + 3,
                                        state: SyntaxState::StringTripleSingle,
                                    });
                                }

                                // Get SQL spans for content
                                let sql_spans = self.tokenize_sql_content(string_content, string_content_start);

                            // Fill gaps with SQL text spans
                            let mut last_end = string_content_start;
                            for sql_span in &sql_spans {
                                // Add SQL text span before SQL span if there's a gap
                                if sql_span.start > last_end {
                                    new_spans.push(HighlightSpan {
                                        start: last_end,
                                        end: sql_span.start,
                                        state: SyntaxState::SqlText,
                                    });
                                }
                                new_spans.push(sql_span.clone());
                                last_end = sql_span.end;
                            }

                            // Add remaining SQL text after last SQL span
                            if last_end < string_content_end {
                                new_spans.push(HighlightSpan {
                                    start: last_end,
                                    end: string_content_end,
                                    state: SyntaxState::SqlText,
                                });
                            }

                            // Add closing quote span
                            new_spans.push(HighlightSpan {
                                start: string_content_end,
                                end: current_pos,
                                state: SyntaxState::StringTripleSingle,
                            });
                            }
                        } else {
                            // Regular string without SQL highlighting
                            new_spans.push(HighlightSpan {
                                start: span_start,
                                end: current_pos,
                                state: SyntaxState::StringTripleSingle,
                            });
                        }

                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                        continue;
                    }
                    current_pos += 1;
                }

                SyntaxState::LineComment => {
                    current_pos = bytes.len();
                }

                SyntaxState::BlockComment => {
                    if current_pos + 1 < bytes.len() &&
                       bytes[current_pos] == b'*' && bytes[current_pos + 1] == b'/' {
                        current_pos += 2;
                        new_spans.push(HighlightSpan {
                            start: span_start,
                            end: current_pos,
                            state: SyntaxState::BlockComment,
                        });
                        span_start = current_pos;
                        current_state = SyntaxState::Normal;
                    } else {
                        current_pos += 1;
                    }
                }

                _ => {
                    current_pos += 1;
                }
            }
        }

        // Add final span if needed
        if current_state == SyntaxState::Normal {
            if span_start < bytes.len() {
                self.tokenize_normal_segment(line_content, span_start, bytes.len(), &mut new_spans);
            }
        } else if span_start < bytes.len() || (span_start == 0 && bytes.len() == 0) {
            new_spans.push(HighlightSpan {
                start: span_start,
                end: bytes.len(),
                state: current_state,
            });
        }

        (new_spans, current_state)
    }
    
    /// Process all dirty lines
    pub fn process_dirty_lines(&mut self, get_line: impl Fn(usize) -> Option<String>) {
        // Early exit if no dirty lines
        if self.dirty_lines.is_empty() {
            return;
        }
        
        // In viewport mode, only process lines within the buffer zone
        let process_limit = if self.viewport_mode { 100 } else { usize::MAX };
        let mut processed = 0;
        
        while let Some(line_index) = self.dirty_lines.pop() {
            // Skip lines outside viewport in viewport mode
            if self.viewport_mode && 
               (line_index < self.viewport_start || line_index > self.viewport_end) {
                continue;
            }
            
            if let Some(line_content) = get_line(line_index) {
                self.process_line(line_index, &line_content, &get_line);
                processed += 1;
                
                // Limit processing per frame in viewport mode to maintain responsiveness
                if processed >= process_limit {
                    break;
                }
            }
        }
    }
    
    /// Get the highlight spans for a line
    pub fn get_line_spans(&self, line_index: usize) -> Option<&[HighlightSpan]> {
        // In viewport mode, only return spans for processed lines
        if self.viewport_mode && 
           (line_index < self.viewport_start || line_index > self.viewport_end) {
            return None; // Outside viewport, no highlighting
        }
        
        self.line_states.get(line_index).map(|state| state.spans.as_slice())
    }
    
    /// Called when lines are inserted
    pub fn lines_inserted(&mut self, at_line: usize, count: usize) {
        // Ensure we have enough line states before the insertion point
        while self.line_states.len() < at_line {
            self.line_states.push(LineState::new());
        }
        
        // Store the exit state before insertion (if it exists)
        let exit_state_before = if at_line > 0 && at_line - 1 < self.line_states.len() {
            Some(self.line_states[at_line - 1].exit_state)
        } else {
            None
        };
        
        // Insert new line states
        for _ in 0..count {
            self.line_states.insert(at_line.min(self.line_states.len()), LineState::new());
        }
        
        // Mark the inserted lines as dirty
        for i in at_line..at_line + count {
            if i < self.line_states.len() {
                self.mark_dirty(i);
            }
        }
        
        // Check if we need to mark subsequent lines as dirty
        // If the insertion might affect the syntactic state of following lines,
        // we need to mark them as dirty too
        if let Some(_prev_exit_state) = exit_state_before {
            // After processing the inserted lines, their exit state might differ
            // from what the next line expects. Mark all subsequent lines that might
            // be affected until we find a stable point
            let mut check_line = at_line + count;
            while check_line < self.line_states.len() {
                // Mark this line as dirty
                self.mark_dirty(check_line);
                
                // In viewport mode, don't propagate too far
                if self.viewport_mode && check_line > at_line + count + 100 {
                    break;
                }
                
                // For small files, we can afford to mark more lines
                if !self.viewport_mode && check_line > at_line + count + 500 {
                    // Stop after checking 500 lines to avoid performance issues
                    break;
                }
                
                check_line += 1;
            }
        } else {
            // No previous state, just mark the immediate next line
            if at_line + count < self.line_states.len() {
                self.mark_dirty(at_line + count);
            }
        }
    }
    
    /// Called when lines are deleted
    pub fn lines_deleted(&mut self, at_line: usize, count: usize) {
        // Remove line states
        for _ in 0..count {
            if at_line < self.line_states.len() {
                self.line_states.remove(at_line);
            }
        }
        
        // Mark the next line as dirty
        if at_line < self.line_states.len() {
            self.mark_dirty(at_line);
        }
    }
    
    /// Called when a line is modified
    pub fn line_modified(&mut self, line_index: usize) {
        // Store the current exit state before marking as dirty
        let old_exit_state = if line_index < self.line_states.len() {
            Some(self.line_states[line_index].exit_state)
        } else {
            None
        };
        
        self.mark_dirty(line_index);
        
        // Mark subsequent lines that might be affected by state changes
        // This is important for multi-line constructs like block comments
        let mut check_line = line_index + 1;
        let max_check = if self.viewport_mode {
            line_index + 100  // Limited propagation in viewport mode
        } else {
            line_index + 500  // More extensive check for small files
        };
        
        while check_line < self.line_states.len() && check_line <= max_check {
            self.mark_dirty(check_line);
            
            // If we know the old exit state was Normal and the line had Normal entry,
            // we might be able to stop early (optimization for future)
            if let Some(SyntaxState::Normal) = old_exit_state {
                if check_line < self.line_states.len() {
                    if self.line_states[check_line].entry_state == SyntaxState::Normal {
                        // Mark one more line and stop
                        check_line += 1;
                        if check_line < self.line_states.len() {
                            self.mark_dirty(check_line);
                        }
                        break;
                    }
                }
            }
            
            check_line += 1;
        }
    }
    
    /// Initialize highlighting for all lines (only for small files)
    pub fn init_all_lines(&mut self, line_count: usize) {
        // Don't init all lines if file is too large
        if line_count > 50_000 {
            self.viewport_mode = true;
            return;
        }
        
        self.viewport_mode = false;
        self.line_states.clear();
        self.dirty_lines.clear();
        for i in 0..line_count {
            self.line_states.push(LineState::new());
            self.dirty_lines.push(i);
        }
    }
}

fn calculate_hash(content: &str) -> u64 {
    // Simple hash function for change detection
    let mut hash: u64 = 5381;
    for byte in content.bytes() {
        hash = ((hash << 5).wrapping_add(hash)).wrapping_add(byte as u64);
    }
    hash
}
