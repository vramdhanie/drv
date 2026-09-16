use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use colored::Colorize;
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use crate::config;

/// Distribution builds bake a default OAuth client in at compile time:
///   DRV_CLIENT_ID=... DRV_CLIENT_SECRET=... cargo build --release
/// (For Google "installed app" clients the secret is not treated as
/// confidential; PKCE protects the flow.) Without them, users bring their
/// own client via `drv auth login --client-id ... --client-secret ...`.
const EMBEDDED_CLIENT_ID: Option<&str> = option_env!("DRV_CLIENT_ID");
const EMBEDDED_CLIENT_SECRET: Option<&str> = option_env!("DRV_CLIENT_SECRET");

const SCOPE: &str = "https://www.googleapis.com/auth/drive";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

const KEYRING_SERVICE: &str = "drv-cli";

fn entry(name: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, name).map_err(|e| anyhow!("keychain: {e}"))
}

fn keychain_get(name: &str) -> Result<Option<String>> {
    match entry(name)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("keychain read ({name}): {e}")),
    }
}

fn keychain_set(name: &str, value: &str) -> Result<()> {
    entry(name)?
        .set_password(value)
        .map_err(|e| anyhow!("keychain write ({name}): {e}"))
}

fn keychain_delete(name: &str) -> Result<()> {
    match entry(name)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow!("keychain delete ({name}): {e}")),
    }
}

pub struct ClientCreds {
    pub id: String,
    pub secret: String,
}

/// Resolve the OAuth client: user-configured (BYO) first, then embedded.
pub fn resolve_client() -> Result<ClientCreds> {
    resolve_client_from(&load_secrets()?)
}

fn resolve_client_from(secrets: &Secrets) -> Result<ClientCreds> {
    let cfg = config::load()?;
    if let Some(id) = cfg.client_id {
        let secret = secrets
            .google_client_secret
            .clone()
            .context("client ID configured but its secret is missing — run `drv auth login --client-id ... --client-secret ...` again")?;
        return Ok(ClientCreds { id, secret });
    }
    match (EMBEDDED_CLIENT_ID, EMBEDDED_CLIENT_SECRET) {
        (Some(id), Some(secret)) => Ok(ClientCreds { id: id.into(), secret: secret.into() }),
        _ => bail!(
            "no OAuth client available. This build has no embedded client, so bring your own:\n  \
             1. console.cloud.google.com → create a project → enable the Google Drive API\n  \
             2. OAuth consent screen → External → publish to production\n  \
             3. Credentials → Create credentials → OAuth client ID → Desktop app\n  \
             4. drv auth login --client-id <ID> --client-secret <SECRET>"
        ),
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
}

/// All of drv's secrets live in ONE Keychain item, so macOS asks for
/// permission once per (re)built binary instead of once per secret.
#[derive(Default, serde::Serialize, Deserialize)]
pub struct Secrets {
    pub google_client_secret: Option<String>,
    pub anthropic_api_key: Option<String>,
    #[serde(default)]
    pub refresh_tokens: std::collections::HashMap<String, String>,
}

const SECRETS_ENTRY: &str = "secrets";

pub fn load_secrets() -> Result<Secrets> {
    if let Some(json) = keychain_get(SECRETS_ENTRY)? {
        return Ok(serde_json::from_str(&json).unwrap_or_default());
    }
    // Migrate the per-secret entries earlier versions created.
    let mut secrets = Secrets::default();
    let mut migrated = false;
    if let Some(v) = keychain_get("google-client-secret")? {
        secrets.google_client_secret = Some(v);
        migrated = true;
    }
    if let Some(v) = keychain_get("anthropic-api-key")? {
        secrets.anthropic_api_key = Some(v);
        migrated = true;
    }
    // v0.1 single-account entry becomes the "default" alias…
    if let Some(v) = keychain_get("google-refresh-token")? {
        secrets.refresh_tokens.insert("default".into(), v);
        crate::config::register_account("default")?;
        migrated = true;
    }
    // …and v0.2 per-alias entries carry their alias over.
    for alias in crate::config::load()?.accounts {
        if let Some(v) = keychain_get(&format!("google-refresh-token:{alias}"))? {
            secrets.refresh_tokens.insert(alias, v);
            migrated = true;
        }
    }
    if migrated {
        save_secrets(&secrets)?;
        for name in ["google-client-secret", "anthropic-api-key", "google-refresh-token"] {
            keychain_delete(name)?;
        }
        for alias in secrets.refresh_tokens.keys() {
            keychain_delete(&format!("google-refresh-token:{alias}"))?;
        }
    }
    Ok(secrets)
}

pub fn save_secrets(secrets: &Secrets) -> Result<()> {
    keychain_set(SECRETS_ENTRY, &serde_json::to_string(secrets)?)
}

pub fn store_refresh(account: &str, refresh: &str) -> Result<()> {
    let mut secrets = load_secrets()?;
    secrets.refresh_tokens.insert(account.into(), refresh.into());
    save_secrets(&secrets)
}

pub fn forget_account(account: &str) -> Result<()> {
    let mut secrets = load_secrets()?;
    secrets.refresh_tokens.remove(account);
    save_secrets(&secrets)
}

/// Run the browser PKCE flow. Returns (refresh_token, access_token) so the
/// caller can identify the account before deciding what to store it under.
pub fn login(client: &ClientCreds) -> Result<(String, String)> {
    // PKCE verifier + challenge
    let mut bytes = [0u8; 64];
    rand::thread_rng().fill_bytes(&mut bytes);
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));

    let listener = TcpListener::bind("127.0.0.1:0").context("binding loopback port")?;
    let port = listener.local_addr()?.port();
    let redirect = format!("http://127.0.0.1:{port}");

    let url = format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        urlencoding::encode(&client.id),
        urlencoding::encode(&redirect),
        urlencoding::encode(SCOPE),
        challenge,
    );

    println!("Opening your browser to sign in to Google…");
    println!("(If it doesn't open, visit:\n  {url}\n)");
    let _ = open::that(&url);

    let code = wait_for_code(&listener)?;

    let resp: TokenResponse = reqwest::blocking::Client::new()
        .post(TOKEN_URL)
        .form(&[
            ("code", code.as_str()),
            ("client_id", &client.id),
            ("client_secret", &client.secret),
            ("redirect_uri", &redirect),
            ("grant_type", "authorization_code"),
            ("code_verifier", &verifier),
        ])
        .send()
        .context("exchanging authorization code")?
        .error_for_status()
        .context("token exchange rejected")?
        .json()
        .context("parsing token response")?;

    let refresh = resp
        .refresh_token
        .context("Google did not return a refresh token (try revoking access at myaccount.google.com/permissions and logging in again)")?;
    Ok((refresh, resp.access_token))
}

/// Block until the loopback server receives the OAuth redirect.
fn wait_for_code(listener: &TcpListener) -> Result<String> {
    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut line = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut line)?;
        // "GET /?code=...&scope=... HTTP/1.1"
        let query = line
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.split_once('?'))
            .map(|(_, q)| q.to_string())
            .unwrap_or_default();
        let mut code = None;
        let mut error = None;
        for pair in query.split('&') {
            match pair.split_once('=') {
                Some(("code", v)) => code = Some(v.to_string()),
                Some(("error", v)) => error = Some(v.to_string()),
                _ => {}
            }
        }
        let body = if code.is_some() {
            "<h2>Signed in.</h2><p>You can close this tab and return to the terminal.</p>"
        } else {
            "<h2>Sign-in failed.</h2><p>Return to the terminal for details.</p>"
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        if let Some(code) = code {
            return Ok(code);
        }
        if let Some(error) = error {
            bail!("authorization denied: {error}");
        }
        // Ignore stray requests (favicon etc.) and keep listening.
    }
    bail!("loopback listener closed unexpectedly")
}

/// Exchange the stored refresh token of an account for a fresh access token.
/// One Keychain read covers both the token and the client secret.
pub fn access_token(account: &str) -> Result<String> {
    let secrets = load_secrets()?;
    let refresh = secrets.refresh_tokens.get(account).cloned().context(format!(
        "account '{account}' has no stored credentials — run {}",
        "drv auth login".green()
    ))?;
    let client = resolve_client_from(&secrets)?;
    let resp = reqwest::blocking::Client::new()
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client.id.as_str()),
            ("client_secret", &client.secret),
            ("refresh_token", &refresh),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .context("refreshing access token")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!(
            "token refresh failed ({status}): {body}\nIf access was revoked, run `drv auth login` again."
        );
    }
    #[derive(Deserialize)]
    struct Refreshed {
        access_token: String,
    }
    let tok: Refreshed = resp.json().context("parsing refresh response")?;
    Ok(tok.access_token)
}
