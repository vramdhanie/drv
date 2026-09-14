use anyhow::{bail, Context, Result};
use colored::Colorize;
use serde_json::json;

use crate::auth;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const MODEL: &str = "claude-opus-5";

/// One Messages API call: question + retrieved excerpts in, answer out.
/// Server-side refusal fallbacks are enabled so a declined request degrades
/// to another model instead of failing outright.
pub fn ask(system: &str, user: &str, model: Option<&str>) -> Result<String> {
    let key = auth::keychain_get("anthropic-api-key")?.context(format!(
        "no Anthropic API key stored — add one with {}",
        "drv auth claude".green()
    ))?;

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
