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
pub(crate) const MAX_TEXT_BYTES: u64 = 1_000_000;

/// Images larger than this are not read into a data URL at all. An image is
/// base64-inflated (~4/3) and the resulting data URL is re-sent on every
/// remaining round of the turn, so an unbounded image can dominate the request
/// and blow a provider's body limit. This mirrors the spirit of
/// [`MAX_TEXT_BYTES`] while staying well under the 25 MB ceiling the Android
/// Termux bridge already enforces on the transport.
pub const MAX_IMAGE_BYTES: u64 = 10_000_000;

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

pub(crate) fn mime_for(ext: &str) -> &'static str {
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

pub(crate) fn is_text_like(ext: &str) -> bool {
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
            // SVG is XML text; excluding it from the raster/image path means an
            // attached SVG is handed to the model as readable text instead of a
            // data URL that most vision APIs reject.
            | "svg"
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

/// Raster image extensions a multimodal model can reliably consume. Vector
/// formats (`svg`) and legacy containers (`ico`) are deliberately excluded:
/// most vision APIs reject them, which makes the whole turn fail provider-side
/// instead of telling the model it cannot view the file. Non-raster files fall
/// back to the text path (SVG is XML text) or a metadata-only attachment.
pub(crate) fn is_raster_image_ext(ext: &str) -> bool {
    matches!(ext, "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
}

/// Whether a path's extension denotes a raster image (png/jpg/gif/webp/bmp).
/// Used by `read_file` to route image files through the vision path instead of
/// trying to decode them as UTF-8 text. SVG and ICO are intentionally *not*
/// images here (see [`is_raster_image_ext`]).
pub fn is_image_path(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    is_raster_image_ext(&ext)
}

/// Read an image file into `(mime, data_url)`, where `data_url` is a
/// `data:<mime>;base64,…` URL ready to hand to a multimodal model. Errors when
/// the path is unreadable, is not a file, is not a raster image by extension,
/// or exceeds [`MAX_IMAGE_BYTES`].
pub fn read_image(path: &str) -> Result<(String, String), String> {
    let p = Path::new(path);
    let meta = std::fs::metadata(p).map_err(|e| format!("{path}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{path} is not a file"));
    }
    let ext = p
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !is_raster_image_ext(&ext) {
        return Err(format!("{path} is not a supported image"));
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "{path}: image is too large to view ({} bytes > {MAX_IMAGE_BYTES} byte limit)",
            meta.len()
        ));
    }
    let mime = mime_for(&ext).to_string();
    let bytes = std::fs::read(p).map_err(|e| format!("{path}: {e}"))?;
    let data_url = format!("data:{};base64,{}", mime, base64_encode(&bytes));
    Ok((mime, data_url))
}

/// Read one file into an [`Attachment`]. Directories and unreadable paths error.
///
/// A raster image larger than [`MAX_IMAGE_BYTES`] is not turned into a data URL;
/// it degrades to a metadata-only `file` attachment (see the image branch).
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

    if is_raster_image_ext(&ext) && size <= MAX_IMAGE_BYTES {
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
    // An oversized raster image deliberately falls through: producing a data URL
    // here would base64-inflate it (~4/3) and re-send it on every remaining round
    // of the turn, which can blow a provider's body limit and fails the whole
    // turn. Instead it becomes a metadata-only `file` attachment (the same
    // fallback as any other unreadable file): the user still sees it attached,
    // and the model is never handed bytes it cannot use. This mirrors the cap
    // the `read_file` tool already enforces via [`read_image`].

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
    fn attachment_oversized_image_falls_back_to_metadata_only() {
        // A raster image past the cap must NOT become a data URL (which would be
        // base64-inflated and re-sent every round); it degrades to a metadata-only
        // `file` attachment. Sparse file: metadata reports the length without
        // writing the bytes.
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-attach-oversize-{}.png", std::process::id()));
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        drop(f);
        let a = read_attachment(&p).unwrap();
        assert_eq!(a.kind, "file");
        assert!(a.data_url.is_none());
        assert_eq!(a.mime, "image/png");
        assert_eq!(a.size, MAX_IMAGE_BYTES + 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_directories() {
        let dir = std::env::temp_dir();
        assert!(read_attachment(&dir).is_err());
    }

    #[test]
    fn is_image_path_checks_extension_case_insensitively() {
        assert!(is_image_path("/tmp/a.png"));
        assert!(is_image_path("/tmp/a.JPG"));
        assert!(is_image_path("photo.webp"));
        assert!(!is_image_path("/tmp/notes.txt"));
        assert!(!is_image_path("/tmp/noext"));
    }

    #[test]
    fn svg_and_ico_are_not_image_paths() {
        // Vector/legacy formats must fall back to the text "cannot view" path
        // rather than being turned into an `image_url` part.
        assert!(!is_image_path("/tmp/logo.svg"));
        assert!(!is_image_path("/tmp/logo.SVG"));
        assert!(!is_image_path("/tmp/favicon.ico"));
        assert!(is_image_path("/tmp/photo.bmp"));
    }

    #[test]
    fn svg_attaches_as_text_not_a_data_url() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-attach-{}.svg", std::process::id()));
        std::fs::write(&p, "<svg><rect/></svg>").unwrap();
        let a = read_attachment(&p).unwrap();
        assert_eq!(a.kind, "file");
        assert!(a.data_url.is_none());
        assert_eq!(a.text.as_deref(), Some("<svg><rect/></svg>"));
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_image_rejects_oversized_files() {
        // Sparse file: metadata reports the length without writing the bytes.
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-oversize-{}.png", std::process::id()));
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MAX_IMAGE_BYTES + 1).unwrap();
        drop(f);
        let err = read_image(p.to_str().unwrap()).unwrap_err();
        assert!(err.contains("too large"), "unexpected error: {err}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_image_rejects_svg() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-read-svg-{}.svg", std::process::id()));
        std::fs::write(&p, "<svg/>").unwrap();
        assert!(read_image(p.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn read_image_returns_data_url_and_rejects_non_images() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-read-image-{}.png", std::process::id()));
        std::fs::write(&p, [0u8, 1, 2, 3]).unwrap();
        let (mime, url) = read_image(p.to_str().unwrap()).unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(url, "data:image/png;base64,AAECAw==");
        let _ = std::fs::remove_file(&p);

        let mut t = std::env::temp_dir();
        t.push(format!("pi-chat-read-image-{}.txt", std::process::id()));
        std::fs::write(&t, "hi").unwrap();
        assert!(read_image(t.to_str().unwrap()).is_err());
        let _ = std::fs::remove_file(&t);
    }
}
