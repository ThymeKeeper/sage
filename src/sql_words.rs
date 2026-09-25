//! SQL-mode autocomplete: Snowflake's own words (keywords, types, parameters
//! and functions), and the names seen in this session's queries (tables,
//! aliases, columns), kept in memory for as long as sage runs.

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

/// Keywords, clauses, commands, object types, data types, date parts, and the
/// parameters and options commonly typed in Snowflake SQL.
const KEYWORDS: &[&str] = &[
    // Queries
    "SELECT", "DISTINCT", "ALL", "FROM", "WHERE", "GROUP", "BY", "HAVING", "QUALIFY", "ORDER",
    "ASC", "DESC", "NULLS", "FIRST", "LAST", "LIMIT", "OFFSET", "FETCH", "NEXT", "ONLY", "TOP",
    "AS", "WITH", "RECURSIVE", "UNION", "INTERSECT", "EXCEPT", "MINUS", "EXCLUDE", "RENAME",
    "REPLACE", "ILIKE", "LIKE", "RLIKE", "REGEXP", "ESCAPE", "ANY", "SOME", "EXISTS", "IN",
    "BETWEEN", "IS", "NOT", "AND", "OR", "NULL", "TRUE", "FALSE", "CASE", "WHEN", "THEN",
    "ELSE", "END", "CAST", "TRY_CAST", "DEFAULT", "IDENTIFIER", "VALUES", "TABLE", "LATERAL",
    // Joins
    "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "OUTER", "CROSS", "NATURAL", "ON", "USING",
    "ASOF", "MATCH_CONDITION", "DIRECTED",
    // Windows and grouping
    "OVER", "PARTITION", "WINDOW", "ROWS", "RANGE", "UNBOUNDED", "PRECEDING", "FOLLOWING",
    "CURRENT", "ROW", "WITHIN", "RESPECT", "IGNORE", "GROUPING", "SETS", "CUBE", "ROLLUP",
    // Pivots, sampling, pattern matching, hierarchies, time travel
    "PIVOT", "UNPIVOT", "INCLUDE", "SAMPLE", "TABLESAMPLE", "BERNOULLI", "SYSTEM", "BLOCK",
    "SEED", "REPEATABLE", "MATCH_RECOGNIZE", "MEASURES", "PATTERN", "DEFINE", "AFTER", "MATCH",
    "SKIP", "PAST", "PER", "ONE", "CONNECT", "PRIOR", "START", "AT", "BEFORE", "TIMESTAMP",
    "STATEMENT", "STREAM", "CHANGES", "INFORMATION", "APPEND_ONLY", "INSERT_ONLY",
    // Changing data
    "INSERT", "INTO", "OVERWRITE", "UPDATE", "SET", "DELETE", "TRUNCATE", "MERGE", "MATCHED",
    "COPY", "PUT", "GET", "LIST", "REMOVE",
    // Objects
    "CREATE", "ALTER", "DROP", "UNDROP", "IF", "TEMPORARY", "TEMP", "TRANSIENT", "VOLATILE",
    "LOCAL", "GLOBAL", "SECURE", "MATERIALIZED", "DYNAMIC", "EXTERNAL", "HYBRID", "ICEBERG",
    "EVENT", "CLONE", "SWAP", "ADD", "MODIFY", "COLUMN", "CONSTRAINT", "PRIMARY", "KEY",
    "FOREIGN", "REFERENCES", "UNIQUE", "CHECK", "AUTOINCREMENT", "IDENTITY", "COLLATE",
    "COMMENT", "TAG", "UNSET", "CLUSTER", "RECLUSTER", "SUSPEND", "RESUME", "REFRESH", "ABORT",
    "MASKING", "POLICY", "ACCESS", "SEQUENCE", "NEXTVAL", "DATABASE", "DATABASES", "SCHEMA",
    "SCHEMAS", "TABLES", "VIEW", "VIEWS", "COLUMNS", "FUNCTION", "FUNCTIONS", "PROCEDURE",
    "PROCEDURES", "TASK", "TASKS", "STREAMS", "PIPE", "PIPES", "STAGE", "STAGES", "FILE",
    "FORMAT", "FORMATS", "INTEGRATION", "INTEGRATIONS", "WAREHOUSE", "WAREHOUSES", "ROLE",
    "ROLES", "USER", "USERS", "SHARE", "SHARES", "ALERT", "ALERTS", "SEQUENCES", "OBJECTS",
    "PARAMETERS", "ACCOUNT", "ORGANIZATION", "SESSION", "NETWORK", "RULE", "SECRET",
    "NOTEBOOK", "STREAMLIT", "SNAPSHOT",
    // Inspecting and running
    "SHOW", "DESCRIBE", "EXPLAIN", "TERSE", "HISTORY", "STARTS", "USE", "CALL", "EXECUTE",
    "IMMEDIATE", "BEGIN", "COMMIT", "ROLLBACK", "TRANSACTION", "WORK", "CANCEL",
    // Access
    "GRANT", "REVOKE", "OWNERSHIP", "USAGE", "PRIVILEGES", "GRANTS", "FUTURE", "OPERATE",
    "MONITOR", "REFERENCE_USAGE", "APPLY",
    // Procedures and scripting
    "RETURNS", "RETURN", "LANGUAGE", "SQL", "PYTHON", "JAVASCRIPT", "JAVA", "SCALA",
    "RUNTIME_VERSION", "PACKAGES", "HANDLER", "IMPORTS", "CALLER", "OWNER", "STRICT",
    "IMMUTABLE", "CALLED", "INPUT", "DECLARE", "LET", "CURSOR", "RESULTSET", "EXCEPTION",
    "RAISE", "LOOP", "FOR", "WHILE", "DO", "REPEAT", "UNTIL", "BREAK", "CONTINUE", "ELSEIF",
    "OPEN", "CLOSE", "ASYNC", "AWAIT", "OTHER", "SQLCODE", "SQLERRM", "SQLSTATE", "SQLROWCOUNT",
    "SQLFOUND", "SQLNOTFOUND",
    // Data types
    "NUMBER", "DECIMAL", "DEC", "NUMERIC", "INT", "INTEGER", "BIGINT", "SMALLINT", "TINYINT",
    "BYTEINT", "FLOAT", "FLOAT4", "FLOAT8", "DOUBLE", "PRECISION", "REAL", "DECFLOAT",
    "VARCHAR", "CHAR", "CHARACTER", "NCHAR", "NVARCHAR", "NVARCHAR2", "STRING", "TEXT",
    "VARYING", "BINARY", "VARBINARY", "BOOLEAN", "DATE", "DATETIME", "TIME", "TIMESTAMP_LTZ",
    "TIMESTAMP_NTZ", "TIMESTAMP_TZ", "VARIANT", "OBJECT", "ARRAY", "MAP", "GEOGRAPHY",
    "GEOMETRY", "VECTOR", "INTERVAL",
    // Date and time parts
    "YEAR", "YEARS", "QUARTER", "MONTH", "MONTHS", "WEEK", "WEEKS", "DAY", "DAYS", "HOUR",
    "HOURS", "MINUTE", "MINUTES", "SECOND", "SECONDS", "MILLISECOND", "MICROSECOND",
    "NANOSECOND", "DAYOFWEEK", "DAYOFYEAR", "WEEKISO", "YEAROFWEEK", "EPOCH_SECOND",
    "EPOCH_MILLISECOND", "EPOCH_MICROSECOND", "EPOCH_NANOSECOND", "TIMEZONE_HOUR",
    "TIMEZONE_MINUTE",
    // FLATTEN and GENERATOR arguments and output columns
    "PATH", "MODE", "SEQ", "INDEX", "VALUE", "THIS", "ROWCOUNT", "TIMELIMIT",
    // Session and object parameters
    "STATEMENT_TIMEOUT_IN_SECONDS", "STATEMENT_QUEUED_TIMEOUT_IN_SECONDS", "QUERY_TAG",
    "TIMEZONE", "USE_CACHED_RESULT", "DATE_INPUT_FORMAT", "DATE_OUTPUT_FORMAT",
    "TIME_INPUT_FORMAT", "TIME_OUTPUT_FORMAT", "TIMESTAMP_INPUT_FORMAT",
    "TIMESTAMP_OUTPUT_FORMAT", "TIMESTAMP_NTZ_OUTPUT_FORMAT", "TIMESTAMP_LTZ_OUTPUT_FORMAT",
    "TIMESTAMP_TZ_OUTPUT_FORMAT", "TIMESTAMP_TYPE_MAPPING", "WEEK_START", "WEEK_OF_YEAR_POLICY",
    "ABORT_DETACHED_QUERY", "AUTOCOMMIT", "BINARY_INPUT_FORMAT", "BINARY_OUTPUT_FORMAT",
    "ERROR_ON_NONDETERMINISTIC_MERGE", "ERROR_ON_NONDETERMINISTIC_UPDATE", "LOCK_TIMEOUT",
    "MULTI_STATEMENT_COUNT", "QUOTED_IDENTIFIERS_IGNORE_CASE", "ROWS_PER_RESULTSET",
    "DATA_RETENTION_TIME_IN_DAYS", "MAX_DATA_EXTENSION_TIME_IN_DAYS", "CHANGE_TRACKING",
    "ENABLE_SCHEMA_EVOLUTION", "SEARCH_OPTIMIZATION", "WAREHOUSE_SIZE", "WAREHOUSE_TYPE",
    "XSMALL", "SMALL", "MEDIUM", "LARGE", "XLARGE", "XXLARGE", "XXXLARGE", "AUTO_SUSPEND",
    "AUTO_RESUME", "INITIALLY_SUSPENDED", "MIN_CLUSTER_COUNT", "MAX_CLUSTER_COUNT",
    "SCALING_POLICY", "ECONOMY", "STANDARD", "RESOURCE_MONITOR", "SCHEDULE", "CRON",
    "USER_TASK_TIMEOUT_MS", "SUSPEND_TASK_AFTER_NUM_FAILURES",
    "USER_TASK_MANAGED_INITIAL_WAREHOUSE_SIZE", "ALLOW_OVERLAPPING_EXECUTION", "FINALIZE",
    "TARGET_LAG", "DOWNSTREAM", "REFRESH_MODE", "INITIALIZE", "INCREMENTAL", "AUTO",
    "ON_CREATE", "ON_SCHEDULE",
    // Stages, file formats and COPY options
    "FILE_FORMAT", "TYPE", "CSV", "JSON", "PARQUET", "AVRO", "ORC", "XML", "FIELD_DELIMITER",
    "RECORD_DELIMITER", "SKIP_HEADER", "PARSE_HEADER", "FIELD_OPTIONALLY_ENCLOSED_BY",
    "NULL_IF", "EMPTY_FIELD_AS_NULL", "SKIP_BLANK_LINES", "COMPRESSION", "GZIP", "BZ2",
    "BROTLI", "ZSTD", "DEFLATE", "RAW_DEFLATE", "SNAPPY", "NONE", "ENCODING",
    "ERROR_ON_COLUMN_COUNT_MISMATCH", "TRIM_SPACE", "DATE_FORMAT", "TIME_FORMAT",
    "TIMESTAMP_FORMAT", "ESCAPE_UNENCLOSED_FIELD", "REPLACE_INVALID_CHARACTERS",
    "STRIP_OUTER_ARRAY", "STRIP_NULL_VALUES", "BINARY_AS_TEXT", "USE_LOGICAL_TYPE",
    "ON_ERROR", "SKIP_FILE", "ABORT_STATEMENT", "PURGE", "FORCE", "FILES", "LOAD_MODE",
    "MATCH_BY_COLUMN_NAME", "CASE_INSENSITIVE", "CASE_SENSITIVE", "VALIDATION_MODE",
    "RETURN_ERRORS", "RETURN_ALL_ERRORS", "HEADER", "SINGLE", "MAX_FILE_SIZE",
    "INCLUDE_QUERY_ID", "DETAILED_OUTPUT", "URL", "CREDENTIALS", "STORAGE_INTEGRATION",
    "ENCRYPTION", "DIRECTORY", "ENABLE", "AUTO_REFRESH", "AUTO_INGEST", "AUTO_COMPRESS",
    "SOURCE_COMPRESSION", "PARALLEL", "LOCATION", "PARTITION_TYPE", "INFER_SCHEMA",
    // Well-known schemas and databases
    "INFORMATION_SCHEMA", "ACCOUNT_USAGE", "SNOWFLAKE", "PUBLIC",
];

/// Table functions (used as `TABLE(...)` or in FROM), which `SHOW FUNCTIONS`
/// doesn't list, and a few context functions missing from it.
const TABLE_FUNCTIONS: &[&str] = &[
    "FLATTEN", "GENERATOR", "RESULT_SCAN", "SPLIT_TO_TABLE", "STRTOK_SPLIT_TO_TABLE",
    "INFER_SCHEMA", "VALIDATE", "GET_QUERY_OPERATOR_STATS", "QUERY_HISTORY",
    "QUERY_HISTORY_BY_USER", "QUERY_HISTORY_BY_SESSION", "QUERY_HISTORY_BY_WAREHOUSE",
    "TASK_HISTORY", "TASK_DEPENDENTS", "COMPLETE_TASK_GRAPHS", "CURRENT_TASK_GRAPHS",
    "SERVERLESS_TASK_HISTORY", "COPY_HISTORY", "LOGIN_HISTORY", "LOGIN_HISTORY_BY_USER",
    "WAREHOUSE_LOAD_HISTORY", "WAREHOUSE_METERING_HISTORY", "DATABASE_STORAGE_USAGE_HISTORY",
    "STAGE_STORAGE_USAGE_HISTORY", "AUTOMATIC_CLUSTERING_HISTORY", "PIPE_USAGE_HISTORY",
    "MATERIALIZED_VIEW_REFRESH_HISTORY", "SEARCH_OPTIMIZATION_HISTORY",
    "DYNAMIC_TABLE_REFRESH_HISTORY", "DYNAMIC_TABLE_GRAPH_HISTORY", "DYNAMIC_TABLES",
    "ALERT_HISTORY", "EXTERNAL_TABLE_FILES", "EXTERNAL_TABLE_FILE_REGISTRATION_HISTORY",
    "POLICY_REFERENCES", "TAG_REFERENCES", "TAG_REFERENCES_ALL_COLUMNS",
    "GET_OBJECT_REFERENCES", "REST_EVENT_HISTORY", "CURRENT_ACCOUNT", "CURRENT_ACCOUNT_NAME",
];

/// Every stock word as written above (upper case), without duplicates.
fn stock_upper() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        KEYWORDS
            .iter()
            .chain(TABLE_FUNCTIONS)
            .chain(crate::sql_functions::FUNCTIONS)
            .copied()
            .collect()
    })
}

/// The stock list: every stock word three times, in upper, lower and title
/// case (`DATE_TRUNC`, `date_trunc`, `Date_Trunc`), sorted.
fn stock() -> &'static [String] {
    static STOCK: OnceLock<Vec<String>> = OnceLock::new();
    STOCK.get_or_init(|| {
        let mut all: Vec<String> = stock_upper().iter().flat_map(|w| case_forms(w)).collect();
        all.sort_unstable();
        all.dedup();
        all
    })
}

/// Whether `word` (any case) is one of Snowflake's stock words.
pub fn is_stock_word(word: &str) -> bool {
    stock_upper().contains(word.to_ascii_uppercase().as_str())
}

/// Stock words starting with `prefix`, matched case-sensitively: `sel` finds
/// `select`, `Sel` finds `Select`, `SEL` finds `SELECT`.
pub fn stock_matches(prefix: &str) -> Vec<String> {
    stock().iter().filter(|w| w.starts_with(prefix)).cloned().collect()
}

/// SQL common to DuckDB, Spark and other databases, for SQL in Python that
/// isn't sent to Snowflake. The database's own functions come from the Python
/// kernel alongside.
const GENERIC_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IN", "EXISTS",
    "JOIN", "LEFT", "RIGHT", "INNER", "OUTER", "FULL", "CROSS", "ON", "USING",
    "GROUP", "BY", "HAVING", "ORDER", "ASC", "DESC", "LIMIT", "OFFSET",
    "INSERT", "INTO", "VALUES", "UPDATE", "SET", "DELETE", "TRUNCATE",
    "CREATE", "ALTER", "DROP", "TABLE", "VIEW", "INDEX", "DATABASE", "SCHEMA",
    "AS", "DISTINCT", "ALL", "UNION", "INTERSECT", "EXCEPT",
    "CASE", "WHEN", "THEN", "ELSE", "END",
    "IS", "NULL", "BETWEEN", "LIKE", "ILIKE", "SIMILAR", "TO",
    "WITH", "RECURSIVE", "CTE",
    "COUNT", "SUM", "AVG", "MIN", "MAX", "STDDEV", "VARIANCE",
    "STRING_AGG", "ARRAY_AGG", "BOOL_AND", "BOOL_OR",
    "OVER", "PARTITION", "ROW_NUMBER", "RANK", "DENSE_RANK",
    "LAG", "LEAD", "FIRST_VALUE", "LAST_VALUE",
    "INTEGER", "INT", "BIGINT", "SMALLINT", "DECIMAL", "NUMERIC",
    "FLOAT", "DOUBLE", "REAL", "VARCHAR", "CHAR", "TEXT",
    "DATE", "TIME", "TIMESTAMP", "INTERVAL", "BOOLEAN", "BOOL",
    "JSON", "JSONB", "ARRAY", "STRUCT", "MAP",
    "CAST", "TRY_CAST", "CONVERT", "COALESCE", "NULLIF", "IFNULL", "NVL",
];

/// Generic SQL words starting with `prefix`, case-sensitive, each in upper,
/// lower and title case (see [`stock_matches`]).
pub fn generic_matches(prefix: &str) -> Vec<String> {
    static GENERIC: OnceLock<Vec<String>> = OnceLock::new();
    GENERIC
        .get_or_init(|| {
            let mut all: Vec<String> = GENERIC_KEYWORDS.iter().flat_map(|w| case_forms(w)).collect();
            all.sort_unstable();
            all.dedup();
            all
        })
        .iter()
        .filter(|w| w.starts_with(prefix))
        .cloned()
        .collect()
}

/// A word in upper, lower and title case, without repeats. Title case
/// capitalises the first letter and each letter after `_` or `$`.
pub fn case_forms(word: &str) -> Vec<String> {
    let mut title = String::with_capacity(word.len());
    let mut cap = true;
    for c in word.chars() {
        title.push(if cap { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() });
        cap = c == '_' || c == '$';
    }
    let mut forms: Vec<String> = Vec::with_capacity(3);
    for form in [word.to_ascii_uppercase(), word.to_ascii_lowercase(), title] {
        if !forms.contains(&form) {
            forms.push(form);
        }
    }
    forms
}

/// One part of a dotted name, as written.
enum Part {
    Plain(String),
    Quoted(String),
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// A quoted identifier's text as it is best typed: bare when Snowflake would
/// read it the same unquoted (upper case letters, digits, `_`, `$`), else in
/// double quotes.
fn quoted_name(inner: &str) -> String {
    let bare = inner.chars().next().map_or(false, |c| c.is_ascii_uppercase() || c == '_')
        && inner.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '$');
    if bare {
        inner.to_string()
    } else {
        format!("\"{}\"", inner.replace('"', "\"\""))
    }
}

/// Walk SQL text, calling `on_name` with each dotted name outside string
/// literals and comments (`a`, `t.col`, `db.schema.table`). Returns false when
/// the text ends inside a string, comment or quoted identifier.
fn scan(sql: &str, mut on_name: impl FnMut(Vec<Part>)) -> bool {
    let c: Vec<char> = sql.chars().collect();
    let n = c.len();
    let at = |i: usize| if i < n { c[i] } else { '\0' };
    let mut i = 0;
    while i < n {
        let ch = c[i];
        if ch == '\'' {
            // 'string', with '' and backslash escapes
            i += 1;
            loop {
                if i >= n {
                    return false;
                }
                match c[i] {
                    '\\' => i += 2,
                    '\'' if at(i + 1) == '\'' => i += 2,
                    '\'' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
        } else if ch == '$' && at(i + 1) == '$' {
            // $$dollar-quoted text$$
            i += 2;
            loop {
                if i + 1 >= n {
                    return false;
                }
                if c[i] == '$' && c[i + 1] == '$' {
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else if (ch == '-' && at(i + 1) == '-') || (ch == '/' && at(i + 1) == '/') {
            // -- or // comment, to the end of the line
            while i < n && c[i] != '\n' {
                i += 1;
            }
            if i >= n {
                return false;
            }
        } else if ch == '/' && at(i + 1) == '*' {
            i += 2;
            loop {
                if i + 1 >= n {
                    return false;
                }
                if c[i] == '*' && c[i + 1] == '/' {
                    i += 2;
                    break;
                }
                i += 1;
            }
        } else if ch == '"' || is_ident_start(ch) {
            let mut parts = Vec::new();
            loop {
                if c[i] == '"' {
                    i += 1;
                    let mut s = String::new();
                    loop {
                        if i >= n {
                            return false;
                        }
                        if c[i] == '"' {
                            if at(i + 1) == '"' {
                                s.push('"');
                                i += 2;
                                continue;
                            }
                            i += 1;
                            break;
                        }
                        s.push(c[i]);
                        i += 1;
                    }
                    parts.push(Part::Quoted(s));
                } else {
                    let start = i;
                    while i < n && is_ident_char(c[i]) {
                        i += 1;
                    }
                    parts.push(Part::Plain(c[start..i].iter().collect()));
                }
                if at(i) == '.' && (at(i + 1) == '"' || is_ident_start(at(i + 1))) {
                    i += 1;
                    continue;
                }
                break;
            }
            on_name(parts);
        } else if ch == '$' || ch.is_ascii_digit() {
            // $variables, $1 and numbers: not names
            i += 1;
            while i < n && (is_ident_char(c[i]) || (ch != '$' && c[i] == '.')) {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    true
}

/// The name being typed at the end of `before` (the text up to the caret):
/// `prefix` is the identifier so far, `qualifier` the dotted name before it
/// (`t` in `t.pa`, `DB.SCH` in `DB.SCH.TA`). None inside a string or comment,
/// or where no name can start (after a digit, say).
#[derive(Debug, Clone, PartialEq)]
pub struct SqlCompletion {
    pub qualifier: Option<String>,
    pub prefix: String,
}

pub fn completion_context(before: &str) -> Option<SqlCompletion> {
    if !scan(before, |_| {}) {
        return None;
    }
    let prefix_start = before
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_ident_char(c))
        .last()
        .map_or(before.len(), |(i, _)| i);
    let prefix = &before[prefix_start..];
    if prefix.chars().next().map_or(false, |c| !is_ident_start(c)) {
        return None;
    }
    let rest = &before[..prefix_start];
    let qualifier = match rest.strip_suffix('.') {
        None => None,
        Some(q) => Some(trailing_name(q)?.to_string()),
    };
    Some(SqlCompletion { qualifier, prefix: prefix.to_string() })
}

/// The dotted name `text` ends with (`t`, `DB.SCH`, `"My T"`), or None when
/// it doesn't end with one (`1.`).
fn trailing_name(text: &str) -> Option<&str> {
    let b = text.as_bytes();
    let mut i = text.len();
    loop {
        if i > 0 && b[i - 1] == b'"' {
            // A quoted part: back to its opening quote ("" inside is a quote).
            let mut j = i - 1;
            loop {
                if j == 0 {
                    return None;
                }
                j -= 1;
                if b[j] == b'"' {
                    if j > 0 && b[j - 1] == b'"' {
                        j -= 1;
                        continue;
                    }
                    break;
                }
            }
            i = j;
        } else {
            let start = text[..i]
                .char_indices()
                .rev()
                .take_while(|&(_, c)| is_ident_char(c))
                .last()
                .map_or(i, |(k, _)| k);
            if start == i || !text[start..].starts_with(is_ident_start) {
                return None;
            }
            i = start;
        }
        if i > 0 && b[i - 1] == b'.' {
            i -= 1;
            continue;
        }
        return Some(&text[i..]);
    }
}

/// Names from this session's queries: every table, alias, column and other
/// name in the text of a query that ran, plus its result's column names.
/// Snowflake's stock words are left out (they are offered anyway). Lives in
/// memory only, for as long as sage runs.
pub struct SessionWords {
    /// Every name to offer. An unquoted name is kept as written and in upper,
    /// lower and title case (Snowflake reads them all the same); a quoted one
    /// only as written, since its case is part of the name.
    words: std::collections::BTreeSet<String>,
    /// Dotted names (`DB.SCH.TABLE`, `T.COL`), upper-case key to text, for
    /// completing the part after a dot.
    dotted: BTreeMap<String, String>,
    /// Leave out Snowflake's stock words (offered on their own alongside).
    skip_stock: bool,
}

impl Default for SessionWords {
    /// The Snowflake session list, which leaves out Snowflake's stock words.
    fn default() -> Self {
        SessionWords { words: Default::default(), dotted: BTreeMap::new(), skip_stock: true }
    }
}

/// The part of a name matched against what's typed: a quoted name without
/// its opening quote, so `na` finds `"name"`.
fn match_text(word: &str) -> &str {
    word.strip_prefix('"').unwrap_or(word)
}

/// A name as it is offered: a quoted name as written; an unquoted one as
/// written and in each case form.
fn offered_forms(word: &str) -> Vec<String> {
    if word.starts_with('"') {
        return vec![word.to_string()];
    }
    let mut forms = vec![word.to_string()];
    for form in case_forms(word) {
        if !forms.contains(&form) {
            forms.push(form);
        }
    }
    forms
}

impl SessionWords {
    /// A list that keeps every name, stock word or not (a database's own
    /// tables, columns and functions, reported by the Python kernel).
    pub fn keep_all() -> Self {
        SessionWords { skip_stock: false, ..Default::default() }
    }

    fn add_word(&mut self, word: String) {
        if word.is_empty() || (self.skip_stock && !word.starts_with('"') && is_stock_word(&word)) {
            return;
        }
        self.words.extend(offered_forms(&word));
    }

    /// Add a known name, dotted or not (`orders`, `orders.amount`). A part
    /// that isn't a plain identifier (`fare class`) is offered in quotes.
    pub fn add_name(&mut self, name: &str) {
        let parts: Vec<String> = name
            .split('.')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let plain = p.chars().next().map_or(false, is_ident_start) && p.chars().all(is_ident_char);
                if plain { p.to_string() } else { format!("\"{}\"", p.replace('"', "\"\"")) }
            })
            .collect();
        if parts.len() > 1 {
            let full = parts.join(".");
            self.dotted.entry(full.to_ascii_uppercase()).or_insert(full);
        }
        for part in parts {
            self.add_word(part);
        }
    }

    /// Remember the names in a query that ran, and its result's column names.
    pub fn add_query(&mut self, sql: &str, result_columns: &[String]) {
        let mut names = Vec::new();
        scan(sql, |parts| names.push(parts));
        for parts in names {
            let texts: Vec<String> = parts
                .into_iter()
                .map(|p| match p {
                    Part::Plain(s) => s,
                    Part::Quoted(s) => quoted_name(&s),
                })
                .collect();
            if texts.len() > 1 {
                let full = texts.join(".");
                self.dotted.entry(full.to_ascii_uppercase()).or_insert(full);
            }
            for t in texts {
                self.add_word(t);
            }
        }
        for name in result_columns {
            // An unnamed expression's column (`COUNT(*)`) isn't a name to type.
            if name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == ' ') {
                self.add_word(quoted_name(name));
            }
        }
    }

    /// Names to offer for `prefix`, matched case-sensitively. After a dot, the
    /// parts that follow `qualifier` in dotted names seen come first (the
    /// qualifier itself matches in any case, as Snowflake reads it), then
    /// every session name (the qualifier may be an alias, whose columns
    /// aren't known). Without a dot, session names. Each list is sorted, with
    /// a word equal to what's typed first.
    pub fn matches(&self, qualifier: Option<&str>, prefix: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        if let Some(q) = qualifier {
            let head = format!("{}.", q.to_ascii_uppercase());
            let mut next: Vec<String> = self
                .dotted
                .iter()
                .filter(|(k, _)| k.starts_with(&head))
                .filter_map(|(_, text)| text[head.len()..].split('.').next())
                .flat_map(|part| offered_forms(part))
                .filter(|part| match_text(part).starts_with(prefix))
                .collect();
            sort_matches(&mut next, prefix);
            for part in next {
                if seen.insert(part.clone()) {
                    out.push(part);
                }
            }
        }
        let mut words: Vec<String> = self
            .words
            .iter()
            .filter(|w| match_text(w).starts_with(prefix))
            .cloned()
            .collect();
        sort_matches(&mut words, prefix);
        for w in words {
            if seen.insert(w.clone()) {
                out.push(w);
            }
        }
        out
    }

    #[cfg(test)]
    fn has(&self, word: &str) -> bool {
        self.words.contains(word)
    }
}

/// Sorted, with a word equal to what's typed first.
fn sort_matches(words: &mut [String], typed: &str) {
    words.sort_by(|a, b| {
        (match_text(a) != typed, match_text(a)).cmp(&(match_text(b) != typed, match_text(b)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(q: Option<&str>, p: &str) -> Option<SqlCompletion> {
        Some(SqlCompletion { qualifier: q.map(str::to_string), prefix: p.to_string() })
    }

    #[test]
    fn stock_words_come_in_three_cases_and_match_case_sensitively() {
        assert_eq!(stock_matches("SELE"), vec!["SELECT"]);
        assert_eq!(stock_matches("sele"), vec!["select"]);
        assert_eq!(stock_matches("Sele"), vec!["Select"]);
        assert!(stock_matches("sElE").is_empty());
        assert!(stock_matches("Date_T").contains(&"Date_Trunc".to_string()));
        assert!(stock_matches("date_t").contains(&"date_trunc".to_string()));
        assert!(!stock_matches("date_t").contains(&"DATE_TRUNC".to_string()));
        assert!(stock_matches("System$Cancel").contains(&"System$Cancel_Query".to_string()));
        assert!(stock_matches("FLAT").contains(&"FLATTEN".to_string()));
        assert!(is_stock_word("qualify") && is_stock_word("IFF") && !is_stock_word("PAX_COUNT"));
        assert_eq!(case_forms("H3_CELL_TO_PARENT"), vec!["H3_CELL_TO_PARENT", "h3_cell_to_parent", "H3_Cell_To_Parent"]);
        assert_eq!(case_forms("X1"), vec!["X1", "x1"]);
    }

    #[test]
    fn a_query_leaves_its_tables_aliases_and_columns() {
        let mut s = SessionWords::default();
        s.add_query(
            "SELECT sc.PAX_COUNT, sc.\"Fare Class\" AS fare_cls, 'NOT_A_NAME' AS lit -- COMMENTED\n\
             FROM DM_FPS_PRD.PRIV.SUPER_COUPONS sc /* BLOCKED */ JOIN dim d ON d.id = sc.dim_id\n\
             WHERE sc.FLIGHTDATE >= $start_date AND x = 10 AND sc.FareBasis IS NOT NULL",
            &["PAX_COUNT".to_string(), "FARE_CLS".to_string(), "COUNT(*)".to_string(), "name".to_string()],
        );
        for w in ["sc", "SC", "Sc", "PAX_COUNT", "pax_count", "Pax_Count", "\"Fare Class\"",
                  "fare_cls", "FARE_CLS", "lit", "DM_FPS_PRD", "PRIV", "SUPER_COUPONS", "dim", "d",
                  "id", "dim_id", "FLIGHTDATE", "Flightdate", "x", "\"name\"", "FareBasis",
                  "FAREBASIS", "farebasis"] {
            assert!(s.has(w), "missing {w}");
        }
        for w in ["NOT_A_NAME", "COMMENTED", "BLOCKED", "start_date", "SELECT", "COUNT(*)",
                  "\"fare class\"", "name", "NAME"] {
            assert!(!s.has(w), "unexpected {w}");
        }
    }

    #[test]
    fn names_are_offered_after_a_dot_and_without_one() {
        let mut s = SessionWords::default();
        s.add_query("SELECT * FROM DM_FPS_PRD.PRIV.SUPER_COUPONS sc WHERE sc.PAX_COUNT > 0", &[
            "PAX_COUNT".into(),
            "PAX_TYPE".into(),
        ]);
        assert_eq!(s.matches(Some("dm_fps_prd.priv"), "SU"), vec!["SUPER_COUPONS"]);
        assert_eq!(s.matches(Some("dm_fps_prd.priv"), "sup"), vec!["super_coupons"]);
        assert_eq!(s.matches(Some("SC"), "pax"), vec!["pax_count", "pax_type"]);
        assert_eq!(s.matches(Some("sc"), "PAX"), vec!["PAX_COUNT", "PAX_TYPE"]);
        assert_eq!(s.matches(None, "Sup"), vec!["Super_Coupons"]);
        assert_eq!(s.matches(None, "sc"), vec!["sc"]); // an exact match stays first
        assert!(s.matches(None, "sUP").is_empty());
    }

    #[test]
    fn a_databases_own_names_are_kept_whole() {
        let mut s = SessionWords::keep_all();
        for n in ["orders", "orders.amount", "orders.fare class", "count", "date_trunc"] {
            s.add_name(n);
        }
        assert_eq!(s.matches(Some("orders"), "am"), vec!["amount"]);
        assert!(s.has("\"fare class\"") && s.has("count") && s.has("COUNT") && s.has("Date_Trunc"));
        assert_eq!(generic_matches("Sel"), vec!["Select"]);
        assert!(generic_matches("QUAL").is_empty()); // Snowflake's own words stay out of other SQL
    }

    #[test]
    fn the_name_being_typed_is_found_outside_strings_and_comments() {
        assert_eq!(completion_context("SELECT pa"), ctx(None, "pa"));
        assert_eq!(completion_context("SELECT sc.pa"), ctx(Some("sc"), "pa"));
        assert_eq!(completion_context("FROM DB.SCH."), ctx(Some("DB.SCH"), ""));
        assert_eq!(completion_context("SELECT \"My T\".co"), ctx(Some("\"My T\""), "co"));
        assert_eq!(completion_context("SELECT "), ctx(None, ""));
        assert_eq!(completion_context("WHERE x = 'pa"), None);
        assert_eq!(completion_context("-- pa"), None);
        assert_eq!(completion_context("/* pa"), None);
        assert_eq!(completion_context("SELECT 1.5"), None);
        assert_eq!(completion_context("SELECT 'a' || pa"), ctx(None, "pa"));
        assert_eq!(completion_context("-- note\nSELECT pa"), ctx(None, "pa"));
    }
}
