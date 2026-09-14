use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Non-secret configuration, stored at ~/.config/drv/config.toml.
/// Secrets (client secret, refresh tokens, Claude key) live in the Keychain.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    /// User-supplied OAuth client ID (bring-your-own-client mode).
    pub client_id: Option<String>,
    /// Known account aliases (each has a refresh token in the Keychain).
    #[serde(default)]
    pub accounts: Vec<String>,
    /// The account commands act on when no --account flag is given.
    pub active_account: Option<String>,
}

pub fn config_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("could not determine config directory")?
        .join("drv");
    Ok(dir.join("config.toml"))
}

/// Per-account local data (the semantic index) lives here.
pub fn data_dir(account: &str) -> Result<PathBuf> {
    let dir = dirs::data_dir()
        .context("could not determine data directory")?
        .join("drv")
        .join(account);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
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

/// Which account should this invocation use? Flag > active > only one known.
pub fn resolve_account(flag: Option<&str>) -> Result<String> {
    let cfg = load()?;
    if let Some(alias) = flag {
        if !cfg.accounts.iter().any(|a| a == alias) {
            bail!(
                "unknown account '{alias}' — known accounts: {}",
                if cfg.accounts.is_empty() { "(none)".into() } else { cfg.accounts.join(", ") }
            );
        }
        return Ok(alias.to_string());
    }
    if let Some(active) = cfg.active_account {
        return Ok(active);
    }
    match cfg.accounts.len() {
        0 => bail!("not signed in — run `drv auth login` first"),
        1 => Ok(cfg.accounts[0].clone()),
        _ => bail!(
            "multiple accounts ({}) and no active one — pick with `drv account use <alias>` or pass --account",
            cfg.accounts.join(", ")
        ),
    }
}

/// Register an account after a successful login and make it active if it is
/// the first (or re-activate it if it already existed).
pub fn register_account(alias: &str) -> Result<()> {
    let mut cfg = load()?;
    if !cfg.accounts.iter().any(|a| a == alias) {
        cfg.accounts.push(alias.to_string());
    }
    if cfg.active_account.is_none() || cfg.accounts.len() == 1 {
        cfg.active_account = Some(alias.to_string());
    }
    save(&cfg)
}
