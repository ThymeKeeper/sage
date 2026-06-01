use std::string::FromUtf8Error;

use reqwest::header::InvalidHeaderValue;
use tokio::sync::AcquireError;
use tokio::task::JoinError;

/// An error that can occur when interacting with Snowflake.
///
/// Note: Errors may include sensitive information from Snowflake.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("HTTP client error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("communication error: {0}")]
    Communication(String),

    #[error("invalid header value: {0}")]
    InvalidHeader(#[from] InvalidHeaderValue),

    #[error("http error: {0}")]
    Http(#[from] http::Error),

    #[error("session expired")]
    SessionExpired,

    #[error("chunk download error: {0}")]
    ChunkDownload(String),

    #[error("io error: {0}")]
    IO(#[from] std::io::Error),

    #[error("json parse error: {0} {1}")]
    Json(serde_json::Error, String),

    #[error("utf-8 error: {0}")]
    Utf8Error(#[from] FromUtf8Error),

    #[error("future join error: {0}")]
    FutureJoin(#[from] JoinError),

    #[error("concurrency limiter closed while acquiring permit to fetch a chunk")]
    ConcurrencyLimiterClosed(#[from] AcquireError),

    #[error("decode error: {0}")]
    Decode(String),

    #[error("url error: {0}")]
    Url(String),

    #[error(
        "query {query_id} may still be running after the connection was interrupted; \
         once it finishes, reattach with: SELECT * FROM TABLE(RESULT_SCAN('{query_id}'))  [{detail}]"
    )]
    QueryInterrupted { query_id: String, detail: String },

    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("async response doesn't contain a URL to poll for results")]
    NoPollingUrlAsyncQuery,

    #[error("timed out waiting for query results")]
    TimedOut,
}

/// A `Result` alias where the `Err` case is `snowflake::Error`.
pub type Result<T> = std::result::Result<T, Error>;

impl From<url::ParseError> for Error {
    fn from(err: url::ParseError) -> Self {
        Error::Url(err.to_string())
    }
}
