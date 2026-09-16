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

/// Resolve which account this invocation acts on (after any v0.1 migration).
fn account_for(flag: Option<&str>) -> Result<String> {
    if let Some(alias) = auth::migrate_legacy()? {
        eprintln!("{} adopted v0.1 credentials as account '{alias}'", "note:".yellow());
    }
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
        auth::keychain_set("google-client-secret", &secret)?;
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
    let _ = auth::migrate_legacy()?;
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
    let claude_key = auth::keychain_get("anthropic-api-key")?.is_some();
    println!(
        "{} Claude API key {}",
        if claude_key { "✓".green().bold() } else { "○".dimmed() },
        if claude_key { "stored (used by search/prompt)".into() } else {
            format!("not set — optional, add with {}", "drv auth claude".green())
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
    auth::keychain_set("anthropic-api-key", key)?;
    println!("{} Claude API key stored in the Keychain.", "✓".green().bold());
    Ok(())
}

// ---------- account ----------

pub fn account_list() -> Result<()> {
    let _ = auth::migrate_legacy()?;
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
        let (mut resp, export_ext) = drive.download(&file)?;

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
    }
    Ok(())
}

// ---------- index ----------

pub fn index(account: Option<&str>, in_folders: &[String], all: bool) -> Result<()> {
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

    println!("indexing content of {total} file(s)…");
    let mut embedder = Embedder::load()?;
    let bar = indicatif::ProgressBar::new(total as u64);
    bar.set_style(
        indicatif::ProgressStyle::with_template("{msg:30!} [{bar:30.green}] {pos}/{len}")
            .unwrap()
            .progress_chars("=> "),
    );
    let mut embedded = 0usize;
    let mut skipped = 0usize;
    loop {
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

    let mut embedder = Embedder::load()?;
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

    let mut embedder = Embedder::load()?;
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
