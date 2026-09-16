use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::json;

use crate::auth;

const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3";

pub const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// (fileId, None) means the file was removed; Some carries the new state.
pub type FileChange = (String, Option<DriveFile>);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub modified_time: Option<String>,
    #[serde(default)]
    pub parents: Option<Vec<String>>,
    #[serde(default)]
    pub web_view_link: Option<String>,
}

impl DriveFile {
    pub fn is_folder(&self) -> bool {
        self.mime_type == FOLDER_MIME
    }
    pub fn size_bytes(&self) -> Option<u64> {
        self.size.as_deref().and_then(|s| s.parse().ok())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileList {
    #[serde(default)]
    next_page_token: Option<String>,
    #[serde(default)]
    files: Vec<DriveFile>,
}

pub struct Drive {
    http: reqwest::blocking::Client,
    /// Access tokens live ~1 hour; long crawls outlive them, so GETs
    /// refresh and retry once on 401 (needs `account` to be set).
    token: std::cell::RefCell<String>,
    account: Option<String>,
}

const FILE_FIELDS: &str = "id,name,mimeType,size,modifiedTime,parents";
const INDEX_FIELDS: &str = "id,name,mimeType,size,modifiedTime,parents,webViewLink";

impl Drive {
    pub fn connect(account: &str) -> Result<Self> {
        let mut drive = Self::with_token(auth::access_token(account)?);
        drive.account = Some(account.to_string());
        Ok(drive)
    }

    /// Build a client from a raw access token (used right after login,
    /// before the account has a stored alias). No 401-refresh possible.
    pub fn with_token(token: String) -> Self {
        Self {
            http: reqwest::blocking::Client::new(),
            token: std::cell::RefCell::new(token),
            account: None,
        }
    }

    fn bearer(&self) -> String {
        self.token.borrow().clone()
    }

    fn get(&self, url: &str, query: &[(&str, &str)]) -> Result<reqwest::blocking::Response> {
        for attempt in 0..2 {
            let resp = self
                .http
                .get(url)
                .bearer_auth(self.bearer())
                .query(query)
                .send()
                .with_context(|| format!("GET {url}"))?;
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                if let Some(account) = &self.account {
                    *self.token.borrow_mut() = auth::access_token(account)?;
                    continue;
                }
            }
            return check(resp);
        }
        unreachable!("retry loop always returns")
    }

    /// All (non-trashed) children of a folder, every page.
    pub fn list_children(&self, folder_id: &str) -> Result<Vec<DriveFile>> {
        let q = format!("'{folder_id}' in parents and trashed = false");
        let fields = format!("nextPageToken,files({FILE_FIELDS})");
        let mut files = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query = vec![
                ("q", q.as_str()),
                ("fields", fields.as_str()),
                ("pageSize", "1000"),
                ("orderBy", "folder,name"),
            ];
            if let Some(token) = page_token.as_deref() {
                query.push(("pageToken", token));
            }
            let page: FileList = self.get(&format!("{API}/files"), &query)?.json()?;
            files.extend(page.files);
            match page.next_page_token {
                Some(t) => page_token = Some(t),
                None => return Ok(files),
            }
        }
    }

    pub fn get_file(&self, id: &str) -> Result<DriveFile> {
        Ok(self
            .get(&format!("{API}/files/{id}"), &[("fields", FILE_FIELDS)])?
            .json()?)
    }

    /// Resolve "A/B/C" (or "id:xyz") to a file. Paths are relative to My Drive root.
    pub fn resolve(&self, path: &str) -> Result<DriveFile> {
        if let Some(id) = path.strip_prefix("id:") {
            return self.get_file(id);
        }
        let mut current = self.get_file("root")?;
        for segment in path.split('/').filter(|s| !s.is_empty()) {
            let escaped = segment.replace('\\', "\\\\").replace('\'', "\\'");
            let q = format!(
                "'{}' in parents and name = '{}' and trashed = false",
                current.id, escaped
            );
            let fields = format!("files({FILE_FIELDS})");
            let found: FileList = self
                .get(
                    &format!("{API}/files"),
                    &[("q", q.as_str()), ("fields", fields.as_str()), ("pageSize", "2")],
                )?
                .json()?;
            match found.files.len() {
                0 => bail!("not found: '{segment}' (while resolving '{path}')"),
                1 => current = found.files.into_iter().next().unwrap(),
                _ => {
                    let first = found.files.into_iter().next().unwrap();
                    eprintln!(
                        "warning: multiple items named '{segment}' — using the first (id:{})",
                        first.id
                    );
                    current = first;
                }
            }
        }
        Ok(current)
    }

    pub fn share(&self, file_id: &str, email: &str, role: &str, notify: bool) -> Result<()> {
        let resp = self
            .http
            .post(format!("{API}/files/{file_id}/permissions"))
            .bearer_auth(self.bearer())
            .query(&[("sendNotificationEmail", if notify { "true" } else { "false" })])
            .json(&json!({ "type": "user", "role": role, "emailAddress": email }))
            .send()
            .context("creating permission")?;
        check(resp)?;
        Ok(())
    }

    pub fn copy(&self, file_id: &str, name: Option<&str>, parent: Option<&str>) -> Result<DriveFile> {
        let mut body = serde_json::Map::new();
        if let Some(name) = name {
            body.insert("name".into(), json!(name));
        }
        if let Some(parent) = parent {
            body.insert("parents".into(), json!([parent]));
        }
        let resp = self
            .http
            .post(format!("{API}/files/{file_id}/copy"))
            .bearer_auth(self.bearer())
            .query(&[("fields", FILE_FIELDS)])
            .json(&serde_json::Value::Object(body))
            .send()
            .context("copying file")?;
        Ok(check(resp)?.json()?)
    }

    /// Move and/or rename a file or folder. Drive moves are re-parenting
    /// operations: addParents/removeParents on an update call.
    pub fn move_file(
        &self,
        file_id: &str,
        add_parent: Option<&str>,
        remove_parent: Option<&str>,
        new_name: Option<&str>,
    ) -> Result<DriveFile> {
        let mut query: Vec<(&str, &str)> = vec![("fields", FILE_FIELDS)];
        if let Some(p) = add_parent {
            query.push(("addParents", p));
        }
        if let Some(p) = remove_parent {
            query.push(("removeParents", p));
        }
        let mut body = serde_json::Map::new();
        if let Some(name) = new_name {
            body.insert("name".into(), json!(name));
        }
        let resp = self
            .http
            .patch(format!("{API}/files/{file_id}"))
            .bearer_auth(self.bearer())
            .query(&query)
            .json(&serde_json::Value::Object(body))
            .send()
            .context("moving file")?;
        Ok(check(resp)?.json()?)
    }

    /// Resumable upload: initiate with metadata, then PUT the bytes.
    pub fn upload(
        &self,
        name: &str,
        parent: Option<&str>,
        content_type: &str,
        body: reqwest::blocking::Body,
        len: u64,
    ) -> Result<DriveFile> {
        let mut meta = serde_json::Map::new();
        meta.insert("name".into(), json!(name));
        if let Some(parent) = parent {
            meta.insert("parents".into(), json!([parent]));
        }
        let init = self
            .http
            .post(format!("{UPLOAD_API}/files"))
            .bearer_auth(self.bearer())
            .query(&[("uploadType", "resumable"), ("fields", FILE_FIELDS)])
            .header("X-Upload-Content-Type", content_type)
            .header("X-Upload-Content-Length", len.to_string())
            .json(&serde_json::Value::Object(meta))
            .send()
            .context("initiating upload")?;
        let init = check(init)?;
        let session = init
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .context("upload session URL missing")?
            .to_string();

        let resp = self
            .http
            .put(session)
            .header("Content-Type", content_type)
            .header("Content-Length", len.to_string())
            .body(body)
            .send()
            .context("uploading content")?;
        Ok(check(resp)?.json()?)
    }

    /// Start a media download (or export for Google-native formats).
    /// Returns the response to stream from, plus the suggested file extension
    /// when the file had to be exported.
    pub fn download(&self, file: &DriveFile) -> Result<(reqwest::blocking::Response, Option<&'static str>)> {
        if let Some((export_mime, ext)) = export_format(&file.mime_type) {
            let resp = self.get(
                &format!("{API}/files/{}/export", file.id),
                &[("mimeType", export_mime)],
            )?;
            Ok((resp, Some(ext)))
        } else {
            let resp = self.get(&format!("{API}/files/{}", file.id), &[("alt", "media")])?;
            Ok((resp, None))
        }
    }

    /// Every non-trashed file in My Drive, with the fields the index needs.
    /// The callback receives each page so huge drives don't buffer entirely.
    pub fn list_all(&self, mut on_page: impl FnMut(Vec<DriveFile>)) -> Result<usize> {
        let fields = format!("nextPageToken,files({INDEX_FIELDS})");
        let mut total = 0;
        let mut page_token: Option<String> = None;
        loop {
            let mut query = vec![
                ("q", "trashed = false"),
                ("fields", fields.as_str()),
                ("pageSize", "1000"),
            ];
            if let Some(token) = page_token.as_deref() {
                query.push(("pageToken", token));
            }
            let page: FileList = self.get(&format!("{API}/files"), &query)?.json()?;
            total += page.files.len();
            on_page(page.files);
            match page.next_page_token {
                Some(t) => page_token = Some(t),
                None => return Ok(total),
            }
        }
    }

    /// Cursor for the changes feed — fetch BEFORE a full crawl so nothing
    /// that changes mid-crawl is missed.
    pub fn changes_start_token(&self) -> Result<String> {
        let v: serde_json::Value = self
            .get(&format!("{API}/changes/startPageToken"), &[])?
            .json()?;
        v["startPageToken"]
            .as_str()
            .map(String::from)
            .context("missing startPageToken")
    }

    /// Everything changed since `token`. Returns (changes, newStartToken):
    /// each change is (fileId, None) for a removal or (fileId, Some(file)).
    pub fn changes_since(&self, token: &str) -> Result<(Vec<FileChange>, String)> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Change {
            file_id: String,
            #[serde(default)]
            removed: bool,
            #[serde(default)]
            file: Option<DriveFile>,
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct ChangeList {
            #[serde(default)]
            changes: Vec<Change>,
            next_page_token: Option<String>,
            new_start_page_token: Option<String>,
        }
        let fields = format!(
            "nextPageToken,newStartPageToken,changes(fileId,removed,file({INDEX_FIELDS},trashed))"
        );
        let mut out = Vec::new();
        let mut cursor = token.to_string();
        loop {
            let page: ChangeList = self
                .get(
                    &format!("{API}/changes"),
                    &[
                        ("pageToken", cursor.as_str()),
                        ("fields", fields.as_str()),
                        ("pageSize", "1000"),
                    ],
                )?
                .json()?;
            for c in page.changes {
                if c.removed || c.file.is_none() {
                    out.push((c.file_id, None));
                } else {
                    out.push((c.file_id, c.file));
                }
            }
            if let Some(next) = page.next_page_token {
                cursor = next;
            } else {
                let new_start = page
                    .new_start_page_token
                    .context("changes feed missing newStartPageToken")?;
                return Ok((out, new_start));
            }
        }
    }

    /// Export a Google-native file as plain text (or CSV for Sheets).
    pub fn export_text(&self, id: &str, export_mime: &str) -> Result<String> {
        Ok(self
            .get(&format!("{API}/files/{id}/export"), &[("mimeType", export_mime)])?
            .text()?)
    }

    /// Download a regular file's bytes, refusing anything over `cap` bytes.
    pub fn download_bytes(&self, id: &str, cap: u64) -> Result<Vec<u8>> {
        let mut resp = self.get(&format!("{API}/files/{id}"), &[("alt", "media")])?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 64 * 1024];
        loop {
            let n = std::io::Read::read(&mut resp, &mut chunk)?;
            if n == 0 {
                return Ok(buf);
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() as u64 > cap {
                bail!("file exceeds {cap} byte extraction cap");
            }
        }
    }

    pub fn about(&self) -> Result<serde_json::Value> {
        Ok(self
            .get(
                &format!("{API}/about"),
                &[("fields", "user(displayName,emailAddress),storageQuota(usage,limit)")],
            )?
            .json()?)
    }
}

/// Google-native formats can't be downloaded raw; export to Office/PDF.
fn export_format(mime: &str) -> Option<(&'static str, &'static str)> {
    match mime {
        "application/vnd.google-apps.document" => Some((
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "docx",
        )),
        "application/vnd.google-apps.spreadsheet" => Some((
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            "xlsx",
        )),
        "application/vnd.google-apps.presentation" => Some((
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "pptx",
        )),
        "application/vnd.google-apps.drawing" => Some(("image/png", "png")),
        _ => None,
    }
}

fn check(resp: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    // Surface Google's error message rather than the whole JSON envelope.
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(String::from))
        .unwrap_or(body);
    bail!("Drive API {status}: {msg}")
}
