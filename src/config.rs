use serde::Deserialize;
use std::path::PathBuf;
use tracing::{debug, info, warn};

const DEFAULT_SERVER: &str = "http://localhost:5051";

#[derive(Debug, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,

    /// PipeWire/ALSA sink name for blip audio (e.g. "alsa_output.pci-...").
    /// If unset, uses the default output device.
    pub blip_device: Option<String>,

    /// Whether blip sounds are enabled (toggled at runtime via double-tap AltGr).
    /// Defaults to true. Only meaningful when blip_device is set.
    #[serde(default = "default_true")]
    pub blips_enabled: bool,
}

fn default_true() -> bool {
    true
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

    /// Toggle `blips_enabled` in the config file and return the new value.
    /// Uses targeted string manipulation — no TOML serializer needed.
    pub fn toggle_blips() -> bool {
        let current = Self::load();
        let new_value = !current.blips_enabled;

        let Some(path) = Self::config_path() else {
            warn!("cannot determine config path, toggle not persisted");
            return new_value;
        };

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        let new_line = format!("blips_enabled = {new_value}");

        let new_contents = if let Some(start) = contents.find("blips_enabled") {
            // Find the end of the line
            let line_end = contents[start..]
                .find('\n')
                .map(|i| start + i)
                .unwrap_or(contents.len());
            format!("{}{}{}", &contents[..start], new_line, &contents[line_end..])
        } else {
            // Insert at the top (before [server] or other sections)
            if contents.is_empty() {
                new_line + "\n"
            } else {
                format!("{new_line}\n{contents}")
            }
        };

        match std::fs::write(&path, &new_contents) {
            Ok(()) => info!(path = %path.display(), blips_enabled = new_value, "config updated"),
            Err(e) => warn!(error = %e, "failed to write config, toggle not persisted"),
        }

        new_value
    }
}
