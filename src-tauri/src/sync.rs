//! Supabase backup + sync — opt-in, local-first.
//!
//! SQLite stays the source of truth; the network is a best-effort mirror. When
//! `sync_enabled` is false or there is no signed-in session the whole module is
//! idle and never blocks the UI. Push upserts locally changed rows (dirty=1)
//! via a single PostgREST POST; pull applies remote rows with last-writer-wins
//! by revision, tombstone handling, and cross-device message re-indexing.
//!
//! Matches the Brave native-`reqwest` pattern: no Node/MCP process, no
//! `supabase-js`. Only the publishable (anon) key is embedded at build time.
//! Session tokens live in the OS keychain and never cross IPC.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::config::ConfigState;
use crate::crypt;
use crate::db::{self, Db};
use crate::memory::MemoryState;
use crate::secrets::{self};

/// Publishable Supabase credentials embedded at build time from `.env` via
/// `build.rs`. The secret/service-role key is never compiled in.
const SUPABASE_URL: &str = match option_env!("SUPABASE_URL") {
    Some(v) => v,
    None => "https://localhost.supabase.co",
};
const SUPABASE_ANON: &str = match option_env!("SUPABASE_PUBLISHABLE_KEY") {
    Some(v) => v,
    None => "anon-placeholder",
};

/// Build-time override (used by tests to point at a local mock server). Set via
/// the `SYNC_BASE_URL` / `SYNC_ANON` environment variables when set, otherwise
/// the embedded real project values.
fn base_url() -> String {
    std::env::var("SYNC_BASE_URL")
        .unwrap_or_else(|_| SUPABASE_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn anon_key() -> String {
    std::env::var("SYNC_ANON").unwrap_or_else(|_| SUPABASE_ANON.to_string())
}

/// Public status surface for the frontend.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub enabled: bool,
    pub logged_in: bool,
    pub email: Option<String>,
    pub syncing: bool,
    pub pending: u64,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
    // Live per-run progress ("push" | "pull" | "" when idle).
    pub phase: String,
    pub pushed: u64,
    pub pulled: u64,
    // Whether client-side encryption is configured (data sent to Supabase is
    // ciphertext; only this device's keychain can decrypt it).
    pub encryption: bool,
    // The remote backup is encrypted but this device has no key yet (a second
    // device that must be unlocked with the passphrase before it may sync).
    pub locked: bool,
}

#[derive(Default)]
struct SyncStats {
    last_sync_at: Option<i64>,
    last_error: Option<String>,
    phase: String,
    pushed: u64,
    pulled: u64,
    locked: bool,
}

/// Shared sync state installed into Tauri: HTTP client, endpoints, and the
/// global "am I syncing" flag + last-run stats.
pub struct SyncState {
    client: reqwest::Client,
    base_url: String,
    anon: String,
    syncing: Arc<AtomicBool>,
    stats: Mutex<SyncStats>,
}

impl SyncState {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(90))
            .build()
            .unwrap_or_default();
        Self {
            client,
            base_url: base_url(),
            anon: anon_key(),
            syncing: Arc::new(AtomicBool::new(false)),
            stats: Mutex::new(SyncStats::default()),
        }
    }

    /// Build a state pointing at an explicit endpoint (used by tests).
    #[allow(dead_code)]
    pub fn for_url(base_url: &str, anon: &str) -> Self {
        Self {
            client: reqwest::Client::builder().build().unwrap_or_default(),
            base_url: base_url.trim_end_matches('/').to_string(),
            anon: anon.to_string(),
            syncing: Arc::new(AtomicBool::new(false)),
            stats: Mutex::new(SyncStats::default()),
        }
    }

    fn set_error(&self, err: String) {
        if let Ok(mut s) = self.stats.lock() {
            s.last_error = Some(err);
            s.phase = String::new();
        }
    }

    fn set_success(&self, at: i64) {
        if let Ok(mut s) = self.stats.lock() {
            s.last_sync_at = Some(at);
            s.last_error = None;
            s.phase = String::new();
        }
    }

    /// Reset the live counters for a fresh run. Phase starts at "push".
    fn begin_run(&self) {
        if let Ok(mut s) = self.stats.lock() {
            s.phase = "push".into();
            s.pushed = 0;
            s.pulled = 0;
        }
    }

    fn set_phase(&self, phase: &str) {
        if let Ok(mut s) = self.stats.lock() {
            s.phase = phase.into();
        }
    }

    fn add_pushed(&self, n: u64) {
        if let Ok(mut s) = self.stats.lock() {
            s.pushed += n;
        }
    }

    fn add_pulled(&self, n: u64) {
        if let Ok(mut s) = self.stats.lock() {
            s.pulled += n;
        }
    }

    fn set_locked(&self, locked: bool) {
        if let Ok(mut s) = self.stats.lock() {
            s.locked = locked;
        }
    }

    fn snapshot(&self) -> (bool, Option<i64>, Option<String>, String, u64, u64, bool) {
        let syncing = self.syncing.load(Ordering::SeqCst);
        match self.stats.lock() {
            Ok(s) => (
                syncing,
                s.last_sync_at,
                s.last_error.clone(),
                s.phase.clone(),
                s.pushed,
                s.pulled,
                s.locked,
            ),
            Err(_) => (syncing, None, None, String::new(), 0, 0, false),
        }
    }
}

/// The Supabase auth session persisted in the OS keychain. Tokens never cross
/// the IPC boundary.
#[derive(Serialize, Deserialize, Clone)]
struct Session {
    access_token: String,
    refresh_token: String,
    user_id: String,
    email: String,
    /// ms epoch when the access token expires (from GoTrue `expires_in`).
    expires_at: i64,
}

fn now_ms() -> i64 {
    db::now_ms()
}

fn load_session() -> Result<Option<Session>, String> {
    match secrets::session_get()? {
        Some(json) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("parse session: {e}")),
        None => Ok(None),
    }
}

fn save_session(session: &Session) -> Result<(), String> {
    let json = serde_json::to_string(session).map_err(|e| format!("serialize session: {e}"))?;
    secrets::session_set(&json)
}

/// Build a `SyncStatus` from the current config, keychain session, and state.
fn status(app: &AppHandle) -> SyncStatus {
    let cfg = app.state::<ConfigState>().get();
    let session = load_session().ok().flatten();
    let sync = app.state::<SyncState>();
    let (syncing, last_sync_at, last_error, phase, pushed, pulled, locked) = sync.snapshot();
    let pending = db::pending_rows(app.state::<Db>().inner()).unwrap_or(0);
    // Prefer the live in-memory value while it is set; otherwise fall back to the
    // timestamp persisted in config so a fresh process does not forget past syncs.
    let last_sync_at = last_sync_at.or(cfg.sync_last_sync_at);
    SyncStatus {
        enabled: cfg.sync_enabled,
        logged_in: session.is_some(),
        email: session.map(|s| s.email),
        syncing,
        pending,
        last_sync_at,
        last_error,
        phase,
        pushed,
        pulled,
        encryption: secrets::has_encryption(),
        locked,
    }
}

/// Enable client-side encryption: generate a random 256-bit key, store it in the
/// OS keychain, and mark every row dirty so the mirror is (re-)uploaded as
/// ciphertext. No passphrase. The key can be shown as a recovery code and pasted
/// on another device (`sync_import_key`).
#[tauri::command]
pub async fn sync_set_encryption(app: AppHandle) -> Result<SyncStatus, String> {
    // Reuse an existing key if present; only generate when there isn't one.
    if secrets::enc_key_get()?.and_then(|k| crypt::decode_key(&k)).is_none() {
        let k = crypt::new_key();
        secrets::enc_key_set(&crypt::encode_key(&k))?;
    }
    db::mark_all_dirty_for_resync(dbref(&app))?;
    app.state::<SyncState>().set_locked(false);
    // Kick off a re-upload (now encrypted) shortly after.
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = do_sync(&app2).await;
    });
    Ok(status(&app))
}

/// Import a recovery code (the base64 key from another device) to unlock this
/// device. Verifies it against the existing remote ciphertext before caching, so
/// a wrong code is rejected rather than corrupting the mirror.
#[tauri::command]
pub async fn sync_import_key(app: AppHandle, code: String) -> Result<SyncStatus, String> {
    let key = crypt::decode_key(&code)
        .ok_or("That doesn't look like a valid recovery code.")?;
    let Some(session) = load_session()? else {
        return Err("Sign in before unlocking.".into());
    };
    let sync = app.state::<SyncState>();
    let token = valid_access_token(&sync, &session).await?;
    let ctx = SyncCtx {
        client: &sync.client,
        base_url: &sync.base_url,
        anon: &sync.anon,
        db: dbref(&app),
        memory: app.state::<MemoryState>().inner(),
        key: Some(key),
    };
    if let Some(sample) = fetch_encrypted_sample(&ctx, &token).await? {
        if crypt::decrypt(&key, &sample).is_none() {
            return Err("Wrong recovery code — it doesn't decrypt this backup.".into());
        }
    }
    secrets::enc_key_set(&crypt::encode_key(&key))?;
    db::mark_all_dirty_for_resync(dbref(&app))?;
    sync.set_locked(false);
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = do_sync(&app2).await;
    });
    Ok(status(&app))
}

/// Return the current key as a portable recovery code (base64). Intended for the
/// user to record it and use on another device. Empty string when encryption is
/// off.
#[tauri::command]
pub fn sync_recovery_code(app: AppHandle) -> Result<String, String> {
    let _ = app;
    Ok(secrets::enc_key_get()?.unwrap_or_default())
}

/// Disable client-side encryption: forgets the data key and re-marks every row
/// dirty so the next push re-uploads plaintext.
#[tauri::command]
pub async fn sync_remove_encryption(app: AppHandle) -> Result<SyncStatus, String> {
    secrets::enc_key_delete()?;
    db::mark_all_dirty_for_resync(app.state::<Db>().inner())?;
    app.state::<SyncState>().set_locked(false);
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = do_sync(&app2).await;
    });
    Ok(status(&app))
}

/// Fetch one content string from the server if it is an encrypted blob. `Ok(None)`
/// means the account has no encrypted data yet (fresh/first-time enable). Used
/// both to detect a locked device and to verify a passphrase.
async fn fetch_encrypted_sample(
    ctx: &SyncCtx<'_>,
    token: &str,
) -> Result<Option<String>, String> {
    for (table, field) in [("conversations", "title"), ("messages", "content")] {
        let url = format!("{}/rest/v1/{}?select={}&limit=1", ctx.base_url, table, field);
        let resp = ctx
            .client
            .get(&url)
            .header("apikey", ctx.anon)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| format!("probe {table}: {e}"))?;
        if !resp.status().is_success() {
            continue;
        }
        let rows: Vec<Value> = resp.json().await.unwrap_or_default();
        if let Some(s) = rows
            .first()
            .and_then(|v| v.get(field))
            .and_then(|v| v.as_str())
        {
            if crypt::looks_encrypted(s) {
                return Ok(Some(s.to_string()));
            }
        }
    }
    Ok(None)
}

#[tauri::command]
pub async fn sync_sign_in(
    app: AppHandle,
    email: String,
    password: String,
) -> Result<SyncStatus, String> {
    let sync = app.state::<SyncState>();
    let url = format!("{}/auth/v1/token?grant_type=password", sync.base_url);
    let resp = sync
        .client
        .post(&url)
        .header("apikey", &sync.anon)
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .map_err(|e| format!("sign in request: {e}"))?;
    let session = parse_gotrue_session(resp).await?;
    save_session(&session)?;
    // Kick off an initial full push+pull shortly after sign-in.
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = do_sync(&app2).await;
    });
    Ok(status(&app))
}

#[tauri::command]
pub fn sync_sign_out(_app: AppHandle) -> Result<(), String> {
    secrets::session_delete()?;
    Ok(())
}

/// Create a new account via GoTrue's `/auth/v1/signup`. Returns the resulting
/// status so the UI knows whether the user is now signed in.
///
/// With **Confirm email OFF** (the current personal-tool setup) GoTrue returns a
/// session immediately, so we persist it and the user is signed in. With it ON,
/// the response carries no `access_token` and the account stays logged-out until
/// the emailed link is clicked — the frontend shows a confirmation notice.
///
/// Errors carry a friendly, actionable message (already registered → sign in
/// instead; weak password; rate limit).
#[tauri::command]
pub async fn sync_sign_up(app: AppHandle, email: String, password: String) -> Result<SyncStatus, String> {
    let sync = app.state::<SyncState>();
    let url = format!("{}/auth/v1/signup", sync.base_url);
    let resp = sync
        .client
        .post(&url)
        .header("apikey", &sync.anon)
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await
        .map_err(|e| format!("sign up request: {e}"))?;
    let http_ok = resp.status().is_success();
    let body = resp.text().await.unwrap_or_default();
    if http_ok {
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        // Confirm-email OFF → a session is returned: auto sign-in. Confirm ON →
        // no token yet: stay logged out (the emailed link completes verification).
        if let Ok(session) = session_from_value(&v) {
            save_session(&session)?;
            let app2 = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = do_sync(&app2).await;
            });
        }
        return Ok(status(&app));
    }
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    let code = v.get("error_code").and_then(|x| x.as_str()).unwrap_or("unknown");
    let detail = v
        .get("msg")
        .and_then(|x| x.as_str())
        .or_else(|| v.get("error_description").and_then(|x| x.as_str()));
    let msg = match code {
        "user_already_exists" | "email_exists" | "user_exists" => {
            "An account with this email already exists — sign in instead.".to_string()
        }
        "weak_password" => "Password is too weak — use at least 6 characters.".to_string(),
        "signup_disabled" => "Sign-ups are disabled for this project.".to_string(),
        "over_email_send_rate_limit" => "Too many sign-up attempts — wait a moment and retry."
            .to_string(),
        _ => format!(
            "Sign up failed: {}",
            detail.unwrap_or(code)
        ),
    };
    Err(msg)
}

#[tauri::command]
pub fn sync_status(app: AppHandle) -> Result<SyncStatus, String> {
    Ok(status(&app))
}

#[tauri::command]
pub async fn sync_toggle(app: AppHandle, on: bool) -> Result<SyncStatus, String> {
    let config = app.state::<ConfigState>();
    let mut cfg = config.get();
    cfg.sync_enabled = on;
    config.set(cfg)?;
    if on {
        // Enabling triggers an initial full push (no-op if not signed in).
        let app2 = app.clone();
        tauri::async_runtime::spawn(async move {
            let _ = do_sync(&app2).await;
        });
    }
    Ok(status(&app))
}

/// Manual push+pull, awaited to completion. Errors are recorded in state
/// (`lastError`) but we still return a status so the UI can reflect them.
#[tauri::command]
pub async fn sync_now(app: AppHandle) -> Result<SyncStatus, String> {
    let _ = do_sync(&app).await;
    Ok(status(&app))
}

/// Background scheduler: when sync is enabled + signed in (and not already
/// syncing) run push then pull every 60 seconds. Never blocks the UI; failures
/// degrade to `lastError` and retry on the next tick.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            if let Err(e) = do_sync(&app).await {
                eprintln!("sync: {e}");
            }
        }
    });
}

async fn parse_gotrue_session(resp: reqwest::Response) -> Result<Session, String> {
    let status = resp.status();
    let body = resp.text().await.map_err(|e| format!("read auth body: {e}"))?;
    let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if let Some(code) = v.get("error_code").and_then(|x| x.as_str()) {
        if code == "email_not_confirmed" {
            return Err(
                "Confirm your email first — check your inbox for the confirmation link.".into(),
            );
        }
        let msg = v
            .get("msg")
            .and_then(|x| x.as_str())
            .unwrap_or_else(|| v.get("error_description").and_then(|x| x.as_str()).unwrap_or(""));
        return Err(format!("auth error {code}: {msg}"));
    }
    if !status.is_success() {
        return Err(format!(
            "auth failed (HTTP {}): {}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        ));
    }
    session_from_value(&v)
}

/// Build a [`Session`] from a GoTrue token response body that carries an
/// `access_token` (a password sign-in, a token refresh, or a sign-up with email
/// confirmation disabled). Errors when the response has no usable token.
fn session_from_value(v: &Value) -> Result<Session, String> {
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or("no access_token in auth response")?
        .to_string();
    let refresh_token = v
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let expires_in = v.get("expires_in").and_then(|x| x.as_i64()).unwrap_or(3600);
    let user = v.get("user").cloned().unwrap_or(Value::Null);
    let user_id = user.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let email = user
        .get("email")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    Ok(Session {
        access_token,
        refresh_token,
        user_id,
        email,
        expires_at: now_ms() + expires_in * 1000,
    })
}

/// Return a valid access token, refreshing from `refresh_token` if the stored
/// one is expired or about to expire. The refreshed session is persisted.
async fn valid_access_token(sync: &SyncState, session: &Session) -> Result<String, String> {
    if session.expires_at == 0 || now_ms() < session.expires_at - 60_000 {
        return Ok(session.access_token.clone());
    }
    let url = format!("{}/auth/v1/token?grant_type=refresh_token", sync.base_url);
    let resp = sync
        .client
        .post(&url)
        .header("apikey", &sync.anon)
        .header(CONTENT_TYPE, "application/json")
        .json(&json!({ "refresh_token": session.refresh_token }))
        .send()
        .await
        .map_err(|e| format!("refresh request: {e}"))?;
    let updated = parse_gotrue_session(resp).await?;
    save_session(&updated)?;
    Ok(updated.access_token)
}

// ---------------------------------------------------------------------------
// Sync engine
// ---------------------------------------------------------------------------

/// Everything the push/pull helpers need, borrowed for the duration of a sync.
/// Everything the push/pull helpers need, borrowed for the duration of a sync.
struct SyncCtx<'a> {
    client: &'a reqwest::Client,
    base_url: &'a str,
    anon: &'a str,
    db: &'a Db,
    memory: &'a MemoryState,
    /// Client-side encryption key, if configured. When `Some`, sync encrypts
    /// content on the way out and decrypts on the way in.
    key: Option<[u8; 32]>,
}

/// One full sync round: push changed rows, then pull remote changes. Guarded so
/// two syncs never run concurrently. Errors are recorded in state by the caller.
pub async fn do_sync(app: &AppHandle) -> Result<(), String> {
    let cfg = app.state::<ConfigState>().get();
    if !cfg.sync_enabled {
        return Ok(());
    }
    let Some(session) = load_session()? else {
        return Ok(());
    };
    let sync = app.state::<SyncState>();
    if sync.syncing.swap(true, Ordering::SeqCst) {
        return Ok(()); // already running
    }
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }
    let _guard = Guard(sync.syncing.clone());

    let token = match valid_access_token(&sync, &session).await {
        Ok(t) => t,
        Err(e) => {
            sync.set_error(e.clone());
            return Err(e);
        }
    };
    let ctx = SyncCtx {
        client: &sync.client,
        base_url: &sync.base_url,
        anon: &sync.anon,
        db: dbref(app),
        memory: app.state::<MemoryState>().inner(),
        key: secrets::enc_key_get()
            .ok()
            .flatten()
            .and_then(|k| crypt::decode_key(&k)),
    };
    // First sync: mark all pre-existing rows + memory files dirty so the initial
    // push backs up full history. Scoped per account (a guarded no-op after each
    // account's first run).
    let mem_names = ctx.memory.file_names();
    db::seed_initial_sync(ctx.db, &session.user_id, &mem_names)?;
    // A device without the key must never push plaintext over an encrypted
    // mirror. Detect it and refuse (the UI then asks for the passphrase).
    if ctx.key.is_none() {
        if let Ok(Some(_)) = fetch_encrypted_sample(&ctx, &token).await {
            sync.set_locked(true);
            sync.set_error(
                "This backup is encrypted — enter your passphrase in Settings to unlock.".into(),
            );
            return Ok(());
        }
    }
    sync.set_locked(false);
    sync.begin_run();
    // Progress (pushed/pending) is updated per batch inside `push`.
    match push(&ctx, &token, &sync).await {
        Ok(_) => {}
        Err(e) => {
            sync.set_error(e.clone());
            return Err(e);
        }
    }
    sync.set_phase("pull");
    match pull(&ctx, &session.user_id, &token).await {
        Ok(n) => sync.add_pulled(n),
        Err(e) => {
            sync.set_error(e.clone());
            return Err(e);
        }
    }
    // Persist the success timestamp so it survives restarts (the live value in
    // SyncState is in-memory only; without this the page would show "never"
    // after a relaunch even though syncs ran in earlier sessions).
    let at = now_ms();
    sync.set_success(at);
    let mut cfg = app.state::<ConfigState>().get();
    cfg.sync_last_sync_at = Some(at);
    let _ = app.state::<ConfigState>().set(cfg);
    Ok(())
}

/// Fetch a `&Db` reference from the app state (State derefs to Db).
fn dbref(app: &AppHandle) -> &Db {
    app.state::<Db>().inner()
}

// --- Push -------------------------------------------------------------------

/// Encrypt a required string field before upload (no-op when encryption is off).
fn enc_str(key: Option<[u8; 32]>, s: String) -> String {
    match key {
        Some(k) => crypt::encrypt(&k, &s),
        None => s,
    }
}

/// Encrypt an optional string field before upload (no-op when off/None).
fn enc_opt(key: Option<[u8; 32]>, s: Option<String>) -> Option<String> {
    s.map(|v| enc_str(key, v))
}

/// Upsert all locally changed rows (conversations, messages, memory files) plus
/// conversation tombstones, as PostgREST `merge-duplicates` upserts. Returns the
/// number of rows pushed. Dirty flags are cleared only for entities whose POST
/// succeeded, so a partial failure is retried next tick.
async fn push(ctx: &SyncCtx<'_>, token: &str, progress: &SyncState) -> Result<u64, String> {
    let mut total: u64 = 0;

    // Conversations (dirty rows + tombstones).
    let (conv_rows, conv_tombstones) = {
        let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        let mut stmt = conn
            .prepare(
                "SELECT id, title, model, provider_id, system_prompt, compaction_summary,
                        last_reflected_index, imported, import_batch, created_at, updated_at, revision
                 FROM conversations WHERE dirty = 1",
            )
            .map_err(|e| e.to_string())?;
        let key = ctx.key;
        let r = stmt
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "title": enc_str(key, r.get::<_, String>(1)?),
                    "model": r.get::<_, Option<String>>(2)?,
                    "provider_id": r.get::<_, Option<String>>(3)?,
                    "system_prompt": enc_opt(key, r.get::<_, Option<String>>(4)?),
                    "compaction_summary": enc_opt(key, r.get::<_, Option<String>>(5)?),
                    "last_reflected_index": r.get::<_, Option<i64>>(6)?,
                    "imported": r.get::<_, i64>(7)? != 0,
                    "import_batch": r.get::<_, i64>(8)?,
                    "created_at": r.get::<_, i64>(9)?,
                    "updated_at": r.get::<_, i64>(10)?,
                    "revision": r.get::<_, i64>(11)?,
                    "deleted_at": Value::Null,
                }))
            })
            .map_err(|e| e.to_string())?;
        for row in r {
            rows.push(row.map_err(|e| e.to_string())?);
        }
        drop(stmt);
        // Tombstoned conversations: fill minimal NOT NULL fields; consumers
        // hard-delete on `deleted_at`, so the placeholder content is never read.
        let mut tombstones = Vec::new();
        let mut tstmt = conn
            .prepare("SELECT id, revision FROM sync_tombstones WHERE entity = 'conversations'")
            .map_err(|e| e.to_string())?;
        let trows = tstmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let now = now_ms();
        for t in trows {
            let (id, rev) = t.map_err(|e| e.to_string())?;
            tombstones.push(json!({
                "id": id, "title": "", "model": Value::Null, "provider_id": Value::Null,
                "system_prompt": Value::Null, "compaction_summary": Value::Null,
                "last_reflected_index": Value::Null, "imported": false, "import_batch": 0,
                "created_at": now, "updated_at": now, "revision": rev, "deleted_at": now,
            }));
        }
        (rows, tombstones)
    };

    let row_count = conv_rows.len() + conv_tombstones.len();
    if !conv_rows.is_empty() || !conv_tombstones.is_empty() {
        let mut body = conv_rows;
        body.extend(conv_tombstones);
        post_upsert(ctx, "conversations", token, &body, progress, |chunk| {
            clear_conversation_keys(ctx.db, chunk)
        })
        .await?;
        total += row_count as u64;
    }

    // Messages.
    let msg_rows = {
        let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, conversation_id, role, \"index\", content, model, provider,
                        thinking_level, thinking, usage, stop_reason, attachments, created_at, revision
                 FROM messages WHERE dirty = 1",
            )
            .map_err(|e| e.to_string())?;
        let key = ctx.key;
        let r = stmt
            .query_map([], |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "conversation_id": r.get::<_, String>(1)?,
                    "role": r.get::<_, String>(2)?,
                    "seq": r.get::<_, i64>(3)?,
                    "content": enc_str(key, r.get::<_, String>(4)?),
                    "model": r.get::<_, Option<String>>(5)?,
                    "provider": r.get::<_, Option<String>>(6)?,
                    "thinking_level": r.get::<_, Option<String>>(7)?,
                    "thinking": enc_opt(key, r.get::<_, Option<String>>(8)?),
                    "usage": enc_opt(key, r.get::<_, Option<String>>(9)?),
                    "stop_reason": r.get::<_, Option<String>>(10)?,
                    "attachments": enc_opt(key, r.get::<_, Option<String>>(11)?),
                    "created_at": r.get::<_, i64>(12)?,
                    "revision": r.get::<_, i64>(13)?,
                    "deleted_at": Value::Null,
                }))
            })
            .map_err(|e| e.to_string())?;
        let rows: Result<Vec<Value>, String> =
            r.map(|row| row.map_err(|e| e.to_string())).collect();
        drop(stmt);
        rows?
    };
    if !msg_rows.is_empty() {
        post_upsert(ctx, "messages", token, &msg_rows, progress, |chunk| {
            clear_message_keys(ctx.db, chunk)
        })
        .await?;
        total += msg_rows.len() as u64;
    }

    // Memory files (live content or tombstone).
    let mem_rows = {
        let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare("SELECT path, revision, last_local_at FROM memory_sync WHERE dirty = 1")
            .map_err(|e| e.to_string())?;
        let r = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let rows: Result<Vec<(String, i64, i64)>, String> =
            r.map(|row| row.map_err(|e| e.to_string())).collect();
        drop(stmt);
        rows?
    };
    let mut mem_body = Vec::new();
    let key = ctx.key;
    for (path, revision, last_local_at) in &mem_rows {
        let row = if ctx.memory.exists(path) {
            let content = enc_str(key, ctx.memory.read(path).unwrap_or_default());
            json!({
                "path": path, "content": content, "revision": revision,
                "deleted_at": Value::Null,
                "created_at": last_local_at, "updated_at": now_ms(),
            })
        } else {
            // Tombstone: the remote `content` column is NOT NULL, so send an empty
            // placeholder rather than null. Consumers delete on `deleted_at` alone
            // (`apply_memory` never reads a tombstone's content), so "" is safe.
            json!({
                "path": path, "content": "", "revision": revision,
                "deleted_at": now_ms(), "created_at": last_local_at, "updated_at": now_ms(),
            })
        };
        mem_body.push(row);
    }
    if !mem_body.is_empty() {
        post_upsert(ctx, "memory_files", token, &mem_body, progress, |chunk| {
            clear_memory_keys(ctx.db, chunk)
        })
        .await?;
        total += mem_body.len() as u64;
    }

    Ok(total)
}

/// Clear the dirty flag (guarded by revision) for the message rows in a batch
/// that just pushed successfully. Rows mutated mid-flight fail the guard and
/// stay dirty for the next tick. This runs per batch so `pending` shrinks live.
fn clear_message_keys(db: &Db, chunk: &[Value]) -> Result<(), String> {
    let mut keys: Vec<(String, i64)> = Vec::new();
    for v in chunk {
        if let (Some(id), Some(rev)) = (
            v.get("id").and_then(|x| x.as_str()),
            v.get("revision").and_then(|x| x.as_i64()),
        ) {
            keys.push((id.to_string(), rev));
        }
    }
    if keys.is_empty() {
        return Ok(());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("UPDATE messages SET dirty = 0 WHERE id = ?1 AND revision = ?2")
        .map_err(|e| e.to_string())?;
    for (id, rev) in &keys {
        stmt.execute(rusqlite::params![id, rev]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Clear dirty flags / tombstones for the conversation rows in a batch. Live
/// rows clear `conversations.dirty`; tombstoned rows are removed from
/// `sync_tombstones` (distinguished by a non-null `deleted_at`).
fn clear_conversation_keys(db: &Db, chunk: &[Value]) -> Result<(), String> {
    let mut live: Vec<(String, i64)> = Vec::new();
    let mut tombs: Vec<(String, i64)> = Vec::new();
    for v in chunk {
        if let (Some(id), Some(rev)) = (
            v.get("id").and_then(|x| x.as_str()),
            v.get("revision").and_then(|x| x.as_i64()),
        ) {
            if v.get("deleted_at").and_then(|x| x.as_i64()).is_some() {
                tombs.push((id.to_string(), rev));
            } else {
                live.push((id.to_string(), rev));
            }
        }
    }
    if live.is_empty() && tombs.is_empty() {
        return Ok(());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    if !live.is_empty() {
        let mut stmt = conn
            .prepare("UPDATE conversations SET dirty = 0 WHERE id = ?1 AND revision = ?2")
            .map_err(|e| e.to_string())?;
        for (id, rev) in &live {
            stmt.execute(rusqlite::params![id, rev]).map_err(|e| e.to_string())?;
        }
    }
    if !tombs.is_empty() {
        let mut stmt = conn
            .prepare(
                "DELETE FROM sync_tombstones
                 WHERE entity = 'conversations' AND id = ?1 AND revision = ?2",
            )
            .map_err(|e| e.to_string())?;
        for (id, rev) in &tombs {
            stmt.execute(rusqlite::params![id, rev]).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Clear the dirty flag for the memory-file rows in a batch.
fn clear_memory_keys(db: &Db, chunk: &[Value]) -> Result<(), String> {
    let mut keys: Vec<(String, i64)> = Vec::new();
    for v in chunk {
        if let Some(p) = v.get("path").and_then(|x| x.as_str()) {
            let rev = v.get("revision").and_then(|x| x.as_i64()).unwrap_or(0);
            keys.push((p.to_string(), rev));
        }
    }
    if keys.is_empty() {
        return Ok(());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("UPDATE memory_sync SET dirty = 0 WHERE path = ?1 AND revision = ?2")
        .map_err(|e| e.to_string())?;
    for (path, rev) in &keys {
        stmt.execute(rusqlite::params![path, rev]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Rows sent per upsert request. The first full-history seed often marks
/// thousands of rows dirty at once, and a single giant POST body makes the
/// Supabase proxy abort with an HTTP 520 ("origin returned unknown error").
/// We split each entity's dirty rows into bounded batches to stay under the
/// request-size/time budget. Merge-duplicate upserts are idempotent, so a
/// mid-batch failure just retries the whole set next tick.
const UPSERT_BATCH_SIZE: usize = 400;

/// How many batches to upload concurrently. Kept modest so we don't re-trip
/// the Supabase proxy (a single giant request was the original HTTP 520); a
/// bounded 8-way burst of small POSTs is ordinary web load.
const PUSH_CONCURRENCY: usize = 8;

async fn post_upsert<F>(
    ctx: &SyncCtx<'_>,
    table: &str,
    token: &str,
    rows: &[Value],
    progress: &SyncState,
    mut on_chunk: F,
) -> Result<(), String>
where
    F: FnMut(&[Value]) -> Result<(), String> + Send,
{
    // Fire each batch as its own task, bounded by a semaphore so at most
    // `PUSH_CONCURRENCY` requests are in flight at once. Every task returns its
    // chunk on success so the caller can clear that chunk's dirty flags.
    let semaphore = Arc::new(Semaphore::new(PUSH_CONCURRENCY));
    let mut set = JoinSet::new();
    let client = ctx.client.clone();
    let anon = ctx.anon.to_string();
    let url = format!("{}/rest/v1/{}", ctx.base_url, table);
    let table = table.to_string();
    let token = token.to_string();
    for chunk in rows.chunks(UPSERT_BATCH_SIZE) {
        if chunk.is_empty() {
            continue;
        }
        let permit = semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        let client = client.clone();
        let anon = anon.clone();
        let url = url.clone();
        let token = token.clone();
        let table = table.clone();
        let chunk = chunk.to_vec();
        set.spawn(async move {
            let _permit = permit;
            let resp = client
                .post(&url)
                .header("apikey", &anon)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .header("Prefer", "resolution=merge-duplicates")
                .header(CONTENT_TYPE, "application/json")
                .json(&chunk)
                .send()
                .await
                .map_err(|e| format!("push {table}: {e}"))?;
            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(format!(
                    "push {table} failed (HTTP {}): {}",
                    status.as_u16(),
                    body.chars().take(300).collect::<String>()
                ));
            }
            Ok(chunk)
        });
    }

    // A batch either landed (bump pushed + clear its dirty) or failed. Failed
    // rows stay dirty and are retried next tick; every success is still
    // recorded even if a sibling batch failed.
    let mut first_err: Option<String> = None;
    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(chunk)) => {
                on_chunk(&chunk)?;
                progress.add_pushed(chunk.len() as u64);
            }
            Ok(Err(e)) => {
                first_err.get_or_insert(e);
            }
            Err(e) => {
                first_err.get_or_insert(format!("push join: {e}"));
            }
        }
    }
    if let Some(e) = first_err {
        return Err(e);
    }
    Ok(())
}

// --- Pull -------------------------------------------------------------------

#[derive(Deserialize)]
struct RemoteConversation {
    id: String,
    title: Option<String>,
    model: Option<String>,
    #[serde(default)]
    provider_id: Option<String>,
    system_prompt: Option<String>,
    compaction_summary: Option<String>,
    last_reflected_index: Option<i64>,
    imported: Option<bool>,
    import_batch: Option<i64>,
    created_at: Option<i64>,
    updated_at: Option<i64>,
    revision: i64,
    #[serde(default)]
    deleted_at: Option<i64>,
}

#[derive(Deserialize)]
struct RemoteMessage {
    id: String,
    conversation_id: String,
    role: String,
    content: String,
    model: Option<String>,
    provider: Option<String>,
    #[serde(default)]
    thinking_level: Option<String>,
    thinking: Option<String>,
    usage: Option<String>,
    stop_reason: Option<String>,
    attachments: Option<String>,
    created_at: i64,
    revision: i64,
    #[serde(default)]
    deleted_at: Option<i64>,
}

#[derive(Deserialize)]
struct RemoteMemory {
    path: String,
    content: Option<String>,
    revision: i64,
    #[serde(default)]
    deleted_at: Option<i64>,
}

/// Pull rows newer than the per-entity high-water mark and apply them. Returns
/// the number of rows fetched (so callers can surface live progress).
async fn pull(ctx: &SyncCtx<'_>, me: &str, token: &str) -> Result<u64, String> {
    let last_conv = last_seen(ctx.db, "conversations");
    let convs = get_table(ctx, "conversations", me, token, last_conv).await?;
    let last_msg = last_seen(ctx.db, "messages");
    let msgs = get_table(ctx, "messages", me, token, last_msg).await?;
    let last_mem = last_seen(ctx.db, "memory");
    let mems = get_table(ctx, "memory_files", me, token, last_mem).await?;

    // Restore plaintext for the content fields before applying to the local DB.
    // A row whose ciphertext can't be decrypted (wrong key) is dropped rather
    // than written to the local DB as ciphertext.
    let convs = if let Some(key) = ctx.key {
        decrypt_rows(&key, convs, &["title", "system_prompt", "compaction_summary"])
    } else {
        convs
    };
    let msgs = if let Some(key) = ctx.key {
        decrypt_rows(&key, msgs, &["content", "thinking", "usage", "attachments"])
    } else {
        msgs
    };
    let mems = if let Some(key) = ctx.key {
        decrypt_rows(&key, mems, &["content"])
    } else {
        mems
    };

    apply_conversations(ctx.db, &convs).await?;
    apply_messages(ctx.db, &msgs).await?;
    apply_memory(ctx.db, ctx.memory, &mems)?;

    Ok((convs.len() + msgs.len() + mems.len()) as u64)
}

/// Decrypt the named string fields of each row. Rows with no encrypted field are
/// kept as-is (pre-encryption plaintext); a row whose encrypted field fails to
/// decrypt is dropped so the local DB never stores ciphertext.
fn decrypt_rows(key: &[u8; 32], rows: Vec<Value>, fields: &[&str]) -> Vec<Value> {
    let mut out = Vec::with_capacity(rows.len());
    for mut row in rows {
        let mut ok = true;
        for f in fields {
            if let Some(Value::String(s)) = row.get_mut(f) {
                if crypt::looks_encrypted(s) {
                    match crypt::decrypt(key, s) {
                        Some(plain) => *s = plain,
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
            }
        }
        if ok {
            out.push(row);
        }
    }
    out
}

fn last_seen(db: &Db, entity: &str) -> i64 {
    let Ok(conn) = db.0.lock() else {
        return 0;
    };
    conn.query_row(
        "SELECT last_seen_revision FROM sync_state WHERE entity = ?1",
        [entity],
        |r| r.get(0),
    )
    .unwrap_or(0)
}

async fn get_table(
    ctx: &SyncCtx<'_>,
    table: &str,
    me: &str,
    token: &str,
    since: i64,
) -> Result<Vec<Value>, String> {
    let url = format!(
        "{}/rest/v1/{}?user_id=eq.{}&revision=gt.{}&order=revision.asc&limit=1000",
        ctx.base_url, table, me, since
    );
    let resp = ctx
        .client
        .get(&url)
        .header("apikey", ctx.anon)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| format!("pull {table}: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "pull {table} failed (HTTP {}): {}",
            status.as_u16(),
            body.chars().take(300).collect::<String>()
        ));
    }
    let text = resp.text().await.map_err(|e| format!("pull {table} body: {e}"))?;
    // A non-array body (e.g. an object/error) parses to an empty vec.
    Ok(serde_json::from_str(&text).unwrap_or_default())
}

/// Advance the high-water mark for an entity to `max_rev` (never backwards).
fn advance_watermark(conn: &rusqlite::Connection, entity: &str, max_rev: i64) -> Result<(), String> {
    conn.execute(
        "INSERT INTO sync_state(entity, last_seen_revision) VALUES (?1, ?2)
         ON CONFLICT(entity) DO UPDATE SET
           last_seen_revision = MAX(last_seen_revision, excluded.last_seen_revision)",
        rusqlite::params![entity, max_rev],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

async fn apply_conversations(db: &Db, rows: &[Value]) -> Result<(), String> {
    let parsed: Vec<RemoteConversation> = rows
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut max_rev = 0;
    for r in &parsed {
        max_rev = max_rev.max(r.revision);
        if r.deleted_at.is_some() {
            tx.execute("DELETE FROM conversations WHERE id = ?1", [&r.id])
                .map_err(|e| e.to_string())?;
            continue;
        }
        let local_rev: Option<i64> = tx
            .query_row("SELECT revision FROM conversations WHERE id = ?1", [&r.id], |x| {
                x.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(lr) = local_rev {
            if lr >= r.revision {
                continue; // local wins
            }
        }
        let title = r.title.clone().unwrap_or_default();
        let imported = if r.imported.unwrap_or(false) { 1 } else { 0 };
        let import_batch = r.import_batch.unwrap_or(0);
        let created_at = r.created_at.unwrap_or_else(now_ms);
        let updated_at = r.updated_at.unwrap_or(created_at);
        if local_rev.is_none() {
            tx.execute(
                "INSERT INTO conversations
                   (id, title, model, provider_id, system_prompt, compaction_summary, last_reflected_index,
                    imported, import_batch, created_at, updated_at, revision, dirty)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 0)",
                rusqlite::params![
                    r.id, title, r.model, r.provider_id, r.system_prompt, r.compaction_summary,
                    r.last_reflected_index, imported, import_batch, created_at, updated_at,
                    r.revision
                ],
            )
            .map_err(|e| e.to_string())?;
        } else {
            tx.execute(
                "UPDATE conversations SET title = ?2, model = ?3, provider_id = ?4,
                        system_prompt = ?5, compaction_summary = ?6, last_reflected_index = ?7,
                        imported = ?8, import_batch = ?9, created_at = ?10, updated_at = ?11,
                        revision = ?12, dirty = 0
                 WHERE id = ?1",
                rusqlite::params![
                    r.id, title, r.model, r.provider_id, r.system_prompt, r.compaction_summary,
                    r.last_reflected_index, imported, import_batch, created_at, updated_at,
                    r.revision
                ],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    advance_watermark(&tx, "conversations", max_rev)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

async fn apply_messages(db: &Db, rows: &[Value]) -> Result<(), String> {
    let parsed: Vec<RemoteMessage> = rows
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut max_rev = 0;
    let mut to_reindex: Vec<String> = Vec::new();
    for r in &parsed {
        max_rev = max_rev.max(r.revision);
        if r.deleted_at.is_some() {
            tx.execute("DELETE FROM messages WHERE id = ?1", [&r.id])
                .map_err(|e| e.to_string())?;
            continue;
        }
        // FK: skip messages whose conversation isn't present locally (its
        // conversation will arrive or be tombstoned separately).
        let conv_exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM conversations WHERE id = ?1",
                [&r.conversation_id],
                |x| x.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if conv_exists.is_none() {
            continue;
        }
        let local_rev: Option<i64> = tx
            .query_row("SELECT revision FROM messages WHERE id = ?1", [&r.id], |x| {
                x.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(lr) = local_rev {
            if lr >= r.revision {
                continue; // local wins
            }
        }
        if local_rev.is_none() {
            tx.execute(
                "INSERT INTO messages
                   (id, conversation_id, role, \"index\", content, model, provider,
                    thinking_level, thinking, usage, stop_reason, attachments, created_at, revision, dirty)
                 VALUES (?1, ?2, ?3, 0, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0)",
                rusqlite::params![
                    r.id, r.conversation_id, r.role, r.content, r.model, r.provider,
                    r.thinking_level, r.thinking, r.usage, r.stop_reason, r.attachments,
                    r.created_at, r.revision
                ],
            )
            .map_err(|e| e.to_string())?;
        } else {
            tx.execute(
                "UPDATE messages SET role = ?2, content = ?3, model = ?4, provider = ?5,
                        thinking_level = ?6, thinking = ?7, usage = ?8, stop_reason = ?9,
                        attachments = ?10, created_at = ?11, revision = ?12, dirty = 0
                 WHERE id = ?1",
                rusqlite::params![
                    r.id, r.role, r.content, r.model, r.provider, r.thinking_level,
                    r.thinking, r.usage, r.stop_reason, r.attachments, r.created_at, r.revision
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        if !to_reindex.contains(&r.conversation_id) {
            to_reindex.push(r.conversation_id.clone());
        }
    }
    for cid in &to_reindex {
        reindex_messages(&tx, cid)?;
    }
    advance_watermark(&tx, "messages", max_rev)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

/// Rewrite a conversation's local `index` to a stable `(created_at, id)` order so
/// cross-device message ordering is consistent without trusting any device's
/// counter. Per-conversation monotonic, preserving the UI/grouping contract.
///
/// The reflection watermark (`conversations.last_reflected_index`) is a numeric
/// index into this ordering, so re-ordering must remap it to the same logical
/// message: otherwise a pull would shift the watermark's meaning and cause a
/// cross-device double-reflection (or a skipped chunk).
fn reindex_messages(tx: &rusqlite::Transaction<'_>, conversation_id: &str) -> Result<(), String> {
    // The message the watermark currently points at, if any. Newly-pulled rows
    // are inserted with a literal `index = 0`, so when the watermark is 0 there
    // can be several candidates — order by rowid to deterministically prefer the
    // original message the watermark pointed at before this apply.
    let watermark_id: Option<String> = tx
        .query_row(
            "SELECT m.id FROM conversations c
             JOIN messages m ON m.conversation_id = c.id AND m.\"index\" = c.last_reflected_index
             WHERE c.id = ?1
             ORDER BY m.rowid ASC LIMIT 1",
            [conversation_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let mut stmt = tx
        .prepare(
            "SELECT id FROM messages WHERE conversation_id = ?1
             ORDER BY created_at ASC, id ASC",
        )
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = stmt
        .query_map([conversation_id], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .map(|r| r.map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, String>>()?;
    drop(stmt);
    let mut upd = tx
        .prepare("UPDATE messages SET \"index\" = ?2 WHERE id = ?1")
        .map_err(|e| e.to_string())?;
    for (i, id) in ids.into_iter().enumerate() {
        let idx = i as i64;
        let rowid: &str = id.as_str();
        upd.execute(rusqlite::params![rowid, idx])
            .map_err(|e| e.to_string())?;
    }
    drop(upd);

    // Remap the watermark to the new index of the same message.
    if let Some(watermark_id) = watermark_id {
        let new_idx: Option<i64> = tx
            .query_row(
                "SELECT \"index\" FROM messages WHERE id = ?1",
                [&watermark_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(idx) = new_idx {
            tx.execute(
                "UPDATE conversations SET last_reflected_index = ?2 WHERE id = ?1",
                rusqlite::params![conversation_id, idx],
            )
            .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Apply remote memory files (whole-file replace) honoring LWW by revision.
/// File I/O happens outside the DB lock; the lock is only taken (twice) to
/// decide the apply set and to record the resulting revisions + high-water.
fn apply_memory(db: &Db, memory: &MemoryState, rows: &[Value]) -> Result<(), String> {
    let parsed: Vec<RemoteMemory> = rows
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    if parsed.is_empty() {
        return Ok(());
    }
    // Phase 1: decide which rows apply (local must not win), without holding the
    // lock during file I/O.
    let mut applies: Vec<bool> = Vec::with_capacity(parsed.len());
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        for r in &parsed {
            let local_rev: i64 = conn
                .query_row(
                    "SELECT revision FROM memory_sync WHERE path = ?1",
                    [&r.path],
                    |x| x.get::<_, i64>(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
                .unwrap_or(-1);
            applies.push(local_rev < r.revision);
        }
    }
    // Phase 2: file I/O (no DB lock held). Record whether each row's write
    // succeeded so phase 3 only marks successful applies as clean/in-sync.
    let mut wrote_ok: Vec<bool> = Vec::with_capacity(parsed.len());
    for (i, r) in parsed.iter().enumerate() {
        if !applies[i] {
            wrote_ok.push(true); // skipped because local wins — nothing to write
            continue;
        }
        let res = if r.deleted_at.is_some() {
            memory.apply_remote_delete(&r.path)
        } else {
            memory.apply_remote(&r.path, r.content.as_deref().unwrap_or(""))
        };
        match res {
            Ok(()) => wrote_ok.push(true),
            Err(e) => {
                eprintln!("sync: memory apply failed for {}: {e}", r.path);
                wrote_ok.push(false);
            }
        }
    }
    // Phase 3: record revisions + advance high-water in one transaction.
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut max_rev = 0;
    let mut any_failed = false;
    for (r, ok) in parsed.iter().zip(wrote_ok.iter()) {
        if !ok {
            // The write failed. Do NOT set dirty (that would make the next push
            // overwrite a newer remote file with stale local content) and do NOT
            // advance the high-water (see below), so the next pull re-fetches the
            // newer remote version and retries the apply.
            any_failed = true;
            continue;
        }
        max_rev = max_rev.max(r.revision);
        let local_rev: i64 = tx
            .query_row(
                "SELECT revision FROM memory_sync WHERE path = ?1",
                [&r.path],
                |x| x.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or(-1);
        if local_rev >= r.revision {
            continue; // local wins; keep any pending dirty flag
        }
        tx.execute(
            "INSERT INTO memory_sync(path, revision, dirty, last_local_at)
             VALUES (?1, ?2, 0, ?3)
             ON CONFLICT(path) DO UPDATE SET revision = excluded.revision, dirty = 0,
               last_local_at = excluded.last_local_at",
            rusqlite::params![r.path, r.revision, now_ms()],
        )
        .map_err(|e| e.to_string())?;
    }
    // Only advance when no write failed: otherwise the high-water could eclipse
    // the failed row's revision and the next pull would never re-arrive it.
    if !any_failed {
        advance_watermark(&tx, "memory", max_rev)?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests;
