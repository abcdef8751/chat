//! File attachments for chat messages.
//!
//! Files are chosen with the Tauri dialog plugin (or dropped onto the window)
//! as absolute paths. This module reads them once and produces a self-contained
//! [`Attachment`]: images carry a base64 data URL, text files carry their
//! contents, and anything else keeps just its metadata.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tauri::Manager;
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
    /// Supabase Storage object key when the file was offloaded (it had nothing
    /// inline to send: oversized text, an oversized image, or a binary). The
    /// model is told this key so it can call `fetch_file`; `None` means the
    /// file was not uploaded (sync off/signed out, or the upload failed).
    #[serde(default)]
    pub remote_path: Option<String>,
}

/// Whether this attachment has **nothing inline** to hand the model — the
/// offload trigger. Text files carry `text`, images carry a `data_url`; anything
/// with neither is invisible to the model unless it is uploaded and named in the
/// prompt (see `chat::user_request_message` + the `fetch_file` tool).
pub(crate) fn needs_offload(a: &Attachment) -> bool {
    a.text.is_none() && a.data_url.is_none()
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

/// The positive allow-list of raster MIME types the vision path actually
/// promises to support (PNG, JPEG, GIF, WebP, BMP), plus the aliases servers
/// use in practice for those same formats — notably the classic `image/jpg`
/// typo, and `image/x-ms-bmp` / `image/x-png`. Returns the **canonical** subtype
/// for an accepted input, or `None` when it is not raster.
///
/// [`is_viewable_image`] is deliberately broader — it also accepts formats a
/// user may legitimately *pick* locally (`image/heic`, `image/avif`, …) — but a
/// **remote** URL that advertises some other `image/*` (TIFF, HEIC, AVIF, …)
/// must not be attached: most vision APIs reject it, which would fail the whole
/// turn provider-side. `fetch_image` promises only these five formats, so it
/// uses this positive check.
///
/// The two roles are split on purpose: **detection** is permissive, because an
/// alias still carries a valid raster body, while **what we attach** must be
/// canonical — the MIME ends up in the `data:` URL handed to the provider and
/// rendered by the UI, where an unknown subtype can be rejected by a vision API
/// (the very failure this list exists to prevent) or refuse to render in the
/// lightbox.
pub(crate) fn canonical_raster_mime(mime: &str) -> Option<&'static str> {
    Some(match mime {
        "image/png" | "image/x-png" => "image/png",
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "image/bmp" | "image/x-ms-bmp" => "image/bmp",
        _ => return None,
    })
}

/// Whether a file should be treated as a viewable image, given what we know
/// about it: its extension and (optionally) a MIME type from a resolver.
///
/// A raster extension is sufficient. Otherwise, a concrete `image/*` MIME that
/// is **not** a vector/legacy format also qualifies — this matters on Android,
/// where a `content://` pick may carry no usable extension in its display name
/// but still reports a real type (`image/heic`, `image/avif`, …). This is the
/// one place the two pickers share the decision, so the desktop path (which
/// derives the MIME from the extension via [`mime_for`]) and the Android path
/// (which trusts the resolver) agree. SVG/ICO never qualify.
pub(crate) fn is_viewable_image(ext: &str, mime: Option<&str>) -> bool {
    if is_raster_image_ext(ext) {
        return true;
    }
    matches!(
        mime,
        Some(m) if m.starts_with("image/")
            && m != "image/svg+xml"
            && m != "image/x-icon"
            && m != "image/vnd.microsoft.icon"
    )
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

/// Identify a raster image from its leading bytes, returning the canonical MIME
/// string [`mime_for`] uses for that format.
///
/// This is what lets an **extensionless** image (a screenshot, a `fetch_file`
/// download) be read as an image rather than fed to the UTF-8 text path, where
/// it failed with an opaque decoding error. Only the five formats the vision
/// path supports are recognised: SVG/ICO deliberately return `None` and stay on
/// the text/metadata path.
///
/// `head` only needs the first few bytes; a shorter slice simply cannot match a
/// longer signature.
pub fn sniff_image_mime(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if head.starts_with(b"\xFF\xD8\xFF") {
        return Some("image/jpeg");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    // BMP's `BM` is only two bytes — plain text starting with e.g. "BM 25" or
    // "BMJ …" would otherwise be misrouted to the image path (and on a vision
    // model, sent as a bogus BMP part that fails the whole turn). Require a
    // plausible `BITMAPFILEHEADER` + DIB header size: `BM`, at least 18 bytes,
    // and a known DIB header size (12/16/40/52/56/64/108/124) at offset 14.
    if head.len() >= 18 && head.starts_with(b"BM") {
        let dib = u32::from_le_bytes([head[14], head[15], head[16], head[17]]);
        if matches!(dib, 12 | 16 | 40 | 52 | 56 | 64 | 108 | 124) {
            return Some("image/bmp");
        }
    }
    None
}

/// How many leading bytes [`is_image_file`] reads to sniff a format. Enough for
/// the longest signature checked (BMP's DIB header size at offset 14..18).
const SNIFF_BYTES: usize = 32;

/// Whether `path` is a raster image: a raster **extension**, or a regular file
/// whose first [`SNIFF_BYTES`] bytes sniff as one of the supported formats.
///
/// Cheap on purpose: only a 32-byte probe is read, never the whole file, and a
/// missing/unreadable path is simply "not an image" so the caller falls back to
/// the ordinary `read_file` path (which reports the real error).
pub fn is_image_file(path: &str) -> bool {
    if is_image_path(path) {
        return true;
    }
    let expanded = crate::shell::expand_tilde(path);
    let Ok(meta) = std::fs::metadata(&expanded) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(&expanded) else {
        return false;
    };
    let mut head = [0u8; SNIFF_BYTES];
    let n = std::io::Read::read(&mut file, &mut head).unwrap_or(0);
    sniff_image_mime(&head[..n]).is_some()
}

/// Read an image file into `(mime, data_url)`, where `data_url` is a
/// `data:<mime>;base64,…` URL ready to hand to a multimodal model. The MIME
/// comes from a raster extension when there is one, otherwise from sniffing the
/// leading bytes. Errors when the path is unreadable, is not a file, is not one
/// of the five supported raster formats, or exceeds [`MAX_IMAGE_BYTES`].
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
    // Extension first (no read needed), then a 32-byte content sniff so a file
    // whose name says nothing about its contents is still recognised.
    let mime = if is_raster_image_ext(&ext) {
        mime_for(&ext)
    } else {
        let mut file = std::fs::File::open(p).map_err(|e| format!("{path}: {e}"))?;
        let mut head = [0u8; SNIFF_BYTES];
        let n = std::io::Read::read(&mut file, &mut head).map_err(|e| format!("{path}: {e}"))?;
        sniff_image_mime(&head[..n]).ok_or_else(|| {
            format!(
                "{path} is not a supported image (read_file only returns png/jpeg/gif/webp/bmp \
                 images)"
            )
        })?
    };
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "{path}: image is too large to view ({} bytes > {MAX_IMAGE_BYTES} byte limit)",
            meta.len()
        ));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("{path}: {e}"))?;
    let data_url = format!("data:{};base64,{}", mime, base64_encode(&bytes));
    Ok((mime.to_string(), data_url))
}

/// Build an image [`Attachment`] from bytes we already hold (a fetched URL, not a
/// local file), with a fresh id and a ready-to-use data URL. The caller is
/// responsible for having validated the MIME type and the size cap; keeping the
/// data-URL shape in one place is why this exists at all.
pub fn image_attachment(name: String, mime: &str, bytes: &[u8]) -> Attachment {
    Attachment {
        id: Uuid::new_v4().to_string(),
        name,
        mime: mime.to_string(),
        size: bytes.len() as u64,
        kind: "image".into(),
        data_url: Some(format!("data:{};base64,{}", mime, base64_encode(bytes))),
        text: None,
        remote_path: None,
    }
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

    if is_viewable_image(&ext, Some(&mime)) && size <= MAX_IMAGE_BYTES {
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
            remote_path: None,
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
                remote_path: None,
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
        remote_path: None,
    })
}

/// Why the offload of `path` must be skipped, or `None` when it fits.
///
/// Consults the file's **metadata length**, never its contents: the whole point
/// is to refuse an over-cap file without buffering it. `known` is the size
/// `read_attachment` already reported, used only when the path has since become
/// unreadable (the read below fails anyway).
async fn offload_size_error(path: &Path, known: u64) -> Option<String> {
    let len = tokio::fs::metadata(path)
        .await
        .map(|meta| meta.len())
        .unwrap_or(known);
    crate::storage::check_upload_size(len).err()
}

/// Read a batch of absolute paths (from the file picker or a drop) into
/// attachments. Paths that fail to read are skipped rather than failing the
/// whole batch. Runs on the blocking pool so large reads don't stall the UI.
///
/// An attachment with **nothing inline** (oversized text, an oversized image, a
/// PDF/zip/other binary) is offloaded to Supabase Storage when cloud sync is
/// signed in, and its object key is recorded in [`Attachment::remote_path`].
/// Every step of that is best-effort: signed out, sync unconfigured, an upload
/// failure, or a file past [`crate::storage::MAX_UPLOAD_BYTES`] all leave
/// `remote_path` as `None` and the attachment in the batch, because the prompt
/// then tells the model the file exists but could not be uploaded (see
/// `chat::user_request_message`) — silently dropping it is the bug this exists
/// to fix. An over-cap file is rejected from its metadata length, before
/// anything is read into memory.
#[tauri::command]
pub async fn read_attachments(
    app: tauri::AppHandle,
    paths: Vec<String>,
) -> Vec<Attachment> {
    let read: Vec<(String, Attachment)> = tokio::task::spawn_blocking(move || {
        paths
            .into_iter()
            .filter_map(|p| read_attachment(Path::new(&p)).ok().map(|a| (p, a)))
            .collect()
    })
    .await
    .unwrap_or_default();

    // `fetch_file` runs inside the tool loop, which has no app handle; publish
    // the downloads root from here as a second chance (setup already does it).
    if let Ok(dir) = app.path().app_data_dir() {
        crate::storage::init_app_data_dir(dir);
    }

    // Only ask the keychain/network for credentials when something actually
    // needs offloading, so an ordinary text/image attach stays entirely local.
    let mut creds: Option<Option<crate::storage::Credentials>> = None;
    let mut out = Vec::with_capacity(read.len());
    for (path, mut a) in read {
        if needs_offload(&a) {
            // Size gate BEFORE the read. `storage::upload` checks the cap too, but
            // by then the whole file is already buffered (and `encrypt_bytes` then
            // copies it again), so a dragged 4 GB ISO could OOM the app just to be
            // rejected. The metadata length is already known, so an over-cap file
            // is never read: it stays metadata-only, and the prompt says it is too
            // large to offload (see `chat::user_request_message`).
            if let Some(too_big) = offload_size_error(Path::new(&path), a.size).await {
                eprintln!("attachments: offload of {path} skipped: {too_big}");
                out.push(a);
                continue;
            }
            if creds.is_none() {
                creds = Some(crate::storage::credentials().await.unwrap_or(None));
            }
            if let Some(Some((_, _, _, user_id))) = creds.as_ref() {
                let key = crate::storage::object_key(user_id, &a.name);
                match tokio::fs::read(&path).await {
                    Ok(bytes) => match crate::storage::upload(&key, &bytes, &a.mime).await {
                        Ok(()) => a.remote_path = Some(key),
                        Err(e) => {
                            // Keep the attachment; the prompt says it is not
                            // readable so the model can tell the user.
                            eprintln!("attachments: offload of {path} failed: {e}");
                        }
                    },
                    Err(e) => eprintln!("attachments: re-reading {path} for offload failed: {e}"),
                }
            }
        }
        out.push(a);
    }
    out
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
    fn needs_offload_tracks_whether_anything_is_inline() {
        // Text and images have something to send; those never need offloading.
        let text = Attachment {
            name: "a.txt".into(),
            text: Some("hi".into()),
            ..Default::default()
        };
        assert!(!needs_offload(&text));
        let image = Attachment {
            name: "a.png".into(),
            data_url: Some("data:image/png;base64,AA==".into()),
            ..Default::default()
        };
        assert!(!needs_offload(&image));
        // A metadata-only attachment (oversized text/image, PDF, zip) has
        // neither, so it must be offloaded or the model never learns it exists.
        let bare = Attachment {
            name: "a.pdf".into(),
            size: 1234,
            ..Default::default()
        };
        assert!(needs_offload(&bare));
    }

    #[test]
    fn oversized_text_file_needs_offload() {
        // A real file past MAX_TEXT_BYTES: read_attachment degrades it to
        // metadata-only, which is exactly the offload trigger. Sparse file, so
        // the test does not write 1 MB.
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-attach-big-{}.txt", std::process::id()));
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MAX_TEXT_BYTES + 1).unwrap();
        drop(f);
        let a = read_attachment(&p).unwrap();
        assert!(a.text.is_none() && a.data_url.is_none());
        assert!(needs_offload(&a));
        // Nothing is known about the remote copy until an upload happens.
        assert!(a.remote_path.is_none());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn rejects_directories() {
        let dir = std::env::temp_dir();
        assert!(read_attachment(&dir).is_err());
    }

    #[test]
    fn raster_mime_allow_list_accepts_common_alias_mislabels() {
        // The canonical types map to themselves.
        for mime in [
            "image/png",
            "image/jpeg",
            "image/gif",
            "image/webp",
            "image/bmp",
        ] {
            assert_eq!(canonical_raster_mime(mime), Some(mime), "{mime}");
        }
        // Real-world aliases whose body is a valid raster image: the classic
        // `image/jpg` typo for `image/jpeg`, plus the MS/PNG shorthands. They are
        // accepted, but normalised so the alias never reaches the wire.
        for (mime, canonical) in [
            ("image/jpg", "image/jpeg"),
            ("image/x-ms-bmp", "image/bmp"),
            ("image/x-png", "image/png"),
        ] {
            assert_eq!(canonical_raster_mime(mime), Some(canonical), "{mime}");
        }
        // Everything the allow-list exists to keep out.
        for mime in [
            "image/svg+xml",
            "image/heic",
            "image/avif",
            "image/tiff",
            "image/jxl",
            "image/x-icon",
            "text/plain",
            "application/octet-stream",
            "",
        ] {
            assert_eq!(canonical_raster_mime(mime), None, "{mime}");
        }
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
    fn is_viewable_image_accepts_raster_ext_or_non_vector_image_mime() {
        // A raster extension is enough, regardless of mime.
        assert!(is_viewable_image("png", None));
        assert!(is_viewable_image("webp", Some("application/octet-stream")));
        // An extensionless / non-table pick is still an image when the resolver
        // reports a concrete non-vector `image/*` mime (the Android regression).
        assert!(is_viewable_image("", Some("image/heic")));
        assert!(is_viewable_image("bin", Some("image/avif")));
        // Vector/legacy mimes never qualify, even with no raster extension.
        assert!(!is_viewable_image("svg", Some("image/svg+xml")));
        assert!(!is_viewable_image("", Some("image/svg+xml")));
        assert!(!is_viewable_image("ico", Some("image/x-icon")));
        // Non-images never qualify.
        assert!(!is_viewable_image("txt", Some("text/plain")));
        assert!(!is_viewable_image("", None));
        assert!(!is_viewable_image("pdf", Some("application/pdf")));
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

    /// The offload gate refuses an over-cap file from its **metadata** length, so
    /// a huge attachment is never read into memory (the old order read it first
    /// and only then let `storage::upload` reject it — and `encrypt_bytes` copied
    /// it again).
    #[tokio::test]
    async fn offload_gate_rejects_an_over_cap_file_without_reading_it() {
        use crate::storage::MAX_UPLOAD_BYTES;

        // Sparse: the length is reported without writing 100 MB to disk.
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-offload-cap-{}.bin", std::process::id()));
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(MAX_UPLOAD_BYTES + 1).unwrap();
        drop(f);

        // The metadata-only read (no bytes touched).
        let a = read_attachment(&p).unwrap();
        assert!(needs_offload(&a), "a binary is the offload path");
        assert_eq!(a.size, MAX_UPLOAD_BYTES + 1);

        let err = offload_size_error(&p, a.size)
            .await
            .expect("an over-cap file must be refused");
        assert!(err.contains("too large"), "{err}");

        // Exactly at the cap is allowed (the check is inclusive).
        let mut ok = std::env::temp_dir();
        ok.push(format!("pi-chat-offload-cap-ok-{}.bin", std::process::id()));
        let f = std::fs::File::create(&ok).unwrap();
        f.set_len(MAX_UPLOAD_BYTES).unwrap();
        drop(f);
        assert!(
            offload_size_error(&ok, MAX_UPLOAD_BYTES).await.is_none(),
            "a file exactly at the cap is uploadable"
        );

        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(&ok);
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
        let err = read_image(t.to_str().unwrap()).unwrap_err();
        assert!(
            err.contains("not a supported image"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_file(&t);
    }

    // -- content sniffing (extensionless images) ------------------------------

    #[test]
    fn sniff_image_mime_recognizes_exactly_the_five_raster_formats() {
        assert_eq!(
            sniff_image_mime(b"\x89PNG\r\n\x1a\n\x00\x00"),
            Some("image/png")
        );
        assert_eq!(sniff_image_mime(b"\xFF\xD8\xFF\xE0\x00"), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(b"GIF87aREST"), Some("image/gif"));
        assert_eq!(sniff_image_mime(b"GIF89aREST"), Some("image/gif"));
        assert_eq!(
            sniff_image_mime(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some("image/webp")
        );
        assert_eq!(
            sniff_image_mime(b"BM\x36\x00\x00\x00\x00\x00\x00\x00\x36\x00\x00\x00\x28\x00\x00\x00"),
            Some("image/bmp")
        );

        // Not images: text, SVG/XML, ICO, a RIFF *non*-WebP container, an empty
        // file, a truncated PNG signature, and — importantly — plain text that
        // merely begins with BMP's two-byte `BM` marker.
        for other in [
            b"hello world".as_slice(),
            b"<svg><rect/></svg>",
            b"\x00\x00\x01\x00",
            b"RIFF\x24\x00\x00\x00WAVEfmt ",
            b"",
            b"\x89PNG",
            b"BMJ 2024 annual review",
            b"BM 25,12.5,hello",
        ] {
            assert_eq!(sniff_image_mime(other), None, "{other:?}");
        }
    }

    #[test]
    fn read_image_uses_content_when_the_extension_says_nothing() {
        // The bytes of a real PNG header (a full image is not needed).
        let png: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR";

        // Extensionless (e.g. a `fetch_file` download or a saved screenshot).
        let mut noext = std::env::temp_dir();
        noext.push(format!("pi-chat-sniff-{}-shot", std::process::id()));
        std::fs::write(&noext, png).unwrap();
        let (mime, url) = read_image(noext.to_str().unwrap()).unwrap();
        assert_eq!(mime, "image/png");
        assert!(url.starts_with("data:image/png;base64,"), "{url}");
        assert!(is_image_file(noext.to_str().unwrap()));
        let _ = std::fs::remove_file(&noext);

        // A misleading name: PNG bytes in a `.txt` still read as an image.
        let mut mislabelled = std::env::temp_dir();
        mislabelled.push(format!("pi-chat-sniff-{}.txt", std::process::id()));
        std::fs::write(&mislabelled, png).unwrap();
        let (mime, _) = read_image(mislabelled.to_str().unwrap()).unwrap();
        assert_eq!(mime, "image/png");
        assert!(is_image_file(mislabelled.to_str().unwrap()));
        let _ = std::fs::remove_file(&mislabelled);
    }

    #[test]
    fn is_image_file_is_false_for_text_and_svg() {
        let mut text = std::env::temp_dir();
        text.push(format!("pi-chat-is-image-{}.txt", std::process::id()));
        std::fs::write(&text, "just some words").unwrap();
        assert!(!is_image_file(text.to_str().unwrap()));
        let _ = std::fs::remove_file(&text);

        let mut svg = std::env::temp_dir();
        svg.push(format!("pi-chat-is-image-{}.svg", std::process::id()));
        std::fs::write(&svg, "<svg><rect/></svg>").unwrap();
        assert!(!is_image_file(svg.to_str().unwrap()));
        let _ = std::fs::remove_file(&svg);

        // A raster *extension* is still enough even when the file is unreadable.
        assert!(is_image_file("/definitely/missing/photo.png"));
        // A missing extensionless path is not an image (the text path reports the
        // real error instead).
        assert!(!is_image_file("/definitely/missing/noext"));
    }
}
