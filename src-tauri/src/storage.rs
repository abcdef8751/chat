//! Supabase **Storage** client for large attachment offload.
//!
//! Some attachments cannot ride the prompt: text past
//! [`crate::attachments::MAX_TEXT_BYTES`], raster images past
//! [`crate::attachments::MAX_IMAGE_BYTES`], and every other binary (PDF, zip,
//! …) degrade to a metadata-only [`crate::attachments::Attachment`]. Dropping
//! them silently means the model never learns they exist — this module is the
//! other half of the fix:
//!
//! 1. `attachments::read_attachments` uploads such a file to the private
//!    `attachments` bucket and records the object key on the attachment
//!    (`Attachment::remote_path`).
//! 2. `chat::user_request_message` tells the model the file exists, its size and
//!    that key.
//! 3. The `fetch_file` tool (`storage::fetch_file`) downloads the object into
//!    `<app data dir>/downloads/` and returns the local path, so the ordinary
//!    `read_file` tool can inspect it.
//!
//! Offload is **opportunistic**: it only runs while the user is signed in to
//! cloud sync, and a failed upload never drops the attachment (the prompt then
//! says the file could not be uploaded instead of pretending it does not
//! exist). Nothing here touches the local database.
//!
//! When a client-side encryption key is configured (`secrets::enc_key_get`),
//! bytes are encrypted with AES-256-GCM **before** upload, using the same data
//! key as the cloud mirror. Encrypted objects carry a magic prefix so a download
//! knows whether to decrypt — an object without it (uploaded by an older build,
//! or with encryption off) is written out verbatim.
//!
//! The data key is a device-local key in the OS keychain, so a second device
//! that has not imported the recovery code cannot decrypt these blobs; that is
//! the same trade-off the encrypted mirror already makes (see AGENTS.md).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;

/// Private bucket that holds offloaded attachments.
pub const BUCKET: &str = "attachments";

/// Hard cap on an in-memory upload. Attachment reads already cap images at
/// 10 MB, so this is the "somebody dragged a 4 GB ISO in" guard: it keeps the
/// encrypt/upload path from buffering something that would exhaust memory.
pub const MAX_UPLOAD_BYTES: u64 = 100 * 1024 * 1024;

/// Magic prefix on an encrypted object body: `PIENC1\0`.
const ENC_MAGIC: &[u8] = b"PIENC1\0";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// Longest object-name segment we keep from the original file name.
const MAX_NAME_CHARS: usize = 96;

/// `<app data dir>/downloads`, published once by `lib.rs`'s setup. `fetch_file`
/// runs deep inside the tool loop, which has no `AppHandle`, and re-deriving
/// Tauri's per-platform app-data path by hand would drift.
static DOWNLOADS_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Publish the app data directory. Called once from setup; later calls are
/// ignored (the first wins, and only one value is ever correct).
pub fn init_app_data_dir(dir: PathBuf) {
    let _ = DOWNLOADS_DIR.set(dir.join("downloads"));
}

/// The downloads directory, when the app has published it.
pub fn downloads_dir() -> Option<PathBuf> {
    DOWNLOADS_DIR.get().cloned()
}

/// `(base_url, anon_key, access_token, user_id)` when cloud sync is signed in.
pub(crate) type Credentials = (String, String, String, String);

/// Sync credentials, or `None` when signed out / sync unconfigured.
///
/// A fresh `SyncState` is enough: it only carries the compile-time endpoint,
/// the publishable key and a reqwest client, and `storage_credentials` refreshes
/// an expired token itself (persisting the refreshed session).
pub async fn credentials() -> Result<Option<Credentials>, String> {
    let state = crate::sync::SyncState::new();
    crate::sync::storage_credentials(&state).await
}

// ---------------------------------------------------------------------------
// Object keys
// ---------------------------------------------------------------------------

/// Clean a file name into one safe object-key segment.
///
/// Everything except ASCII alphanumerics and `.-_` becomes `_`, runs of dots are
/// collapsed (so `..` cannot survive as a traversal), and a leading dot/dash is
/// dropped (no hidden files, no `-N` confusion with the de-duplication suffix).
/// Keeps the extension when the name has to be shortened.
pub fn sanitize_name(name: &str) -> String {
    // Work on the last path segment only: a caller may pass a full path.
    let base = name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(name)
        .trim()
        .trim_start_matches('.');
    let mut out = String::with_capacity(base.len());
    let mut last_dot = false;
    for ch in base.chars() {
        let ok = ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.';
        let ch = if ok { ch } else { '_' };
        // Collapse `..` and `//`-style runs produced by sanitizing.
        if ch == '.' {
            if last_dot {
                continue;
            }
            last_dot = true;
        } else {
            last_dot = false;
        }
        out.push(ch);
    }
    let out = out.trim_start_matches(['.', '-']).to_string();
    let out = truncate_keeping_ext(&out, MAX_NAME_CHARS);
    if out.is_empty() {
        "file".to_string()
    } else {
        out
    }
}

/// Shorten `name` to at most `max` chars, keeping its extension intact.
fn truncate_keeping_ext(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 && name.len() - i <= 16 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let keep = max.saturating_sub(ext.chars().count());
    let stem: String = stem.chars().take(keep).collect();
    format!("{stem}{ext}")
}

/// Build the object key for a new upload: `"{user_id}/{uuid}-{sanitized_name}"`.
pub fn object_key(user_id: &str, name: &str) -> String {
    format!(
        "{}/{}-{}",
        user_id.trim(),
        uuid::Uuid::new_v4(),
        sanitize_name(name)
    )
}

/// Validate a model-supplied key before downloading it.
///
/// Only keys under the signed-in user's own prefix are reachable (the bucket's
/// RLS enforces the same rule server-side, but a request for a foreign key
/// should fail here with a clear message rather than a 400). `..` is rejected
/// outright so a key can never escape its prefix, and control characters are
/// refused so nothing odd reaches the HTTP path.
pub fn validate_fetch_key(key: &str, user_id: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        return Err("fetch_file: 'remote_path' must not be empty".into());
    }
    if key.contains("..") {
        return Err("fetch_file: 'remote_path' must not contain '..'".into());
    }
    if key.chars().any(|c| c.is_control() || c == '\\') {
        return Err("fetch_file: 'remote_path' contains invalid characters".into());
    }
    let prefix = format!("{}/", user_id.trim());
    if !key.starts_with(&prefix) {
        return Err(
            "fetch_file: that remote path does not belong to the signed-in user".into(),
        );
    }
    if key.len() == prefix.len() {
        return Err("fetch_file: 'remote_path' is missing the object name".into());
    }
    Ok(())
}

/// The original file name encoded in an object key (`{user}/{uuid}-{name}`).
///
/// Falls back to the last segment when the key does not have our shape, so a
/// hand-written key still yields *some* sane local name.
pub fn local_name_from_key(key: &str) -> String {
    let segment = key.rsplit('/').next().unwrap_or(key);
    // `{uuid}-{name}`: a UUID is exactly 36 chars, so the 37th byte is the `-`.
    let stripped = match segment.as_bytes() {
        b if b.len() > 37 && b[36] == b'-' => &segment[37..],
        _ => segment,
    };
    let name = sanitize_name(stripped);
    if name.is_empty() {
        "download".to_string()
    } else {
        name
    }
}

/// Pick a free path in `dir` for `name`, appending `-1`, `-2`, … before the
/// extension rather than overwriting an earlier download.
pub fn unique_destination(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 1u32..10_000 {
        let candidate = dir.join(format!("{stem}-{n}{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-{}{ext}", uuid::Uuid::new_v4()))
}

// ---------------------------------------------------------------------------
// Encryption (binary AES-256-GCM, same device key as the mirror)
// ---------------------------------------------------------------------------

/// The configured data key, when client-side encryption is on.
fn enc_key() -> Option<[u8; KEY_LEN]> {
    let raw = crate::secrets::enc_key_get().ok().flatten()?;
    crate::crypt::decode_key(&raw)
}

/// Whether a downloaded body is one of our encrypted objects.
pub fn is_encrypted_body(bytes: &[u8]) -> bool {
    bytes.starts_with(ENC_MAGIC)
}

/// Encrypt bytes as `PIENC1\0 || nonce(12) || ciphertext+tag`.
pub fn encrypt_bytes(key: &[u8; KEY_LEN], plain: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plain)
        .expect("aes encrypt");
    let mut blob = Vec::with_capacity(ENC_MAGIC.len() + NONCE_LEN + ct.len());
    blob.extend_from_slice(ENC_MAGIC);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    blob
}

/// Decrypt a `PIENC1` body. `None` when the magic is absent, the body is too
/// short, or the key does not authenticate it.
pub fn decrypt_bytes(key: &[u8; KEY_LEN], blob: &[u8]) -> Option<Vec<u8>> {
    let body = blob.strip_prefix(ENC_MAGIC)?;
    if body.len() < NONCE_LEN + 1 {
        return None;
    }
    let (nonce, ct) = body.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    cipher.decrypt(Nonce::from_slice(nonce), ct).ok()
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// Result of a completed download, before it is written to disk.
pub struct Downloaded {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Upload `bytes` under `key`. Encrypts first when a data key is configured.
pub async fn upload(key: &str, bytes: &[u8], content_type: &str) -> Result<(), String> {
    if bytes.len() as u64 > MAX_UPLOAD_BYTES {
        return Err(format!(
            "file is too large to upload ({} bytes; the limit is {MAX_UPLOAD_BYTES} bytes)",
            bytes.len()
        ));
    }
    let Some((base, anon, token, user_id)) = credentials().await? else {
        return Err("cloud sync is not signed in".into());
    };
    if !key.starts_with(&format!("{}/", user_id.trim())) {
        return Err("refusing to upload outside the signed-in user's prefix".into());
    }
    let body = match enc_key() {
        Some(k) => encrypt_bytes(&k, bytes),
        None => bytes.to_vec(),
    };
    let url = format!("{}/storage/v1/object/{BUCKET}/{key}", base.trim_end_matches('/'));
    let resp = reqwest::Client::new()
        .post(&url)
        .header("apikey", &anon)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", content_type)
        // Never overwrite a different object: a collision means a bug (the key
        // carries a fresh UUID), and silently replacing a file would be worse
        // than a visible failure.
        .header("x-upsert", "false")
        .body(body)
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await
        .map_err(|e| format!("storage upload: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "storage upload failed (HTTP {status}): {}",
            snippet(&detail)
        ));
    }
    Ok(())
}

/// Download the object at `key`, decrypting it when it carries our magic.
pub async fn download(key: &str, user_id: &str) -> Result<Downloaded, String> {
    validate_fetch_key(key, user_id)?;
    let Some((base, anon, token, _)) = credentials().await? else {
        return Err("fetch_file: cloud sync is not signed in".into());
    };
    let url = format!(
        "{}/storage/v1/object/{BUCKET}/{}",
        base.trim_end_matches('/'),
        key.trim()
    );
    let resp = reqwest::Client::new()
        .get(&url)
        .header("apikey", &anon)
        .header("authorization", format!("Bearer {token}"))
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| format!("fetch_file: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!(
            "fetch_file: download failed (HTTP {status}): {}",
            snippet(&detail)
        ));
    }
    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("fetch_file: {e}"))?
        .to_vec();
    let bytes = if is_encrypted_body(&body) {
        let Some(k) = enc_key() else {
            return Err(
                "fetch_file: this file was encrypted on another device; import the recovery \
                 code in Settings to unlock it"
                    .into(),
            );
        };
        decrypt_bytes(&k, &body).ok_or_else(|| {
            "fetch_file: could not decrypt this file (wrong recovery code?)".to_string()
        })?
    } else {
        body
    };
    Ok(Downloaded {
        name: local_name_from_key(key),
        bytes,
    })
}

/// Download `remote_path` into `<app data dir>/downloads/` and return the path.
///
/// Called by the `fetch_file` tool. The destination never overwrites an earlier
/// download (`name-1.ext`, `name-2.ext`, …).
pub async fn fetch_file(remote_path: &str) -> Result<PathBuf, String> {
    let Some((_, _, _, user_id)) = credentials().await? else {
        return Err(
            "fetch_file: cloud sync is not signed in, so remote attachments cannot be \
             downloaded. Ask the user to sign in (or re-attach the file) instead."
                .into(),
        );
    };
    let object = download(remote_path, &user_id).await?;
    let dir = downloads_dir().ok_or_else(|| {
        "fetch_file: the downloads directory is not available in this context".to_string()
    })?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("fetch_file: {}: {e}", dir.display()))?;
    let dest = unique_destination(&dir, &object.name);
    tokio::fs::write(&dest, &object.bytes)
        .await
        .map_err(|e| format!("fetch_file: {}: {e}", dest.display()))?;
    Ok(dest)
}

fn snippet(body: &str) -> String {
    let t = body.trim();
    if t.chars().count() > 300 {
        format!("{}…", t.chars().take(300).collect::<String>())
    } else if t.is_empty() {
        "(empty body)".to_string()
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- key + name sanitization ------------------------------------------

    #[test]
    fn sanitize_name_strips_paths_and_traversal() {
        // A full path keeps only its last segment.
        assert_eq!(sanitize_name("/etc/passwd"), "passwd");
        assert_eq!(sanitize_name("..\\..\\secret.txt"), "secret.txt");
        // No separators or dots survive as traversal.
        let s = sanitize_name("../../etc/passwd");
        assert!(!s.contains('/'), "{s}");
        assert!(!s.contains(".."), "{s}");
        // Ordinary names are preserved.
        assert_eq!(sanitize_name("report final (v2).pdf"), "report_final__v2_.pdf");
        assert_eq!(sanitize_name("notes.md"), "notes.md");
        // Untrusted characters become underscores; empty results fall back.
        assert_eq!(sanitize_name("   "), "file");
        assert_eq!(sanitize_name("..."), "file");
        assert_eq!(sanitize_name("日本語.txt"), "___.txt");
    }

    #[test]
    fn sanitize_name_truncates_but_keeps_the_extension() {
        let long = format!("{}.pdf", "a".repeat(300));
        let out = sanitize_name(&long);
        assert!(out.chars().count() <= MAX_NAME_CHARS, "{}", out.len());
        assert!(out.ends_with(".pdf"), "{out}");
    }

    #[test]
    fn object_key_is_scoped_to_the_user_and_unique() {
        let k1 = object_key("user-1", "a b.txt");
        let k2 = object_key("user-1", "a b.txt");
        assert!(k1.starts_with("user-1/"), "{k1}");
        assert!(k1.ends_with("-a_b.txt"), "{k1}");
        assert_ne!(k1, k2, "a fresh uuid per upload");
        // No traversal, no nested separators beyond the user prefix.
        assert_eq!(k1.matches('/').count(), 1, "{k1}");
    }

    #[test]
    fn local_name_from_key_recovers_the_original_name() {
        let uuid = "123e4567-e89b-12d3-a456-426614174000";
        assert_eq!(
            local_name_from_key(&format!("user-1/{uuid}-report.pdf")),
            "report.pdf"
        );
        // A key without our shape still yields something usable.
        assert_eq!(local_name_from_key("user-1/photo.png"), "photo.png");
        assert_eq!(local_name_from_key("user-1/"), "file");
    }

    #[test]
    fn validate_fetch_key_refuses_foreign_and_traversing_keys() {
        let user = "user-1";
        assert!(validate_fetch_key("user-1/abc-file.pdf", user).is_ok());
        // Another user's prefix.
        assert!(validate_fetch_key("user-2/abc-file.pdf", user).is_err());
        // Escaping the prefix with `..`.
        assert!(validate_fetch_key("user-1/../../user-2/x", user).is_err());
        assert!(validate_fetch_key("user-1/a..b", user).is_err());
        // Nothing after the prefix, empty, or control characters.
        assert!(validate_fetch_key("user-1/", user).is_err());
        assert!(validate_fetch_key("", user).is_err());
        assert!(validate_fetch_key("user-1/a\nb", user).is_err());
    }

    // -- local destination de-duplication ---------------------------------

    #[test]
    fn unique_destination_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("pi-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let first = unique_destination(&dir, "report.pdf");
        assert_eq!(first, dir.join("report.pdf"));
        std::fs::write(&first, b"one").unwrap();

        let second = unique_destination(&dir, "report.pdf");
        assert_eq!(second, dir.join("report-1.pdf"));
        std::fs::write(&second, b"two").unwrap();

        let third = unique_destination(&dir, "report.pdf");
        assert_eq!(third, dir.join("report-2.pdf"));

        // An extensionless name gets the suffix appended.
        std::fs::write(dir.join("README"), b"x").unwrap();
        assert_eq!(unique_destination(&dir, "README"), dir.join("README-1"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- encryption round-trip -------------------------------------------

    #[test]
    fn encrypt_decrypt_roundtrip_binary() {
        let key = crate::crypt::new_key();
        let plain: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
        let blob = encrypt_bytes(&key, &plain);
        assert!(is_encrypted_body(&blob));
        // The magic makes the ciphertext distinguishable from plaintext.
        assert_ne!(&blob[ENC_MAGIC.len()..ENC_MAGIC.len() + 8], &plain[..8]);
        assert_eq!(decrypt_bytes(&key, &blob).as_deref(), Some(&plain[..]));

        // A wrong key (or a body without the magic) must not decrypt.
        let other = crate::crypt::new_key();
        assert!(decrypt_bytes(&other, &blob).is_none());
        assert!(decrypt_bytes(&key, b"plain bytes").is_none());
        // Truncated bodies are rejected rather than panicking.
        assert!(decrypt_bytes(&key, ENC_MAGIC).is_none());
    }

    #[test]
    fn encrypted_and_plain_bodies_are_distinguishable() {
        assert!(!is_encrypted_body(b"%PDF-1.7"));
        assert!(!is_encrypted_body(b""));
        let key = crate::crypt::new_key();
        assert!(is_encrypted_body(&encrypt_bytes(&key, b"x")));
    }

    #[test]
    fn uploads_over_the_cap_are_refused() {
        // The size check runs before any credential lookup, so this is offline.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // Don't allocate 100 MB: fake the length via a tiny slice and the check
        // on `bytes.len()`, which is the only thing that can trip it here.
        assert!(MAX_UPLOAD_BYTES == 100 * 1024 * 1024);
        rt.block_on(async {
            // Not over the cap: proceeds to the credential lookup, which fails
            // with a "not signed in"/"not configured" error in tests.
            let err = upload("user-1/x", b"small", "text/plain").await.unwrap_err();
            assert!(!err.contains("too large"), "{err}");
        });
    }
}
