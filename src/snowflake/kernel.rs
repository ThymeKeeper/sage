use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::connector::{SnowflakeClient as SfClient, SnowflakeSession};
use tempfile::NamedTempFile;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use crate::kernel::{
    CancelHandle, CompletionItem, ExecutionOutput, ExecutionResult, Kernel, KernelInfo,
    SqlMetadata, TypeRelationships,
};

use super::client::{ColumnMeta, SnowflakeClient};
use super::config::SnowflakeConfig;

/// Maximum rows rendered inline in the output-pane table — a quick peek only.
/// Anything beyond this lives in the spool tempfile: open the first 10k in the
/// spreadsheet viewer with F8, or export the full result with F9. Keeps the
/// in-memory output `String` bounded regardless of result size.
const PREVIEW_ROW_LIMIT: usize = 10;

/// Per-execution state shared between the executing thread and the cancel
/// handle. Wrapped in `Arc<Mutex<...>>` so the host can short-circuit the
/// in-flight execute() without taking ownership of the kernel.
struct QueryState {
    /// Fresh token per execute(); cancelling it makes execute() return at once
    /// while the abort (SYSTEM$CANCEL_ALL_QUERIES) fires on a detached thread.
    cancel_token: CancellationToken,
}

/// Snowflake kernel that talks the SQL API v2 directly over HTTP. Stateless
/// from Snowflake's perspective (PAT-authenticated bearer requests), so
/// `disconnect` is a no-op and there's no session token to refresh.
pub struct SnowflakeKernel {
    info: KernelInfo,
    config: SnowflakeConfig,
    client: Option<SnowflakeClient>,
    rt: Arc<Runtime>,
    state: Arc<Mutex<QueryState>>,
    /// Sequence counter shown in cell prompts; mirrors DirectKernel's counter
    /// so output_pane numbering is consistent across kernels.
    execution_count: usize,
    /// CSV tempfile holding the full row set of the most recent successful
    /// execution. Wrapped in NamedTempFile so the file is RAII-cleaned when
    /// rotated or when the kernel drops. Exposed via `latest_result_file()`
    /// for the save-results command.
    last_result_file: Option<NamedTempFile>,
}

impl SnowflakeKernel {
    pub fn new(config: SnowflakeConfig) -> Result<Self, Box<dyn Error>> {
        // Multi-thread runtime is required: execute() block_ons the query on the
        // background execution thread, while cancel() block_ons the abort call
        // from a different OS thread. A current-thread runtime can't do both,
        // and the connector spawns its own tasks for chunk downloads.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let display_name = format!("Snowflake [{}] {}", config.account, config.user);
        let info = KernelInfo {
            name: "snowflake".to_string(),
            display_name,
            // Reused field; not a Python interpreter path. Empty so any code
            // that filters by extension/existence ignores it.
            python_path: String::new(),
        };
        Ok(Self {
            info,
            config,
            client: None,
            rt: Arc::new(rt),
            state: Arc::new(Mutex::new(QueryState {
                cancel_token: CancellationToken::new(),
            })),
            execution_count: 0,
            last_result_file: None,
        })
    }

    /// Rebuild the session after expiry. Best-effort: on failure the client is
    /// left as-is and the next execute() will surface the error again.
    fn reconnect_session(&mut self) -> Result<(), Box<dyn Error>> {
        let rt = self.rt.clone();
        if let Some(client) = self.client.as_mut() {
            rt.block_on(client.reconnect())?;
        }
        Ok(())
    }
}

/// Result of one streamed execute(): the preview rows kept in memory for the
/// output pane, the column headers (absent only for a zero-row result, where
/// the connector exposes no metadata), the total row count, and the spool
/// tempfile holding the full result set (if any rows were produced).
struct ExecuteOutcome {
    preview: Vec<Vec<serde_json::Value>>,
    columns: Option<Vec<ColumnMeta>>,
    total_rows: u64,
    spool: Option<NamedTempFile>,
    /// Snowflake query id of this statement, shown in the footer so the user
    /// can reattach to it later via RESULT_SCAN.
    query_id: Option<String>,
}

/// Append a partition's rows to the spool CSV and capture the first
/// `preview_limit` total rows in `preview`. The partition's row Vec is
/// borrowed; the caller drops it after this call so memory doesn't grow.
///
/// `kinds` carries each column's temporal classification so DATE/TIME/TIMESTAMP
/// cells — which Snowflake encodes as raw epoch numbers — are decoded to ISO
/// 8601 once here, feeding both the CSV spool and the in-memory preview so the
/// exported file and the output-pane table always agree.
fn write_partition_and_capture_preview(
    file: &mut std::fs::File,
    rows: &[Vec<serde_json::Value>],
    kinds: &[TemporalKind],
    preview: &mut Vec<Vec<serde_json::Value>>,
    preview_limit: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::io::Write as _;
    let mut line = String::new();
    for row in rows {
        line.clear();
        let capture = preview.len() < preview_limit;
        let mut display_row: Vec<serde_json::Value> =
            if capture { Vec::with_capacity(row.len()) } else { Vec::new() };
        for (i, cell) in row.iter().enumerate() {
            let kind = kinds.get(i).copied().unwrap_or(TemporalKind::None);
            let display = cell_to_display_string(cell, kind);
            if i > 0 {
                line.push(',');
            }
            // Encode a SQL null as an unquoted-empty field (the crate::dsv
            // convention) so the CSV/TSV viewer shows it as ∅; non-null cells
            // write their display string, with empty strings quoted as "".
            let field = if cell.is_null() { None } else { Some(display.as_str()) };
            crate::dsv::serialize_field(&mut line, field, b',');
            if capture {
                // Re-wrap the decoded string for the preview the table renderer
                // consumes (nulls already rendered as ∅ by cell_to_string).
                display_row.push(serde_json::Value::String(display));
            }
        }
        line.push('\n');
        file.write_all(line.as_bytes())?;
        if capture {
            preview.push(display_row);
        }
    }
    Ok(())
}

/// Run one statement on the session and stream its result set into a spool CSV,
/// capturing the first `preview_limit` rows for the output pane. Chunks are
/// fetched and written one at a time, so memory peaks at one chunk plus the
/// preview regardless of total result size. Column headers are taken from the
/// first row (the connector's only metadata source), so a zero-row result
/// yields `columns: None` and no spool.
async fn run_query(
    session: &SnowflakeSession,
    code: &str,
    preview_limit: usize,
) -> Result<ExecuteOutcome, Box<dyn Error + Send + Sync>> {
    let executor = session.execute(code).await?;
    let query_id = executor.query_id().map(|s| s.to_string());

    let mut preview: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut columns: Option<Vec<ColumnMeta>> = None;
    let mut kinds: Vec<TemporalKind> = Vec::new();
    let mut spool: Option<NamedTempFile> = None;
    let mut total_rows: u64 = 0;

    while let Some(batch) = executor.fetch_next_chunk().await? {
        if batch.is_empty() {
            continue;
        }
        // First non-empty chunk: derive columns, classify temporal types once,
        // and open the spool with its header row.
        if columns.is_none() {
            let cols = super::client::columns_of(&batch[0]);
            kinds = cols.iter().map(|c| temporal_kind(&c.type_name)).collect();
            let mut tmp = tempfile::Builder::new()
                .prefix("sage-snowflake-")
                .suffix(".csv")
                .tempfile()?;
            {
                use std::io::Write as _;
                let mut line = String::new();
                for (i, c) in cols.iter().enumerate() {
                    if i > 0 {
                        line.push(',');
                    }
                    crate::dsv::serialize_field(&mut line, Some(&c.name), b',');
                }
                line.push('\n');
                tmp.as_file_mut().write_all(line.as_bytes())?;
            }
            spool = Some(tmp);
            columns = Some(cols);
        }

        let ncols = columns.as_ref().map(|c| c.len()).unwrap_or(0);
        let rows: Vec<Vec<serde_json::Value>> = batch
            .iter()
            .map(|r| super::client::row_to_values(r, ncols))
            .collect();
        total_rows += rows.len() as u64;
        write_partition_and_capture_preview(
            spool.as_mut().unwrap().as_file_mut(),
            &rows,
            &kinds,
            &mut preview,
            preview_limit,
        )?;
    }

    Ok(ExecuteOutcome {
        preview,
        columns,
        total_rows,
        spool,
        query_id,
    })
}

impl Kernel for SnowflakeKernel {
    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        if self.client.is_some() {
            return Ok(());
        }
        // create_session() is async; block_on it on the kernel's runtime. Clone
        // config/rt first so neither borrow of self outlives the assignment.
        let config = self.config.clone();
        let rt = self.rt.clone();
        let client = rt.block_on(SnowflakeClient::connect(&config))?;
        self.client = Some(client);
        Ok(())
    }

    fn execute(&mut self, code: &str) -> Result<ExecutionResult, Box<dyn Error>> {
        let session = self
            .client
            .as_ref()
            .ok_or("Snowflake kernel not connected")?
            .session();

        // Fresh cancel token for this statement. Cancelling it short-circuits
        // the select! below so execute() returns immediately; the cancel handle
        // separately aborts the query server-side.
        let token = {
            let mut st = self.state.lock().unwrap();
            st.cancel_token = CancellationToken::new();
            st.cancel_token.clone()
        };

        // Drop any previous spool eagerly so we don't briefly hold two on disk.
        self.last_result_file = None;

        self.execution_count += 1;
        let rt = self.rt.clone();
        let code = code.to_string();

        // Run the statement, racing it against cancellation. The query streams
        // chunk-by-chunk into the spool (see run_query), so memory peaks at one
        // chunk + the preview rows regardless of result size.
        let outcome: Result<ExecuteOutcome, Box<dyn Error + Send + Sync>> =
            rt.block_on(async move {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => Err("cancelled by user".into()),
                    res = run_query(&session, &code, PREVIEW_ROW_LIMIT) => res,
                }
            });

        let outputs = match outcome {
            Ok(out) => {
                if let Some(tmp) = out.spool {
                    self.last_result_file = Some(tmp);
                }
                format_outcome(
                    out.columns.as_deref(),
                    &out.preview,
                    out.total_rows,
                    out.query_id.as_deref(),
                )
            }
            Err(e) => {
                let msg = e.to_string();
                let lower = msg.to_ascii_lowercase();
                // Token renewal (the heartbeat plus reactive renewal on 390112)
                // normally keeps the session alive. If an auth-expiry still
                // surfaces here — session token (390112) or master token
                // (390114), i.e. renewal has lapsed — rebuild the session so the
                // next statement works and tell the user their session-local
                // state was reset.
                // A QueryInterrupted error ("…may still be running…") must keep
                // its reattach hint rather than be turned into a reconnect.
                let expired = !lower.contains("may still be running")
                    && (lower.contains("session expired")
                        || lower.contains("authentication token has expired")
                        || lower.contains("390114"));
                if expired {
                    let _ = self.reconnect_session();
                    vec![ExecutionOutput::Error {
                        ename: "SnowflakeError".to_string(),
                        evalue: "Session expired and was reconnected — session-local \
                                 state (temporary tables, variables) was reset. Re-run \
                                 any setup statements."
                            .to_string(),
                        traceback: vec![],
                    }]
                } else {
                    vec![ExecutionOutput::Error {
                        ename: "SnowflakeError".to_string(),
                        evalue: msg,
                        traceback: vec![],
                    }]
                }
            }
        };

        let success = !outputs
            .iter()
            .any(|o| matches!(o, ExecutionOutput::Error { .. }));

        Ok(ExecutionResult {
            outputs,
            execution_count: Some(self.execution_count),
            success,
            completions: Vec::<CompletionItem>::new(),
            type_relationships: TypeRelationships::default(),
            sql_metadata: SqlMetadata::default(),
        })
    }

    fn disconnect(&mut self) -> Result<(), Box<dyn Error>> {
        // Dropping the wrapper drops the session; Snowflake reclaims it on its
        // idle timeout (the connector exposes no explicit logout). The next
        // connect() logs in fresh. Session-local state is intentionally gone.
        self.client = None;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    fn info(&self) -> KernelInfo {
        self.info.clone()
    }

    fn cancel_handle(&self) -> Option<Arc<dyn CancelHandle>> {
        let client = self.client.as_ref()?;
        let handle: Arc<dyn CancelHandle> = Arc::new(SnowflakeCancelHandle {
            control: client.control_client(),
            session_id: client.session_id().to_string(),
            rt: self.rt.clone(),
            state: self.state.clone(),
        });
        Some(handle)
    }

    fn cancel_preserves_session(&self) -> bool {
        // Cancellation aborts the running query via SYSTEM$CANCEL_ALL_QUERIES
        // and short-circuits the execute() select!; the login session, HTTP
        // client, and runtime all stay alive, so the kernel is reusable.
        true
    }

    fn latest_result_file(&self) -> Option<PathBuf> {
        self.last_result_file.as_ref().map(|t| t.path().to_path_buf())
    }
}

/// Cancels the in-flight statement. The cancel token short-circuits execute()
/// immediately (host UI returns to responsive); the server-side abort runs
/// `SYSTEM$CANCEL_ALL_QUERIES(<session_id>)` from a throwaway control session on
/// a detached OS thread, so the cancel call never blocks the UI. The function
/// is session-scoped — it only touches queries in this kernel's session, not
/// other sessions under the same user.
struct SnowflakeCancelHandle {
    control: SfClient,
    session_id: String,
    rt: Arc<Runtime>,
    state: Arc<Mutex<QueryState>>,
}

impl CancelHandle for SnowflakeCancelHandle {
    fn cancel(&self) {
        self.state.lock().unwrap().cancel_token.cancel();
        let control = self.control.clone();
        let session_id = self.session_id.clone();
        let rt = self.rt.clone();
        std::thread::spawn(move || {
            rt.block_on(async move {
                if let Ok(session) = control.create_session().await {
                    let _ = session
                        .query(format!("SELECT SYSTEM$CANCEL_ALL_QUERIES({session_id})"))
                        .await;
                }
            });
        });
    }
}

/// Render an ExecuteOutcome as ExecutionOutput entries. For row results, builds
/// a DuckDB-style table from the in-memory preview and notes how many more rows
/// are on disk. DDL/DML come back as a one-row status result and render as a
/// small table; a genuinely empty result set has no columns to show.
fn format_outcome(
    columns: Option<&[ColumnMeta]>,
    preview: &[Vec<serde_json::Value>],
    total_rows: u64,
    query_id: Option<&str>,
) -> Vec<ExecutionOutput> {
    let id = query_id_suffix(query_id);
    let columns = match columns {
        Some(c) if !c.is_empty() => c,
        // Zero-row results expose no column metadata via the connector.
        _ => return vec![ExecutionOutput::Stdout(format!("(0 rows){id}\n"))],
    };

    let table = render_table(columns, preview);
    let shown = preview.len() as u64;
    let count = if shown < total_rows {
        format!(
            "(showing first {} of {} rows — F8 to open in viewer, F9 to export)",
            shown, total_rows
        )
    } else {
        format!("({} row{})", total_rows, if total_rows == 1 { "" } else { "s" })
    };
    vec![ExecutionOutput::Result(format!("{table}\n{count}{id}\n"))]
}

/// A trailing " · query <id>" note for the result footer, enabling reattach via
/// `SELECT * FROM TABLE(RESULT_SCAN('<id>'))`. Plain text (no ANSI) so it
/// renders the same in any output kind.
fn query_id_suffix(query_id: Option<&str>) -> String {
    match query_id {
        Some(id) if !id.is_empty() => format!("  ·  query {id}"),
        _ => String::new(),
    }
}

// Unicode box-drawing characters for the DuckDB-style table renderer.
// `─│` = thin horizontal/vertical, `┌┐└┘` = corners,
// `├┤┬┴` = T-junctions, `┼` = cross.
const BOX_H: char = '─';
const BOX_V: char = '│';
const BOX_TL: char = '┌';
const BOX_TR: char = '┐';
const BOX_BL: char = '└';
const BOX_BR: char = '┘';
const BOX_LT: char = '├';
const BOX_RT: char = '┤';
const BOX_TT: char = '┬';
const BOX_BT: char = '┴';
const BOX_X: char = '┼';

/// ANSI used to dim the type-name header and null (∅) cells: an explicit muted
/// grey foreground (256-color 244), closed with SGR 39 (default foreground) so
/// the surrounding background is left intact. An explicit colour rather than
/// SGR 2 (faint) because faint isn't rendered by every terminal (e.g. it shows
/// undimmed in the output pane), whereas 256-colour is.
const DIM: (&str, &str) = ("\x1b[38;5;244m", "\x1b[39m");

/// Glyph displayed for a SQL null (distinct from an empty string). Rendered
/// dimly (see DIM) in the output-pane table.
const NULL_SENTINEL: &str = "∅";

/// Render result rows as a DuckDB-style boxed table. Two header rows: the
/// column names on top, the Snowflake type names dimmed below (e.g. TEXT,
/// FIXED, REAL, TIMESTAMP_NTZ). Numeric columns are right-aligned (detected
/// by type name); everything else left-aligned. Cells truncated at
/// `MAX_CELL` with `…`.
fn render_table(
    columns: &[ColumnMeta],
    rows: &[Vec<serde_json::Value>],
) -> String {
    use unicode_width::UnicodeWidthStr;
    const MAX_CELL: usize = 40;

    let aligns: Vec<Align> = columns.iter().map(|c| align_for(&c.type_name)).collect();
    let type_labels: Vec<String> = columns.iter().map(|c| short_type(&c.type_name)).collect();
    // Quoted identifiers can contain newlines too, so flatten names like cells.
    let names: Vec<String> = columns.iter().map(|c| flatten_control(&c.name)).collect();

    // Width per column is the max of: column name, type label, and any
    // value's display width — capped at MAX_CELL.
    let mut widths: Vec<usize> = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            UnicodeWidthStr::width(name.as_str())
                .max(UnicodeWidthStr::width(type_labels[i].as_str()))
                .min(MAX_CELL)
        })
        .collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i >= widths.len() {
                break;
            }
            let s = flatten_control(&cell_to_string(cell));
            let w = UnicodeWidthStr::width(s.as_str()).min(MAX_CELL);
            if w > widths[i] {
                widths[i] = w;
            }
        }
    }

    let mut out = String::new();
    push_border(&mut out, &widths, BOX_TL, BOX_TT, BOX_TR);
    // Column-name row: always left-aligned so headers don't drift around
    // when paired with right-aligned numeric values below.
    push_row(
        &mut out,
        names.iter().map(|s| s.as_str()),
        &widths,
        &vec![Align::Left; widths.len()],
        |_| None,
    );
    // Type row: dimmed to a muted grey (see DIM). The output pane preserves
    // these escapes when it renders the result text.
    push_row(
        &mut out,
        type_labels.iter().map(|s| s.as_str()),
        &widths,
        &vec![Align::Left; widths.len()],
        |_| Some(DIM),
    );
    push_border(&mut out, &widths, BOX_LT, BOX_X, BOX_RT);
    for row in rows {
        let cells: Vec<String> = (0..widths.len())
            .map(|i| {
                row.get(i)
                    .map(|v| flatten_control(&cell_to_string(v)))
                    .unwrap_or_else(|| NULL_SENTINEL.to_string())
            })
            .collect();
        // Dim by rendered glyph, not JSON type: the preview feeds us nulls as
        // the already-decoded display string (cell_to_string turned Value::Null
        // into the sentinel upstream), so there's no Value::Null left to test.
        push_row(
            &mut out,
            cells.iter().map(|s| s.as_str()),
            &widths,
            &aligns,
            |i| {
                if cells.get(i).map(|s| s == NULL_SENTINEL).unwrap_or(false) {
                    Some(DIM)
                } else {
                    None
                }
            },
        );
    }
    push_border(&mut out, &widths, BOX_BL, BOX_BT, BOX_BR);
    out
}

/// Snowflake sometimes returns long type strings (e.g. `TIMESTAMP_NTZ`). For
/// the type header we mostly pass through, but normalize a couple of the
/// less-readable ones to match what users expect to see.
fn short_type(type_name: &str) -> String {
    match type_name.to_uppercase().as_str() {
        "FIXED" => "NUMBER".to_string(),
        "REAL" => "FLOAT".to_string(),
        other => other.to_string(),
    }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Right,
}

fn align_for(type_name: &str) -> Align {
    // Snowflake numeric types (case-insensitive prefix match handles
    // variants like NUMBER(38,0), DECIMAL(10,2), etc.).
    let upper = type_name.to_uppercase();
    let numeric_prefixes = [
        "NUMBER", "DECIMAL", "NUMERIC", "INT", "INTEGER", "BIGINT", "SMALLINT",
        "TINYINT", "BYTEINT", "FIXED", "FLOAT", "DOUBLE", "REAL",
    ];
    if numeric_prefixes.iter().any(|p| upper.starts_with(p)) {
        Align::Right
    } else {
        Align::Left
    }
}

fn push_border(out: &mut String, widths: &[usize], left: char, mid: char, right: char) {
    out.push(left);
    for (i, &w) in widths.iter().enumerate() {
        for _ in 0..w + 2 {
            out.push(BOX_H);
        }
        if i + 1 < widths.len() {
            out.push(mid);
        } else {
            out.push(right);
        }
    }
    out.push('\n');
}

fn push_row<'a, I: IntoIterator<Item = &'a str>>(
    out: &mut String,
    cells: I,
    widths: &[usize],
    aligns: &[Align],
    ansi: impl Fn(usize) -> Option<(&'static str, &'static str)>,
) {
    use unicode_width::UnicodeWidthStr;
    out.push(BOX_V);
    for (i, (cell, &w)) in cells.into_iter().zip(widths.iter()).enumerate() {
        let cell_ansi = ansi(i);
        let truncated = truncate(cell, w);
        let cell_w = UnicodeWidthStr::width(truncated.as_str());
        let pad = w.saturating_sub(cell_w);
        out.push(' ');
        if let Some((open, _)) = cell_ansi {
            out.push_str(open);
        }
        match aligns.get(i).copied().unwrap_or(Align::Left) {
            Align::Left => {
                out.push_str(&truncated);
                for _ in 0..pad {
                    out.push(' ');
                }
            }
            Align::Right => {
                for _ in 0..pad {
                    out.push(' ');
                }
                out.push_str(&truncated);
            }
        }
        if let Some((_, close)) = cell_ansi {
            out.push_str(close);
        }
        out.push(' ');
        out.push(BOX_V);
    }
    out.push('\n');
}

/// Collapse newlines — and any other control characters — to single spaces so
/// a multi-line value can't break the table's one-line-per-row box layout
/// (CRLF becomes one space, not two). Display-only: the CSV spool and the F8
/// viewer keep the raw value.
fn flatten_control(s: &str) -> String {
    if !s.contains(|c: char| c.is_control()) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut prev_cr = false;
    for c in s.chars() {
        if prev_cr && c == '\n' {
            prev_cr = false;
            continue;
        }
        prev_cr = c == '\r';
        out.push(if c.is_control() { ' ' } else { c });
    }
    out
}

fn cell_to_string(v: &serde_json::Value) -> String {
    match v {
        // Display nulls as the ∅ sentinel (matches the CSV/TSV viewer). The CSV
        // spool encodes nulls structurally instead — see
        // write_partition_and_capture_preview — so this only affects rendering.
        serde_json::Value::Null => NULL_SENTINEL.to_string(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A column's temporal category, derived once from its Snowflake type name so
/// per-cell decoding doesn't re-parse the type string. `None` means "render as
/// usual" — the vast majority of columns.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TemporalKind {
    None,
    Date,
    Time,
    TimestampNtz,
    TimestampLtz,
    TimestampTz,
}

/// Map a Snowflake column type (case-insensitive) to its `TemporalKind`.
fn temporal_kind(type_name: &str) -> TemporalKind {
    match type_name.to_ascii_lowercase().as_str() {
        "date" => TemporalKind::Date,
        "time" => TemporalKind::Time,
        "timestamp_ntz" => TemporalKind::TimestampNtz,
        "timestamp_ltz" => TemporalKind::TimestampLtz,
        "timestamp_tz" => TemporalKind::TimestampTz,
        _ => TemporalKind::None,
    }
}

/// Display string for one result cell. Temporal cells (per `kind`) are decoded
/// from Snowflake's raw epoch encoding to ISO 8601; everything else — and any
/// temporal value that fails to parse — falls back to the plain rendering.
fn cell_to_display_string(cell: &serde_json::Value, kind: TemporalKind) -> String {
    if kind == TemporalKind::None {
        return cell_to_string(cell);
    }
    decode_temporal(cell, kind).unwrap_or_else(|| cell_to_string(cell))
}

/// Decode a Snowflake temporal cell into an ISO 8601-style string. Snowflake's
/// JSON result format encodes these as numeric strings: DATE as days since the
/// Unix epoch, TIME as fractional seconds since midnight, and the TIMESTAMP
/// family as fractional seconds since the epoch (TIMESTAMP_TZ additionally
/// carries a trailing minute offset). Timestamps separate the date and time
/// with a space rather than the canonical `T`: RFC 3339 permits it, and Excel
/// only recognizes the value as a datetime (not text) without the `T`. Returns
/// `None` for NULLs, non-numeric payloads, or values that don't parse, letting
/// the caller fall back to plain rendering.
fn decode_temporal(cell: &serde_json::Value, kind: TemporalKind) -> Option<String> {
    use chrono::{FixedOffset, NaiveTime, TimeZone, Timelike, Utc};

    // Normally a JSON string ("1761177600.000"); accept a bare number too.
    let raw: String = match cell {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => return None,
    };
    if raw.is_empty() {
        return None;
    }

    match kind {
        TemporalKind::None => None,
        TemporalKind::Date => raw
            .parse::<i64>()
            .ok()
            .and_then(decode_date_from_epoch)
            .map(|d| d.format("%Y-%m-%d").to_string()),
        TemporalKind::Time => parse_fractional_seconds(&raw)
            .and_then(|(secs, nanos)| {
                let secs = secs.rem_euclid(86_400) as u32;
                NaiveTime::from_num_seconds_from_midnight_opt(secs, nanos)
            })
            .map(|t| {
                format!(
                    "{}{}",
                    t.format("%H:%M:%S"),
                    format_fractional_nanos(t.nanosecond())
                )
            }),
        TemporalKind::TimestampTz => {
            // "<fractional seconds since epoch> <offset>", where the offset is
            // the timezone offset in minutes biased by +1440 (so 1440 == UTC).
            // Require exactly those two parts: the driver protocol can also emit
            // a single float with the offset baked in, which this decoder can't
            // interpret — fall back to the raw string rather than show a wrong
            // time.
            let parts: Vec<&str> = raw.split_whitespace().collect();
            if parts.len() != 2 {
                return None;
            }
            let ts = parts[0];
            let offset_minutes = parts[1].parse::<i64>().ok().map(|o| o - 1440)?;
            parse_fractional_seconds(ts).and_then(|(secs, nanos)| {
                FixedOffset::east_opt((offset_minutes * 60) as i32).and_then(|off| {
                    Utc.timestamp_opt(secs, nanos).single().map(|dt| {
                        let dt = dt.with_timezone(&off);
                        format!(
                            "{}{}{}",
                            dt.format("%Y-%m-%d %H:%M:%S"),
                            format_fractional_nanos(dt.nanosecond()),
                            dt.format("%:z")
                        )
                    })
                })
            })
        }
        // Both are fractional seconds since the epoch and render as a zoneless
        // date-time with no designator. (TIMESTAMP_LTZ is technically an
        // absolute instant, but the session timezone it should display in isn't
        // carried in the result metadata, so we render its UTC wall clock the
        // same way as TIMESTAMP_NTZ rather than mislabel the offset.)
        TemporalKind::TimestampNtz | TemporalKind::TimestampLtz => {
            parse_fractional_seconds(&raw)
                .and_then(|(secs, nanos)| Utc.timestamp_opt(secs, nanos).single())
                .map(|dt| {
                    let naive = dt.naive_utc();
                    format!(
                        "{}{}",
                        naive.format("%Y-%m-%d %H:%M:%S"),
                        format_fractional_nanos(naive.nanosecond())
                    )
                })
        }
    }
}

/// Decode a Snowflake DATE cell into a `NaiveDate`, detecting the epoch unit.
///
/// Snowflake encodes DATE as a whole number of days since the Unix epoch, and
/// that's what every well-formed cell carries. Occasionally a column surfaces a
/// value in a finer unit instead — seconds, milliseconds, microseconds, or
/// nanoseconds since the epoch — e.g. data loaded through a path that preserved
/// a timestamp's resolution. Because a date has no intra-day component, such a
/// value is always an exact multiple of that unit's count-per-day, so the day
/// count is recoverable without guessing.
///
/// The plain days reading is tried first, so any value already inside
/// Snowflake's DATE domain (0001-01-01 ..= 9999-12-31) is taken verbatim and
/// well-formed dates are never second-guessed. Only when days is out of that
/// domain do we test the finer units, coarsest first; the in-range check on
/// each candidate makes the choice unambiguous for any realistic date. Returns
/// `None` when no unit yields an in-range date, letting the caller fall back to
/// raw text instead of crashing.
fn decode_date_from_epoch(value: i64) -> Option<chrono::NaiveDate> {
    use chrono::{Duration, NaiveDate};

    // Snowflake's DATE domain as days since the epoch: 0001-01-01 ..= 9999-12-31.
    const MIN_DAYS: i64 = -719_162;
    const MAX_DAYS: i64 = 2_932_896;

    let from_days = |days: i64| -> Option<NaiveDate> {
        if !(MIN_DAYS..=MAX_DAYS).contains(&days) {
            return None;
        }
        // Bounded above, so `try_days` can't overflow — but it keeps the path
        // panic-free regardless, unlike the `Duration::days` it replaces.
        NaiveDate::from_ymd_opt(1970, 1, 1)?.checked_add_signed(Duration::try_days(days)?)
    };

    // Native encoding (days) first: every in-domain value is taken as-is.
    if let Some(date) = from_days(value) {
        return Some(date);
    }

    // Otherwise the value is in a finer unit. A date divides evenly by the
    // unit's count-per-day; the quotient is the day count. Coarsest first — the
    // in-range check rejects a too-coarse guess (its quotient overshoots the
    // domain), so the first hit is the right unit for any realistic date.
    const UNITS_PER_DAY: [i64; 4] = [
        86_400,             // seconds
        86_400_000,         // milliseconds
        86_400_000_000,     // microseconds
        86_400_000_000_000, // nanoseconds
    ];
    UNITS_PER_DAY.into_iter().find_map(|per_day| {
        if value % per_day == 0 {
            from_days(value / per_day)
        } else {
            None
        }
    })
}

/// Parse a decimal "seconds[.fraction]" string into whole seconds plus a
/// nanosecond remainder in [0, 1_000_000_000), handling negative (pre-epoch)
/// values correctly.
fn parse_fractional_seconds(raw: &str) -> Option<(i64, u32)> {
    let raw = raw.trim();
    let negative = raw.starts_with('-');
    let body = raw.strip_prefix('-').unwrap_or(raw);
    let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
    let mut secs: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let mut frac = frac_part.to_string();
    if frac.len() > 9 {
        frac.truncate(9);
    }
    while frac.len() < 9 {
        frac.push('0');
    }
    let mut nanos: i64 = frac.parse().ok()?;
    if negative {
        if nanos > 0 {
            secs += 1;
            nanos = 1_000_000_000 - nanos;
        }
        secs = -secs;
    }
    Some((secs, nanos as u32))
}

/// Render a nanosecond remainder as a trimmed fractional-seconds suffix (e.g.
/// ".5", ".123"), or an empty string when there's no sub-second component.
fn format_fractional_nanos(nanos: u32) -> String {
    if nanos == 0 {
        return String::new();
    }
    let mut s = format!("{:09}", nanos);
    while s.ends_with('0') {
        s.pop();
    }
    format!(".{}", s)
}

/// Truncate a string to fit in `max` display columns. Appends `…` when
/// truncated. Operates on character boundaries (not bytes) so UTF-8 stays
/// valid.
/// Truncate `s` to at most `max` display columns, appending `…` when cut.
/// Budgeted in columns (not chars) so width-2 CJK/fullwidth content can't
/// overflow the computed column width and push the box borders out of line.
fn truncate(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return take_columns(s, max);
    }
    let mut t = take_columns(s, max - 1);
    t.push('…');
    t
}

/// The longest prefix of `s` that fits in `budget` display columns.
fn take_columns(s: &str, budget: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + w > budget {
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn disp(raw: &str, kind: TemporalKind) -> String {
        cell_to_display_string(&Value::String(raw.to_string()), kind)
    }

    #[test]
    fn classifies_types_case_insensitively() {
        assert!(matches!(temporal_kind("date"), TemporalKind::Date));
        assert!(matches!(temporal_kind("TIMESTAMP_NTZ"), TemporalKind::TimestampNtz));
        assert!(matches!(temporal_kind("Timestamp_Tz"), TemporalKind::TimestampTz));
        assert!(matches!(temporal_kind("TEXT"), TemporalKind::None));
        assert!(matches!(temporal_kind("NUMBER(38,0)"), TemporalKind::None));
    }

    #[test]
    fn decodes_date_from_epoch_days() {
        assert_eq!(disp("20384", TemporalKind::Date), "2025-10-23");
        assert_eq!(disp("0", TemporalKind::Date), "1970-01-01");
        assert_eq!(disp("-1", TemporalKind::Date), "1969-12-31");
    }

    #[test]
    fn decodes_date_from_finer_epoch_units() {
        // 2025-10-23 is 20384 days since the epoch. The same date arriving in a
        // finer unit is detected by exact divisibility and normalized to days.
        // (20384 * 86_400 = 1_761_177_600 seconds, and so on up the units.)
        assert_eq!(disp("20384", TemporalKind::Date), "2025-10-23"); // days
        assert_eq!(disp("1761177600", TemporalKind::Date), "2025-10-23"); // seconds
        assert_eq!(disp("1761177600000", TemporalKind::Date), "2025-10-23"); // millis
        assert_eq!(disp("1761177600000000", TemporalKind::Date), "2025-10-23"); // micros
        assert_eq!(disp("1761177600000000000", TemporalKind::Date), "2025-10-23"); // nanos
        // A pre-epoch date in millis resolves with the sign intact. 1950-01-01
        // is -7305 days, i.e. -631_152_000_000 ms; the seconds reading
        // (-7_305_000 days) overshoots the domain, so millis is chosen.
        assert_eq!(disp("-631152000000", TemporalKind::Date), "1950-01-01");
    }

    #[test]
    fn out_of_range_date_falls_back_instead_of_panicking() {
        // Values that aren't a valid date in days or any finer epoch unit must
        // fall back to raw text, never panic. Regression test for the
        // `TimeDelta::days out of bounds` panic on `Duration::days`.
        assert_eq!(disp("99999999999999", TemporalKind::Date), "99999999999999");
        assert_eq!(disp(&i64::MAX.to_string(), TemporalKind::Date), i64::MAX.to_string());
        assert_eq!(disp(&i64::MIN.to_string(), TemporalKind::Date), i64::MIN.to_string());
    }

    #[test]
    fn decodes_time_with_fractional_seconds() {
        assert_eq!(disp("0", TemporalKind::Time), "00:00:00");
        assert_eq!(disp("3661", TemporalKind::Time), "01:01:01");
        assert_eq!(disp("3661.5", TemporalKind::Time), "01:01:01.5");
    }

    #[test]
    fn decodes_timestamp_ntz_and_ltz() {
        // The exact epoch value from the bug report.
        assert_eq!(
            disp("1761177600.000", TemporalKind::TimestampNtz),
            "2025-10-23 00:00:00"
        );
        assert_eq!(
            disp("1761177600.500000000", TemporalKind::TimestampNtz),
            "2025-10-23 00:00:00.5"
        );
        // LTZ renders the same as NTZ (zoneless wall clock, no designator).
        assert_eq!(
            disp("1761177600.000", TemporalKind::TimestampLtz),
            "2025-10-23 00:00:00"
        );
    }

    #[test]
    fn decodes_timestamp_tz_with_offset() {
        // 1770 == +05:30 (1440 bias + 330 minutes).
        assert_eq!(
            disp("1761177600.000 1770", TemporalKind::TimestampTz),
            "2025-10-23 05:30:00+05:30"
        );
        // 1440 == UTC.
        assert_eq!(
            disp("1761177600.000 1440", TemporalKind::TimestampTz),
            "2025-10-23 00:00:00+00:00"
        );
    }

    #[test]
    fn decodes_pre_epoch_timestamp() {
        assert_eq!(
            disp("-1.5", TemporalKind::TimestampNtz),
            "1969-12-31 23:59:58.5"
        );
    }

    #[test]
    fn non_temporal_and_unparseable_pass_through() {
        assert_eq!(disp("hello", TemporalKind::None), "hello");
        // A temporal column whose cell can't be parsed keeps its raw text.
        assert_eq!(disp("not-a-number", TemporalKind::Date), "not-a-number");
        // NULL cells render as the ∅ sentinel.
        assert_eq!(
            cell_to_display_string(&Value::Null, TemporalKind::TimestampNtz),
            "∅"
        );
    }

    #[test]
    fn spool_encodes_null_distinctly_and_preview_shows_sentinel() {
        use std::io::{Read, Seek};
        let rows = vec![vec![
            Value::String("a".into()),
            Value::Null,
            Value::String(String::new()),
        ]];
        let kinds = vec![TemporalKind::None; 3];
        let mut preview: Vec<Vec<Value>> = Vec::new();
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write_partition_and_capture_preview(tmp.as_file_mut(), &rows, &kinds, &mut preview, 100)
            .unwrap();

        // On disk: null -> unquoted-empty (,,), empty string -> quoted "".
        let mut content = String::new();
        tmp.as_file_mut().seek(std::io::SeekFrom::Start(0)).unwrap();
        tmp.as_file_mut().read_to_string(&mut content).unwrap();
        assert_eq!(content, "a,,\"\"\n");

        // In the output-pane preview: null shows the ∅ sentinel, "" stays blank.
        assert_eq!(preview[0][0], Value::String("a".into()));
        assert_eq!(preview[0][1], Value::String("∅".into()));
        assert_eq!(preview[0][2], Value::String(String::new()));
    }

    #[test]
    fn render_table_dims_null_cells() {
        let columns = vec![ColumnMeta {
            name: "x".to_string(),
            type_name: "TEXT".to_string(),
        }];
        // The preview feeds render_table already-decoded display strings, so a
        // null arrives as the sentinel *string* (not Value::Null) — the case the
        // real flow hits. Include a literal Value::Null too for good measure.
        let rows = vec![
            vec![Value::String(NULL_SENTINEL.into())],
            vec![Value::Null],
            vec![Value::String("v".into())],
        ];
        let table = render_table(&columns, &rows);
        // The null sentinel is wrapped in the dim grey; the real value is not.
        assert!(table.contains("\x1b[38;5;244m∅"), "null cell should be dimmed: {table:?}");
        assert!(!table.contains("\x1b[38;5;244mv"), "value cell should not be dimmed");
    }

    #[test]
    fn render_table_flattens_newlines() {
        let columns = vec![ColumnMeta {
            // Quoted identifiers can carry newlines too.
            name: "col\nname".to_string(),
            type_name: "TEXT".to_string(),
        }];
        let rows = vec![
            vec![Value::String("line1\nline2".into())],
            vec![Value::String("a\r\nb\tc".into())],
        ];
        let table = render_table(&columns, &rows);
        assert!(table.contains("col name"), "header should flatten: {table:?}");
        assert!(table.contains("line1 line2"), "LF should become a space: {table:?}");
        assert!(table.contains("a b c"), "CRLF/tab should each become one space: {table:?}");
        // Every rendered line must still start and end on a box-drawing
        // character — an unflattened newline would split a row in two.
        for line in table.lines() {
            let first = line.chars().next().unwrap();
            let last = line.chars().last().unwrap();
            assert!([BOX_TL, BOX_LT, BOX_BL, BOX_V].contains(&first), "broken row: {line:?}");
            assert!([BOX_TR, BOX_RT, BOX_BR, BOX_V].contains(&last), "broken row: {line:?}");
        }
    }

    #[test]
    fn render_table_keeps_wide_cells_within_borders() {
        use unicode_width::UnicodeWidthStr;
        let columns = vec![ColumnMeta {
            name: "w".to_string(),
            type_name: "TEXT".to_string(),
        }];
        // 30 width-2 chars split across two lines: flattens to 61 display
        // columns, forcing a truncation that must land on a column budget,
        // not a char count.
        let rows = vec![vec![Value::String(format!(
            "{}\n{}",
            "好".repeat(15),
            "好".repeat(15)
        ))]];
        let table = render_table(&columns, &rows);
        // Every ANSI-free line (borders, header, data) renders to the same
        // display width; a char-counted truncation overflows the border.
        let border_w = UnicodeWidthStr::width(table.lines().next().unwrap());
        for line in table.lines().filter(|l| !l.contains('\x1b')) {
            assert_eq!(UnicodeWidthStr::width(line), border_w, "misaligned: {line:?}");
        }
    }

    #[test]
    fn truncate_budgets_by_display_width() {
        use unicode_width::UnicodeWidthStr;
        let wide = "好".repeat(21); // 42 display columns
        let t = truncate(&wide, 40);
        assert!(t.ends_with('…'), "over-wide value should be cut: {t:?}");
        assert!(UnicodeWidthStr::width(t.as_str()) <= 40);
        // Under the cap, values pass through untouched.
        assert_eq!(truncate("abc", 40), "abc");
        assert_eq!(truncate(&"好".repeat(20), 40), "好".repeat(20));
    }
}
