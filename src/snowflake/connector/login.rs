//! Login against `session/v1/login-request`. Trimmed from the upstream crate to
//! the password/PAT path only (no JWT/key-pair, OAuth, or external-browser).

use reqwest::{Client, Url};
use serde_json::{Value, json};

use super::{Error, Result, SnowflakeAuthMethod, SnowflakeClientConfig, SnowflakeConnectionConfig};

/// Tokens and metadata captured from a successful login. The master token is
/// what lets us later renew the (short-lived) session token without re-auth.
pub(super) struct LoginTokens {
    pub(super) token: String,
    pub(super) master_token: String,
    /// Session-token validity in seconds, used to schedule proactive renewal.
    pub(super) validity_seconds: Option<i64>,
}

pub(super) fn get_base_url(
    config: &SnowflakeClientConfig,
    connection_config: &Option<SnowflakeConnectionConfig>,
) -> Result<Url> {
    if let Some(connection_config) = connection_config {
        let host = &connection_config.host;
        let protocol = connection_config
            .protocol
            .clone()
            .unwrap_or_else(|| "https".to_string());
        let mut url = Url::parse(&format!("{protocol}://{host}"))?;
        if let Some(port) = connection_config.port {
            url.set_port(Some(port))
                .map_err(|_| Error::Url("invalid base url port".to_string()))?;
        }
        Ok(url)
    } else {
        Ok(Url::parse(&format!(
            "https://{}.snowflakecomputing.com",
            config.account
        ))?)
    }
}

fn base_login_request_data(username: &str, config: &SnowflakeClientConfig) -> Value {
    json!({
        "ACCOUNT_NAME": config.account,
        "LOGIN_NAME": username,
    })
}

/// Log in to Snowflake and return the session + master tokens.
pub(super) async fn login(
    http: &Client,
    username: &str,
    auth: &SnowflakeAuthMethod,
    config: &SnowflakeClientConfig,
    connection_config: &Option<SnowflakeConnectionConfig>,
) -> Result<LoginTokens> {
    let base_url = get_base_url(config, connection_config)?;
    let url = base_url.join("session/v1/login-request")?;

    let mut queries: Vec<(&str, &str)> = vec![];
    if let Some(warehouse) = &config.warehouse {
        queries.push(("warehouse", warehouse));
    }
    if let Some(database) = &config.database {
        queries.push(("databaseName", database));
    }
    if let Some(schema) = &config.schema {
        queries.push(("schemaName", schema));
    }
    if let Some(role) = &config.role {
        queries.push(("roleName", role));
    }

    let login_data = login_request_data(username, auth, config);

    let resp = http
        .post(url)
        .query(&queries)
        .json(&json!({ "data": login_data }))
        .send()
        .await?;

    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(Error::Communication(body));
    }

    let parsed: Response = serde_json::from_str(&body).map_err(|e| Error::Json(e, body))?;
    if !parsed.success {
        return Err(Error::Communication(parsed.message.unwrap_or_default()));
    }

    let data = parsed
        .data
        .ok_or_else(|| Error::Communication("missing login-response data".to_string()))?;

    Ok(LoginTokens {
        token: data.token,
        master_token: data.master_token.unwrap_or_default(),
        validity_seconds: data.validity_in_seconds,
    })
}

fn login_request_data(
    username: &str,
    auth: &SnowflakeAuthMethod,
    config: &SnowflakeClientConfig,
) -> Value {
    match auth {
        SnowflakeAuthMethod::Password(password) => {
            let mut data = base_login_request_data(username, config);
            if let Some(obj) = data.as_object_mut() {
                obj.insert("PASSWORD".to_string(), json!(password));
            }
            data
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoginResponseData {
    token: String,
    #[serde(default)]
    master_token: Option<String>,
    #[serde(default)]
    validity_in_seconds: Option<i64>,
    #[serde(default)]
    master_validity_in_seconds: Option<i64>,
}

#[derive(serde::Deserialize)]
struct Response {
    data: Option<LoginResponseData>,
    message: Option<String>,
    success: bool,
}
