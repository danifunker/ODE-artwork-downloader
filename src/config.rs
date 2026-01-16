//! Application configuration
//!
//! Handles loading and managing configuration from config.json and secrets.json

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Global application config
static APP_CONFIG: OnceLock<AppConfig> = OnceLock::new();

/// Global secrets
static APP_SECRETS: OnceLock<AppSecrets> = OnceLock::new();

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
    /// Load secrets from secrets.json
    pub fn load() -> Self {
        // Try to load from current directory first
        if let Ok(secrets) = Self::load_from_path("secrets.json") {
            log::info!("Loaded secrets from ./secrets.json");
            return secrets;
        }

        // Try to load from executable directory
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let secrets_path = exe_dir.join("secrets.json");
                if let Ok(secrets) = Self::load_from_path(&secrets_path) {
                    log::info!("Loaded secrets from {}", secrets_path.display());
                    return secrets;
                }
            }
        }

        log::info!("No secrets.json found, Discogs API will use anonymous access");
        Self::default()
    }

    fn load_from_path(path: impl Into<PathBuf>) -> Result<Self, Box<dyn std::error::Error>> {
        let path = path.into();
        let content = fs::read_to_string(&path)?;
        let secrets: AppSecrets = serde_json::from_str(&content)?;
        Ok(secrets)
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
        }
    }
}

impl AppConfig {
    /// Load configuration from config.json
    pub fn load() -> Self {
        // Try to load from current directory first
        if let Ok(config) = Self::load_from_path("config.json") {
            log::info!("Loaded config from ./config.json");
            return config;
        }

        // Try to load from executable directory
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                let config_path = exe_dir.join("config.json");
                if let Ok(config) = Self::load_from_path(&config_path) {
                    log::info!("Loaded config from {}", config_path.display());
                    return config;
                }
            }
        }

        log::info!("No config.json found, using defaults");
        Self::default()
    }

    fn load_from_path(path: impl Into<PathBuf>) -> Result<Self, Box<dyn std::error::Error>> {
        let path = path.into();
        let content = fs::read_to_string(&path)?;
        let config: AppConfig = serde_json::from_str(&content)?;
        Ok(config)
    }
}
