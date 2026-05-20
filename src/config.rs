//! Application configuration
//!
//! Handles loading and managing configuration from config.json and secrets.json

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Global application config
static APP_CONFIG: OnceLock<AppConfig> = OnceLock::new();

/// Global secrets
static APP_SECRETS: OnceLock<AppSecrets> = OnceLock::new();

/// Per-user config directory (e.g. ~/Library/Application Support/ODE-artwork-downloader
/// on macOS). The directory is created on demand so callers can write into it
/// immediately.
pub fn config_dir() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("", "", "ODE-artwork-downloader")
        .ok_or_else(|| "could not resolve a per-user config directory".to_string())?;
    let dir = dirs.config_dir().to_path_buf();
    fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    Ok(dir)
}

/// Canonical location of `config.json` under the per-user config directory.
pub fn config_file_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("config.json"))
}

/// Canonical location of `secrets.json` under the per-user config directory.
pub fn secrets_file_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("secrets.json"))
}

/// Update a single top-level field in `config.json`, preserving everything else.
/// The file is created if it does not exist.
pub fn save_config_field(key: &str, value: serde_json::Value) -> Result<(), String> {
    let path = config_file_path()?;
    let mut json: serde_json::Value = match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s)
            .map_err(|e| format!("Failed to parse config.json: {e}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(format!("Failed to read config.json: {e}")),
    };
    if !json.is_object() {
        json = serde_json::json!({});
    }
    json[key] = value;
    let updated = serde_json::to_string_pretty(&json)
        .map_err(|e| format!("Failed to serialize config: {e}"))?;
    fs::write(&path, updated).map_err(|e| format!("Failed to write config.json: {e}"))?;
    Ok(())
}

/// Get the global application config
pub fn get_config() -> &'static AppConfig {
    APP_CONFIG.get_or_init(AppConfig::load)
}

/// Get the global application secrets
pub fn get_secrets() -> &'static AppSecrets {
    APP_SECRETS.get_or_init(AppSecrets::load)
}

/// Root application configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub update_check: UpdateCheckConfig,
    #[serde(default)]
    pub discogs: DiscogsConfig,
    /// UI log verbosity. One of: error, warn, info, debug, trace, off.
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Application secrets (loaded from secrets.json)
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct AppSecrets {
    #[serde(default)]
    pub discogs: DiscogsSecrets,
}

/// Discogs API secrets
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct DiscogsSecrets {
    /// Consumer Key (API Key)
    #[serde(default)]
    pub consumer_key: String,
    /// Consumer Secret
    #[serde(default)]
    pub consumer_secret: String,
}

impl DiscogsSecrets {
    /// Check if API credentials are configured
    pub fn has_credentials(&self) -> bool {
        !self.consumer_key.is_empty() && !self.consumer_secret.is_empty()
    }
}

impl AppSecrets {
    /// Load secrets from the per-user `secrets.json`.
    pub fn load() -> Self {
        let path = match secrets_file_path() {
            Ok(p) => p,
            Err(e) => {
                log::warn!("{e}; Discogs API will use anonymous access");
                return Self::default();
            }
        };
        match fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<AppSecrets>(&content) {
                Ok(secrets) => {
                    log::info!("Loaded secrets from {}", path.display());
                    secrets
                }
                Err(e) => {
                    log::warn!("Failed to parse {}: {e}", path.display());
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::info!(
                    "No secrets.json at {}, Discogs API will use anonymous access",
                    path.display()
                );
                Self::default()
            }
            Err(e) => {
                log::warn!("Failed to read {}: {e}", path.display());
                Self::default()
            }
        }
    }
}

/// Update check configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct UpdateCheckConfig {
    #[serde(default = "default_update_enabled")]
    pub enabled: bool,
    #[serde(default = "default_repository_url")]
    pub repository_url: String,
}

fn default_update_enabled() -> bool {
    true
}

fn default_repository_url() -> String {
    "https://github.com/danifunker/ODE-artwork-downloader".to_string()
}

impl Default for UpdateCheckConfig {
    fn default() -> Self {
        Self {
            enabled: default_update_enabled(),
            repository_url: default_repository_url(),
        }
    }
}

impl UpdateCheckConfig {
    /// Get the API URL for checking releases
    pub fn api_url(&self) -> String {
        if let Some(path) = self.repository_url.strip_prefix("https://github.com/") {
            format!("https://api.github.com/repos/{}/releases/latest", path.trim_end_matches('/'))
        } else {
            self.repository_url.clone()
        }
    }

    /// Get the releases page URL
    pub fn releases_url(&self) -> String {
        format!("{}/releases", self.repository_url.trim_end_matches('/'))
    }
}

/// Discogs API configuration (URLs only - secrets are in secrets.json)
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct DiscogsConfig {
    /// Request Token URL (OAuth 1.0)
    #[serde(default = "default_request_token_url")]
    pub request_token_url: String,
    /// Authorize URL (OAuth 1.0)
    #[serde(default = "default_authorize_url")]
    pub authorize_url: String,
    /// Access Token URL (OAuth 1.0)
    #[serde(default = "default_access_token_url")]
    pub access_token_url: String,
}

fn default_request_token_url() -> String {
    "https://api.discogs.com/oauth/request_token".to_string()
}

fn default_authorize_url() -> String {
    "https://www.discogs.com/oauth/authorize".to_string()
}

fn default_access_token_url() -> String {
    "https://api.discogs.com/oauth/access_token".to_string()
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            update_check: UpdateCheckConfig::default(),
            discogs: DiscogsConfig::default(),
            log_level: default_log_level(),
        }
    }
}

impl AppConfig {
    /// Load configuration from the per-user `config.json`.
    pub fn load() -> Self {
        let path = match config_file_path() {
            Ok(p) => p,
            Err(e) => {
                log::warn!("{e}; using default config");
                return Self::default();
            }
        };
        match fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<AppConfig>(&content) {
                Ok(config) => {
                    log::info!("Loaded config from {}", path.display());
                    config
                }
                Err(e) => {
                    log::warn!("Failed to parse {}: {e}", path.display());
                    Self::default()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                log::info!("No config.json at {}, using defaults", path.display());
                Self::default()
            }
            Err(e) => {
                log::warn!("Failed to read {}: {e}", path.display());
                Self::default()
            }
        }
    }
}
