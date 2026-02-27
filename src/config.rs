use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::{debug, info, warn};

const DEFAULT_SERVER: &str = "http://localhost:5051";

static CONFIG_RELOAD_REQUESTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,

    #[serde(default)]
    pub blip_device: Option<String>,

    #[serde(default = "default_true")]
    pub blips_enabled: bool,

    #[serde(default)]
    pub midi_cc: Option<u8>,

    #[serde(default)]
    pub input_device: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            blip_device: None,
            blips_enabled: true,
            midi_cc: None,
            input_device: None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Serialize)]
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

    pub fn load() -> Self {
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

    pub fn toggle_blips() -> bool {
        let mut current = Self::load();
        current.blips_enabled = !current.blips_enabled;
        let new_value = current.blips_enabled;

        let Some(path) = Self::config_path() else {
            warn!("cannot determine config path, toggle not persisted");
            return new_value;
        };

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let new_contents = match toml::to_string_pretty(&current) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "failed to serialize config");
                return new_value;
            }
        };

        match std::fs::write(&path, &new_contents) {
            Ok(()) => info!(path = %path.display(), blips_enabled = new_value, "config updated"),
            Err(e) => warn!(error = %e, "failed to write config, toggle not persisted"),
        }

        new_value
    }

    pub fn request_reload() {
        CONFIG_RELOAD_REQUESTED.store(true, Ordering::Relaxed);
    }

    pub fn reload_if_requested() -> bool {
        if CONFIG_RELOAD_REQUESTED.load(Ordering::Relaxed) {
            CONFIG_RELOAD_REQUESTED.store(false, Ordering::Relaxed);
            info!("config reloaded via SIGHUP");
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let config = Config::default();
        assert_eq!(config.server.url, "http://localhost:5051");
        assert!(config.blips_enabled);
        assert!(config.blip_device.is_none());
        assert!(config.midi_cc.is_none());
    }

    #[test]
    fn test_config_deserialization() {
        let toml = r#"blip_device = "my-sink"
blips_enabled = false
midi_cc = 65
input_device = "my-mic"

[server]
url = "http://localhost:9090"
        "#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.server.url, "http://localhost:9090");
        assert_eq!(config.blip_device, Some("my-sink".to_string()));
        assert!(!config.blips_enabled);
        assert_eq!(config.midi_cc, Some(65));
        assert_eq!(config.input_device, Some("my-mic".to_string()));
    }

    #[test]
    fn test_config_partial_deserialization() {
        let toml = r#"
            [server]
            url = "http://custom:1234"
        "#;

        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.server.url, "http://custom:1234");
        assert!(config.blips_enabled);
    }
}
