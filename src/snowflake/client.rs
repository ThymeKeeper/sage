//! Session-based Snowflake client built on `snowflake-connector-rs`.
//!
//! Unlike the stateless SQL API v2, this opens a real, stateful session via the
//! driver login protocol (login-request → session token → query-request), so
//! temporary tables and session variables persist across `execute()` calls. The
//! PAT is supplied as the password (no MFA prompt). Each statement still runs in
//! one session, so `USE` works natively — no client-side emulation needed.
//!
//! Cancellation is driven by `SnowflakeKernel`: the connector exposes no abort
//! and hides the query id, so the kernel runs `SYSTEM$CANCEL_ALL_QUERIES(<id>)`
//! from a throwaway control session. That's why we capture the session id at
//! connect time.

use std::error::Error;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use super::connector::{
    SnowflakeAuthMethod, SnowflakeClient as SfClient, SnowflakeClientConfig, SnowflakeRow,
    SnowflakeSession,
};

use super::config::SnowflakeConfig;

/// Column header for the table renderer: the column name (the connector returns
/// these uppercased) and the Snowflake type string (e.g. "text", "fixed",
/// "timestamp_ntz"). The type string drives numeric alignment, the dimmed type
/// label, and temporal decoding over in `kernel.rs`.
#[derive(Debug, Clone)]
pub struct ColumnMeta {
    pub name: String,
    pub type_name: String,
}

/// Wraps the connector's client plus one live session. The client is retained
/// so the kernel can mint short-lived control sessions (for cancellation) and
/// so an expired session can be rebuilt via `reconnect`.
pub struct SnowflakeClient {
    sf: SfClient,
    session: Arc<SnowflakeSession>,
    session_id: String,
    /// Held only for its `Drop`, which stops the session's heartbeat task.
    _heartbeat: HeartbeatGuard,
}

impl SnowflakeClient {
    /// Open a session: read the PAT from the keyring, log in (no MFA), and
    /// capture the session id for later cancellation.
    pub async fn connect(config: &SnowflakeConfig) -> Result<Self, Box<dyn Error>> {
        let pat = config.fetch_token()?;
        let sf = SfClient::new(
            &config.user,
            SnowflakeAuthMethod::Password(pat),
            SnowflakeClientConfig {
                account: config.account.clone(),
                role: config.role.clone(),
                warehouse: config.warehouse.clone(),
                database: config.database.clone(),
                schema: config.schema.clone(),
                // Per-statement poll budget. Generous so long analytical /
                // ML-training queries aren't cut off client-side; Ctrl+Backspace
                // (cancel) is the responsive stop, and token renewal keeps the
                // poll authenticated across the session-token expiry.
                timeout: Some(std::time::Duration::from_secs(24 * 60 * 60)),
            },
        )?;
        let (session, session_id) = Self::open_session(&sf).await?;
        let session = Arc::new(session);
        let heartbeat = spawn_heartbeat(&session);
        Ok(Self {
            sf,
            session,
            session_id,
            _heartbeat: heartbeat,
        })
    }

    /// Log in and read back `CURRENT_SESSION()` so we have an id to cancel.
    async fn open_session(sf: &SfClient) -> Result<(SnowflakeSession, String), Box<dyn Error>> {
        let session = sf.create_session().await?;
        let rows = session.query("SELECT CURRENT_SESSION()").await?;
        let session_id = rows
            .first()
            .and_then(|r| r.at::<Option<String>>(0).ok().flatten())
            .ok_or("CURRENT_SESSION() returned no value")?;
        Ok((session, session_id))
    }

    /// Rebuild the session after expiry. Session-local state (temp tables,
    /// variables) is gone with the old session; the caller surfaces that.
    pub async fn reconnect(&mut self) -> Result<(), Box<dyn Error>> {
        let (session, session_id) = Self::open_session(&self.sf).await?;
        let session = Arc::new(session);
        let heartbeat = spawn_heartbeat(&session);
        self.session = session;
        self.session_id = session_id;
        // Replacing the guard drops the old one, stopping the previous session's
        // heartbeat task.
        self._heartbeat = heartbeat;
        Ok(())
    }

    /// Cheap `Arc` clone of the live session for the execute path.
    pub fn session(&self) -> Arc<SnowflakeSession> {
        self.session.clone()
    }

    /// Clone of the underlying connector client, used to mint a throwaway
    /// control session for cancellation without disturbing the live session.
    pub fn control_client(&self) -> SfClient {
        self.sf.clone()
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Column headers from a result row — the connector's only public source of
/// column metadata (so a zero-row result yields no headers).
pub fn columns_of(row: &SnowflakeRow) -> Vec<ColumnMeta> {
    row.column_types()
        .into_iter()
        .map(|c| ColumnMeta {
            name: c.name().to_string(),
            type_name: c.column_type().snowflake_type().to_string(),
        })
        .collect()
}

/// Convert one result row into the `serde_json::Value` cells the table renderer
/// and CSV spool consume. Every cell is read as its raw string (NULL → Null);
/// temporal decoding to ISO 8601 happens later in `kernel.rs`, keyed off the
/// column type — exactly as it did for the SQL API's JSON values.
pub fn row_to_values(row: &SnowflakeRow, ncols: usize) -> Vec<serde_json::Value> {
    (0..ncols)
        .map(|i| match row.at::<Option<String>>(i) {
            Ok(Some(s)) => serde_json::Value::String(s),
            _ => serde_json::Value::Null,
        })
        .collect()
}

/// Cancels the session's heartbeat task on drop (disconnect or reconnect).
struct HeartbeatGuard(CancellationToken);

impl Drop for HeartbeatGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Spawn a background task that renews the session token on a timer, so the
/// session — and its temp tables / session variables — survives the (~1h,
/// fixed) session-token expiry whether the kernel is idle or running a long
/// query. Holds a `Weak`, so it exits once the session is dropped; the returned
/// guard cancels it promptly on disconnect/reconnect. Must be called from
/// within the tokio runtime (it is — via the kernel's `block_on`).
fn spawn_heartbeat(session: &Arc<SnowflakeSession>) -> HeartbeatGuard {
    let weak = Arc::downgrade(session);
    let interval = session.renew_interval();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = task_cancel.cancelled() => break,
                _ = tokio::time::sleep(interval) => {
                    let Some(session) = weak.upgrade() else { break };
                    if session.renew().await.is_err() {
                        // Master token likely lapsed; stop renewing. The next
                        // user statement hits an expired session and the kernel
                        // reconnects fresh.
                        break;
                    }
                }
            }
        }
    });
    HeartbeatGuard(cancel)
}
