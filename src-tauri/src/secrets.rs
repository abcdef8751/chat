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

fn entry() -> Result<Entry, String> {
    Entry::new(SERVICE, USER).map_err(|e| format!("open keychain entry: {e}"))
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
