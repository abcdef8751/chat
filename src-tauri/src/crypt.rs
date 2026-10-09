//! Client-side encryption for the cloud mirror.
//!
//! Everything that leaves the device for Supabase goes through AES-256-GCM so
//! the host never sees plaintext. Only the *content-bearing* fields of the
//! synced rows are encrypted; the structural columns (`id`, `revision`,
//! `conversation_id`, `created_at`, `deleted_at`, `path`) stay plaintext so the
//! server can still do last-writer-wins / tombstones / ordering. The local
//! SQLite DB is NOT encrypted — the OS's disk encryption covers idle-at-rest on
//! the device; this only protects the remote mirror.
//!
//! Blob format: `enc:v1:<base64(nonce || ciphertext || tag)>`. The `v1` marker
//! lets old plaintext rows and new ciphertext coexist during migration.
//!
//! The 256-bit data key is a random key generated on the device and stored in
//! the OS keychain (never in `config.json`). It can be shown as a portable
//! "recovery code" and pasted on another device to unlock the same mirror.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;

const PREFIX: &str = "enc:v1:";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// Encrypt `plain` with `key`, returning the versioned `enc:v1:` blob.
pub fn encrypt(key: &[u8; KEY_LEN], plain: &str) -> String {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    // encrypt() returns ciphertext || tag (GCM).
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plain.as_bytes())
        .expect("aes encrypt");
    let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    format!("{PREFIX}{}", B64.encode(blob))
}

/// Decrypt an `enc:v1:` blob. Returns `None` when the blob isn't a recognized
/// encrypted value or the key/auth tag doesn't match (wrong/missing key).
pub fn decrypt(key: &[u8; KEY_LEN], blob: &str) -> Option<String> {
    let raw = blob.strip_prefix(PREFIX)?;
    let data = B64.decode(raw).ok()?;
    if data.len() < NONCE_LEN + 1 {
        return None;
    }
    let (nonce, ct) = data.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let plain = cipher.decrypt(Nonce::from_slice(nonce), ct).ok()?;
    String::from_utf8(plain).ok()
}

/// Whether a stored value is an `enc:v1:` ciphertext blob.
pub fn looks_encrypted(s: &str) -> bool {
    s.starts_with(PREFIX)
}

/// Generate a fresh random 256-bit data key.
pub fn new_key() -> [u8; KEY_LEN] {
    let mut k = [0u8; KEY_LEN];
    OsRng.fill_bytes(&mut k);
    k
}

pub fn encode_key(key: &[u8; KEY_LEN]) -> String {
    B64.encode(key)
}

pub fn decode_key(s: &str) -> Option<[u8; KEY_LEN]> {
    let bytes = B64.decode(s.trim()).ok()?;
    bytes.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let key = new_key();
        let blob = encrypt(&key, "Hello, secret world");
        assert!(looks_encrypted(&blob));
        assert_eq!(decrypt(&key, &blob).as_deref(), Some("Hello, secret world"));
        // Distinct blobs for identical plaintext (random nonce).
        assert_ne!(blob, encrypt(&key, "Hello, secret world"));
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let k1 = new_key();
        let k2 = new_key();
        let blob = encrypt(&k1, "data");
        assert_eq!(decrypt(&k2, &blob), None);
    }

    #[test]
    fn plaintext_is_not_treated_as_encrypted() {
        let key = new_key();
        assert_eq!(decrypt(&key, "just plain text"), None);
        assert!(!looks_encrypted("plain"));
        assert!(looks_encrypted("enc:v1:abc"));
    }

    #[test]
    fn encode_decode_key_roundtrips_recovery_code() {
        let k = new_key();
        assert_eq!(decode_key(&encode_key(&k)), Some(k));
        // Whitespace tolerated (pasted recovery code).
        assert_eq!(decode_key(&format!(" {} \n", encode_key(&k))), Some(k));
        assert_eq!(decode_key("not base64!!"), None);
    }
}
