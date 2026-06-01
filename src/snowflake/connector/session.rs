use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Url;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde_json::json;

use super::{
    Error, Result, SnowflakeRow,
    query::{QueryExecutor, QueryRequest},
};

/// Shared, renewable credential state for one session. Cloned cheaply (it's all
/// `Arc`s / a `Client`) so the query path and the background heartbeat operate
/// on the same tokens. Renewal swaps the session token (and master token) in
/// place via `/session/token-request`, so the underlying Snowflake session —
/// and its temp tables / session variables — survives token expiry.
#[derive(Clone)]
pub(super) struct Renewer {
    http: reqwest::Client,
    base_url: Url,
    session_token: Arc<Mutex<String>>,
    master_token: Arc<Mutex<String>>,
    /// Serializes renewals so a heartbeat tick and a reactive 390112 renewal
    /// don't fire two token-requests against the same old token at once.
    renew_lock: Arc<tokio::sync::Mutex<()>>,
}

impl Renewer {
    fn current_token(&self) -> String {
        self.session_token.lock().unwrap().clone()
    }

    /// Exchange the master token for a fresh session token (and master token).
    /// Serialized via `renew_lock`; each caller reads the latest token as
    /// `oldSessionToken`, so a redundant concurrent call is still well-formed.
    async fn renew(&self) -> Result<()> {
        let _guard = self.renew_lock.lock().await;
        let master = self.master_token.lock().unwrap().clone();
        let old_session = self.session_token.lock().unwrap().clone();
        if master.is_empty() {
            return Err(Error::Communication(
                "no master token available to renew session".to_string(),
            ));
        }

        let url = self.base_url.join("session/token-request")?;
        let resp = self
            .http
            .post(url)
            .header(ACCEPT, "application/snowflake")
            .header(AUTHORIZATION, format!(r#"Snowflake Token="{master}""#))
            .json(&json!({
                "oldSessionToken": old_session,
                "requestType": "RENEW",
            }))
            .send()
            .await?;

        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            return Err(Error::Communication(body));
        }
        let parsed: RenewResponse =
            serde_json::from_str(&body).map_err(|e| Error::Json(e, body))?;
        if !parsed.success {
            return Err(Error::Communication(parsed.message.unwrap_or_default()));
        }
        let data = parsed
            .data
            .ok_or_else(|| Error::Communication("missing token-request data".to_string()))?;

        *self.session_token.lock().unwrap() = data.session_token;
        if let Some(mt) = data.master_token {
            *self.master_token.lock().unwrap() = mt;
        }
        Ok(())
    }
}

#[derive(serde::Deserialize)]
struct RenewResponse {
    data: Option<RenewData>,
    message: Option<String>,
    success: bool,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenewData {
    session_token: String,
    #[serde(default)]
    master_token: Option<String>,
}

pub struct SnowflakeSession {
    pub(super) renewer: Renewer,
    pub(super) timeout: Option<Duration>,
    /// Proactive renewal cadence, derived from the session token's validity.
    renew_interval: Duration,
}

impl SnowflakeSession {
    pub(super) fn new(
        http: reqwest::Client,
        base_url: Url,
        session_token: String,
        master_token: String,
        timeout: Option<Duration>,
        validity_seconds: Option<i64>,
    ) -> Self {
        // Renew at half the reported validity, clamped to [1 min, 30 min].
        // Defaults conservatively if the server didn't report a validity.
        let half = validity_seconds.unwrap_or(3600).max(120) / 2;
        let renew_interval = Duration::from_secs(half.clamp(60, 1800) as u64);
        Self {
            renewer: Renewer {
                http,
                base_url,
                session_token: Arc::new(Mutex::new(session_token)),
                master_token: Arc::new(Mutex::new(master_token)),
                renew_lock: Arc::new(tokio::sync::Mutex::new(())),
            },
            timeout,
            renew_interval,
        }
    }

    pub(super) fn http(&self) -> &reqwest::Client {
        &self.renewer.http
    }

    pub(super) fn base_url(&self) -> &Url {
        &self.renewer.base_url
    }

    pub(super) fn current_token(&self) -> String {
        self.renewer.current_token()
    }

    /// Renew the session token via the master token. Public so the kernel's
    /// heartbeat task can call it on a timer; also used reactively on 390112.
    pub async fn renew(&self) -> Result<()> {
        self.renewer.renew().await
    }

    /// How often the kernel should proactively renew this session.
    pub fn renew_interval(&self) -> Duration {
        self.renew_interval
    }

    /// Run a query and fetch all results.
    pub async fn query<Q: Into<QueryRequest>>(&self, request: Q) -> Result<Vec<SnowflakeRow>> {
        let executor = QueryExecutor::create(self, request).await?;
        executor.fetch_all().await
    }

    pub async fn execute<Q: Into<QueryRequest>>(&self, request: Q) -> Result<QueryExecutor> {
        QueryExecutor::create(self, request).await
    }
}
