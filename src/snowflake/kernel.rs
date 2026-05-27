use std::error::Error;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;

use crate::kernel::{
    CancelHandle, CompletionItem, ExecutionOutput, ExecutionResult, Kernel, KernelInfo,
    SqlMetadata, TypeRelationships,
};

use super::client::{SnowflakeClient, StatusResponse};
use super::config::SnowflakeConfig;

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
        })
    }
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

        self.execution_count += 1;
        let state = self.state.clone();
        let code = code.to_string();

        let outcome = self.rt.block_on(async move {
            let qid = client.submit_async(&code).await?;
            state.lock().unwrap().current_qid = Some(qid.clone());

            // Poll loop: respect the cancel token, otherwise wait 250ms between
            // status checks. The 250ms cadence keeps "Executing... 1.2s" timer
            // updates feeling live without hammering Snowflake.
            loop {
                tokio::select! {
                    _ = token.cancelled() => {
                        return Err::<StatusResponse, Box<dyn Error + Send + Sync>>(
                            "cancelled by user".into(),
                        );
                    }
                    _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                }
                let (done, resp) = client.poll(&qid).await?;
                if done {
                    return Ok(resp);
                }
            }
        });

        // Clear qid regardless of outcome — the query is no longer "current".
        self.state.lock().unwrap().current_qid = None;

        let outputs = match outcome {
            Ok(resp) => format_response(&resp),
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

/// Render a successful response as ExecutionOutput entries. For results with
/// rows, builds an ASCII table; for "no rows" responses (DDL, DML without
/// RETURNING), shows the Snowflake success message.
fn format_response(resp: &StatusResponse) -> Vec<ExecutionOutput> {
    let meta = match &resp.result_set_meta_data {
        Some(m) => m,
        None => {
            let msg = resp
                .message
                .clone()
                .unwrap_or_else(|| "Statement executed.".to_string());
            return vec![ExecutionOutput::Stdout(msg + "\n")];
        }
    };

    let data = resp.data.as_deref().unwrap_or(&[]);
    if meta.row_type.is_empty() {
        let msg = resp
            .message
            .clone()
            .unwrap_or_else(|| "Statement executed.".to_string());
        return vec![ExecutionOutput::Stdout(msg + "\n")];
    }

    let table = render_table(&meta.row_type, data);
    let total = meta.num_rows.unwrap_or(data.len() as u64);
    let footer = format!("\n({} row{})\n", total, if total == 1 { "" } else { "s" });
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
