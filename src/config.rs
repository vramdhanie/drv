use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Non-secret configuration, stored at ~/.config/drv/config.toml.
/// Secrets (client secret, refresh token, Claude key) live in the Keychain.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// User-supplied OAuth client ID (bring-your-own-client mode).
    pub client_id: Option<String>,
}

pub fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("could not determine config directory")?
        .join("drv");
    Ok(dir.join("config.toml"))
}

pub fn load() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

pub fn save(config: &Config) -> Result<()> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, toml::to_string_pretty(config)?)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
