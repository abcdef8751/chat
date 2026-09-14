//! File attachments for chat messages.
//!
//! Files are chosen with the Tauri dialog plugin (or dropped onto the window)
//! as absolute paths. This module reads them once and produces a self-contained
//! [`Attachment`]: images carry a base64 data URL, text files carry their
//! contents, and anything else keeps just its metadata.

use std::path::Path;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Text files larger than this are attached by name only (keeps messages sane).
const MAX_TEXT_BYTES: u64 = 1_000_000;

/// One attached file, serialized straight to the frontend and stored (as JSON)
/// on the message row.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// `image` | `file`
    pub kind: String,
    /// `data:<mime>;base64,…` for images.
    #[serde(default)]
    pub data_url: Option<String>,
    /// Decoded contents for text-like files.
    #[serde(default)]
    pub text: Option<String>,
}

fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" | "ini" | "conf" | "toml" | "yaml" | "yml" => "text/plain",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "ts" | "tsx" => "text/typescript",
        "rs" => "text/x-rust",
        "py" => "text/x-python",
        "go" => "text/x-go",
        "java" => "text/x-java",
        "sh" | "bash" | "zsh" => "text/x-sh",
        _ => "application/octet-stream",
    }
}

fn is_text_like(ext: &str) -> bool {
    matches!(
        ext,
        "txt"
            | "log"
            | "ini"
            | "conf"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "csv"
            | "md"
            | "markdown"
            | "xml"
            | "html"
            | "htm"
            | "css"
            | "js"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "jsx"
            | "rs"
            | "py"
            | "go"
            | "java"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "sh"
            | "bash"
            | "zsh"
            | "sql"
            | "env"
            | "gitignore"
    )
}

/// Minimal standard base64 encoder (no extra dependency needed).
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Read one file into an [`Attachment`]. Directories and unreadable paths error.
pub fn read_attachment(path: &Path) -> Result<Attachment, String> {
    let meta =
        std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    let ext = path
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mime = mime_for(&ext).to_string();
    let size = meta.len();
    let id = Uuid::new_v4().to_string();

    if mime.starts_with("image/") {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let data_url = format!("data:{};base64,{}", mime, base64_encode(&bytes));
        return Ok(Attachment {
            id,
            name,
            mime,
            size,
            kind: "image".into(),
            data_url: Some(data_url),
            text: None,
        });
    }

    if is_text_like(&ext) && size <= MAX_TEXT_BYTES {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if let Ok(text) = String::from_utf8(bytes) {
            return Ok(Attachment {
                id,
                name,
                mime,
                size,
                kind: "file".into(),
                data_url: None,
                text: Some(text),
            });
        }
    }

    Ok(Attachment {
        id,
        name,
        mime,
        size,
        kind: "file".into(),
        data_url: None,
        text: None,
    })
}

/// Read a batch of absolute paths (from the file picker or a drop) into
/// attachments. Paths that fail to read are skipped rather than failing the
/// whole batch. Runs on the blocking pool so large reads don't stall the UI.
#[tauri::command]
pub async fn read_attachments(paths: Vec<String>) -> Vec<Attachment> {
    tokio::task::spawn_blocking(move || {
        paths
            .iter()
            .filter_map(|p| read_attachment(Path::new(p)).ok())
            .collect()
    })
    .await
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn reads_text_file_contents() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-attach-{}.txt", std::process::id()));
        std::fs::write(&p, "hello attachment").unwrap();
        let a = read_attachment(&p).unwrap();
        assert_eq!(a.kind, "file");
        assert_eq!(a.name, format!("pi-chat-attach-{}.txt", std::process::id()));
        assert_eq!(a.text.as_deref(), Some("hello attachment"));
        assert!(a.data_url.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn reads_image_as_data_url() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-attach-{}.png", std::process::id()));
        std::fs::write(&p, [0u8, 1, 2, 3]).unwrap();
        let a = read_attachment(&p).unwrap();
        assert_eq!(a.kind, "image");
        assert_eq!(a.mime, "image/png");
        assert_eq!(a.data_url.as_deref(), Some("data:image/png;base64,AAECAw=="));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_directories() {
        let dir = std::env::temp_dir();
        assert!(read_attachment(&dir).is_err());
    }
}
