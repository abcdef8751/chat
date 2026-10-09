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
//! The data key is a random 256-bit AES key. So it can be passphrase-backed
//! (and re-derived on another device), it is wrapped with a key derived from the
//! user's passphrase via Argon2id; the wrapped blob + salt are stored in the OS
//! keychain and are useless without the passphrase.

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::Argon2;
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;

const PREFIX: &str = "enc:v1:";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const SALT_LEN: usize = 16;

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

/// Derive a 256-bit data key from a passphrase + salt via Argon2id.
pub fn derive_key(passphrase: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    let mut out = [0u8; KEY_LEN];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut out)
        .expect("argon2 derive");
    out
}

/// Derive a passphrase-bound key-encryption key (KEK).
pub fn kek(passphrase: &str, salt: &[u8]) -> [u8; KEY_LEN] {
    derive_key(passphrase, salt)
}

/// Wrap a data key with a passphrase-derived KEK, returning a portable blob
/// (salt || wrapped-key). This is stored in the keychain and could be copied to
/// another device so the same passphrase recovers the same data key there.
pub fn wrap_data_key(data_key: &[u8; KEY_LEN], passphrase: &str) -> String {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    let kek = kek(passphrase, &salt);
    let cipher = Aes256Gcm::new_from_slice(&kek).expect("kek");
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), data_key.as_slice())
        .expect("wrap");
    let mut blob = Vec::with_capacity(SALT_LEN + NONCE_LEN + ct.len());
    blob.extend_from_slice(&salt);
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    B64.encode(blob)
}

/// Unwrap a data key using a passphrase. Returns `Err` when the passphrase is
/// wrong (AEAD auth fails) or the blob is malformed.
pub fn unwrap_data_key(wrapped: &str, passphrase: &str) -> Result<[u8; KEY_LEN], String> {
    let data = B64.decode(wrapped).map_err(|e| format!("bad wrapped key: {e}"))?;
    if data.len() < SALT_LEN + NONCE_LEN + 1 {
        return Err("wrapped key too short".into());
    }
    let (salt_bytes, rest) = data.split_at(SALT_LEN);
    let (nonce, ct) = rest.split_at(NONCE_LEN);
    let salt: [u8; SALT_LEN] = salt_bytes
        .try_into()
        .map_err(|_| "bad salt len".to_string())?;
    let kek = kek(passphrase, &salt);
    let cipher = Aes256Gcm::new_from_slice(&kek).map_err(|e| e.to_string())?;
    let key_bytes = cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| "wrong passphrase — could not unlock encryption".to_string())?;
    key_bytes
        .try_into()
        .map_err(|_| "unwrapped key not 32 bytes".to_string())
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
    let bytes = B64.decode(s).ok()?;
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
    fn wrap_unwrap_roundtrip() {
        let data_key = new_key();
        let wrapped = wrap_data_key(&data_key, "hunter2-pass");
        let recovered = unwrap_data_key(&wrapped, "hunter2-pass").unwrap();
        assert_eq!(recovered, data_key);
        // Wrong passphrase must fail.
        assert!(unwrap_data_key(&wrapped, "wrong").is_err());
    }

    #[test]
    fn sha_salt_per_passphrase_deterministic() {
        // Same data key wrapped with the same passphrase → same recoverable key
        // regardless of a fresh random KEK salt (unwrap uses the embedded salt).
        let data_key = new_key();
        let w1 = wrap_data_key(&data_key, "pw");
        let w2 = wrap_data_key(&data_key, "pw");
        assert_ne!(w1, w2); // different KEK salts
        assert_eq!(unwrap_data_key(&w1, "pw").unwrap(), data_key);
        assert_eq!(unwrap_data_key(&w2, "pw").unwrap(), data_key);
    }

    #[test]
    fn encode_decode_key() {
        let k = new_key();
        assert_eq!(decode_key(&encode_key(&k)), Some(k));
        assert_eq!(decode_key("not base64!!"), None);
    }
}
