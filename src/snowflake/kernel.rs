use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tempfile::NamedTempFile;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use crate::kernel::{
    CancelHandle, CompletionItem, ExecutionOutput, ExecutionResult, Kernel, KernelInfo,
    SqlMetadata, TypeRelationships,
};

use super::client::{SnowflakeClient, StatusResponse};
use super::config::SnowflakeConfig;

/// Maximum rows rendered inline in the output pane. Anything beyond this is
/// only on disk in the spool tempfile — accessible via the save-results
/// keybinding. Keeps the in-memory output `String` bounded regardless of
/// result size.
const PREVIEW_ROW_LIMIT: usize = 100;

/// Per-execution state shared between the executing thread and the cancel
/// handle. Wrapped in `Arc<Mutex<...>>` so the host can read the current
/// `statementHandle` to abort it without taking ownership of the kernel.
struct QueryState {
    /// statementHandle (query_id) of the active query, set after submit returns
    /// and cleared when execute completes (success, error, or cancel).
    current_qid: Option<String>,
    /// Fresh token per execute(); cancelling it short-circuits the poll loop.
    cancel_token: CancellationToken,
}

/// Snowflake kernel that talks the SQL API v2 directly over HTTP. Stateless
/// from Snowflake's perspective (PAT-authenticated bearer requests), so
/// `disconnect` is a no-op and there's no session token to refresh.
pub struct SnowflakeKernel {
    info: KernelInfo,
    config: SnowflakeConfig,
    client: Option<Arc<SnowflakeClient>>,
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
        // Multi-thread runtime is required: execute() block_ons the poll loop
        // on the background execution thread, while cancel() block_ons the
        // abort call from a different OS thread. Current-thread runtime can't
        // satisfy both at once.
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
                current_qid: None,
                cancel_token: CancellationToken::new(),
            })),
            execution_count: 0,
            last_result_file: None,
        })
    }

}

/// Result of one streamed execute(): the preview rows kept in memory for the
/// output pane, the metadata, the row count, the spool tempfile (if any),
/// and the server's message line (used as fallback display for DDL/DML).
struct ExecuteOutcome {
    preview: Vec<Vec<serde_json::Value>>,
    meta: Option<super::client::ResultSetMetaData>,
    total_rows: u64,
    spool: Option<NamedTempFile>,
    message: Option<String>,
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
    let mut wtr = csv::WriterBuilder::new()
        .has_headers(false)
        .from_writer(file);
    let mut record: Vec<String> = Vec::new();
    for row in rows {
        record.clear();
        for (i, cell) in row.iter().enumerate() {
            let kind = kinds.get(i).copied().unwrap_or(TemporalKind::None);
            record.push(cell_to_display_string(cell, kind));
        }
        if preview.len() < preview_limit {
            // Re-wrap the decoded strings as JSON values for the preview the
            // table renderer consumes. Bounded to preview_limit rows, so the
            // clone here is cheap regardless of total result size.
            preview.push(record.iter().cloned().map(serde_json::Value::String).collect());
        }
        wtr.write_record(&record)?;
    }
    wtr.flush()?;
    Ok(())
}

impl Kernel for SnowflakeKernel {
    fn connect(&mut self) -> Result<(), Box<dyn Error>> {
        if self.client.is_some() {
            return Ok(());
        }
        let client = SnowflakeClient::new(self.config.clone())?;
        self.client = Some(Arc::new(client));
        Ok(())
    }

    fn execute(&mut self, code: &str) -> Result<ExecutionResult, Box<dyn Error>> {
        let client = self
            .client
            .as_ref()
            .ok_or("Snowflake kernel not connected")?
            .clone();

        // Reset per-execute state: fresh cancel token, clear any stale qid.
        let token = {
            let mut st = self.state.lock().unwrap();
            st.current_qid = None;
            st.cancel_token = CancellationToken::new();
            st.cancel_token.clone()
        };

        // Drop any previous spool eagerly so we don't briefly hold two on disk.
        self.last_result_file = None;

        self.execution_count += 1;
        let state = self.state.clone();
        let code = code.to_string();

        // Streamed execution: each partition is fetched, written to the spool
        // tempfile, and dropped before fetching the next. Memory usage peaks
        // at one partition + the preview rows (PREVIEW_ROW_LIMIT max),
        // independent of total result size.
        let outcome: Result<
            ExecuteOutcome,
            Box<dyn Error + Send + Sync>,
        > = self.rt.block_on(async move {
            let qid = client.submit_async(&code).await?;
            state.lock().unwrap().current_qid = Some(qid.clone());

            // Poll loop: respect the cancel token, otherwise wait 250ms between
            // status checks. The 250ms cadence keeps "Executing... 1.2s" timer
            // updates feeling live without hammering Snowflake.
            let mut resp = loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        return Err("cancelled by user".into());
                    }
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                }
                let (done, r) = client.poll(&qid).await?;
                if done {
                    break r;
                }
            };

            let meta = match resp.result_set_meta_data.take() {
                Some(m) => m,
                None => {
                    // DDL/DML success with no result set.
                    return Ok(ExecuteOutcome {
                        preview: Vec::new(),
                        meta: None,
                        total_rows: 0,
                        spool: None,
                        message: resp.message.clone(),
                    });
                }
            };
            let total_rows = meta.num_rows.unwrap_or(0);
            let partition_count = meta.partition_info.len().max(1);

            // No columns -> still treat as DDL/DML.
            if meta.row_type.is_empty() {
                return Ok(ExecuteOutcome {
                    preview: Vec::new(),
                    meta: Some(meta),
                    total_rows,
                    spool: None,
                    message: resp.message.clone(),
                });
            }

            // Open the spool tempfile and write the header up front.
            let mut tmp = tempfile::Builder::new()
                .prefix("sage-snowflake-")
                .suffix(".csv")
                .tempfile()?;
            {
                let mut wtr = csv::Writer::from_writer(tmp.as_file_mut());
                wtr.write_record(meta.row_type.iter().map(|c| &c.name))?;
                wtr.flush()?;
            }

            // Classify each column once so temporal cells decode to ISO 8601
            // as partitions stream through (shared by the spool and preview).
            let kinds: Vec<TemporalKind> =
                meta.row_type.iter().map(|c| temporal_kind(&c.type_name)).collect();

            // Process partition 0 (came back inline with the poll).
            let mut preview: Vec<Vec<serde_json::Value>> = Vec::new();
            let part0 = resp.data.take().unwrap_or_default();
            write_partition_and_capture_preview(
                tmp.as_file_mut(),
                &part0,
                &kinds,
                &mut preview,
                PREVIEW_ROW_LIMIT,
            )?;
            drop(part0);

            // Fetch remaining partitions one at a time, write, drop.
            for partition in 1..partition_count {
                if token.is_cancelled() {
                    return Err("cancelled by user".into());
                }
                let part = client.fetch_partition(&qid, partition).await?;
                write_partition_and_capture_preview(
                    tmp.as_file_mut(),
                    &part,
                    &kinds,
                    &mut preview,
                    PREVIEW_ROW_LIMIT,
                )?;
                drop(part);
            }

            Ok(ExecuteOutcome {
                preview,
                meta: Some(meta),
                total_rows,
                spool: Some(tmp),
                message: resp.message.clone(),
            })
        });

        // Clear qid regardless of outcome — the query is no longer "current".
        self.state.lock().unwrap().current_qid = None;

        let outputs = match outcome {
            Ok(out) => {
                if let Some(tmp) = out.spool {
                    self.last_result_file = Some(tmp);
                }
                format_outcome(out.meta.as_ref(), &out.preview, out.total_rows, &out.message)
            }
            Err(e) => vec![ExecutionOutput::Error {
                ename: "SnowflakeError".to_string(),
                evalue: e.to_string(),
                traceback: vec![],
            }],
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
        // PAT-authenticated requests are stateless server-side. Drop the
        // client; the next connect() rebuilds it.
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
        let client = self.client.as_ref()?.clone();
        let handle: Arc<dyn CancelHandle> = Arc::new(SnowflakeCancelHandle {
            client,
            rt: self.rt.clone(),
            state: self.state.clone(),
        });
        Some(handle)
    }

    fn cancel_preserves_session(&self) -> bool {
        // Cancellation just POSTs an abort and short-circuits the poll loop;
        // the HTTP client (PAT, base URL) and tokio runtime stay alive.
        true
    }

    fn latest_result_file(&self) -> Option<PathBuf> {
        self.last_result_file.as_ref().map(|t| t.path().to_path_buf())
    }
}

/// Cancels an in-flight Snowflake statement by POSTing to the abort endpoint.
/// The cancel token short-circuits the poll loop immediately (host UI returns
/// to responsive); the abort fires on a detached OS thread so the cancel call
/// itself never blocks the UI.
struct SnowflakeCancelHandle {
    client: Arc<SnowflakeClient>,
    rt: Arc<Runtime>,
    state: Arc<Mutex<QueryState>>,
}

impl CancelHandle for SnowflakeCancelHandle {
    fn cancel(&self) {
        let qid = {
            let st = self.state.lock().unwrap();
            st.cancel_token.cancel();
            st.current_qid.clone()
        };
        if let Some(qid) = qid {
            let client = self.client.clone();
            let rt = self.rt.clone();
            std::thread::spawn(move || {
                rt.block_on(async move {
                    let _ = client.abort(&qid).await;
                });
            });
        }
    }
}

/// Render an ExecuteOutcome as ExecutionOutput entries. For row results,
/// builds a DuckDB-style table from the in-memory preview and notes how
/// many additional rows are on disk. For DDL/DML, shows the server message.
fn format_outcome(
    meta: Option<&super::client::ResultSetMetaData>,
    preview: &[Vec<serde_json::Value>],
    total_rows: u64,
    message: &Option<String>,
) -> Vec<ExecutionOutput> {
    let meta = match meta {
        Some(m) if !m.row_type.is_empty() => m,
        _ => {
            let msg = message
                .clone()
                .unwrap_or_else(|| "Statement executed.".to_string());
            return vec![ExecutionOutput::Stdout(msg + "\n")];
        }
    };

    let table = render_table(&meta.row_type, preview);
    let shown = preview.len() as u64;
    let footer = if shown < total_rows {
        format!(
            "\n(showing first {} of {} rows — F9 to export full result)\n",
            shown, total_rows
        )
    } else {
        format!("\n({} row{})\n", total_rows, if total_rows == 1 { "" } else { "s" })
    };
    vec![ExecutionOutput::Result(table + &footer)]
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

/// Render result rows as a DuckDB-style boxed table. Two header rows: the
/// column names on top, the Snowflake type names dimmed below (e.g. TEXT,
/// FIXED, REAL, TIMESTAMP_NTZ). Numeric columns are right-aligned (detected
/// by type name); everything else left-aligned. Cells truncated at
/// `MAX_CELL` with `…`.
fn render_table(
    columns: &[super::client::ColumnMeta],
    rows: &[Vec<serde_json::Value>],
) -> String {
    use unicode_width::UnicodeWidthStr;
    const MAX_CELL: usize = 40;

    let aligns: Vec<Align> = columns.iter().map(|c| align_for(&c.type_name)).collect();
    let type_labels: Vec<String> = columns.iter().map(|c| short_type(&c.type_name)).collect();

    // Width per column is the max of: column name, type label, and any
    // value's display width — capped at MAX_CELL.
    let mut widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            UnicodeWidthStr::width(c.name.as_str())
                .max(UnicodeWidthStr::width(type_labels[i].as_str()))
                .min(MAX_CELL)
        })
        .collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i >= widths.len() {
                break;
            }
            let s = cell_to_string(cell);
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
        columns.iter().map(|c| c.name.as_str()),
        &widths,
        &vec![Align::Left; widths.len()],
        None,
    );
    // Type row: dimmed via ANSI SGR 2, closed with a full reset (SGR 0).
    // Closing with SGR 22 alone (intensity-only reset) isn't honored by
    // every terminal — full reset is the compatibility-safe choice. The
    // same code path that color-codes Python errors handles these escapes.
    push_row(
        &mut out,
        type_labels.iter().map(|s| s.as_str()),
        &widths,
        &vec![Align::Left; widths.len()],
        Some(("\x1b[2m", "\x1b[0m")),
    );
    push_border(&mut out, &widths, BOX_LT, BOX_X, BOX_RT);
    for row in rows {
        let cells: Vec<String> = (0..widths.len())
            .map(|i| {
                row.get(i)
                    .map(cell_to_string)
                    .unwrap_or_else(|| "NULL".to_string())
            })
            .collect();
        push_row(
            &mut out,
            cells.iter().map(|s| s.as_str()),
            &widths,
            &aligns,
            None,
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
    ansi: Option<(&str, &str)>,
) {
    use unicode_width::UnicodeWidthStr;
    out.push(BOX_V);
    for (i, (cell, &w)) in cells.into_iter().zip(widths.iter()).enumerate() {
        let truncated = truncate(cell, w);
        let cell_w = UnicodeWidthStr::width(truncated.as_str());
        let pad = w.saturating_sub(cell_w);
        out.push(' ');
        if let Some((open, _)) = ansi {
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
        if let Some((_, close)) = ansi {
            out.push_str(close);
        }
        out.push(' ');
        out.push(BOX_V);
    }
    out.push('\n');
}

fn cell_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Null => "NULL".to_string(),
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

/// Decode a Snowflake temporal cell into an ISO 8601 string. Snowflake's JSON
/// result format encodes these as numeric strings: DATE as days since the Unix
/// epoch, TIME as fractional seconds since midnight, and the TIMESTAMP family
/// as fractional seconds since the epoch (TIMESTAMP_TZ additionally carries a
/// trailing minute offset). Returns `None` for NULLs, non-numeric payloads, or
/// values that don't parse, letting the caller fall back to plain rendering.
fn decode_temporal(cell: &serde_json::Value, kind: TemporalKind) -> Option<String> {
    use chrono::{Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};

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
            .and_then(|days| {
                NaiveDate::from_ymd_opt(1970, 1, 1)
                    .and_then(|epoch| epoch.checked_add_signed(Duration::days(days)))
            })
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
            let mut parts = raw.split_whitespace();
            let ts = parts.next().unwrap_or("");
            let offset_minutes = parts
                .next()
                .and_then(|o| o.parse::<i64>().ok())
                .map(|o| o - 1440)
                .unwrap_or(0);
            parse_fractional_seconds(ts).and_then(|(secs, nanos)| {
                FixedOffset::east_opt((offset_minutes * 60) as i32).and_then(|off| {
                    Utc.timestamp_opt(secs, nanos).single().map(|dt| {
                        let dt = dt.with_timezone(&off);
                        format!(
                            "{}{}{}",
                            dt.format("%Y-%m-%dT%H:%M:%S"),
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
                        naive.format("%Y-%m-%dT%H:%M:%S"),
                        format_fractional_nanos(naive.nanosecond())
                    )
                })
        }
    }
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
fn truncate(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    if max <= 1 {
        return s.chars().take(max).collect();
    }
    let take = max.saturating_sub(1);
    let mut t: String = s.chars().take(take).collect();
    t.push('…');
    t
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
            "2025-10-23T00:00:00"
        );
        assert_eq!(
            disp("1761177600.500000000", TemporalKind::TimestampNtz),
            "2025-10-23T00:00:00.5"
        );
        // LTZ renders the same as NTZ (zoneless wall clock, no designator).
        assert_eq!(
            disp("1761177600.000", TemporalKind::TimestampLtz),
            "2025-10-23T00:00:00"
        );
    }

    #[test]
    fn decodes_timestamp_tz_with_offset() {
        // 1770 == +05:30 (1440 bias + 330 minutes).
        assert_eq!(
            disp("1761177600.000 1770", TemporalKind::TimestampTz),
            "2025-10-23T05:30:00+05:30"
        );
        // 1440 == UTC.
        assert_eq!(
            disp("1761177600.000 1440", TemporalKind::TimestampTz),
            "2025-10-23T00:00:00+00:00"
        );
    }

    #[test]
    fn decodes_pre_epoch_timestamp() {
        assert_eq!(
            disp("-1.5", TemporalKind::TimestampNtz),
            "1969-12-31T23:59:58.5"
        );
    }

    #[test]
    fn non_temporal_and_unparseable_pass_through() {
        assert_eq!(disp("hello", TemporalKind::None), "hello");
        // A temporal column whose cell can't be parsed keeps its raw text.
        assert_eq!(disp("not-a-number", TemporalKind::Date), "not-a-number");
        // NULL cells fall back to the plain NULL rendering.
        assert_eq!(
            cell_to_display_string(&Value::Null, TemporalKind::TimestampNtz),
            "NULL"
        );
    }
}
