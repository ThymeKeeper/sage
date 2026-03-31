use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snippet {
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub snippets: Vec<Snippet>,
}

impl Config {
    /// Returns the path to the config file: ~/.config/sage/config.toml (Unix)
    /// or %APPDATA%/sage/config.toml (Windows)
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("sage").join("config.toml"))
    }

    /// Load config from the default path. Returns default config if file doesn't exist.
    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Config::default();
        };

        let Ok(contents) = fs::read_to_string(&path) else {
            return Config::default();
        };

        toml::from_str(&contents).unwrap_or_default()
    }

    /// Save config to the default path, creating directories as needed.
    #[allow(dead_code)]
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = Self::config_path().ok_or("No config directory available")?;

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let contents = toml::to_string_pretty(self)?;
        fs::write(&path, contents)?;
        Ok(())
    }
}
