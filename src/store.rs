use anyhow::{Context, Result};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet, VecDeque};

use crate::config;
use crate::drive::DriveFile;

/// The per-account local index: file metadata + embedded text chunks,
/// in a single SQLite database under the user's data directory.
pub struct Store {
    pub db: Connection,
}

pub struct SearchHit {
    pub file_id: String,
    pub text: String,
    pub score: f32,
}

impl Store {
    pub fn open(account: &str) -> Result<Self> {
        let path = config::data_dir(account)?.join("index.sqlite");
        let db = Connection::open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS files (
               id TEXT PRIMARY KEY,
               name TEXT NOT NULL,
               mime TEXT NOT NULL,
               modified TEXT,
               size INTEGER,
               parent TEXT,
               web_link TEXT,
               extracted_modified TEXT
             );
             CREATE TABLE IF NOT EXISTS chunks (
               id INTEGER PRIMARY KEY,
               file_id TEXT NOT NULL,
               seq INTEGER NOT NULL,
               text TEXT NOT NULL,
               embedding BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS chunks_file ON chunks(file_id);
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        Ok(Self { db })
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.db.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub fn upsert_file(&self, f: &DriveFile) -> Result<()> {
        self.db.execute(
            "INSERT INTO files(id, name, mime, modified, size, parent, web_link)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name, mime = excluded.mime,
               modified = excluded.modified, size = excluded.size,
               parent = excluded.parent, web_link = excluded.web_link",
            rusqlite::params![
                f.id,
                f.name,
                f.mime_type,
                f.modified_time,
                f.size_bytes().map(|s| s as i64),
                f.parents.as_ref().and_then(|p| p.first().cloned()),
                f.web_view_link,
            ],
        )?;
        Ok(())
    }

    pub fn remove_file(&self, id: &str) -> Result<()> {
        self.db.execute("DELETE FROM chunks WHERE file_id = ?1", [id])?;
        self.db.execute("DELETE FROM files WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Files whose content has changed since we last extracted it (or was
    /// never extracted) — restricted to the given indexable mime check.
    pub fn stale_files(&self, indexable: impl Fn(&str) -> bool) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.db.prepare(
            "SELECT id, name, mime FROM files
             WHERE extracted_modified IS NULL OR extracted_modified != COALESCE(modified, '')",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, mime) = row?;
            if indexable(&mime) {
                out.push((id, name, mime));
            }
        }
        Ok(out)
    }

    pub fn file_meta(&self, id: &str) -> Result<Option<(Option<String>, Option<i64>)>> {
        let mut stmt = self
            .db
            .prepare("SELECT modified, size FROM files WHERE id = ?1")?;
        let mut rows = stmt.query([id])?;
        Ok(rows
            .next()?
            .map(|r| Ok::<_, rusqlite::Error>((r.get(0)?, r.get(1)?)))
            .transpose()?)
    }

    /// Replace a file's chunks and mark its content as extracted.
    pub fn set_chunks(&mut self, file_id: &str, chunks: &[(String, Vec<u8>)]) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM chunks WHERE file_id = ?1", [file_id])?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO chunks(file_id, seq, text, embedding) VALUES(?1, ?2, ?3, ?4)",
            )?;
            for (seq, (text, emb)) in chunks.iter().enumerate() {
                ins.execute(rusqlite::params![file_id, seq as i64, text, emb])?;
            }
        }
        tx.execute(
            "UPDATE files SET extracted_modified = COALESCE(modified, '') WHERE id = ?1",
            [file_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Mark a file as processed without content (unindexable / failed) so it
    /// isn't retried every run until it changes again.
    pub fn mark_extracted(&self, file_id: &str) -> Result<()> {
        self.db.execute(
            "UPDATE files SET extracted_modified = COALESCE(modified, '') WHERE id = ?1",
            [file_id],
        )?;
        Ok(())
    }

    /// The set of file IDs under a folder (inclusive), using stored parents.
    pub fn descendants(&self, folder_id: &str) -> Result<HashSet<String>> {
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        let mut stmt = self.db.prepare("SELECT id, parent FROM files WHERE parent IS NOT NULL")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, parent) = row?;
            children.entry(parent).or_default().push(id);
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::from([folder_id.to_string()]);
        while let Some(id) = queue.pop_front() {
            if !seen.insert(id.clone()) {
                continue;
            }
            if let Some(kids) = children.get(&id) {
                queue.extend(kids.iter().cloned());
            }
        }
        Ok(seen)
    }

    /// Resolve a slash path offline against the stored file tree.
    /// Returns the folder's file ID.
    pub fn resolve_folder(&self, path: &str) -> Result<String> {
        if let Some(id) = path.strip_prefix("id:") {
            return Ok(id.to_string());
        }
        // The root folder's ID is stored at index time.
        let mut current = self
            .meta_get("root_id")?
            .context("index has no root — run `drv index` first")?;
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            let mut stmt = self.db.prepare(
                "SELECT id FROM files WHERE parent = ?1 AND name = ?2 AND mime = ?3",
            )?;
            let mut rows = stmt.query(rusqlite::params![
                current,
                segment,
                crate::drive::FOLDER_MIME
            ])?;
            match rows.next()? {
                Some(r) => current = r.get(0)?,
                None => anyhow::bail!("folder not found in index: '{segment}' (run `drv index` to refresh)"),
            }
        }
        Ok(current)
    }

    /// Brute-force cosine search over every chunk (optionally scoped to a
    /// set of file IDs). Personal-drive scale makes this instant.
    pub fn search(
        &self,
        query_embedding: &[f32],
        scope: Option<&HashSet<String>>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let mut stmt = self
            .db
            .prepare("SELECT file_id, text, embedding FROM chunks")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Vec<u8>>(2)?,
            ))
        })?;
        let mut hits: Vec<SearchHit> = Vec::new();
        for row in rows {
            let (file_id, text, blob) = row?;
            if let Some(scope) = scope {
                if !scope.contains(&file_id) {
                    continue;
                }
            }
            let emb = crate::embed::from_blob(&blob);
            let score = crate::embed::cosine(query_embedding, &emb);
            hits.push(SearchHit { file_id, text, score });
        }
        hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn file_info(&self, id: &str) -> Result<Option<(String, Option<String>)>> {
        let mut stmt = self
            .db
            .prepare("SELECT name, web_link FROM files WHERE id = ?1")?;
        let mut rows = stmt.query([id])?;
        Ok(rows
            .next()?
            .map(|r| Ok::<_, rusqlite::Error>((r.get(0)?, r.get(1)?)))
            .transpose()?)
    }

    /// Human path of a file, walking stored parents up to the root.
    pub fn path_of(&self, id: &str) -> Result<String> {
        let mut parts: Vec<String> = Vec::new();
        let mut current = id.to_string();
        for _ in 0..64 {
            let mut stmt = self
                .db
                .prepare("SELECT name, parent FROM files WHERE id = ?1")?;
            let mut rows = stmt.query([current.as_str()])?;
            match rows.next()? {
                Some(r) => {
                    parts.push(r.get::<_, String>(0)?);
                    match r.get::<_, Option<String>>(1)? {
                        Some(parent) => current = parent,
                        None => break,
                    }
                }
                None => break,
            }
        }
        parts.reverse();
        Ok(parts.join("/"))
    }

    pub fn stats(&self) -> Result<(i64, i64)> {
        let files: i64 = self.db.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0))?;
        let chunks: i64 = self.db.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        Ok((files, chunks))
    }
}
