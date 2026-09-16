use anyhow::{bail, Context, Result};
use colored::Colorize;
use serde_json::json;

use crate::auth;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-opus-5";

/// Answer a question over retrieved excerpts. Two backends:
/// a stored API key wins; otherwise the local Claude Code CLI (`claude -p`)
/// is used, riding the user's existing subscription.
pub fn ask(system: &str, user: &str, model: Option<&str>) -> Result<String> {
    match auth::load_secrets()?.anthropic_api_key {
        Some(key) => ask_api(&key, system, user, model),
        None if claude_cli_available() => ask_claude_code(system, user, model),
        None => anyhow::bail!(
            "no way to reach Claude — store an API key with {} or install Claude Code (the `claude` CLI)",
            "drv auth claude".green()
        ),
    }
}

pub fn claude_cli_available() -> bool {
    std::process::Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Non-interactive Claude Code: prompt on stdin, plain text out.
fn ask_claude_code(system: &str, user: &str, model: Option<&str>) -> Result<String> {
    use std::io::Write;
    let mut cmd = std::process::Command::new("claude");
    cmd.args(["-p", "--output-format", "text"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(model) = model {
        cmd.args(["--model", model]);
    }
    let mut child = cmd.spawn().context("launching the claude CLI")?;
    child
        .stdin
        .take()
        .context("claude stdin unavailable")?
        .write_all(format!("{system}\n\n{user}").as_bytes())
        .context("writing prompt to claude")?;
    let out = child.wait_with_output().context("waiting for claude")?;
    if !out.status.success() {
        bail!(
            "claude CLI failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        bail!("claude CLI returned no output");
    }
    Ok(text)
}

/// Direct Messages API call. Server-side refusal fallbacks are enabled so a
/// declined request degrades to another model instead of failing outright.
fn ask_api(key: &str, system: &str, user: &str, model: Option<&str>) -> Result<String> {

    let body = json!({
        "model": model.unwrap_or(MODEL),
        "max_tokens": 16000,
        "system": system,
        "fallbacks": "default",
        "messages": [{ "role": "user", "content": user }],
    });

    let resp = reqwest::blocking::Client::new()
        .post(API_URL)
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "server-side-fallback-2026-07-01")
        .json(&body)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .context("calling the Claude API")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or(body);
        bail!("Claude API {status}: {msg}");
    }

    let v: serde_json::Value = resp.json().context("parsing Claude response")?;
    if v["stop_reason"].as_str() == Some("refusal") {
        bail!(
            "Claude declined this request ({})",
            v["stop_details"]["explanation"].as_str().unwrap_or("no detail")
        );
    }
    let text: String = v["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if text.is_empty() {
        bail!("Claude returned no text (stop_reason: {})", v["stop_reason"]);
    }
    Ok(text)
}
