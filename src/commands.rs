use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::auth::{self, ClientCreds};
use crate::config;
use crate::drive::{Drive, DriveFile};
use crate::ui;

// ---------- auth ----------

pub fn auth_login(client_id: Option<String>, client_secret: Option<String>) -> Result<()> {
    let creds = if let (Some(id), Some(secret)) = (client_id, client_secret) {
        // Bring-your-own client: persist it for future runs.
        let mut cfg = config::load()?;
        cfg.client_id = Some(id.clone());
        config::save(&cfg)?;
        auth::keychain_set("google-client-secret", &secret)?;
        println!("Using your own OAuth client ({}).", "saved for future runs".dimmed());
        ClientCreds { id, secret }
    } else {
        auth::resolve_client()?
    };

    auth::login(&creds)?;
    println!("{} signed in to Google.", "✓".green().bold());

    let drive = Drive::connect()?;
    let about = drive.about()?;
    if let Some(email) = about["user"]["emailAddress"].as_str() {
        println!("  account: {}", email.bold());
    }
    Ok(())
}

pub fn auth_status() -> Result<()> {
    if auth::keychain_get("google-refresh-token")?.is_none() {
        println!("{} not signed in — run {}.", "✗".red(), "drv auth login".green());
    } else {
        let drive = Drive::connect()?;
        let about = drive.about()?;
        let user = &about["user"];
        println!(
            "{} signed in as {} <{}>",
            "✓".green().bold(),
            user["displayName"].as_str().unwrap_or("?").bold(),
            user["emailAddress"].as_str().unwrap_or("?"),
        );
        let quota = &about["storageQuota"];
        if let (Some(usage), Some(limit)) = (
            quota["usage"].as_str().and_then(|s| s.parse::<u64>().ok()),
            quota["limit"].as_str().and_then(|s| s.parse::<u64>().ok()),
        ) {
            println!(
                "  storage: {} of {} used",
                ui::human_size(usage),
                ui::human_size(limit)
            );
        }
    }
    let claude = auth::keychain_get("anthropic-api-key")?.is_some();
    println!(
        "{} Claude API key {}",
        if claude { "✓".green().bold() } else { "○".dimmed() },
        if claude { "stored (used by v0.2 search/prompt)".into() } else {
            format!("not set — optional, add with {}", "drv auth claude".green())
        }
    );
    Ok(())
}

pub fn auth_logout() -> Result<()> {
    auth::keychain_delete("google-refresh-token")?;
    println!("{} Google credentials removed from the Keychain.", "✓".green().bold());
    println!(
        "  (To fully revoke access: {})",
        "myaccount.google.com/permissions".underline()
    );
    Ok(())
}

pub fn auth_claude() -> Result<()> {
    let key = rpassword::prompt_password("Anthropic API key (input hidden): ")
        .context("reading key")?;
    let key = key.trim();
    if key.is_empty() {
        bail!("no key entered");
    }
    if !key.starts_with("sk-ant-") {
        println!("{} key doesn't look like an Anthropic key (sk-ant-…) — storing anyway.", "note:".yellow());
    }
    auth::keychain_set("anthropic-api-key", key)?;
    println!("{} Claude API key stored in the Keychain.", "✓".green().bold());
    Ok(())
}

// ---------- ls ----------

pub fn ls(path: Option<&str>, recursive: bool, long: bool) -> Result<()> {
    let drive = Drive::connect()?;
    let target = drive.resolve(path.unwrap_or(""))?;
    if !target.is_folder() {
        print_entry(&target, long, 0);
        return Ok(());
    }
    if recursive {
        print_tree(&drive, &target.id, long, 0)?;
    } else {
        let children = drive.list_children(&target.id)?;
        if children.is_empty() {
            println!("{}", "(empty)".dimmed());
        }
        for file in &children {
            print_entry(file, long, 0);
        }
    }
    Ok(())
}

fn print_tree(drive: &Drive, folder_id: &str, long: bool, depth: usize) -> Result<()> {
    for file in drive.list_children(folder_id)? {
        print_entry(&file, long, depth);
        if file.is_folder() {
            print_tree(drive, &file.id, long, depth + 1)?;
        }
    }
    Ok(())
}

fn print_entry(file: &DriveFile, long: bool, depth: usize) {
    let indent = "  ".repeat(depth);
    if long {
        let size = file
            .size_bytes()
            .map(ui::human_size)
            .unwrap_or_else(|| "-".into());
        let modified = file
            .modified_time
            .as_deref()
            .map(ui::short_time)
            .unwrap_or_default();
        println!(
            "{:>9}  {:16}  {}  {}{}",
            size,
            modified.dimmed(),
            format!("id:{}", file.id).dimmed(),
            indent,
            ui::painted_name(file)
        );
    } else {
        println!("{indent}{}", ui::painted_name(file));
    }
}

// ---------- share ----------

pub fn share(path: &str, email: &str, role: &str, notify: bool) -> Result<()> {
    let api_role = match role {
        "viewer" => "reader",
        "commenter" => "commenter",
        "editor" => "writer",
        other => bail!("unknown role '{other}'"),
    };
    let drive = Drive::connect()?;
    let file = drive.resolve(path)?;
    drive.share(&file.id, email, api_role, notify)?;
    println!(
        "{} shared {} with {} as {}{}",
        "✓".green().bold(),
        file.name.bold(),
        email.bold(),
        role,
        if notify { " (notification sent)" } else { "" }
    );
    Ok(())
}

// ---------- cp ----------

pub fn cp(path: &str, new_name: Option<&str>, to: Option<&str>) -> Result<()> {
    let drive = Drive::connect()?;
    let source = drive.resolve(path)?;
    if source.is_folder() {
        bail!("Drive cannot server-side copy folders — copy individual files, or download and re-upload");
    }
    let parent = match to {
        Some(dest) => {
            let folder = drive.resolve(dest)?;
            if !folder.is_folder() {
                bail!("--to target '{dest}' is not a folder");
            }
            Some(folder.id)
        }
        None => None,
    };
    let copy = drive.copy(&source.id, new_name, parent.as_deref())?;
    println!(
        "{} copied {} → {} ({})",
        "✓".green().bold(),
        source.name,
        copy.name.bold(),
        format!("id:{}", copy.id).dimmed()
    );
    Ok(())
}

// ---------- upload ----------

pub fn upload(files: &[PathBuf], to: Option<&str>) -> Result<()> {
    let drive = Drive::connect()?;
    let parent = match to {
        Some(dest) => {
            let folder = drive.resolve(dest)?;
            if !folder.is_folder() {
                bail!("--to target '{dest}' is not a folder");
            }
            Some(folder.id)
        }
        None => None,
    };

    for local in files {
        let name = local
            .file_name()
            .and_then(|n| n.to_str())
            .with_context(|| format!("bad file name: {}", local.display()))?
            .to_string();
        let file = std::fs::File::open(local)
            .with_context(|| format!("opening {}", local.display()))?;
        let len = file.metadata()?.len();
        let content_type = guess_mime(local);

        let bar = ui::transfer_bar(len, &name);
        let reader = ui::ProgressReader::new(file, bar.clone());
        let body = reqwest::blocking::Body::sized(reader, len);
        let uploaded = drive.upload(&name, parent.as_deref(), content_type, body, len)?;
        bar.finish_and_clear();
        println!(
            "{} uploaded {} ({}, {})",
            "✓".green().bold(),
            uploaded.name.bold(),
            ui::human_size(len),
            format!("id:{}", uploaded.id).dimmed()
        );
    }
    Ok(())
}

fn guess_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "txt" | "md" => "text/plain",
        "html" => "text/html",
        "csv" => "text/csv",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
}

// ---------- download ----------

pub fn download(paths: &[String], out: Option<&Path>) -> Result<()> {
    let drive = Drive::connect()?;
    let out_dir = out.unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("creating {}", out_dir.display()))?;

    for path in paths {
        let file = drive.resolve(path)?;
        if file.is_folder() {
            bail!("'{path}' is a folder — download individual files (recursive download is planned)");
        }
        let (mut resp, export_ext) = drive.download(&file)?;

        let mut file_name = file.name.clone();
        if let Some(ext) = export_ext {
            if !file_name.to_lowercase().ends_with(&format!(".{ext}")) {
                file_name = format!("{file_name}.{ext}");
            }
        }
        let dest = out_dir.join(&file_name);

        let len = resp
            .content_length()
            .or(file.size_bytes())
            .unwrap_or(0);
        let bar = ui::transfer_bar(len, &file_name);
        let mut writer = std::fs::File::create(&dest)
            .with_context(|| format!("creating {}", dest.display()))?;
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = resp.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut writer, &buf[..n])?;
            bar.inc(n as u64);
        }
        bar.finish_and_clear();
        let exported = export_ext.map(|e| format!(" (exported as .{e})")).unwrap_or_default();
        println!(
            "{} downloaded {}{}",
            "✓".green().bold(),
            dest.display().to_string().bold(),
            exported
        );
    }
    Ok(())
}
