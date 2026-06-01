use serde::Deserialize;
use std::error::Error;
use std::path::PathBuf;

/// Non-secret Snowflake settings, loaded from `~/.config/sage/snowflake.toml`.
/// The PAT itself lives in the OS keyring; this struct only knows where to
/// look for it.
#[derive(Debug, Clone, Deserialize)]
pub struct SnowflakeConfig {
    /// Account locator, e.g. "westjetprod.west-us-2.azure". Becomes the
    /// hostname `<account>.snowflakecomputing.com`.
    pub account: String,
    /// Snowflake user the PAT belongs to.
    pub user: String,
    /// Default role for queries. Optional — caller can issue USE ROLE.
    #[serde(default)]
    pub role: Option<String>,
    /// Default warehouse. Optional but you almost always want one.
    #[serde(default)]
    pub warehouse: Option<String>,
    /// Default database for unqualified table refs.
    #[serde(default)]
    pub database: Option<String>,
    /// Default schema for unqualified table refs.
    #[serde(default)]
    pub schema: Option<String>,
    /// Keyring service name to look up. Defaults to "snowflake" — matches
    /// abp.py's `keyring.get_password("snowflake", ...)` convention.
    #[serde(default = "default_keyring_service")]
    pub keyring_service: String,
    /// Keyring account name. Defaults to `user` if unset.
    #[serde(default)]
    pub keyring_account: Option<String>,
}

fn default_keyring_service() -> String {
    "snowflake".to_string()
}

#[derive(Deserialize)]
struct TomlRoot {
    snowflake: SnowflakeConfig,
}

impl SnowflakeConfig {
    /// Load from the default path: `~/.config/sage/snowflake.toml`.
    pub fn load() -> Result<Self, Box<dyn Error>> {
        let path = default_config_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &PathBuf) -> Result<Self, Box<dyn Error>> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {}", path.display(), e))?;
        let root: TomlRoot = toml::from_str(&content)
            .map_err(|e| format!("parsing {}: {}", path.display(), e))?;
        Ok(root.snowflake)
    }

    pub fn keyring_account(&self) -> &str {
        self.keyring_account.as_deref().unwrap_or(&self.user)
    }

    /// Fetch the PAT from the OS keyring. Errors include "no such entry"
    /// (PAT not stored), which the caller should surface to the user.
    ///
    /// On Windows, Python's `keyring` package stores credentials in Windows
    /// Credential Manager under a bare `TargetName=<service>` (no username
    /// suffix). The Rust `keyring` crate's default composes the target as
    /// `<service>.<user>`, which doesn't match and yields a NoEntry error.
    /// We override the target explicitly so we read the same key abp.py
    /// wrote.
    pub fn fetch_token(&self) -> Result<String, Box<dyn Error>> {
        #[cfg(not(unix))]
        let entry = keyring::Entry::new_with_target(
            &self.keyring_service,
            &self.keyring_service,
            self.keyring_account(),
        )?;
        #[cfg(unix)]
        let entry = keyring::Entry::new(&self.keyring_service, self.keyring_account())?;
        Ok(entry.get_password()?)
    }
}

pub fn default_config_path() -> Result<PathBuf, Box<dyn Error>> {
    // Hardcoded to the user's dotfiles directory. Cross-platform support would
    // need a feature gate or env-var override; sage is a personal tool today.
    Ok(PathBuf::from(r"C:\.dotfile\snowflake.toml"))
}
