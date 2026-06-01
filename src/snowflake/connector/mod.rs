//! Vendored from `snowflake-connector-rs` v0.9.0 (MIT/Apache-2.0), trimmed to
//! the password/PAT login path sage uses and adapted so we can extend it
//! (session-token renewal, long-running-query waits, query-id capture) — none
//! of which the upstream crate supports. Key-pair/JWT, OAuth, and
//! external-browser auth were removed along with their crypto dependencies.
//!
//! `allow(dead_code, unused_imports)` because this is a vendored library
//! surface: we keep its full API (bindings, row decoders, streaming helpers,
//! and the re-exports that give them public paths) even where sage doesn't yet
//! call every part by name.
#![allow(dead_code, unused_imports)]

mod chunk;
mod error;
mod login;
mod query;
mod row;
mod session;

use std::time::Duration;

pub use error::{Error, Result};
pub use query::{Binding, BindingType, QueryExecutor, QueryRequest};
pub use row::{SnowflakeColumn, SnowflakeColumnType, SnowflakeDecode, SnowflakeRow};
pub use session::SnowflakeSession;

use login::login;

use reqwest::{Client, ClientBuilder, Proxy};

#[derive(Clone)]
pub struct SnowflakeClient {
    http: Client,

    username: String,
    auth: SnowflakeAuthMethod,
    config: SnowflakeClientConfig,
    connection_config: Option<SnowflakeConnectionConfig>,
}

#[derive(Default, Clone)]
pub struct SnowflakeClientConfig {
    pub account: String,

    pub warehouse: Option<String>,
    pub database: Option<String>,
    pub schema: Option<String>,
    pub role: Option<String>,
    pub timeout: Option<Duration>,
}

#[derive(Default, Clone)]
pub(crate) struct SnowflakeConnectionConfig {
    pub(crate) host: String,
    pub(crate) port: Option<u16>,
    pub(crate) protocol: Option<String>,
}

/// Authentication method. Trimmed to password — a Snowflake PAT is passed here
/// as the password, which the login endpoint accepts without MFA.
#[derive(Clone)]
pub enum SnowflakeAuthMethod {
    Password(String),
}

impl SnowflakeClient {
    pub fn new(
        username: &str,
        auth: SnowflakeAuthMethod,
        config: SnowflakeClientConfig,
    ) -> Result<Self> {
        let client = ClientBuilder::new().gzip(true).use_rustls_tls().build()?;
        Ok(Self {
            http: client,
            username: username.to_string(),
            auth,
            config,
            connection_config: None,
        })
    }

    pub fn with_proxy(self, host: &str, port: u16, username: &str, password: &str) -> Result<Self> {
        let proxy =
            Proxy::all(format!("http://{host}:{port}").as_str())?.basic_auth(username, password);

        let client = ClientBuilder::new()
            .gzip(true)
            .use_rustls_tls()
            .proxy(proxy)
            .build()?;
        Ok(Self {
            http: client,
            username: self.username,
            auth: self.auth,
            config: self.config,
            connection_config: self.connection_config,
        })
    }

    pub fn with_address(
        self,
        host: &str,
        port: Option<u16>,
        protocol: Option<String>,
    ) -> Result<Self> {
        Ok(Self {
            http: self.http,
            username: self.username,
            auth: self.auth,
            config: self.config,
            connection_config: Some(SnowflakeConnectionConfig {
                host: host.to_string(),
                port,
                protocol,
            }),
        })
    }

    pub async fn create_session(&self) -> Result<SnowflakeSession> {
        let tokens = login(
            &self.http,
            &self.username,
            &self.auth,
            &self.config,
            &self.connection_config,
        )
        .await?;
        let base_url = login::get_base_url(&self.config, &self.connection_config)?;
        Ok(SnowflakeSession::new(
            self.http.clone(),
            base_url,
            tokens.token,
            tokens.master_token,
            self.config.timeout,
            tokens.validity_seconds,
        ))
    }
}
