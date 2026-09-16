use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::auth::{self, ClientCreds};
use crate::claude;
use crate::config;
use crate::drive::{Drive, DriveFile};
use crate::embed::Embedder;
use crate::extract;
use crate::store::Store;
use crate::ui;

/// Resolve which account this invocation acts on. Loading secrets first
/// runs any migration of older per-secret Keychain entries.
fn account_for(flag: Option<&str>) -> Result<String> {
    let _ = auth::load_secrets()?;
    config::resolve_account(flag)
}

fn connect(flag: Option<&str>) -> Result<(String, Drive)> {
    let account = account_for(flag)?;
    let drive = Drive::connect(&account)?;
    Ok((account, drive))
}

// ---------- auth ----------

pub fn auth_login(
    client_id: Option<String>,
    client_secret: Option<String>,
    alias: Option<String>,
) -> Result<()> {
    let creds = if let (Some(id), Some(secret)) = (client_id, client_secret) {
        // Bring-your-own client: persist it for future runs.
        let mut cfg = config::load()?;
        cfg.client_id = Some(id.clone());
        config::save(&cfg)?;
        let mut secrets = auth::load_secrets()?;
        secrets.google_client_secret = Some(secret.clone());
        auth::save_secrets(&secrets)?;
        println!("Using your own OAuth client ({}).", "saved for future runs".dimmed());
        ClientCreds { id, secret }
    } else {
        auth::resolve_client()?
    };

    let (refresh, access) = auth::login(&creds)?;

    // Identify the account so it can be stored under a sensible alias.
    let drive = Drive::with_token(access);
    let about = drive.about()?;
    let email = about["user"]["emailAddress"].as_str().unwrap_or("unknown").to_string();
    let alias = alias.unwrap_or_else(|| email.clone());

    auth::store_refresh(&alias, &refresh)?;
    config::register_account(&alias)?;
    println!("{} signed in as {} (account alias: {})", "✓".green().bold(), email.bold(), alias.bold());
    let active = config::load()?.active_account;
    if active.as_deref() != Some(alias.as_str()) {
        println!(
            "  (active account is still {}; switch with {})",
            active.unwrap_or_default(),
            format!("drv account use {alias}").green()
        );
    }
    Ok(())
}

pub fn auth_status(flag: Option<&str>) -> Result<()> {
    let secrets = auth::load_secrets()?;
    let cfg = config::load()?;
    if cfg.accounts.is_empty() {
        println!("{} not signed in — run {}.", "✗".red(), "drv auth login".green());
    } else {
        for alias in &cfg.accounts {
            // With a flag, only report that one account.
            if flag.is_some() && flag != Some(alias.as_str()) {
                continue;
            }
            let active = cfg.active_account.as_deref() == Some(alias.as_str());
            match Drive::connect(alias).and_then(|d| d.about()) {
                Ok(about) => {
                    let user = &about["user"];
                    let quota = &about["storageQuota"];
                    let storage = match (
                        quota["usage"].as_str().and_then(|s| s.parse::<u64>().ok()),
                        quota["limit"].as_str().and_then(|s| s.parse::<u64>().ok()),
                    ) {
                        (Some(u), Some(l)) => {
                            format!(", {} of {} used", ui::human_size(u), ui::human_size(l))
                        }
                        _ => String::new(),
                    };
                    println!(
                        "{} {}{} — {} <{}>{}",
                        "✓".green().bold(),
                        alias.bold(),
                        if active { " (active)".green().to_string() } else { String::new() },
                        user["displayName"].as_str().unwrap_or("?"),
                        user["emailAddress"].as_str().unwrap_or("?"),
                        storage,
                    );
                }
                Err(e) => println!("{} {} — {e:#}", "✗".red(), alias.bold()),
            }
        }
    }
    let claude_key = secrets.anthropic_api_key.is_some();
    let claude_cli = !claude_key && claude::claude_cli_available();
    println!(
        "{} Claude {}",
        if claude_key || claude_cli { "✓".green().bold() } else { "○".dimmed() },
        if claude_key {
            "API key stored (used by prompt)".into()
        } else if claude_cli {
            "via your local Claude Code CLI (no API key needed)".into()
        } else {
            format!("not available — add a key with {} or install Claude Code", "drv auth claude".green())
        }
    );
    Ok(())
}

pub fn auth_logout(flag: Option<&str>) -> Result<()> {
    let account = account_for(flag)?;
    auth::forget_account(&account)?;
    let mut cfg = config::load()?;
    cfg.accounts.retain(|a| a != &account);
    if cfg.active_account.as_deref() == Some(account.as_str()) {
        cfg.active_account = cfg.accounts.first().cloned();
    }
    config::save(&cfg)?;
    println!("{} removed credentials for {}.", "✓".green().bold(), account.bold());
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
    let mut secrets = auth::load_secrets()?;
    secrets.anthropic_api_key = Some(key.to_string());
    auth::save_secrets(&secrets)?;
    println!("{} Claude API key stored in the Keychain.", "✓".green().bold());
    Ok(())
}

// ---------- account ----------

pub fn account_list() -> Result<()> {
    let _ = auth::load_secrets()?;
    let cfg = config::load()?;
    if cfg.accounts.is_empty() {
        println!("No accounts — run {}.", "drv auth login".green());
        return Ok(());
    }
    for alias in &cfg.accounts {
        let marker = if cfg.active_account.as_deref() == Some(alias.as_str()) {
            "●".green().to_string()
        } else {
            "○".dimmed().to_string()
        };
        println!("{marker} {alias}");
    }
    Ok(())
}

pub fn account_use(alias: &str) -> Result<()> {
    let mut cfg = config::load()?;
    if !cfg.accounts.iter().any(|a| a == alias) {
        bail!(
            "unknown account '{alias}' — known: {}",
            if cfg.accounts.is_empty() { "(none)".into() } else { cfg.accounts.join(", ") }
        );
    }
    cfg.active_account = Some(alias.to_string());
    config::save(&cfg)?;
    println!("{} active account is now {}.", "✓".green().bold(), alias.bold());
    Ok(())
}

// ---------- ls ----------

pub fn ls(account: Option<&str>, path: Option<&str>, recursive: bool, long: bool) -> Result<()> {
    let (_, drive) = connect(account)?;
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

// ---------- browse ----------

/// Interactive Drive browser: arrow keys / type-to-filter to scroll,
/// Enter opens a folder (or shows a file's details), ".." walks back up,
/// Esc leaves. Shared items and shortcuts are marked.
pub fn browse(account: Option<&str>) -> Result<()> {
    let (_, drive) = connect(account)?;
    let root = drive.get_file("root")?;
    // Breadcrumb stack of (folder id, display name).
    let mut stack: Vec<(String, String)> = vec![(root.id, "My Drive".into())];

    loop {
        let (current_id, _) = stack.last().cloned().unwrap();
        let children = drive.list_children(&current_id)?;

        enum Entry {
            Up,
            Multi,
            Item(usize),
        }
        let mut entries: Vec<Entry> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        if stack.len() > 1 {
            entries.push(Entry::Up);
            labels.push("⬑ ..".into());
        }
        if !children.is_empty() {
            entries.push(Entry::Multi);
            labels.push("☑ select multiple…".into());
        }
        for (i, f) in children.iter().enumerate() {
            entries.push(Entry::Item(i));
            labels.push(item_label(f));
        }
        if labels.is_empty() {
            println!("{}", "(empty folder)".dimmed());
            stack.pop();
            if stack.is_empty() {
                return Ok(());
            }
            continue;
        }

        let breadcrumb: Vec<&str> = stack.iter().map(|(_, n)| n.as_str()).collect();
        let prompt = format!(
            "{}  {}",
            breadcrumb.join(" / ").bold(),
            "(type to filter · Enter to open · Esc to exit)".dimmed()
        );
        let picked = dialoguer::FuzzySelect::new()
            .with_prompt(prompt)
            .items(&labels)
            .max_length(20)
            .default(0)
            .interact_opt()?;

        let Some(picked) = picked else {
            return Ok(()); // Esc
        };

        let file = match entries[picked] {
            Entry::Up => {
                stack.pop();
                continue;
            }
            Entry::Multi => {
                multi_select_action(&drive, &children)?;
                continue;
            }
            Entry::Item(i) => &children[i],
        };

        if file.is_folder() {
            stack.push((file.id.clone(), file.name.clone()));
        } else if file.is_folder_shortcut() {
            // Following a folder shortcut enters its target.
            if let Some(target) = file.shortcut_details.as_ref().and_then(|d| d.target_id.clone()) {
                stack.push((target, format!("{} ↗", file.name)));
            }
        } else {
            // A file (or file shortcut): show its details and stay here.
            let shown = if file.is_shortcut() {
                // Describe the target, not the pointer.
                match file.shortcut_details.as_ref().and_then(|d| d.target_id.as_deref()) {
                    Some(target) => drive.get_file(target).unwrap_or_else(|_| file.clone()),
                    None => file.clone(),
                }
            } else {
                file.clone()
            };
            println!("\n  {}", file.name.bold());
            if file.is_shortcut() {
                println!("  {} this is a shortcut — the file itself lives elsewhere in Drive", "link:".yellow());
            }
            if file.shared == Some(true) {
                println!("  {} shared", "sharing:".dimmed());
            }
            if let Some(size) = shown.size_bytes() {
                println!("  {} {}", "size:".dimmed(), ui::human_size(size));
            }
            if let Some(modified) = shown.modified_time.as_deref() {
                println!("  {} {}", "modified:".dimmed(), ui::short_time(modified));
            }
            println!("  {} id:{}\n", "id:".dimmed(), shown.id);
        }
    }
}

fn item_label(f: &DriveFile) -> String {
    let icon = if f.is_folder() {
        "📁"
    } else if f.is_shortcut() {
        "🔗"
    } else {
        "· "
    };
    let mut label = format!("{icon} {}", f.name);
    if f.is_shortcut() {
        label.push_str("  (link — not stored here)");
    }
    if f.shared == Some(true) {
        label.push_str("  (shared)");
    }
    label
}

/// Tick items in the current folder, pick an action, run it on all of them.
fn multi_select_action(drive: &Drive, children: &[DriveFile]) -> Result<()> {
    let labels: Vec<String> = children.iter().map(item_label).collect();
    let Some(picked) = dialoguer::MultiSelect::new()
        .with_prompt("Space to tick, Enter to confirm, Esc to cancel")
        .items(&labels)
        .max_length(20)
        .interact_opt()?
    else {
        return Ok(());
    };
    if picked.is_empty() {
        return Ok(());
    }
    let selection: Vec<&DriveFile> = picked.iter().map(|&i| &children[i]).collect();
    let has_folder = selection.iter().any(|f| f.is_folder());

    let mut actions: Vec<&str> = vec!["Move to…", "Trash"];
    if !has_folder {
        actions.insert(0, "Download");
        actions.insert(2, "Share");
    }
    actions.push("Cancel");
    let Some(action) = dialoguer::Select::new()
        .with_prompt(format!("{} item(s) selected — action", selection.len()))
        .items(&actions)
        .default(0)
        .interact_opt()?
    else {
        return Ok(());
    };

    match actions[action] {
        "Download" => {
            let dir: String = dialoguer::Input::new()
                .with_prompt("Download into")
                .default(".".to_string())
                .interact_text()?;
            let out_dir = Path::new(&dir);
            std::fs::create_dir_all(out_dir)?;
            for f in &selection {
                save_file(drive, f, out_dir)?;
            }
        }
        "Share" => {
            let email: String = dialoguer::Input::new()
                .with_prompt("Share with (email)")
                .interact_text()?;
            let roles = ["viewer", "commenter", "editor"];
            let role = dialoguer::Select::new()
                .with_prompt("Role")
                .items(roles)
                .default(0)
                .interact()?;
            let api_role = ["reader", "commenter", "writer"][role];
            for f in &selection {
                drive.share(&f.id, &email, api_role, false)?;
                println!("{} shared {} with {}", "✓".green().bold(), f.name.bold(), email);
            }
        }
        "Move to…" => {
            if let Some((target_id, target_path)) = pick_folder(drive)? {
                for f in &selection {
                    let from = f.parents.as_ref().and_then(|p| p.first().cloned());
                    drive.move_file(&f.id, Some(&target_id), from.as_deref(), None)?;
                    println!("{} moved {} → {}", "✓".green().bold(), f.name.bold(), target_path.bold());
                }
            }
        }
        "Trash" => {
            let names: Vec<&str> = selection.iter().map(|f| f.name.as_str()).collect();
            let sure = dialoguer::Confirm::new()
                .with_prompt(format!("Move to Trash: {}?", names.join(", ")))
                .default(false)
                .interact()?;
            if sure {
                for f in &selection {
                    drive.trash(&f.id)?;
                    println!("{} trashed {} (recoverable ~30 days)", "✓".green().bold(), f.name.bold());
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Navigate to a destination folder; returns (id, breadcrumb) or None.
fn pick_folder(drive: &Drive) -> Result<Option<(String, String)>> {
    let root = drive.get_file("root")?;
    let mut stack: Vec<(String, String)> = vec![(root.id, "My Drive".into())];
    loop {
        let (current_id, _) = stack.last().cloned().unwrap();
        let folders: Vec<DriveFile> = drive
            .list_children(&current_id)?
            .into_iter()
            .filter(|f| f.is_folder())
            .collect();

        let mut labels: Vec<String> = vec!["✔ move here".into()];
        if stack.len() > 1 {
            labels.push("⬑ ..".into());
        }
        labels.extend(folders.iter().map(|f| format!("📁 {}", f.name)));

        let breadcrumb: Vec<&str> = stack.iter().map(|(_, n)| n.as_str()).collect();
        let Some(picked) = dialoguer::FuzzySelect::new()
            .with_prompt(format!("Destination: {}", breadcrumb.join(" / ").bold()))
            .items(&labels)
            .max_length(20)
            .default(0)
            .interact_opt()?
        else {
            return Ok(None);
        };

        let has_up = stack.len() > 1;
        if picked == 0 {
            let path: Vec<&str> = stack.iter().map(|(_, n)| n.as_str()).collect();
            return Ok(Some((stack.last().unwrap().0.clone(), path.join("/"))));
        }
        if has_up && picked == 1 {
            stack.pop();
            continue;
        }
        let folder = &folders[picked - 1 - usize::from(has_up)];
        stack.push((folder.id.clone(), folder.name.clone()));
    }
}

// ---------- share ----------

pub fn share(account: Option<&str>, path: &str, email: &str, role: &str, notify: bool) -> Result<()> {
    let api_role = match role {
        "viewer" => "reader",
        "commenter" => "commenter",
        "editor" => "writer",
        other => bail!("unknown role '{other}'"),
    };
    let (_, drive) = connect(account)?;
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

pub fn cp(account: Option<&str>, path: &str, new_name: Option<&str>, to: Option<&str>) -> Result<()> {
    let (_, drive) = connect(account)?;
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

// ---------- cat ----------

/// Follow a shortcut to its target; other files pass through.
fn deref_shortcut(drive: &Drive, file: DriveFile) -> Result<DriveFile> {
    if file.is_shortcut() {
        if let Some(target) = file.shortcut_details.as_ref().and_then(|d| d.target_id.as_deref()) {
            return drive.get_file(target);
        }
    }
    Ok(file)
}

pub fn cat(account: Option<&str>, path: &str) -> Result<()> {
    use std::io::{IsTerminal, Write};
    let (_, drive) = connect(account)?;
    let file = deref_shortcut(&drive, drive.resolve(path)?)?;
    if file.is_folder() {
        bail!("'{path}' is a folder — use drv ls");
    }

    // Google-native formats have no raw bytes; print their text export.
    let export = match file.mime_type.as_str() {
        "application/vnd.google-apps.document" | "application/vnd.google-apps.presentation" => {
            Some("text/plain")
        }
        "application/vnd.google-apps.spreadsheet" => Some("text/csv"),
        _ => None,
    };
    let mut stdout = std::io::stdout().lock();
    if let Some(mime) = export {
        let text = drive.export_text(&file.id, mime)?;
        stdout.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
            let _ = writeln!(stdout);
        }
        return Ok(());
    }

    let texty = file.mime_type.starts_with("text/")
        || matches!(
            file.mime_type.as_str(),
            "application/json" | "application/xml" | "application/rtf" | "application/x-sh"
        );
    if !texty && std::io::stdout().is_terminal() {
        bail!(
            "'{}' is {} — refusing to write binary to your terminal.\nRedirect it (drv cat … > file) or use drv download",
            file.name,
            file.mime_type
        );
    }
    // Stream raw bytes: real `cat` semantics, so redirection copies the file.
    let (mut resp, _) = drive.download(&file)?;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = resp.read(&mut buf)?;
        if n == 0 {
            break;
        }
        stdout.write_all(&buf[..n])?;
    }
    Ok(())
}

// ---------- edit / vim ----------

pub fn edit(account: Option<&str>, path: &str) -> Result<()> {
    use sha2::Digest;
    let (_, drive) = connect(account)?;
    let file = deref_shortcut(&drive, drive.resolve(path)?)?;
    if file.is_folder() {
        bail!("'{path}' is a folder");
    }
    if file.mime_type.starts_with("application/vnd.google-apps.") {
        bail!(
            "'{}' is a Google-native document — it has no editable raw content (export-only).\nEdit it at its Drive link, or use drv cat to read its text",
            file.name
        );
    }

    let original = drive.download_bytes(&file.id, 32 * 1024 * 1024)?;
    let before = sha2::Sha256::digest(&original);

    // Keep the original extension so the editor picks the right filetype.
    let tmp = std::env::temp_dir().join(format!("drv-edit-{}-{}", &file.id[..8.min(file.id.len())], file.name));
    std::fs::write(&tmp, &original).with_context(|| format!("writing {}", tmp.display()))?;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vim".into());
    let status = std::process::Command::new(&editor)
        .arg(&tmp)
        .status()
        .with_context(|| format!("launching {editor}"))?;
    if !status.success() {
        let _ = std::fs::remove_file(&tmp);
        bail!("{editor} exited with {status} — nothing uploaded");
    }

    let edited = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    if sha2::Sha256::digest(&edited) == before {
        println!("{}", "no changes — nothing uploaded".dimmed());
        return Ok(());
    }
    let len = edited.len() as u64;
    drive.update_content(&file.id, &file.mime_type, edited)?;
    println!(
        "{} saved {} back to Drive ({})",
        "✓".green().bold(),
        file.name.bold(),
        ui::human_size(len)
    );
    Ok(())
}

// ---------- mv ----------

pub fn mv(account: Option<&str>, source: &str, dest: &str) -> Result<()> {
    let (_, drive) = connect(account)?;
    let src = drive.resolve(source)?;
    let current_parent = src.parents.as_ref().and_then(|p| p.first().cloned());

    // Unix mv semantics: an existing folder destination means "move into
    // it"; otherwise the last path segment is the new name.
    let (target_folder, new_name): (Option<String>, Option<String>) = match drive.resolve(dest) {
        Ok(d) if d.is_folder() => (Some(d.id), None),
        Ok(d) => bail!(
            "destination '{dest}' already exists as a file ({}) — pick a folder or a new name",
            d.name
        ),
        Err(_) => {
            let (parent_path, leaf) = match dest.rsplit_once('/') {
                Some((parent, leaf)) => (parent.to_string(), leaf.to_string()),
                None => (String::new(), dest.to_string()),
            };
            let parent = drive.resolve(&parent_path)?;
            if !parent.is_folder() {
                bail!("'{parent_path}' is not a folder");
            }
            (Some(parent.id), Some(leaf))
        }
    };

    // Skip the re-parenting when the file is already in the target folder
    // (pure rename) — Drive rejects addParents == existing parent politely,
    // but there's no reason to send it.
    let (add, remove) = match (&target_folder, &current_parent) {
        (Some(t), Some(c)) if t == c => (None, None),
        (Some(t), c) => (Some(t.as_str()), c.as_deref()),
        (None, _) => (None, None),
    };
    if add.is_none() && new_name.is_none() {
        bail!("'{source}' is already there");
    }

    let moved = drive.move_file(&src.id, add, remove, new_name.as_deref())?;
    println!(
        "{} moved {} → {} ({})",
        "✓".green().bold(),
        source.bold(),
        dest.bold(),
        format!("id:{}", moved.id).dimmed()
    );
    Ok(())
}

// ---------- rm ----------

pub fn rm(account: Option<&str>, paths: &[String]) -> Result<()> {
    let (_, drive) = connect(account)?;
    for path in paths {
        let file = drive.resolve(path)?;
        drive.trash(&file.id)?;
        println!(
            "{} moved {} to Trash ({}) — recoverable at drive.google.com/drive/trash",
            "✓".green().bold(),
            file.name.bold(),
            format!("id:{}", file.id).dimmed()
        );
    }
    Ok(())
}

// ---------- upload ----------

pub fn upload(account: Option<&str>, files: &[PathBuf], to: Option<&str>) -> Result<()> {
    let (_, drive) = connect(account)?;
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

pub fn download(account: Option<&str>, paths: &[String], out: Option<&Path>) -> Result<()> {
    let (_, drive) = connect(account)?;
    let out_dir = out.unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("creating {}", out_dir.display()))?;

    for path in paths {
        let file = drive.resolve(path)?;
        if file.is_folder() {
            bail!("'{path}' is a folder — download individual files (recursive download is planned)");
        }
        save_file(&drive, &file, out_dir)?;
    }
    Ok(())
}

/// Stream one Drive file to disk (exporting Google-native formats),
/// printing a progress bar and a confirmation line.
fn save_file(drive: &Drive, file: &DriveFile, out_dir: &Path) -> Result<()> {
    let (mut resp, export_ext) = drive.download(file)?;

    let mut file_name = file.name.clone();
    if let Some(ext) = export_ext {
        if !file_name.to_lowercase().ends_with(&format!(".{ext}")) {
            file_name = format!("{file_name}.{ext}");
        }
    }
    let dest = out_dir.join(&file_name);

    let len = resp.content_length().or(file.size_bytes()).unwrap_or(0);
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
    Ok(())
}

// ---------- do (natural-language tasks) ----------

#[derive(serde::Deserialize)]
struct TaskPlan {
    #[serde(default)]
    steps: Vec<TaskStep>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum TaskStep {
    Rename { id: String, new_name: String },
    Move { id: String, dest: String },
    Copy { id: String, new_name: Option<String>, dest: Option<String> },
    Trash { id: String },
    Share { id: String, email: String, role: String },
    Download { id: String, out: Option<String> },
    Mkdir { name: String, dest: Option<String> },
}

const DO_SYSTEM: &str = r#"You plan file operations on the user's Google Drive from a natural-language instruction. You are given metadata listings of one or more folders (id, name, mimeType, size in bytes, modifiedTime, createdTime per item), each labeled with its path. All paths anywhere are relative to the My Drive root (a leading '/' is fine). Respond with ONLY a JSON object — no prose, no markdown fences.

If you need to see inside folders that are not yet listed (subfolders, or folders named in the instruction), respond with an exploration request and you will be called again with those listings added:

{"explore": ["/Some Folder/Sub", ...], "notes": null}

Once you can see everything the instruction requires, respond with the plan:

{"steps": [ ... ], "notes": "one short sentence for the user, or null"}

Allowed steps (use ONLY ids present in the provided listings):
  {"op":"rename","id":"...","new_name":"..."}
  {"op":"move","id":"...","dest":"/path/of/destination/folder"}
  {"op":"copy","id":"...","new_name":null,"dest":null}            // file duplicate; folders cannot be copied
  {"op":"trash","id":"..."}                                       // recoverable; the ONLY form of deletion
  {"op":"share","id":"...","email":"...","role":"viewer|commenter|editor"}
  {"op":"download","id":"...","out":null}                         // out: local directory, default "."
  {"op":"mkdir","name":"...","dest":null}                         // dest: parent folder path, null = the first listed folder

Rules:
- Explore before guessing: never plan against folders whose contents you have not seen. At most 20 paths per exploration request.
- Steps run in order; a folder made by mkdir may be used as a later dest.
- Moving a folder moves everything inside it — prefer moving one folder over moving its files individually when the instruction allows.
- Only operate on items the instruction actually describes. When the instruction is ambiguous or matches nothing, return an empty steps array and explain in notes.
- Never invent email addresses — share only with addresses given in the instruction.
- You only have metadata. If the instruction needs file CONTENTS (e.g. a date printed inside a document), say so in notes and plan only what metadata supports.
- At most 200 steps."#;

pub fn do_task(
    account: Option<&str>,
    folder: Option<&str>,
    instruction: &str,
    yes: bool,
    dry_run: bool,
) -> Result<()> {
    let (_, drive) = connect(account)?;
    let scope = drive.resolve(folder.unwrap_or(""))?;
    if !scope.is_folder() {
        bail!("--in target is not a folder");
    }
    let children = drive.list_children(&scope.id)?;
    if children.len() > 1500 {
        bail!(
            "folder has {} items — too many for one plan; narrow the scope with --in",
            children.len()
        );
    }
    // Explore-then-plan loop: Claude may request more folder listings
    // before committing to a plan.
    let mut gathered: Vec<(String, Vec<DriveFile>)> =
        vec![(folder.unwrap_or("/").to_string(), children)];
    let mut explore_errors: Vec<String> = Vec::new();
    let mut plan: Option<TaskPlan> = None;

    for round in 0..5 {
        let mut context = String::new();
        for (path, files) in &gathered {
            let listing = serde_json::to_string(
                &files
                    .iter()
                    .map(|f| {
                        serde_json::json!({
                            "id": f.id,
                            "name": f.name,
                            "mimeType": f.mime_type,
                            "size": f.size_bytes(),
                            "modifiedTime": f.modified_time,
                            "createdTime": f.created_time,
                        })
                    })
                    .collect::<Vec<_>>(),
            )?;
            context.push_str(&format!("Folder '{path}' ({} items):\n{listing}\n\n", files.len()));
        }
        if !explore_errors.is_empty() {
            context.push_str(&format!("Exploration errors: {}\n\n", explore_errors.join("; ")));
        }

        eprintln!("{}", if round == 0 { "planning…" } else { "planning with explored folders…" }.dimmed());
        let raw = claude::ask(DO_SYSTEM, &format!("{context}Instruction: {instruction}"), None)?;
        // Tolerate a fenced response despite instructions.
        let raw = raw
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        let value: serde_json::Value = serde_json::from_str(raw)
            .with_context(|| format!("could not parse Claude's response:\n{raw}"))?;

        if let Some(paths) = value.get("explore").and_then(|e| e.as_array()) {
            if round == 4 {
                bail!("exploration did not converge on a plan after 5 rounds — try a more specific instruction");
            }
            for path in paths.iter().filter_map(|p| p.as_str()).take(20) {
                if gathered.iter().any(|(p, _)| p == path) {
                    continue;
                }
                eprintln!("{}", format!("  exploring {path}…").dimmed());
                match drive.resolve(path) {
                    Ok(f) if f.is_folder() => {
                        let kids = drive.list_children(&f.id)?;
                        gathered.push((path.to_string(), kids));
                    }
                    Ok(_) => explore_errors.push(format!("'{path}' is a file, not a folder")),
                    Err(e) => explore_errors.push(format!("'{path}': {e:#}")),
                }
            }
            let total: usize = gathered.iter().map(|(_, f)| f.len()).sum();
            if total > 5000 {
                bail!("explored listings exceed 5000 items — narrow the instruction");
            }
            continue;
        }

        plan = Some(
            serde_json::from_value(value)
                .with_context(|| format!("could not parse the plan Claude returned:\n{raw}"))?,
        );
        break;
    }
    let Some(plan) = plan else {
        bail!("no plan produced");
    };
    let by_id: std::collections::HashMap<&str, &DriveFile> = gathered
        .iter()
        .flat_map(|(_, files)| files.iter())
        .map(|f| (f.id.as_str(), f))
        .collect();

    if let Some(notes) = &plan.notes {
        println!("{} {notes}", "note:".yellow());
    }
    if plan.steps.is_empty() {
        println!("{}", "nothing to do".dimmed());
        return Ok(());
    }
    if plan.steps.len() > 200 {
        bail!("plan has {} steps — refusing; narrow the instruction", plan.steps.len());
    }

    // Validate ids and describe each step before anything happens.
    let name_of = |id: &str| -> Result<&str> {
        by_id
            .get(id)
            .map(|f| f.name.as_str())
            .with_context(|| format!("plan references id '{id}' that is not in this folder — refusing"))
    };
    println!("\n{}", format!("Plan ({} step(s)):", plan.steps.len()).bold());
    for (i, step) in plan.steps.iter().enumerate() {
        let line = match step {
            TaskStep::Rename { id, new_name } => format!("rename  {} → {}", name_of(id)?, new_name.bold()),
            TaskStep::Move { id, dest } => format!("move    {} → {}/", name_of(id)?, dest.bold()),
            TaskStep::Copy { id, new_name, dest } => format!(
                "copy    {}{}{}",
                name_of(id)?,
                new_name.as_deref().map(|n| format!(" as {}", n.bold())).unwrap_or_default(),
                dest.as_deref().map(|d| format!(" → {d}/")).unwrap_or_default()
            ),
            TaskStep::Trash { id } => format!("trash   {} {}", name_of(id)?, "(recoverable)".dimmed()),
            TaskStep::Share { id, email, role } => {
                if !["viewer", "commenter", "editor"].contains(&role.as_str()) {
                    bail!("plan contains invalid share role '{role}' — refusing");
                }
                format!("share   {} with {} as {role}", name_of(id)?, email.bold())
            }
            TaskStep::Download { id, out } => {
                format!("download {} → {}/", name_of(id)?, out.as_deref().unwrap_or("."))
            }
            TaskStep::Mkdir { name, dest } => format!(
                "mkdir   {}/ in {}/",
                name.bold(),
                dest.as_deref().unwrap_or("(this folder)")
            ),
        };
        println!("  {:>3}. {line}", i + 1);
    }

    if dry_run {
        println!("{}", "dry run — nothing executed".dimmed());
        return Ok(());
    }
    if !yes {
        let go = dialoguer::Confirm::new()
            .with_prompt("Execute this plan?")
            .default(false)
            .interact()?;
        if !go {
            println!("{}", "cancelled — nothing was changed".dimmed());
            return Ok(());
        }
    }

    // Dest paths are root-relative and resolve at execution time, so
    // folders created by earlier mkdir steps are usable as destinations.
    let resolve_dest = |dest: &str| -> Result<DriveFile> {
        let folder = drive.resolve(dest)?;
        if !folder.is_folder() {
            bail!("'{dest}' is not a folder");
        }
        Ok(folder)
    };

    let (mut done, mut failed) = (0usize, 0usize);
    for (i, step) in plan.steps.iter().enumerate() {
        let result: Result<()> = (|| {
            match step {
                TaskStep::Rename { id, new_name } => {
                    drive.move_file(id, None, None, Some(new_name))?;
                }
                TaskStep::Move { id, dest } => {
                    let target = resolve_dest(dest)?;
                    let from = by_id.get(id.as_str()).and_then(|f| f.parents.as_ref()).and_then(|p| p.first().cloned());
                    drive.move_file(id, Some(&target.id), from.as_deref(), None)?;
                }
                TaskStep::Copy { id, new_name, dest } => {
                    let parent = dest.as_deref().map(resolve_dest).transpose()?;
                    drive.copy(id, new_name.as_deref(), parent.as_ref().map(|f| f.id.as_str()))?;
                }
                TaskStep::Trash { id } => drive.trash(id)?,
                TaskStep::Share { id, email, role } => {
                    let api_role = match role.as_str() {
                        "viewer" => "reader",
                        "commenter" => "commenter",
                        _ => "writer",
                    };
                    drive.share(id, email, api_role, false)?;
                }
                TaskStep::Download { id, out } => {
                    let out_dir = PathBuf::from(out.as_deref().unwrap_or("."));
                    std::fs::create_dir_all(&out_dir)?;
                    let file = drive.get_file(id)?;
                    save_file(&drive, &file, &out_dir)?;
                }
                TaskStep::Mkdir { name, dest } => {
                    let parent = match dest.as_deref() {
                        Some(d) => resolve_dest(d)?,
                        None => scope.clone(),
                    };
                    drive.create_folder(name, &parent.id)?;
                }
            }
            Ok(())
        })();
        match result {
            Ok(()) => {
                done += 1;
                println!("{} step {}", "✓".green().bold(), i + 1);
            }
            Err(e) => {
                failed += 1;
                println!("{} step {} failed: {e:#}", "✗".red().bold(), i + 1);
            }
        }
    }
    println!(
        "\n{} {done} step(s) done{}",
        if failed == 0 { "✓".green().bold() } else { "!".yellow().bold() },
        if failed > 0 { format!(", {failed} failed") } else { String::new() }
    );
    Ok(())
}

// ---------- index ----------

/// Peak resident memory in bytes (ru_maxrss is bytes on macOS, KB on Linux).
fn peak_rss() -> u64 {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } != 0 {
        return 0;
    }
    let raw = usage.ru_maxrss as u64;
    if cfg!(target_os = "macos") { raw } else { raw * 1024 }
}

/// Demote this process so indexing never competes with the user's apps:
/// lowest CPU priority, and (on macOS) throttled disk I/O.
fn go_background() {
    // Not in the libc crate: constants from <sys/resource.h>.
    #[cfg(target_os = "macos")]
    extern "C" {
        fn setiopolicy_np(iotype: libc::c_int, scope: libc::c_int, policy: libc::c_int) -> libc::c_int;
    }
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 19);
        #[cfg(target_os = "macos")]
        setiopolicy_np(0 /* IOPOL_TYPE_DISK */, 0 /* IOPOL_SCOPE_PROCESS */, 3 /* IOPOL_THROTTLE */);
    }
}

pub fn index(
    account: Option<&str>,
    in_folders: &[String],
    all: bool,
    threads: usize,
    max_mem_gb: u64,
) -> Result<()> {
    go_background();
    let (account, drive) = connect(account)?;
    let mut store = Store::open(&account)?;

    // Scope handling: --in folders persist; --all clears them.
    if all && !in_folders.is_empty() {
        bail!("--all and --in are mutually exclusive");
    }
    if all {
        store.set_roots(&[])?;
    }
    if !in_folders.is_empty() {
        let mut roots = Vec::new();
        for path in in_folders {
            let folder = drive.resolve(path)?;
            if !folder.is_folder() {
                bail!("--in target '{path}' is not a folder");
            }
            roots.push((folder.id, path.clone()));
        }
        store.set_roots(&roots)?;
    }

    // Sync metadata: incremental via the changes feed when we have a cursor,
    // full crawl otherwise. Grab the next cursor BEFORE crawling so changes
    // made mid-crawl aren't lost.
    match store.meta_get("changes_token")? {
        Some(token) => {
            let (changes, new_token) = drive.changes_since(&token)?;
            let count = changes.len();
            for (file_id, file) in changes {
                match file {
                    Some(f) => store.upsert_file(&f)?,
                    None => store.remove_file(&file_id)?,
                }
            }
            store.meta_set("changes_token", &new_token)?;
            println!("synced {count} change(s) from Drive");
        }
        None => {
            let next_token = drive.changes_start_token()?;
            let root = drive.get_file("root")?;
            store.meta_set("root_id", &root.id)?;
            println!("first run — crawling your Drive's metadata…");
            let total = drive.list_all(|page| {
                for f in &page {
                    let _ = store.upsert_file(f);
                }
            })?;
            store.meta_set("changes_token", &next_token)?;
            println!("catalogued {total} files");
        }
    }

    // Establish the content scope for this run.
    let roots = store.get_roots()?;
    let scoped = !roots.is_empty();
    if scoped {
        let ids: Vec<String> = roots.iter().map(|(id, _)| id.clone()).collect();
        let size = store.build_scope(&ids)?;
        let names: Vec<&str> = roots.iter().map(|(_, p)| p.as_str()).collect();
        println!("scope: {} ({size} files)", names.join(", ").bold());
    } else {
        println!(
            "scope: whole Drive, Google docs + PDFs only — name folders with {} for deeper indexing",
            "drv index --in <folder>".green()
        );
    }

    // Extract + embed whatever is stale, in batches so a multi-million-file
    // catalogue never has to sit in memory.
    let total = store.stale_count(scoped)?;
    if total == 0 {
        let (files, chunks) = store.stats()?;
        println!(
            "{} index up to date ({files} files catalogued, {chunks} chunks embedded)",
            "✓".green().bold()
        );
        return Ok(());
    }

    println!("indexing content of {total} file(s)… ({threads} embedding thread(s), background priority)");
    let mut embedder = Embedder::load(threads)?;
    let mem_ceiling = max_mem_gb * 1024 * 1024 * 1024;
    let bar = indicatif::ProgressBar::new(total as u64);
    bar.set_style(
        indicatif::ProgressStyle::with_template("{msg:30!} [{bar:30.green}] {pos}/{len}")
            .unwrap()
            .progress_chars("=> "),
    );
    let mut embedded = 0usize;
    let mut skipped = 0usize;
    loop {
        // Extraction leaks a little on malformed files and ONNX arenas only
        // grow, so a long run's memory ratchets upward. Past the ceiling,
        // stop cleanly — everything done so far is saved, and the next
        // `drv index` (a fresh process) continues from where this left off.
        if peak_rss() > mem_ceiling {
            bar.finish_and_clear();
            println!(
                "{} memory ceiling ({max_mem_gb} GB) reached after {embedded} file(s) — progress saved.\n  Run {} again to continue (or loop it: {})",
                "⏸".yellow().bold(),
                "drv index".green(),
                "while drv index; [ $? -eq 75 ]; do :; done".dimmed()
            );
            std::process::exit(75); // EX_TEMPFAIL: try again
        }
        let batch = store.stale_batch(scoped, 200)?;
        if batch.is_empty() {
            break;
        }
        for (id, name, mime, size) in batch {
            bar.set_message(name.clone());
            match extract::text_of(&drive, &id, &mime, size) {
                Ok(Some(text)) => {
                    let chunks = extract::chunk(&text);
                    let embeddings = embedder.embed_documents(&chunks)?;
                    let rows: Vec<(String, Vec<u8>)> = chunks
                        .into_iter()
                        .zip(embeddings.iter().map(|e| crate::embed::to_blob(e)))
                        .collect();
                    store.set_chunks(&id, &rows)?;
                    embedded += 1;
                }
                Ok(None) => {
                    store.mark_extracted(&id)?;
                    skipped += 1;
                }
                Err(e) => {
                    bar.println(format!("{} {name}: {e:#}", "skip".yellow()));
                    store.mark_extracted(&id)?;
                    skipped += 1;
                }
            }
            bar.inc(1);
        }
    }
    bar.finish_and_clear();
    let (files, chunks) = store.stats()?;
    println!(
        "{} indexed {embedded} file(s), skipped {skipped} — {files} files catalogued, {chunks} chunks embedded",
        "✓".green().bold()
    );
    Ok(())
}

// ---------- search ----------

pub fn search(account: Option<&str>, query: &str, folder: Option<&str>, limit: usize) -> Result<()> {
    let account = account_for(account)?;
    let store = Store::open(&account)?;
    let (_, chunks) = store.stats()?;
    if chunks == 0 {
        bail!("the index is empty — run {} first", "drv index".green());
    }

    let scope = match folder {
        Some(path) => Some(store.resolve_folder(path).and_then(|id| store.descendants(&id))?),
        None => None,
    };

    let mut embedder = Embedder::load(2)?;
    let query_emb = embedder.embed_query(query)?;
    let hits = store.search(&query_emb, scope.as_ref(), limit * 3)?;

    // Show each file once, at its best-scoring chunk.
    let mut seen = std::collections::HashSet::new();
    let mut shown = 0;
    for hit in hits {
        if !seen.insert(hit.file_id.clone()) || shown >= limit {
            continue;
        }
        let (name, link) = store
            .file_info(&hit.file_id)?
            .unwrap_or((hit.file_id.clone(), None));
        let path = store.path_of(&hit.file_id).unwrap_or_else(|_| name.clone());
        let snippet: String = hit.text.chars().take(180).collect::<String>().replace('\n', " ");
        println!(
            "{:.2}  {}  {}",
            hit.score,
            path.bold(),
            format!("id:{}", hit.file_id).dimmed()
        );
        println!("      {}", snippet.dimmed());
        if let Some(link) = link {
            println!("      {}", link.blue().underline());
        }
        shown += 1;
    }
    if shown == 0 {
        println!("{}", "no matches".dimmed());
    }
    Ok(())
}

// ---------- prompt ----------

pub fn prompt(
    account: Option<&str>,
    question: &str,
    folder: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    let account = account_for(account)?;
    let store = Store::open(&account)?;
    let (_, chunk_count) = store.stats()?;
    if chunk_count == 0 {
        bail!("the index is empty — run {} first", "drv index".green());
    }

    let scope = match folder {
        Some(path) => Some(store.resolve_folder(path).and_then(|id| store.descendants(&id))?),
        None => None,
    };

    let mut embedder = Embedder::load(2)?;
    let query_emb = embedder.embed_query(question)?;
    let hits = store.search(&query_emb, scope.as_ref(), 12)?;
    if hits.is_empty() {
        bail!("nothing relevant found in the index for that question");
    }

    // Number the excerpts and remember which file each came from.
    let mut sources: Vec<(String, Option<String>)> = Vec::new();
    let mut excerpts = String::new();
    for (i, hit) in hits.iter().enumerate() {
        let (name, link) = store
            .file_info(&hit.file_id)?
            .unwrap_or((hit.file_id.clone(), None));
        let path = store.path_of(&hit.file_id).unwrap_or_else(|_| name.clone());
        excerpts.push_str(&format!("[{}] {path}\n{}\n\n", i + 1, hit.text));
        sources.push((path, link));
    }

    let system = "You answer questions about the user's Google Drive files. \
        Base your answer only on the numbered excerpts provided. Cite excerpts \
        inline as [n]. If the excerpts don't contain the answer, say so plainly. \
        Be concise.";
    let user = format!("Question: {question}\n\nExcerpts from my files:\n\n{excerpts}");

    eprintln!("{}", "asking Claude…".dimmed());
    let answer = claude::ask(system, &user, model)?;
    println!("{answer}\n");
    println!("{}", "sources:".dimmed());
    let mut listed = std::collections::HashSet::new();
    for (i, (path, link)) in sources.iter().enumerate() {
        if !listed.insert(path.clone()) {
            continue;
        }
        match link {
            Some(link) => println!("  [{}] {} — {}", i + 1, path, link.blue().underline()),
            None => println!("  [{}] {}", i + 1, path),
        }
    }
    Ok(())
}
