use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::time::Duration;

use super::config::SnowflakeConfig;

/// Thin async client for Snowflake's public SQL API v2.
///
/// Authenticates with a Programmatic Access Token (PAT) passed as a Bearer
/// token plus the `X-Snowflake-Authorization-Token-Type` header. All calls go
/// through the same `reqwest::Client`, which is `Send + Sync + Clone`-cheap,
/// so the kernel can hand `Arc<SnowflakeClient>` to both the execute thread
/// and the cancel handle without contention.
pub struct SnowflakeClient {
    config: SnowflakeConfig,
    token: String,
    http: Client,
    base_url: String,
}

#[derive(Serialize)]
struct SubmitBody<'a> {
    statement: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    schema: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warehouse: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
}

/// Match Snowpark / Python-connector behavior: unquoted identifiers (the 99%
/// case) get folded to uppercase. SQL API v2 is strict about case — the docs
/// say role/warehouse/database/schema values "must match the case of the field
/// returned by a SQL SHOW command," which for unquoted names is uppercase.
/// Already-quoted identifiers (anyone with truly lowercase role names) are
/// left alone.
fn fold_identifier(s: &str) -> String {
    if s.starts_with('"') && s.ends_with('"') {
        s.to_string()
    } else {
        s.to_uppercase()
    }
}

/// Subset of the SQL API v2 response we care about. The API returns the same
/// envelope shape for both 202 (still running) and 200 (completed) — the
/// distinguishing fields (`data`, `resultSetMetaData`) only populate when done.
#[derive(Debug, Clone, Deserialize)]
pub struct StatusResponse {
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(rename = "sqlState", default)]
    pub sql_state: Option<String>,
    #[serde(rename = "statementHandle", default)]
    pub statement_handle: Option<String>,
    #[serde(rename = "resultSetMetaData", default)]
    pub result_set_meta_data: Option<ResultSetMetaData>,
    /// One row per outer element; each row is one cell per inner element.
    /// Values are JSON strings (Snowflake serializes numbers/dates as strings
    /// in the JSON result format).
    #[serde(default)]
    pub data: Option<Vec<Vec<serde_json::Value>>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResultSetMetaData {
    #[serde(rename = "numRows", default)]
    pub num_rows: Option<u64>,
    #[serde(rename = "rowType", default)]
    pub row_type: Vec<ColumnMeta>,
    /// One entry per partition (including partition 0). Snowflake returns
    /// partition 0 inline with the initial poll response; later partitions
    /// must be fetched with `?partition=N` to assemble the full result.
    #[serde(rename = "partitionInfo", default)]
    pub partition_info: Vec<PartitionInfo>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PartitionInfo {
    #[serde(rename = "rowCount", default)]
    pub row_count: Option<u64>,
    #[serde(rename = "uncompressedSize", default)]
    pub uncompressed_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ColumnMeta {
    pub name: String,
    #[serde(rename = "type", default)]
    pub type_name: String,
    #[serde(default)]
    pub nullable: Option<bool>,
}

impl SnowflakeClient {
    pub fn new(config: SnowflakeConfig) -> Result<Self, Box<dyn Error>> {
        let token = config.fetch_token()?;
        let base_url = config.account_url();
        let http = Client::builder()
            .timeout(Duration::from_secs(60))
            .user_agent(concat!("sage/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { config, token, http, base_url })
    }

    /// Read the response body and deserialize as `StatusResponse`. On failure,
    /// include HTTP status, Content-Encoding, and a body excerpt so the user
    /// sees what Snowflake actually sent instead of just "error decoding
    /// response body". Replaces `resp.json()` at every call site.
    async fn parse_status(
        resp: Response,
    ) -> Result<(StatusCode, StatusResponse), Box<dyn Error + Send + Sync>> {
        let status = resp.status();
        let encoding = resp
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("identity")
            .to_string();
        let bytes = resp.bytes().await?;
        match serde_json::from_slice::<StatusResponse>(&bytes) {
            Ok(parsed) => Ok((status, parsed)),
            Err(e) => {
                let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(400)]);
                Err(format!(
                    "decoding response body failed (HTTP {}, encoding={}, {} bytes): {} — body starts: {:?}",
                    status,
                    encoding,
                    bytes.len(),
                    e,
                    preview
                )
                .into())
            }
        }
    }

    fn auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        req.header("Authorization", format!("Bearer {}", self.token))
            .header(
                "X-Snowflake-Authorization-Token-Type",
                "PROGRAMMATIC_ACCESS_TOKEN",
            )
            .header("Accept", "application/json")
    }

    /// Submit a statement in async mode. Returns the `statementHandle` (a.k.a.
    /// query_id) immediately, before the query starts producing results.
    pub async fn submit_async(&self, statement: &str) -> Result<String, Box<dyn Error + Send + Sync>> {
        let url = format!("{}/api/v2/statements?async=true", self.base_url);
        let body = SubmitBody {
            statement,
            database: self.config.database.as_deref().map(fold_identifier),
            schema: self.config.schema.as_deref().map(fold_identifier),
            warehouse: self.config.warehouse.as_deref().map(fold_identifier),
            role: self.config.role.as_deref().map(fold_identifier),
        };
        let resp = self.auth(self.http.post(&url)).json(&body).send().await?;
        let (status, parsed) = Self::parse_status(resp).await?;
        // 202 is the expected async-submit response; 200 happens if Snowflake
        // chose to execute synchronously despite the async hint (small queries).
        if status != StatusCode::ACCEPTED && status != StatusCode::OK {
            return Err(format!(
                "submit failed (HTTP {}): {} {}",
                status,
                parsed.code.as_deref().unwrap_or(""),
                parsed.message.as_deref().unwrap_or("")
            )
            .into());
        }
        parsed
            .statement_handle
            .ok_or_else(|| "submit returned no statementHandle".into())
    }

    /// Check status / fetch results for a previously-submitted statement.
    /// Returns `(done, response)`. When `done` is `false` the response is just
    /// status metadata; when `true`, `response.data` holds the result rows.
    pub async fn poll(&self, handle: &str) -> Result<(bool, StatusResponse), Box<dyn Error + Send + Sync>> {
        let url = format!("{}/api/v2/statements/{}", self.base_url, handle);
        let resp = self.auth(self.http.get(&url)).send().await?;
        let (status, parsed) = Self::parse_status(resp).await?;
        match status {
            StatusCode::OK => Ok((true, parsed)),
            StatusCode::ACCEPTED => Ok((false, parsed)),
            _ => Err(format!(
                "poll failed (HTTP {}): {} {}",
                status,
                parsed.code.as_deref().unwrap_or(""),
                parsed.message.as_deref().unwrap_or("")
            )
            .into()),
        }
    }

    /// Fetch one additional partition of a completed statement's result set.
    /// Partition 0 already comes back inline with `poll`, so this is called
    /// for partitions 1..N. Returns the row data as parsed JSON values.
    pub async fn fetch_partition(
        &self,
        handle: &str,
        partition: usize,
    ) -> Result<Vec<Vec<serde_json::Value>>, Box<dyn Error + Send + Sync>> {
        let url = format!(
            "{}/api/v2/statements/{}?partition={}",
            self.base_url, handle, partition
        );
        let resp = self.auth(self.http.get(&url)).send().await?;
        let (status, parsed) = Self::parse_status(resp).await?;
        if status != StatusCode::OK {
            return Err(format!(
                "fetch partition {} failed (HTTP {}): {} {}",
                partition,
                status,
                parsed.code.as_deref().unwrap_or(""),
                parsed.message.as_deref().unwrap_or("")
            )
            .into());
        }
        Ok(parsed.data.unwrap_or_default())
    }

    /// Tell Snowflake to cancel a running statement. Idempotent: cancelling a
    /// statement that's already finished is a no-op as far as the caller is
    /// concerned — we treat 4xx as success here so a slow Ctrl+Backspace
    /// doesn't surface a spurious error.
    pub async fn abort(&self, handle: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
        let url = format!("{}/api/v2/statements/{}/cancel", self.base_url, handle);
        let resp = self.auth(self.http.post(&url)).send().await?;
        let status = resp.status();
        if status.is_server_error() {
            return Err(format!("abort failed (HTTP {})", status).into());
        }
        Ok(())
    }

    pub fn config(&self) -> &SnowflakeConfig {
        &self.config
    }
}
