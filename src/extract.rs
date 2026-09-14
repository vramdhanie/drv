use anyhow::Result;

use crate::drive::Drive;

/// Files larger than this are skipped for content extraction.
const MAX_EXTRACT_BYTES: u64 = 8 * 1024 * 1024;

/// Can we get text out of this mime type at all?
pub fn indexable(mime: &str) -> bool {
    matches!(
        mime,
        "application/vnd.google-apps.document"
            | "application/vnd.google-apps.spreadsheet"
            | "application/vnd.google-apps.presentation"
            | "application/pdf"
    ) || mime.starts_with("text/")
        || matches!(mime, "application/json" | "application/xml" | "application/rtf")
}

/// Pull the plain text of a file, best effort. `Ok(None)` means "skipped".
pub fn text_of(drive: &Drive, id: &str, mime: &str, size: Option<i64>) -> Result<Option<String>> {
    if let Some(size) = size {
        if size as u64 > MAX_EXTRACT_BYTES {
            return Ok(None);
        }
    }
    let text = match mime {
        "application/vnd.google-apps.document"
        | "application/vnd.google-apps.presentation" => {
            Some(drive.export_text(id, "text/plain")?)
        }
        "application/vnd.google-apps.spreadsheet" => Some(drive.export_text(id, "text/csv")?),
        "application/pdf" => {
            let bytes = drive.download_bytes(id, MAX_EXTRACT_BYTES)?;
            // pdf-extract panics on some malformed files; contain it.
            std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes))
                .ok()
                .and_then(|r| r.ok())
        }
        m if m.starts_with("text/") || matches!(m, "application/json" | "application/xml" | "application/rtf") => {
            let bytes = drive.download_bytes(id, MAX_EXTRACT_BYTES)?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => None,
    };
    Ok(text.map(normalize).filter(|t| !t.is_empty()))
}

fn normalize(text: String) -> String {
    text.replace('\r', "").trim().to_string()
}

const CHUNK_CHARS: usize = 1400;
const OVERLAP_CHARS: usize = 200;

/// Split text into overlapping chunks, preferring paragraph boundaries.
pub fn chunk(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= CHUNK_CHARS {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let hard_end = (start + CHUNK_CHARS).min(chars.len());
        let mut end = hard_end;
        if hard_end < chars.len() {
            // Walk back to the nearest paragraph or sentence break.
            let window: String = chars[start..hard_end].iter().collect();
            if let Some(pos) = window.rfind("\n\n").or_else(|| window.rfind(". ")) {
                let candidate = start + window[..pos].chars().count() + 1;
                if candidate > start + CHUNK_CHARS / 2 {
                    end = candidate;
                }
            }
        }
        chunks.push(chars[start..end].iter().collect::<String>().trim().to_string());
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(OVERLAP_CHARS);
    }
    chunks.retain(|c| !c.is_empty());
    chunks
}
