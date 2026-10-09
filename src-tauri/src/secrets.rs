//! OS-keychain storage for the API key.
//!
//! The key never crosses the IPC boundary: commands here report presence and
//! accept updates, and backend code (`chat`, `models`) reads the secret
//! directly via [`get`].

use keyring::Entry;

/// Keychain service identifier (matches the Tauri app identifier).
const SERVICE: &str = "com.rp.chat";
/// Keychain user/account name for the API key entry.
const USER: &str = "api_key";
/// Keychain account name for the Brave Search API key (separate from the LLM
/// key so the two never collide).
const BRAVE_USER: &str = "brave_key";
/// Keychain account name for the Supabase sync session (a small JSON blob of
/// tokens). Kept out of `config.json` so tokens never sit on disk in plaintext.
const SESSION_USER: &str = "sync_session";

fn entry() -> Result<Entry, String> {
    Entry::new(SERVICE, USER).map_err(|e| format!("open keychain entry: {e}"))
}

fn brave_entry() -> Result<Entry, String> {
    Entry::new(SERVICE, BRAVE_USER).map_err(|e| format!("open keychain entry: {e}"))
}

fn session_entry() -> Result<Entry, String> {
    Entry::new(SERVICE, SESSION_USER).map_err(|e| format!("open keychain entry: {e}"))
}

/// Read the stored API key. `Ok(None)` when no key has been saved yet.
pub fn get() -> Result<Option<String>, String> {
    match entry()?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keychain read: {e}")),
    }
}

/// Store (or replace) the API key.
pub fn set(key: &str) -> Result<(), String> {
    entry()?
        .set_password(key)
        .map_err(|e| format!("keychain write: {e}"))
}

/// Remove the stored API key. Removing a missing entry is a no-op.
pub fn delete() -> Result<(), String> {
    match entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keychain delete: {e}")),
    }
}

/// Whether an API key is stored in the keychain.
#[tauri::command]
pub fn has_api_key() -> Result<bool, String> {
    Ok(get()?.is_some())
}

/// Update the stored API key. `None`/empty removes it.
#[tauri::command]
pub fn set_api_key(api_key: Option<String>) -> Result<(), String> {
    match api_key.as_deref() {
        Some(k) if !k.trim().is_empty() => set(k.trim()),
        _ => delete(),
    }
}

fn brave_get() -> Result<Option<String>, String> {
    match brave_entry()?.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keychain read: {e}")),
    }
}

fn brave_set(key: &str) -> Result<(), String> {
    brave_entry()?
        .set_password(key)
        .map_err(|e| format!("keychain write: {e}"))
}

fn brave_delete() -> Result<(), String> {
    match brave_entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keychain delete: {e}")),
    }
}

// --- Supabase sync session -------------------------------------------------

/// Read the stored sync-session JSON blob. `Ok(None)` when none exists yet.
pub fn session_get() -> Result<Option<String>, String> {
    match session_entry()?.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keychain read: {e}")),
    }
}

/// Store (or replace) the sync-session JSON blob.
pub fn session_set(json: &str) -> Result<(), String> {
    session_entry()?
        .set_password(json)
        .map_err(|e| format!("keychain write: {e}"))
}

/// Remove the stored sync session. Removing a missing entry is a no-op.
pub fn session_delete() -> Result<(), String> {
    match session_entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keychain delete: {e}")),
    }
}

/// Whether a Brave Search API key is stored. Used by `tools::brave_available`
/// to decide whether the ungated Brave web tools are offered at all.
pub fn has_brave_key() -> bool {
    match brave_get() {
        Ok(Some(k)) => !k.is_empty(),
        _ => false,
    }
}

/// Whether a Brave Search API key is stored (frontend status check).
/// Renamed so the IPC name is `has_brave_key` (the Rust helper above is a plain
/// function, not a command, so there's no collision).
#[tauri::command(rename = "has_brave_key")]
pub fn has_brave_key_cmd() -> bool {
    has_brave_key()
}

/// The stored Brave key, for native Brave REST calls (`tools::BraveSearch`).
pub fn get_brave_key() -> Result<Option<String>, String> {
    brave_get()
}

/// Update the stored Brave key. `None`/empty removes it.
#[tauri::command]
pub fn set_brave_key(brave_key: Option<String>) -> Result<(), String> {
    match brave_key.as_deref() {
        Some(k) if !k.trim().is_empty() => brave_set(k.trim()),
        _ => brave_delete(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_delete_roundtrip() {
        // Requires a working OS keychain (Secret Service on Linux); skips
        // cleanly when the entry can't be created (headless CI).
        let entry = match Entry::new(SERVICE, "test-entry") {
            Ok(e) => e,
            Err(_) => return,
        };
        entry.set_password("secret-value").unwrap();
        assert_eq!(entry.get_password().unwrap(), "secret-value");
        entry.delete_credential().unwrap();
        assert!(matches!(
            entry.get_password(),
            Err(keyring::Error::NoEntry)
        ));
    }
}
