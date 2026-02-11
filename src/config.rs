use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, warn};

const DEFAULT_SERVER: &str = "http://localhost:5051";

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_server_url")]
    pub url: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            url: DEFAULT_SERVER.to_string(),
        }
    }
}

fn default_server_url() -> String {
    DEFAULT_SERVER.to_string()
}

impl Config {
    /// Load config with priority: CLI arg > env var > config file > default.
    pub fn resolve_server_url(cli_server: Option<String>) -> String {
        if let Some(url) = cli_server {
            return url;
        }

        if let Ok(url) = std::env::var("NEMOSPEECH_URL") {
            return url;
        }

        let config = Self::load();
        config.server.url
    }

    fn config_path() -> Option<PathBuf> {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
            .ok()
            .map(|c| c.join("justspeak/config.toml"))
    }

    fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self::default();
        };

        if !path.exists() {
            debug!(path = %path.display(), "no config file found, using defaults");
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(contents) => match toml::from_str(&contents) {
                Ok(config) => {
                    debug!(path = %path.display(), "loaded config");
                    config
                }
                Err(e) => {
                    warn!(path = %path.display(), error = %e, "failed to parse config");
                    Self::default()
                }
            },
            Err(e) => {
                warn!(path = %path.display(), error = %e, "failed to read config");
                Self::default()
            }
        }
    }
}
