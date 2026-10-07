//! Gateway configuration: optional TOML file, overridden key by key by the
//! `NX_AZURE_CACHE_<KEY>` environment variables.

use serde::Deserialize;
use std::path::PathBuf;

/// Recognised keys, in spec order. Each one can be overridden from the environment.
const KEYS: &[&str] = &[
    "port",
    "account",
    "container",
    "credential",
    "tenant_id",
    "client_id",
    "managed_client_id",
    "token_store",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub port: u16,
    /// Storage account; without it, the Gateway runs but reports itself unusable.
    pub account: Option<String>,
    pub container: String,
    pub credential: String,
    /// Entra tenant of the `user` Identity.
    pub tenant_id: Option<String>,
    /// Public app registration used by the developer sign-in flows (created by the runbook).
    pub client_id: Option<String>,
    pub managed_client_id: Option<String>,
    pub token_store: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            port: 7484,
            account: None,
            container: "nx-cache".into(),
            credential: "auto".into(),
            tenant_id: None,
            client_id: None,
            managed_client_id: None,
            token_store: "keyring".into(),
        }
    }
}

/// OS configuration directory: `%APPDATA%\nx-azure-cache` on Windows,
/// `$XDG_CONFIG_HOME/nx-azure-cache` (or `~/.config/...`) elsewhere.
pub fn config_dir() -> Result<PathBuf, String> {
    user_dir("APPDATA", "XDG_CONFIG_HOME", ".config")
}

/// Logs, in the user's data directory:
/// `%LOCALAPPDATA%\nx-azure-cache\logs` on Windows,
/// `$XDG_DATA_HOME/nx-azure-cache/logs` (or `~/.local/share/...`) elsewhere.
pub fn log_dir() -> Result<PathBuf, String> {
    Ok(user_dir("LOCALAPPDATA", "XDG_DATA_HOME", ".local/share")?.join("logs"))
}

fn user_dir(windows: &str, xdg: &str, home_default: &str) -> Result<PathBuf, String> {
    let base = if cfg!(windows) {
        std::env::var_os(windows)
            .map(PathBuf::from)
            .ok_or(format!("{windows} is not set"))?
    } else {
        match std::env::var_os(xdg).filter(|v| !v.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => {
                PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?).join(home_default)
            }
        }
    };
    Ok(base.join("nx-azure-cache"))
}

impl Config {
    /// Reads `config.toml` in `dir` if present, then applies the environment overrides.
    pub fn load(dir: &std::path::Path) -> Result<Config, String> {
        let path = dir.join("config.toml");
        let mut table = match std::fs::read_to_string(&path) {
            Ok(text) => text
                .parse::<toml::Table>()
                .map_err(|e| format!("{} is unreadable: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml::Table::new(),
            Err(e) => return Err(format!("{} is unreadable: {e}", path.display())),
        };
        for key in KEYS {
            let var = format!("NX_AZURE_CACHE_{}", key.to_uppercase());
            if let Ok(raw) = std::env::var(&var) {
                let value = if *key == "port" {
                    toml::Value::Integer(raw.parse().map_err(|_| format!("invalid {var}: {raw}"))?)
                } else {
                    toml::Value::String(raw)
                };
                table.insert(key.to_string(), value);
            }
        }
        table.try_into().map_err(|e| format!("invalid config: {e}"))
    }
}
