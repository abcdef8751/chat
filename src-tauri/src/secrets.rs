//! OS-keychain storage for the API key.
//!
//! The key never crosses the IPC boundary: commands here report presence and
//! accept updates, and backend code (`chat`, `models`) reads the secret
//! directly via [`get`].
//!
//! **Desktop** stores each secret in the OS keychain via `keyring`. **Android**
//! has no `keyring` backend (`v1` is cfg-gated away from Android), so we fall
//! back to one small file per secret in the application's private data
//! directory — device-encrypted and sandboxed to the app, mirroring the stance
//! of keeping the local SQLite DB plaintext on-device. Secrets are never sent
//! to Supabase; only backend code reads them, never through the IPC boundary.

#[cfg(not(target_os = "android"))]
use keyring::Entry;

/// Keychain service identifier (matches the Tauri app identifier). Not used on
/// Android, where the file-store keys on the account name alone.
#[cfg(not(target_os = "android"))]
const SERVICE: &str = "com.rp.chat";
/// Keychain user/account name for the API key entry.
const USER: &str = "api_key";
/// Keychain account prefix for per-provider API keys: the account is
/// `provider:<id>`, one per configured provider (see `providers`).
const PROVIDER_KEY_PREFIX: &str = "provider:";
/// Keychain account name for the Brave Search API key (separate from the LLM
/// key so the two never collide).
const BRAVE_USER: &str = "brave_key";
/// Keychain account name for the Supabase sync session (a small JSON blob of
/// tokens). Kept out of `config.json` so tokens never sit on disk in plaintext.
const SESSION_USER: &str = "sync_session";
/// Keychain account for the client-side encryption data key (base64). Caching
/// it here is what makes background auto-sync work without re-entering the
/// passphrase every launch ("remember on this device").
const ENC_KEY_USER: &str = "sync_enc_key";

// --- Storage backends -------------------------------------------------------
// Desktop: the OS keychain via `keyring`, unless `PI_SECRETS_DIR` is set, in
// which case secrets live in plain files under that directory (test-harness
// isolation: the GUI e2e never touches the user's real keychain). Android:
// app-private files (see the module docs). The three operations
// (`read`/`write`/`remove`) are the seam the rest of this file uses, so every
// secret maps to a single account name across all platforms/backends.

/// One small file per secret, named after a lossless hex encoding of the
/// account name. Shared by the Android store and the `PI_SECRETS_DIR`
/// test-harness override so both bytes-match.
mod file_store {
    use std::path::Path;

    fn path_for(dir: &Path, user: &str) -> std::path::PathBuf {
        // Lossless hex encoding of the account name so two distinct accounts
        // can never collide — provider ids may contain `/`, `:`, `_`, etc.,
        // and a collapsing replace would let e.g. `a/b` and `a_b` share a file.
        use std::fmt::Write as _;
        let mut name = String::with_capacity(user.len() * 2);
        for b in user.bytes() {
            let _ = write!(name, "{b:02x}");
        }
        dir.join(format!("secret_{name}"))
    }

    pub fn read(dir: &Path, user: &str) -> Result<Option<String>, String> {
        let p = path_for(dir, user);
        match std::fs::read_to_string(&p) {
            // Match desktop keychain semantics: an empty value is "not set",
            // never a present-but-empty secret.
            Ok(s) if s.is_empty() => Ok(None),
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("secrets read {}: {e}", p.display())),
        }
    }

    pub fn write(dir: &Path, user: &str, value: &str) -> Result<(), String> {
        let p = path_for(dir, user);
        std::fs::write(&p, value).map_err(|e| format!("secrets write {}: {e}", p.display()))?;
        // Tighten to owner-only (the dir itself is already sandboxed).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    pub fn remove(dir: &Path, user: &str) -> Result<(), String> {
        let p = path_for(dir, user);
        match std::fs::remove_file(&p) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("secrets delete {}: {e}", p.display())),
        }
    }
}

/// When set, every secret operation is redirected from the OS keychain into
/// plain files under this directory instead. This is the GUI e2e harness's
/// isolation boundary: `e2e/run.sh` points it at the per-run temp data dir, so
/// a run can never read, write, or delete the user's real keychain entries.
/// Unset (the default) keeps the keyring backend byte-for-byte unchanged.
fn secrets_dir_override() -> Option<std::path::PathBuf> {
    let raw = std::env::var_os("PI_SECRETS_DIR")?;
    if raw.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(raw))
}

/// Read a stored secret for `user`. `Ok(None)` when none has been saved yet.
#[cfg(not(target_os = "android"))]
fn read(user: &str) -> Result<Option<String>, String> {
    if let Some(dir) = secrets_dir_override() {
        return file_store::read(&dir, user);
    }
    let entry = Entry::new(SERVICE, user).map_err(|e| format!("open keychain entry: {e}"))?;
    match entry.get_password() {
        Ok(key) => Ok(Some(key)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("keychain read: {e}")),
    }
}

/// Overwrite (or create) a stored secret for `user`.
#[cfg(not(target_os = "android"))]
fn write(user: &str, value: &str) -> Result<(), String> {
    if let Some(dir) = secrets_dir_override() {
        return file_store::write(&dir, user, value);
    }
    let entry = Entry::new(SERVICE, user).map_err(|e| format!("open keychain entry: {e}"))?;
    entry
        .set_password(value)
        .map_err(|e| format!("keychain write: {e}"))
}

/// Remove a stored secret for `user`. Removing a missing entry is a no-op.
#[cfg(not(target_os = "android"))]
fn remove(user: &str) -> Result<(), String> {
    if let Some(dir) = secrets_dir_override() {
        return file_store::remove(&dir, user);
    }
    let entry = Entry::new(SERVICE, user).map_err(|e| format!("open keychain entry: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("keychain delete: {e}")),
    }
}

/// Android file-backed store (see module docs). One file per `user` name under
/// the app data dir; `init` is called once from `setup`.
#[cfg(target_os = "android")]
mod android_store {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    static DIR: OnceLock<PathBuf> = OnceLock::new();

    /// Initialize the secrets directory. Called once from `setup` with the app
    /// data dir (already created there), so secrets land in app-private,
    /// device-encrypted storage.
    pub fn init(dir: PathBuf) {
        let _ = DIR.set(dir);
    }

    fn dir() -> &'static PathBuf {
        DIR.get()
            .expect("secrets::android_store::init must run before any secret access")
    }

    pub fn read(user: &str) -> Result<Option<String>, String> {
        super::file_store::read(dir(), user)
    }

    pub fn write(user: &str, value: &str) -> Result<(), String> {
        super::file_store::write(dir(), user, value)
    }

    pub fn remove(user: &str) -> Result<(), String> {
        super::file_store::remove(dir(), user)
    }
}

#[cfg(target_os = "android")]
fn read(user: &str) -> Result<Option<String>, String> {
    android_store::read(user)
}

#[cfg(target_os = "android")]
fn write(user: &str, value: &str) -> Result<(), String> {
    android_store::write(user, value)
}

#[cfg(target_os = "android")]
fn remove(user: &str) -> Result<(), String> {
    android_store::remove(user)
}

/// Seed the Android file-store with the app data dir. A no-op on every other
/// platform; called from `setup` so Android secrets resolve a directory.
#[cfg(target_os = "android")]
pub fn init_android(dir: std::path::PathBuf) {
    android_store::init(dir);
}

/// Seed the Android file-store with the app data dir. A no-op on every other
/// platform; called from `setup` so Android secrets resolve a directory.
#[cfg(not(target_os = "android"))]
pub fn init_android(_dir: std::path::PathBuf) {}

// --- API key ----------------------------------------------------------------

/// Read the stored API key. `Ok(None)` when no key has been saved yet.
pub fn get() -> Result<Option<String>, String> {
    read(USER)
}

/// Store (or replace) the API key.
pub fn set(key: &str) -> Result<(), String> {
    write(USER, key)
}

/// Remove the stored API key. Removing a missing entry is a no-op.
pub fn delete() -> Result<(), String> {
    remove(USER)
}

// --- Per-provider API keys --------------------------------------------------
// Each configured provider stores its own key under the account `provider:<id>`.
// This keeps one key per provider so switching providers swaps credentials too.

/// Read a provider's stored API key. `Ok(None)` when none is saved yet.
pub fn get_provider_key(provider_id: &str) -> Result<Option<String>, String> {
    read(&format!("{PROVIDER_KEY_PREFIX}{provider_id}"))
}

/// Store (or replace) a provider's API key.
pub fn set_provider_key(provider_id: &str, key: &str) -> Result<(), String> {
    write(&format!("{PROVIDER_KEY_PREFIX}{provider_id}"), key)
}

/// Remove a provider's API key. Removing a missing entry is a no-op.
pub fn delete_provider_key(provider_id: &str) -> Result<(), String> {
    remove(&format!("{PROVIDER_KEY_PREFIX}{provider_id}"))
}

/// Whether a specific provider has a stored key.
pub fn has_provider_key(provider_id: &str) -> bool {
    matches!(get_provider_key(provider_id), Ok(Some(k)) if !k.is_empty())
}

/// Whether an API key is stored in the keychain.
#[tauri::command]
pub fn has_api_key() -> Result<bool, String> {
    Ok(get()?.is_some())
}

/// Keychain-presence check that never errors (used by provider listing).
pub fn has_api_key_quiet() -> bool {
    get().map(|k| k.is_some()).unwrap_or(false)
}

/// Update the stored API key. `None`/empty removes it.
#[tauri::command]
pub fn set_api_key(api_key: Option<String>) -> Result<(), String> {
    match api_key.as_deref() {
        Some(k) if !k.trim().is_empty() => set(k.trim()),
        _ => delete(),
    }
}

// --- Brave Search API key ---------------------------------------------------

fn brave_get() -> Result<Option<String>, String> {
    read(BRAVE_USER)
}

fn brave_set(key: &str) -> Result<(), String> {
    write(BRAVE_USER, key)
}

fn brave_delete() -> Result<(), String> {
    remove(BRAVE_USER)
}

/// Whether a Brave Search API key is stored. Used by `tools::brave_available`
/// to decide whether the ungated Brave web tools are offered at all.
pub fn has_brave_key() -> bool {
    matches!(brave_get(), Ok(Some(k)) if !k.is_empty())
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

// --- Supabase sync session -------------------------------------------------

/// Read the stored sync-session JSON blob. `Ok(None)` when none exists yet.
pub fn session_get() -> Result<Option<String>, String> {
    read(SESSION_USER)
}

/// Store (or replace) the sync-session JSON blob.
pub fn session_set(json: &str) -> Result<(), String> {
    write(SESSION_USER, json)
}

/// Remove the stored sync session. Removing a missing entry is a no-op.
pub fn session_delete() -> Result<(), String> {
    remove(SESSION_USER)
}

// --- Client-side encryption keys --------------------------------------------

/// Read the cached data key (base64) from the keychain. `Ok(None)` = off.
pub fn enc_key_get() -> Result<Option<String>, String> {
    read(ENC_KEY_USER)
}

pub fn enc_key_set(key: &str) -> Result<(), String> {
    write(ENC_KEY_USER, key)
}

pub fn enc_key_delete() -> Result<(), String> {
    remove(ENC_KEY_USER)
}

/// Whether client-side encryption is configured (data key present).
pub fn has_encryption() -> bool {
    matches!(enc_key_get(), Ok(Some(k)) if !k.is_empty())
}

#[cfg(all(test, not(target_os = "android")))]
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
