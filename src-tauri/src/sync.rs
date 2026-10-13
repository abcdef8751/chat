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

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use reqwest::header::{AUTHORIZATION, CONTENT_RANGE, CONTENT_TYPE};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};
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

/// Sync can't work when the resolved base URL is the build-time fallback
/// (`https://localhost.supabase.co`), i.e. `.env` was absent or empty at compile
/// time — see `build.rs`. Every request would hit a bogus host and fail with an
/// opaque "error sending request". Callers check the *resolved* URL first so a
/// mis-built binary reports something actionable instead. (Checked against the
/// URL, not the const, so the in-process test override is unaffected.)
fn ensure_configured(base_url: &str) -> Result<(), String> {
    if base_url.trim_end_matches('/') == "https://localhost.supabase.co" {
        return Err(
            "Cloud sync is not configured in this build: SUPABASE_URL was empty at compile time. \
             Populate .env and rebuild the app."
                .into(),
        );
    }
    Ok(())
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
    // Total remote rows the ongoing pull knows it must fetch, learned from
    // PostgREST `Prefer: count=exact` on each table's first page. 0 = unknown
    // (server didn't report a count), in which case the UI shows an
    // indeterminate bar. The push-side analogue is `pending`.
    pub pull_total: u64,
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
    pull_total: u64,
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
            s.pull_total = 0;
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

    /// Record the total remote rows for the ongoing pull. Accumulated per table
    /// (each table's first page reports its own count), so it sums across the
    /// four pull loops.
    fn add_pull_total(&self, n: u64) {
        if let Ok(mut s) = self.stats.lock() {
            s.pull_total += n;
        }
    }

    fn set_locked(&self, locked: bool) {
        if let Ok(mut s) = self.stats.lock() {
            s.locked = locked;
        }
    }

    fn snapshot(&self) -> (bool, Option<i64>, Option<String>, String, u64, u64, u64, bool) {
        let syncing = self.syncing.load(Ordering::SeqCst);
        match self.stats.lock() {
            Ok(s) => (
                syncing,
                s.last_sync_at,
                s.last_error.clone(),
                s.phase.clone(),
                s.pushed,
                s.pulled,
                s.pull_total,
                s.locked,
            ),
            Err(_) => (syncing, None, None, String::new(), 0, 0, 0, false),
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
    let (syncing, last_sync_at, last_error, phase, pushed, pulled, pull_total, locked) =
        sync.snapshot();
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
        pull_total,
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
        config: &app.state::<ConfigState>(),
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
    for (table, field) in [
        ("conversations", "title"),
        ("messages", "content"),
        ("user_config", "payload"),
    ] {
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
    ensure_configured(&sync.base_url)?;
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
    ensure_configured(&sync.base_url)?;
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
    // Read-modify-write under the config lock: a blind get/set here would rewrite
    // the whole config from a stale read and could revert a syncable field the
    // engine had just projected.
    config.update(|cfg| {
        cfg.sync_enabled = on;
        true
    })?;
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
///
/// Runs one round IMMEDIATELY on startup instead of waiting out the first 60s
/// sleep: launching the app is the most likely moment for changes made on
/// another device to be waiting, so the config and chat list should reconcile as
/// soon as the window opens rather than a minute later. Cheap when inactive —
/// `do_sync` returns on `!sync_enabled` before it reads the keychain — and the
/// `syncing` swap-guard keeps it from overlapping a manual "Sync now".
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(e) = do_sync(&app).await {
                eprintln!("sync: {e}");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
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
    /// Syncable user config (preferences/providers/overrides) state, written
    /// through on pull so a remote provider/preference change lands in Settings.
    config: &'a ConfigState,
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
    ensure_configured(&sync.base_url)?;
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
        config: &app.state::<ConfigState>(),
        key: secrets::enc_key_get()
            .ok()
            .flatten()
            .and_then(|k| crypt::decode_key(&k)),
    };
    // First sync: mark all pre-existing rows + memory files dirty so the initial
    // push backs up full history. Scoped per account (a guarded no-op after each
    // account's first run).
    // Only seed memory files that actually hold content. A fresh device auto-
    // creates EMPTY core files (profile/preferences/goals) at startup; if those
    // empty stubs were marked dirty here, their freshly-bumped revisions would
    // beat the real content already on the mirror under last-writer-wins and
    // silently drop it on pull. An empty file has nothing to back up, so skip it.
    let mem_names = ctx
        .memory
        .file_names()
        .into_iter()
        .filter(|n| ctx.memory.read(n).map(|c| !c.trim().is_empty()).unwrap_or(false))
        .collect::<Vec<_>>();
    db::seed_initial_sync(ctx.db, &session.user_id, &mem_names)?;
    // Existing imported chats (from an Anthropic archive) were written before the
    // sync-dirty fix landed, so they (and thousands of their messages) sit at
    // dirty = 0 / revision = 0 and would never be pushed to the mirror. Re-mark
    // any never-synced row dirty so the next push backs it up. Idempotent: once
    // pushed they get a real revision and drop out of the `revision = 0` filter.
    // `role != 'memory'` excludes the never-synced consolidation notes.
    {
        let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
        conn.execute("UPDATE conversations SET dirty = 1 WHERE revision = 0", [])
            .map_err(|e| e.to_string())?;
        conn.execute(
            "UPDATE messages SET dirty = 1 WHERE revision = 0 AND role != 'memory'",
            [],
        )
        .map_err(|e| e.to_string())?;
    }
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
    // Record the success time as soon as the push lands — not only after the pull.
    // A device may have successfully pushed (dirty flags cleared) yet hit a
    // retryable pull error; without this the config would stay "never synced"
    // indefinitely even while data flows. The final write below may refresh it.
    let at = now_ms();
    sync.set_success(at);
    {
        // `update` (read-modify-write under the config lock) so this can't clobber
        // a syncable field the engine wrote from a stale read.
        let _ = app.state::<ConfigState>().update(|cfg| {
            cfg.sync_last_sync_at = Some(at);
            true
        });
    }
    sync.set_phase("pull");
    // Progress (pulled / pull_total) is updated per page inside `pull`.
    let n = match pull(&ctx, &session.user_id, &token, Some(&sync)).await {
        Ok(n) => n,
        Err(e) => {
            sync.set_error(e.clone());
            return Err(e);
        }
    };
    // The pull is done — clear the phase so the panel stops showing "Pulling
    // changes…" (with a frozen full bar) while the config tail below runs its
    // own round-trip. `syncing` stays true until this function returns, so the
    // row remains visible but reads as a generic "Syncing…".
    sync.set_phase("");
    // Mirror + reconcile the user config after the pull. Split into a helper so
    // the ordering is testable without an `AppHandle`.
    reconcile_config_after_pull(&ctx, &token).await?;
    // Persist the success timestamp so it survives restarts (the live value in
    // SyncState is in-memory only; without this the page would show "never"
    // after a relaunch even though syncs ran in earlier sessions).
    let at = now_ms();
    sync.set_success(at);
    // `update` here too: `set` would rewrite the whole config from a stale read and
    // could revert a syncable field the engine projected during this run.
    let _ = app.state::<ConfigState>().update(|cfg| {
        cfg.sync_last_sync_at = Some(at);
        true
    });
    // Broadcast that local data may have changed (a successful pull applied
    // remote rows). Refresh is cheap, so emit unconditionally after a pull —
    // the frontend refreshes the chat list and open conversation on receipt.
    let _ = app.emit("sync-changed", json!({ "changed": n > 0, "pulled": n }));
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
        // Legacy/imported rows can sit at revision 0; bump them so the push emits
        // a non-zero revision and they become visible to pulls. (See bump_zero_dirty.)
        db::bump_zero_dirty(&conn, "conversations", PUSH_CONV_BATCH)?;
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

    // Messages: drain the whole backlog within this run, in bounded chunks.
    // Building ~55k messages into one in-memory JSON payload at once can exhaust
    // the heap and abort the process (a crash we hit with imported archives), so
    // each chunk is capped at PUSH_MSG_BATCH and uploaded+cleared before the next
    // is read. The loop keeps going until nothing is left dirty, so a large
    // backfill completes in one sync run rather than one chunk per 60s tick.
    loop {
        let chunk = {
            let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
            // Bump a bounded batch of still-revision-0 rows to visible revisions.
            // `revision > 0` in the SELECT guarantees we never re-upload a
            // revision-0 row. (See bump_zero_dirty.)
            db::bump_zero_dirty(&conn, "messages", PUSH_MSG_BATCH)?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, conversation_id, role, \"index\", content, model, provider,
                            thinking_level, thinking, usage, stop_reason, attachments, created_at, revision
                     FROM messages WHERE dirty = 1 AND revision > 0 LIMIT ?1",
                )
                .map_err(|e| e.to_string())?;
            let key = ctx.key;
            let r = stmt
                .query_map([PUSH_MSG_BATCH], |r| {
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
        if chunk.is_empty() {
            break;
        }
        post_upsert(ctx, "messages", token, &chunk, progress, |c| {
            clear_message_keys(ctx.db, c)
        })
        .await?;
        total += chunk.len() as u64;
    }

    // Message tombstones (edit-time truncation): a hard-deleted message must
    // carry a remote tombstone or a second device keeps the stale tail. The
    // local row is already gone, so the stored `conversation_id` plus
    // placeholder NOT NULL fields are sent; consumers delete on `deleted_at`
    // alone and never read the placeholders.
    //
    // Residual limitation (shared with conversation tombstones): a tombstone is
    // only fetched when its revision exceeds the `messages` pull high-water mark,
    // and revisions are wall-clock based. A device whose clock lags can mint a
    // tombstone revision at or below another device's watermark, in which case
    // that device never pulls the deletion. Same pre-existing skew edge as
    // `tombstone_conversation`; fixing it needs a server-side monotonic revision.
    let msg_tombstones = {
        let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, revision, conversation_id FROM sync_tombstones WHERE entity = 'messages'",
            )
            .map_err(|e| e.to_string())?;
        let r = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|e| e.to_string())?;
        let rows: Result<Vec<(String, i64, Option<String>)>, String> =
            r.map(|row| row.map_err(|e| e.to_string())).collect();
        drop(stmt);
        rows?
    };
    if !msg_tombstones.is_empty() {
        let now = now_ms();
        let body: Vec<Value> = msg_tombstones
            .iter()
            .filter_map(|(id, rev, cid)| {
                // Without a conversation_id the remote row would violate its
                // NOT NULL / FK; skip rather than emit a malformed tombstone.
                let cid = cid.as_deref()?;
                Some(json!({
                    "id": id, "conversation_id": cid, "role": "user", "seq": 0,
                    "content": "", "model": Value::Null, "provider": Value::Null,
                    "thinking_level": Value::Null, "thinking": Value::Null,
                    "usage": Value::Null, "stop_reason": Value::Null,
                    "attachments": Value::Null, "created_at": now,
                    "revision": rev, "deleted_at": now,
                }))
            })
            .collect();
        if !body.is_empty() {
            let n = body.len() as u64;
            post_upsert(ctx, "messages", token, &body, progress, |c| {
                clear_message_tombstones(ctx.db, c)
            })
            .await?;
            total += n;
        }
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

    // User config (preferences + providers + price/context overrides): a single
    // 'main' row. Small, so fetched + pushed in one go.
    let cfg_pending: Vec<(String, String, i64)> = {
        let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, payload, revision FROM user_config WHERE dirty = 1",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
            })
            .map_err(|e| e.to_string())?;
        let out: Result<Vec<_>, _> = rows.collect();
        drop(stmt);
        out.map_err(|e| e.to_string())?
    };
    let mut cfg_rows: Vec<Value> = Vec::new();
    for (id, payload, revision) in cfg_pending {
        // Nothing user-owned to back up (mirror-only config on the default
        // endpoint). Clear the dirty flag locally and never publish it — a
        // near-empty device must not blank a configured mirror.
        if syncable_payload_is_empty(&payload) {
            let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
            conn.execute(
                "UPDATE user_config SET dirty = 0 WHERE id = ?1 AND revision = ?2",
                rusqlite::params![id, revision],
            )
            .map_err(|e| e.to_string())?;
            continue;
        }
        // Read-before-write, made CONTENT-AWARE: `user_config` is a single
        // global row mirrored under whole-row last-writer-wins, so a plain push
        // from a device whose config is empty in some fields would blank those
        // fields on the server (the phone-blanked-the-desktop clobber). So we
        // fetch the server's payload and upsert the UNION — our values win the
        // fields we set, the server's fill the ones we left unset — and we only
        // push when we actually ADD content it lacks. A push therefore never
        // drops server content in the fields we left unset (providers and
        // overrides merge per key); a scalar conflict on a field we set still
        // resolves local-wins, which is the app's documented policy.
        let (push_payload, push_rev) = match fetch_user_config_remote(ctx, token, &id).await {
            Ok(RemoteConfig::Live(server_rev, server_raw)) => {
                // A payload we cannot read (locked device) must not be
                // overwritten — same rule as the reconcile.
                let Some(server_clear) = decrypt_server_payload(ctx.key, &server_raw) else {
                    continue;
                };
                match (parse_syncable_at(&payload, revision), parse_syncable_at(&server_clear, server_rev)) {
                    (Some(local), Some(server)) => {
                        let merged = local.merge(&server);
                        let merged_json =
                            serde_json::to_value(&merged).map_err(|e| e.to_string())?;
                        let server_json =
                            serde_json::to_value(&server).map_err(|e| e.to_string())?;
                        if merged_json == server_json {
                            // The server already holds everything we have —
                            // nothing to add. Clear dirty and skip; the server's
                            // fields (if any we lack) arrive on the pull.
                            let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
                            conn.execute(
                                "UPDATE user_config SET dirty = 0 WHERE id = ?1 AND revision = ?2",
                                rusqlite::params![id, revision],
                            )
                            .map_err(|e| e.to_string())?;
                            continue;
                        }
                        // We add content → push the union at a revision above
                        // the server's so it lands, and persist it locally
                        // (adopting the server's fields into our config). If a
                        // concurrent edit moved the row, skip: it will push.
                        let rev = revision.max(server_rev.saturating_add(1));
                        if !persist_merged_user_config(ctx, &id, &merged, rev, revision)? {
                            continue;
                        }
                        (merged_json.to_string(), rev)
                    }
                    // One side isn't mergeable JSON (legacy/garbage payload) —
                    // fall back to the revision guard: push ours only when it
                    // isn't staler than the server's.
                    _ => {
                        if revision < server_rev {
                            continue;
                        }
                        (payload, revision)
                    }
                }
            }
            // No server row → nothing to merge; push ours as-is.
            Ok(RemoteConfig::Absent) => (payload, revision),
            // The mirror row was DELETED server-side: pushing would resurrect it.
            Ok(RemoteConfig::Deleted) => continue,
            // Can't confirm the server state → don't risk overwriting it.
            Err(_) => continue,
        };
        cfg_rows.push(json!({
            "id": id,
            "payload": enc_str(ctx.key, push_payload),
            "revision": push_rev,
            "deleted_at": Value::Null,
            "created_at": now_ms(),
            "updated_at": now_ms(),
        }));
    }
    if !cfg_rows.is_empty() {
        post_upsert(ctx, "user_config", token, &cfg_rows, progress, |chunk| {
            clear_user_config_keys(ctx.db, chunk)
        })
        .await?;
        total += cfg_rows.len() as u64;
    }

    Ok(total)
}

/// Persist a merged `user_config` payload into the local row (dirty, at `rev`)
/// and adopt its server-filled fields into the LIVE config, so the UI reflects
/// the union immediately and the post-pull mirror stays a no-op. Used by the
/// push after it unions with the server.
///
/// The row write is revision-guarded: a concurrent Settings edit that bumped the
/// row past `expect_rev` (it can land during the push's awaited fetch) is left
/// untouched. Returns `false` in that case so the caller skips the push for this
/// row too — the concurrent edit is authoritative and will be pushed next tick.
/// Adopting the union into the live config in that window would revert the edit
/// in memory (and then the post-pull mirror would overwrite the row with the
/// reverted value, losing it for good), so we bail before touching the config.
/// The row write and the config projection share one critical section, so a
/// Settings save cannot land between them either.
fn persist_merged_user_config(
    ctx: &SyncCtx<'_>,
    id: &str,
    merged: &SyncableConfig,
    rev: i64,
    expect_rev: i64,
) -> Result<bool, String> {
    let payload = serde_json::to_string(merged).map_err(|e| e.to_string())?;
    // ONE critical section: the guarded row write and the config projection happen
    // under the db lock — the same lock every edit path takes around its own
    // (config, row) pair. A Settings save therefore cannot slip between them, and
    // cannot be reverted by the projection. (`db -> config` is the only order.)
    let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
    let touched = conn
        .execute(
            "UPDATE user_config SET payload = ?3, revision = ?4, dirty = 1
             WHERE id = ?1 AND revision = ?2",
            rusqlite::params![id, expect_rev, payload, rev],
        )
        .map_err(|e| e.to_string())?;
    if touched == 0 {
        // Row moved under us — a concurrent edit owns it now.
        return Ok(false);
    }
    // The same projection the converge path uses, so the two can't disagree.
    // Best-effort: the row is already written dirty, so a failed config write only
    // delays the local adoption (the next tick re-pushes and retries it) — it must
    // not abort the run before the pull has even run.
    let _ = ctx.config.update(|cfg| project_merged(cfg, merged));
    Ok(true)
}

/// What the server holds for a `user_config` row.
///
/// A tombstone is NOT the same as no row: it is a deliberate server-side delete,
/// and treating it as "no row" would re-create it — the push would upsert it
/// clean, and a converge would arm the local row to push it back. So the two are
/// distinguished, and only `Absent` licenses converging against an empty remote.
enum RemoteConfig {
    Absent,
    Deleted,
    Live(i64, String),
}

/// Fetch the server's `user_config` row for `id`.
async fn fetch_user_config_remote(
    ctx: &SyncCtx<'_>,
    token: &str,
    id: &str,
) -> Result<RemoteConfig, String> {
    let url = format!(
        "{}/rest/v1/user_config?id=eq.{}&select=revision,payload,deleted_at",
        ctx.base_url, id
    );
    let resp = ctx
        .client
        .get(&url)
        .header("apikey", ctx.anon)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .map_err(|e| format!("read server user_config: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("read server user_config: HTTP {}", resp.status()));
    }
    let text = resp
        .text()
        .await
        .map_err(|e| format!("read server user_config body: {e}"))?;
    let rows: Vec<Value> = serde_json::from_str(&text)
        .map_err(|e| format!("read server user_config parse: {e}"))?;
    let Some(row) = rows.first() else {
        return Ok(RemoteConfig::Absent);
    };
    if row.get("deleted_at").and_then(|v| v.as_i64()).is_some() {
        return Ok(RemoteConfig::Deleted);
    }
    match (
        row.get("revision").and_then(|v| v.as_i64()),
        row.get("payload").and_then(|v| v.as_str()),
    ) {
        (Some(revision), Some(payload)) => Ok(RemoteConfig::Live(revision, payload.to_string())),
        _ => Ok(RemoteConfig::Absent),
    }
}

/// Mirror the live config into the sync row and reconcile it against the server
/// — the post-pull config tail of `do_sync`, split out so its ordering is
/// testable without an `AppHandle`.
///
/// The mirror runs AFTER the pull, never before: `user_config` is a single
/// global 'main' row, so a device that mirror-pushes its own config ahead of
/// the pull would push its (default) row over the authoritative remote config
/// under a clock-based revision — clobbering it on the server and then seeing
/// local-wins on its own pull (nothing appears). Pulling first lets a
/// fresh/second device adopt the canonical config, at which point the mirror is
/// a no-op (its config equals what it just applied); a genuine local edit still
/// wins because `sync_user_config` bumps the revision above the latest
/// applied/pulled one, and is pushed on the next tick.
///
/// The reconcile then catches what the pull cannot: a row stuck at the server's
/// revision with a divergent payload (equal-revision residue, invisible to the
/// pull high-water). It is best-effort — a hiccup must not fail a sync that
/// otherwise succeeded, so it repeats on the next tick.
async fn reconcile_config_after_pull(ctx: &SyncCtx<'_>, token: &str) -> Result<(), String> {
    // The row is mirrored by `converge_user_config_row` itself, under the db lock.
    // Mirroring again here would be a second, unprotected read of the config — and
    // a poisoned lock would read as defaults and wipe the row. Best-effort: a
    // failure is retried on the next tick.
    let _ = reconcile_user_config(ctx, token).await;
    Ok(())
}

/// Post-pull convergence pass.
///
/// The pull only fetches rows the high-water hasn't passed, so a row it refused —
/// or never saw, because this device's watermark already ran past the server's
/// revision — would otherwise never be revisited. This re-reads the row BY ID and
/// runs the same per-key merge. That is what heals a device wedged under the old
/// row-revision gate, and it also converges a stale local row without waiting for
/// the server's revision to move.
///
/// Best-effort by design: a fetch/parse failure must not fail a sync that
/// otherwise succeeded — it is simply retried on the next tick. A server payload
/// we cannot decrypt is never overwritten (the locked-device contract).
async fn reconcile_user_config(ctx: &SyncCtx<'_>, token: &str) -> Result<(), String> {
    let (remote, server_rev) = match fetch_user_config_remote(ctx, token, "main").await? {
        RemoteConfig::Live(server_rev, server_payload) => {
            let Some(server_clear) = decrypt_server_payload(ctx.key, &server_payload) else {
                return Ok(()); // unreadable (locked) — never overwrite it
            };
            let Some(remote) = parse_syncable_at(&server_clear, server_rev) else {
                return Ok(()); // unparseable remote — leave it alone
            };
            (remote, server_rev)
        }
        // A tombstone is a deliberate server-side delete: never resurrect it.
        RemoteConfig::Deleted => return Ok(()),
        // No row yet: converge against an EMPTY remote anyway. That is what mirrors
        // a config which predates its row (an install upgraded to sync, or a first
        // run) and arms it dirty for the push — otherwise the row is only ever
        // created by an edit, and pre-existing content would never be backed up.
        RemoteConfig::Absent => (SyncableConfig::new_v2(), 0),
    };
    converge_user_config_row(ctx, "main", &remote, server_rev)?;
    Ok(())
}

/// An EMPTY config must never be published: `user_config` is a single global row
/// and a blank device's default payload would clobber the server's canonical
/// config. A device with nothing to say instead just clears its dirty flag, so the
/// row is not re-pushed every tick.
///
/// v2 makes "nothing to say" precise: it means NO KEY HAS EVER BEEN SET (no
/// revisions at all), which is not the same as "the values happen to be empty". A
/// device that CLEARED a field has revisions and does have something to publish.
/// A v1 payload is migrated on parse, so the same test covers both.
pub(crate) fn syncable_payload_is_empty(payload: &str) -> bool {
    match parse_syncable(payload) {
        Some(sc) => !sc.has_content(),
        // Unparseable — assume non-empty so it is pushed as-is rather than dropped.
        None => false,
    }
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
fn clear_memory_keys(db: &Db, chunk: &[Value]) -> Result<(), String> {    let mut keys: Vec<(String, i64)> = Vec::new();
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

/// Remove the *message* tombstones (edit-time truncation) in a batch that just
/// pushed successfully, guarded by revision so one recorded mid-flight stays.
fn clear_message_tombstones(db: &Db, chunk: &[Value]) -> Result<(), String> {
    let mut rows: Vec<(String, i64)> = Vec::new();
    for v in chunk {
        if let (Some(id), Some(rev)) = (
            v.get("id").and_then(|x| x.as_str()),
            v.get("revision").and_then(|x| x.as_i64()),
        ) {
            rows.push((id.to_string(), rev));
        }
    }
    if rows.is_empty() {
        return Ok(());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "DELETE FROM sync_tombstones
             WHERE entity = 'messages' AND id = ?1 AND revision = ?2",
        )
        .map_err(|e| e.to_string())?;
    for (id, rev) in &rows {
        stmt.execute(rusqlite::params![id, rev]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Clear the dirty flag (guarded by revision) for the user-config row(s) in a
/// batch that just pushed successfully. Rows mutated mid-flight stay dirty.
fn clear_user_config_keys(db: &Db, chunk: &[Value]) -> Result<(), String> {
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
        .prepare("UPDATE user_config SET dirty = 0 WHERE id = ?1 AND revision = ?2")
        .map_err(|e| e.to_string())?;
    for (id, rev) in &keys {
        stmt.execute(rusqlite::params![id, rev]).map_err(|e| e.to_string())?;
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

/// How many dirty messages to bring into an in-memory push payload per chunk.
/// A first sync after enabling sync on a large imported archive can have tens of
/// thousands of dirty messages; building too many at once both sizes the heap
/// and (holding the DB lock while serializing + clearing) stalls the UI's own
/// DB reads, which makes a big push feel laggy. Keep the chunk small; the push
/// loop drains the whole backlog one chunk at a time within a single run.
const PUSH_MSG_BATCH: i64 = 500;

/// Conversations are far fewer than messages, so this cap is generous — just
/// enough headroom to never become the binding constraint in practice.
const PUSH_CONV_BATCH: i64 = 100_000;

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

/// Pull rows newer than the per-entity high-water mark and apply them, draining
/// the entire backlog within this one run rather than one 1000-row page per tick.
/// A first sync on a large archive can have tens of thousands of rows; looping
/// here (each page advancing the watermark before the next is fetched) completes
/// the backfill in a single run — the same "batch it within one sync" behaviour
/// the push uses. Returns the number of rows fetched.
async fn pull(
    ctx: &SyncCtx<'_>,
    me: &str,
    token: &str,
    progress: Option<&SyncState>,
) -> Result<u64, String> {
    let mut total: u64 = 0;

    // Conversations.
    // `want_count` is true for each table's first page only: with
    // `Prefer: count=exact` PostgREST reports the remote total in the
    // Content-Range header, which is what turns the UI into a real progress
    // bar. Later pages skip it (and servers that don't support it simply never
    // contribute a total, leaving the bar indeterminate).
    let mut want_count = true;
    loop {
        let since = last_seen(ctx.db, "conversations");
        let (mut rows, remote_total) =
            get_table(ctx, "conversations", me, token, since, want_count).await?;
        if let (Some(progress), Some(t)) = (progress, remote_total) {
            progress.add_pull_total(t);
        }
        want_count = false;
        if rows.is_empty() {
            break;
        }
        let n = rows.len() as u64;
        if let Some(key) = ctx.key {
            rows = decrypt_rows(&key, rows, &["title", "system_prompt", "compaction_summary"]);
        }
        apply_conversations(ctx.db, &rows).await?;
        if let Some(progress) = progress {
            progress.add_pulled(n);
        }
        total += n;
        // Stop when the page wasn't full (drained) or made no progress (e.g. all
        // rows were local-wins/tombstoned and the watermark didn't move).
        if rows.len() < 1000 || last_seen(ctx.db, "conversations") <= since {
            break;
        }
    }

    // Messages.
    let mut want_count = true;
    loop {
        let since = last_seen(ctx.db, "messages");
        let (mut rows, remote_total) = get_table(ctx, "messages", me, token, since, want_count).await?;
        if let (Some(progress), Some(t)) = (progress, remote_total) {
            progress.add_pull_total(t);
        }
        want_count = false;
        if rows.is_empty() {
            break;
        }
        let n = rows.len() as u64;
        if let Some(key) = ctx.key {
            rows = decrypt_rows(&key, rows, &["content", "thinking", "usage", "attachments"]);
        }
        apply_messages(ctx.db, &rows).await?;
        if let Some(progress) = progress {
            progress.add_pulled(n);
        }
        total += n;
        if rows.len() < 1000 || last_seen(ctx.db, "messages") <= since {
            break;
        }
    }

    // Memory files.
    let mut want_count = true;
    loop {
        let since = last_seen(ctx.db, "memory");
        let (mut rows, remote_total) = get_table(ctx, "memory_files", me, token, since, want_count).await?;
        if let (Some(progress), Some(t)) = (progress, remote_total) {
            progress.add_pull_total(t);
        }
        want_count = false;
        if rows.is_empty() {
            break;
        }
        let n = rows.len() as u64;
        if let Some(key) = ctx.key {
            rows = decrypt_rows(&key, rows, &["content"]);
        }
        apply_memory(ctx.db, ctx.memory, &rows)?;
        if let Some(progress) = progress {
            progress.add_pulled(n);
        }
        total += n;
        if rows.len() < 1000 || last_seen(ctx.db, "memory") <= since {
            break;
        }
    }

    // Syncable user config (preferences/providers/overrides).
    let mut want_count = true;
    loop {
        let since = last_seen(ctx.db, "user_config");
        let (mut rows, remote_total) =
            get_table(ctx, "user_config", me, token, since, want_count).await?;
        if let (Some(progress), Some(t)) = (progress, remote_total) {
            progress.add_pull_total(t);
        }
        want_count = false;
        if rows.is_empty() {
            break;
        }
        let n = rows.len() as u64;
        if let Some(key) = ctx.key {
            rows = decrypt_rows(&key, rows, &["payload"]);
        }
        apply_user_config(ctx, &rows)?;
        if let Some(progress) = progress {
            progress.add_pulled(n);
        }
        total += n;
        if rows.len() < 1000 || last_seen(ctx.db, "user_config") <= since {
            break;
        }
    }

    Ok(total)
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
    want_count: bool,
) -> Result<(Vec<Value>, Option<u64>), String> {
    let url = format!(
        "{}/rest/v1/{}?user_id=eq.{}&revision=gt.{}&order=revision.asc&limit=1000",
        ctx.base_url, table, me, since
    );
    let mut req = ctx
        .client
        .get(&url)
        .header("apikey", ctx.anon)
        .header(AUTHORIZATION, format!("Bearer {token}"));
    // First page of a table: ask PostgREST for the exact remote row count so
    // the frontend can render real pull progress (pulled / total). The count
    // comes back in the Content-Range header (`0-999/1042`). Servers without
    // `count=exact` support (or the mock in tests, unless asked) simply omit
    // it, and `remote_total` stays `None` → indeterminate progress.
    if want_count {
        req = req.header("Prefer", "count=exact");
    }
    let resp = req.send().await.map_err(|e| format!("pull {table}: {e}"))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "pull {table} failed (HTTP {}): {}",
            status.as_u16(),
            body.chars().take(300).collect::<String>()
        ));
    }
    let remote_total = total_from_content_range(resp.headers().get(CONTENT_RANGE));
    let text = resp.text().await.map_err(|e| format!("pull {table} body: {e}"))?;
    // A non-array body (e.g. an object/error) parses to an empty vec.
    let rows = serde_json::from_str(&text).unwrap_or_default();
    Ok((rows, remote_total))
}

/// Parse PostgREST's `Content-Range` total, e.g. `0-999/1042` → `1042`.
/// A missing or non-numeric total → `None` (indeterminate progress). An empty
/// result set reports `*/0` → `Some(0)`: `pull_total` then stays 0 and the bar
/// remains indeterminate, which is indistinguishable from an uncounted server —
/// harmless, since an empty pull registers no progress to show anyway.
fn total_from_content_range(h: Option<&reqwest::header::HeaderValue>) -> Option<u64> {
    let s = h?.to_str().ok()?;
    let total = s.rsplit('/').next()?;
    if total == "*" {
        return None;
    }
    total.parse().ok()
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
        if r.deleted_at.is_some() {
            tx.execute("DELETE FROM messages WHERE id = ?1", [&r.id])
                .map_err(|e| e.to_string())?;
            max_rev = max_rev.max(r.revision); // tombstone absorbed
            continue;
        }
        // FK: skip messages whose conversation isn't present locally (its
        // conversation will arrive or be tombstoned separately). Do NOT fold the
        // skipped row into the watermark — otherwise `pull`'s `revision > last_seen`
        // filter would exclude it forever once its conversation finally lands, and
        // a large account (>1000 convs) could silently lose message slices.
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
        max_rev = max_rev.max(r.revision); // absorbed (inserted, updated, or local-wins)
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
/// A row of the `user_config` mirror table as pulled from Supabase.
#[derive(Deserialize)]
struct RemoteUserConfig {
    id: String,
    payload: String,
    revision: i64,
    #[serde(default)]
    deleted_at: Option<i64>,
}

/// Keys in [`SyncableConfig::revs`]. Scalars use their own field name; collection
/// entries are namespaced so a provider id can never collide with a field name.
const KEY_PREFERENCES: &str = "preferences";
const KEY_ACTIVE_PROVIDER: &str = "activeProviderId";
const KEY_BASE_URL: &str = "baseUrl";
fn key_provider(id: &str) -> String {
    format!("provider:{id}")
}
fn key_override(model: &str) -> String {
    format!("override:{model}")
}

/// `base_url` / `model` count as "unset" when empty OR still the factory default:
/// a legacy (pre-multi-provider) user whose only content is a CUSTOM base_url must
/// still read as configured, and a device that merely kept the defaults must not
/// look like it chose them.
fn base_url_unset(v: &str) -> bool {
    v.is_empty() || v == crate::config::DEFAULT_BASE_URL
}
fn model_unset(v: &str) -> bool {
    v.is_empty() || v == crate::config::DEFAULT_MODEL
}

// NOTE: there is deliberately no `KEY_MODEL`. The `model` mirror is not content
// on its own (see `migrate_v1`), and it is ordered by `KEY_BASE_URL` so a payload
// can never pair OUR endpoint with THEIR model — in the legacy shape
// `base_url` + `model` together define the one provider.

/// Pick a scalar by per-key revision. With a real order (one side strictly newer)
/// the newer side wins outright. On a TIE — no ordering information, which is
/// every key of a v1 payload — fall back to the v1 value rule: the side that
/// CARRIES a value wins (`self` when both do, i.e. local-wins), so an unset side
/// can never blank a configured one.
fn pick<'a>(
    mine: &'a str,
    mine_rev: i64,
    theirs: &'a str,
    their_rev: i64,
    unset: impl Fn(&str) -> bool,
) -> &'a str {
    if mine_rev > their_rev {
        return mine;
    }
    if their_rev > mine_rev {
        return theirs;
    }
    if unset(mine) && !unset(theirs) {
        theirs
    } else {
        mine
    }
}

/// Merge two keyed collections by per-entry revision. An entry from `mine` is kept
/// unless `theirs` set that key strictly more recently; a `theirs` entry we lack is
/// taken unless OUR key is strictly newer — which is how a removal wins (a removed
/// entry keeps its revision while its value goes away, i.e. a per-key tombstone).
/// On a tie the entry that still exists wins, so a v1 payload (no revisions at all,
/// both sides 0) unions exactly as it always did.
fn merge_entries<T: Clone>(
    mine: &[T],
    theirs: &[T],
    key_of: impl Fn(&T) -> String,
    mine_rev: impl Fn(&str) -> i64,
    their_rev: impl Fn(&str) -> i64,
) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for item in mine {
        let key = key_of(item);
        if mine_rev(&key) >= their_rev(&key) {
            kept.push(key);
            out.push(item.clone());
        }
    }
    for item in theirs {
        let key = key_of(item);
        if kept.contains(&key) {
            continue;
        }
        // Ours doesn't hold it: take theirs unless our (tombstoned) key is
        // strictly newer.
        if mine_rev(&key) <= their_rev(&key) {
            out.push(item.clone());
        }
    }
    out
}

/// The syncable config subset, deserialized from `user_config.payload`.
///
/// ## v2: per-key revisions
///
/// v1 carried values only, which made three different states indistinguishable:
/// a field the user never set, a field they deliberately CLEARED, and a field the
/// device merely inherited — all just "empty". With the row revision as the only
/// ordering signal, "whose config is newer" had to stand in for "whose content is
/// authoritative", which is wrong every time a row is CREATED rather than edited:
/// a blank device mirroring its config stamped a fresh revision and could then
/// never absorb the canonical one, and a cleared field silently came back.
///
/// So every key now records the revision at which THIS device last set it:
///
///   * key absent from `revs` (or 0) → **never set** → adoptable from the other side;
///   * `revs[key] > 0` with an empty/absent value → **cleared** at that revision,
///     and a clear is a real edit that propagates;
///   * otherwise the value, ordered against the other side by that same revision.
///
/// `v` marks the format; a payload without it is v1 and is migrated on read.
#[derive(Deserialize, Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct SyncableConfig {
    /// Payload format. Absent/1 = the v1 value-only shape.
    #[serde(default)]
    v: u8,
    #[serde(default)]
    preferences: String,
    #[serde(default)]
    providers: Vec<crate::config::Provider>,
    #[serde(default)]
    active_provider_id: String,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    model_overrides: HashMap<String, crate::config::ModelOverride>,
    /// Per-key last-set revision (ms): the scalar field names plus
    /// `provider:<id>` / `override:<model>` for collection entries.
    #[serde(default)]
    revs: HashMap<String, i64>,
}

impl SyncableConfig {
    /// Whether each field is "unset" — the same default-aware notion
    /// `syncable_payload_is_empty` uses (the legacy `base_url`/`model` mirrors
    /// count as unset when they are empty or still the factory defaults).
    fn preferences_empty(&self) -> bool {
        self.preferences.trim().is_empty()
    }
    fn providers_empty(&self) -> bool {
        self.providers.is_empty()
    }
    fn active_provider_empty(&self) -> bool {
        self.active_provider_id.is_empty()
    }
    fn base_url_empty(&self) -> bool {
        base_url_unset(&self.base_url)
    }
    // `model` deliberately has no `*_empty` helper: it is not revision-tracked
    // (see `migrate_v1`); `merge`'s tie rule uses `model_unset` directly.
    fn overrides_empty(&self) -> bool {
        self.model_overrides.is_empty()
    }

    /// A payload in the current format.
    fn new_v2() -> Self {
        SyncableConfig {
            v: 2,
            ..Default::default()
        }
    }

    /// This device's revision for `key` (0 = never set).
    fn rev(&self, key: &str) -> i64 {
        self.revs.get(key).copied().unwrap_or(0)
    }

    /// Has this device ever SET something? A config that has never been edited
    /// carries no information and must never be PUBLISHED — publishing it is what
    /// blanked the configured mirror. A cleared field, by contrast, DOES carry
    /// information (cleared at revision N) and must be published. This is the v2
    /// replacement for `carries_nothing`'s value heuristic, and it cannot be fooled
    /// by a model pick or a factory default.
    fn has_content(&self) -> bool {
        self.revs.values().any(|r| *r > 0)
    }

    /// Migrate a v1 payload in place. v1's only ordering signal was the row's own
    /// revision, so every key that CARRIES something takes that revision, while
    /// every empty/default key stays 0 = "never set" and is therefore adoptable.
    /// Recovering that distinction is the entire point: it is what v1 could not
    /// express, and what let a merely-created blank row outrank real content.
    fn migrate_v1(&mut self, row_revision: i64) {
        if self.v >= 2 || !self.revs.is_empty() {
            self.v = 2;
            return;
        }
        self.v = 2;
        if row_revision <= 0 {
            return;
        }
        if !self.preferences_empty() {
            self.revs.insert(KEY_PREFERENCES.into(), row_revision);
        }
        if !self.active_provider_empty() {
            self.revs.insert(KEY_ACTIVE_PROVIDER.into(), row_revision);
        }
        if !self.base_url_empty() {
            self.revs.insert(KEY_BASE_URL.into(), row_revision);
        }
        // `model` is deliberately NOT stamped: v1 excluded it from the emptiness
        // test, and its rationale still holds — a device whose only non-default
        // content is the `model` mirror carries nothing transferable (that WAS the
        // "empty phone blanked the canonical config" clobber). It rides along by
        // value where the payload is legacy content for some other reason
        // (a custom base_url), resolved by the tie rule in `merge`.
        for p in &self.providers {
            self.revs.insert(key_provider(&p.id), row_revision);
        }
        for model in self.model_overrides.keys() {
            self.revs.insert(key_override(model), row_revision);
        }
    }

    /// Field-wise merge — the v2 replacement for `union_with`. Per key the side
    /// with the higher revision wins; on a tie (including both-never-set, which is
    /// every key of a v1 payload) it falls back to the v1 value rule, so this is
    /// behaviour-identical to `union_with` for two v1 payloads that share a row
    /// revision, while giving real per-field ordering as soon as the revisions
    /// differ (which is the point: a v1 payload is parsed at its OWN row's
    /// revision, so a newer row's keys win rather than "local always wins"). A winner whose value is EMPTY
    /// is a deliberate clear and is kept — that is what v2 adds.
    fn merge(&self, other: &SyncableConfig) -> SyncableConfig {
        let mut out = SyncableConfig::new_v2();
        // Carry the highest revision seen for every key on either side, so the
        // merged payload stays mergeable with a third device.
        for (k, r) in self.revs.iter().chain(other.revs.iter()) {
            let slot = out.revs.entry(k.clone()).or_insert(0);
            *slot = (*slot).max(*r);
        }
        out.preferences = pick(
            &self.preferences,
            self.rev(KEY_PREFERENCES),
            &other.preferences,
            other.rev(KEY_PREFERENCES),
            |s| s.trim().is_empty(),
        )
        .to_string();
        out.active_provider_id = pick(
            &self.active_provider_id,
            self.rev(KEY_ACTIVE_PROVIDER),
            &other.active_provider_id,
            other.rev(KEY_ACTIVE_PROVIDER),
            |s| s.is_empty(),
        )
        .to_string();
        out.base_url = pick(
            &self.base_url,
            self.rev(KEY_BASE_URL),
            &other.base_url,
            other.rev(KEY_BASE_URL),
            base_url_unset,
        )
        .to_string();
        // Ordered by the endpoint's key, not its own: the legacy pair moves
        // together (see the note on `model_unset`).
        out.model = pick(
            &self.model,
            self.rev(KEY_BASE_URL),
            &other.model,
            other.rev(KEY_BASE_URL),
            model_unset,
        )
        .to_string();
        // Collections: per-entry winner, with a newer removal beating a stale entry.
        out.providers = merge_entries(
            &self.providers,
            &other.providers,
            |p| key_provider(&p.id),
            |k| self.rev(k),
            |k| other.rev(k),
        );
        let mine_overrides: Vec<(String, crate::config::ModelOverride)> = self
            .model_overrides
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let their_overrides: Vec<(String, crate::config::ModelOverride)> = other
            .model_overrides
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        out.model_overrides = merge_entries(
            &mine_overrides,
            &their_overrides,
            |(model, _)| key_override(model),
            |k| self.rev(k),
            |k| other.rev(k),
        )
        .into_iter()
        .collect();
        // Never emit a dangling active id: if the winning id names a provider the
        // merge dropped (a newer removal won), take the other side's id when it is
        // valid, else clear it. The revision is kept — the decision was made.
        if !out.active_provider_id.is_empty()
            && !out
                .providers
                .iter()
                .any(|p| p.id == out.active_provider_id)
        {
            let alt = if out.active_provider_id == self.active_provider_id {
                &other.active_provider_id
            } else {
                &self.active_provider_id
            };
            out.active_provider_id = if out.providers.iter().any(|p| p.id == *alt) {
                alt.clone()
            } else {
                String::new()
            };
        }
        out
    }

    /// True when WE hold something the other side would not already have: a key we
    /// set more recently than they did. A CLEAR counts — it says "empty at revision
    /// N", which is information. The reconcile uses this to decide whether the row
    /// still needs to be pushed; a conflict where they are at or ahead of us is not
    /// "adding", so two configured devices never fight every tick.
    fn adds_content_over(&self, server: &SyncableConfig) -> bool {
        if self.has_content() || server.has_content() {
            return self.revs.iter().any(|(k, r)| *r > 0 && *r > server.rev(k));
        }
        // Neither side carries revisions (both v1): fall back to the value rule, so
        // a v1 device with content it genuinely set still re-pushes.
        (!self.preferences_empty() && server.preferences_empty())
            || (!self.providers_empty() && server.providers_empty())
            || self
                .providers
                .iter()
                .any(|p| !server.providers.iter().any(|q| q.id == p.id))
            || (!self.active_provider_empty() && server.active_provider_empty())
            || (!self.overrides_empty() && server.overrides_empty())
            || self
                .model_overrides
                .keys()
                .any(|k| !server.model_overrides.contains_key(k))
            || (!self.base_url_empty() && server.base_url_empty())
    }
}

/// The value of a scalar key, or `None` when it is unset — empty, or (for the
/// legacy mirrors) still the factory default. That is the same "unset" notion
/// `migrate_v1` stamps and `merge` orders, so the three can't disagree.
fn scalar_of<'a>(sc: &'a SyncableConfig, key: &str) -> Option<&'a str> {
    match key {
        KEY_PREFERENCES if !sc.preferences.trim().is_empty() => Some(&sc.preferences),
        KEY_ACTIVE_PROVIDER if !sc.active_provider_id.is_empty() => Some(&sc.active_provider_id),
        KEY_BASE_URL if !base_url_unset(&sc.base_url) => Some(&sc.base_url),
        _ => None,
    }
}

/// Which keys' values differ between the previous payload and the next one. A key
/// present on one side and absent on the other counts as changed — that is how an
/// added or REMOVED provider / override gets its own revision, and a removal keeps
/// it (a per-key tombstone).
fn changed_keys(prev: Option<&SyncableConfig>, next: &SyncableConfig) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in [KEY_PREFERENCES, KEY_ACTIVE_PROVIDER, KEY_BASE_URL] {
        if scalar_of(next, key) != prev.and_then(|p| scalar_of(p, key)) {
            keys.push(key.to_string());
        }
    }
    let mut ids: Vec<String> = next.providers.iter().map(|p| p.id.clone()).collect();
    if let Some(p) = prev {
        for q in &p.providers {
            if !ids.contains(&q.id) {
                ids.push(q.id.clone());
            }
        }
    }
    for id in ids {
        let now = next.providers.iter().find(|p| p.id == id);
        let was = prev.and_then(|p| p.providers.iter().find(|q| q.id == id));
        if now != was {
            keys.push(key_provider(&id));
        }
    }
    let mut models: Vec<String> = next.model_overrides.keys().cloned().collect();
    if let Some(p) = prev {
        for m in p.model_overrides.keys() {
            if !models.contains(m) {
                models.push(m.clone());
            }
        }
    }
    for model in models {
        let now = next.model_overrides.get(&model);
        let was = prev.and_then(|p| p.model_overrides.get(&model));
        if now != was {
            keys.push(key_override(&model));
        }
    }
    keys
}

/// The v2 payload for `cfg`, with a fresh revision stamped on every key whose value
/// CHANGED since `previous` (the row's stored payload) and the previous revision
/// kept on every key that didn't. `None` when nothing changed, so a no-op mirror
/// costs nothing and never burns a revision.
///
/// Diffing the values rather than tracking edit provenance is what keeps "cleared"
/// and "never set" apart: clearing a field IS a change (it was set, now it isn't),
/// so it gets a revision and propagates — while a field that was never set has no
/// revision to keep and stays adoptable. `model` is deliberately never stamped:
/// picking a model is not content (see `migrate_v1`).
///
/// `next_revision` is called at most once, and only if something changed.
pub(crate) fn stamp_user_config_payload(
    previous: Option<(&str, i64)>,
    cfg: &crate::config::AppConfig,
    next_revision: impl FnOnce() -> Result<i64, String>,
) -> Result<Option<(i64, SyncableConfig)>, String> {
    let prev = previous.and_then(|(payload, rev)| parse_syncable_at(payload, rev));
    let mut next = SyncableConfig::new_v2();
    next.preferences = cfg.preferences.clone();
    next.providers = cfg.providers.clone();
    next.active_provider_id = cfg.active_provider_id.clone();
    next.base_url = cfg.base_url.clone();
    next.model = cfg.model.clone();
    next.model_overrides = cfg.model_overrides.clone();

    let changed = changed_keys(prev.as_ref(), &next);
    if changed.is_empty() {
        return Ok(None);
    }
    let rev = next_revision()?;
    let mut revs = prev.map(|p| p.revs).unwrap_or_default();
    for key in changed {
        revs.insert(key, rev);
    }
    next.revs = revs;
    Ok(Some((rev, next)))
}

/// Project a merged payload onto the live config. The merge has already arbitrated
/// PER KEY, so this is not "fill the empties" — a remote key newer than ours must
/// actually win, which is what per-key revisions buy. The one exception is the
/// legacy `base_url`/`model` mirrors: a payload that doesn't carry them never
/// blanks them, so a device can't lose its endpoint to an unset remote.
fn project_merged(cfg: &mut crate::config::AppConfig, merged: &SyncableConfig) -> bool {
    let mut changed = false;
    if cfg.preferences != merged.preferences {
        cfg.preferences = merged.preferences.clone();
        changed = true;
    }
    if cfg.providers != merged.providers {
        cfg.providers = merged.providers.clone();
        changed = true;
    }
    if cfg.active_provider_id != merged.active_provider_id {
        cfg.active_provider_id = merged.active_provider_id.clone();
        changed = true;
    }
    if cfg.model_overrides != merged.model_overrides {
        cfg.model_overrides = merged.model_overrides.clone();
        changed = true;
    }
    if !merged.base_url.is_empty() && cfg.base_url != merged.base_url {
        cfg.base_url = merged.base_url.clone();
        changed = true;
    }
    if !merged.model.is_empty() && cfg.model != merged.model {
        cfg.model = merged.model.clone();
        changed = true;
    }
    changed
}

/// Merge one remote `user_config` payload into our row and adopt the result.
///
/// The ROW is the local side: it carries the per-key revisions (the live config is
/// mirrored into it on every edit, and this is why), so the merge is a straight
/// per-key last-writer-wins. There is no row-revision gate — that gate is what let
/// a blank-but-newer row refuse the canonical config forever.
///
/// Returns whether anything changed. Either way the row ends up holding the merged
/// payload: dirty when we still hold a key the remote lacks or has older (so the
/// next push publishes it), clean at the remote's revision when we only absorbed.
fn converge_user_config_row(
    ctx: &SyncCtx<'_>,
    id: &str,
    remote: &SyncableConfig,
    remote_revision: i64,
) -> Result<bool, String> {
    // ONE critical section from here to the end, under the db lock: every edit path
    // takes that same lock around its own (config, row) pair (see `set_config`), so
    // nothing can interleave and revert either half — reading the config here and
    // writing it back under the same lock is what makes the guard below sufficient.
    // Lock order is `db -> config` (no path nests the other way).
    let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
    // Fold anything not yet mirrored into the row FIRST: the row is the merge's
    // local side, so an edit still sitting only in the live config would otherwise
    // be invisible here and could be read as a clear.
    let Some(cfg_now) = ctx.config.try_get() else {
        return Ok(false); // poisoned lock — never mirror a defaulted read
    };
    db::sync_user_config(&conn, &cfg_now)?;
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT payload, revision FROM user_config WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let (local, local_revision) = match row {
        Some((payload, rev)) => (parse_syncable_at(&payload, rev).unwrap_or_default(), rev),
        None => (SyncableConfig::new_v2(), 0),
    };
    let merged = local.merge(remote);
    let we_add = local.adds_content_over(remote);
    let they_add = remote.adds_content_over(&local);
    if !we_add && !they_add {
        return Ok(false); // already converged
    }
    apply_converged_row(
        &conn,
        ctx.config,
        id,
        &merged,
        local_revision,
        we_add,
        remote_revision,
    )
}

/// Apply a converged merge: guard, project into the live config, write the row.
///
/// The guard: only proceed if the row is STILL the revision the merge was computed
/// against. A Settings save or provider edit that landed in the meantime owns the
/// row (it is dirty and will push), and overwriting it — or projecting the stale
/// merge into the live config — would silently lose that edit.
///
/// The PROJECTION comes before the row write: if the config write fails, the row
/// must stay untouched, or the next tick's mirror would re-stamp the stale config
/// over the values we just absorbed. (The reverse order is safe the other way: a
/// failed row write leaves the config ahead, and the mirror then writes the row
/// from the config we hold.)
///
/// Caller must hold the db lock; every writer of the (config, row) pair holds it,
/// so within this critical section nothing can move.
fn apply_converged_row(
    conn: &rusqlite::Connection,
    config: &crate::config::ConfigState,
    id: &str,
    merged: &SyncableConfig,
    expect_revision: i64,
    we_add: bool,
    remote_revision: i64,
) -> Result<bool, String> {
    if row_revision(conn, id)? != expect_revision {
        return Ok(false);
    }
    config.update(|cfg| project_merged(cfg, merged))?;
    let payload = serde_json::to_string(merged).map_err(|e| e.to_string())?;
    let (rev, dirty) = if we_add {
        // Above the remote's revision so the next push lands, and never below the
        // row's own — a fresh bump is lower when the row or the watermark runs
        // ahead of the clock.
        let bumped = db::bump_revision_c(conn)?;
        (
            bumped
                .max(remote_revision.saturating_add(1))
                .max(expect_revision),
            1,
        )
    } else {
        // We only absorbed: clean at the remote's revision, never lowering ours.
        (expect_revision.max(remote_revision), 0)
    };
    conn.execute(
        "INSERT INTO user_config(id, payload, revision, dirty) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET
           payload = excluded.payload, revision = excluded.revision, dirty = excluded.dirty",
        rusqlite::params![id, payload, rev, dirty],
    )
    .map_err(|e| e.to_string())?;
    Ok(true)
}

/// Our stored revision for a `user_config` row (0 when we have none).
fn row_revision(conn: &rusqlite::Connection, id: &str) -> Result<i64, String> {
    let rev: Option<i64> = conn
        .query_row(
            "SELECT revision FROM user_config WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(rev.unwrap_or(0))
}

/// Parse a `user_config` payload into the syncable subset, or `None` when it
/// isn't valid JSON for it (a legacy/garbage payload we can't safely merge).
///
/// A **v1** payload (no `revs`, no `v`) is migrated here: `row_revision` is stamped
/// on every key it carries, so a v1 payload keeps its relative order. Where only
/// "has anything been set?" matters, 1 will do — the exact value is irrelevant.
fn parse_syncable_at(payload: &str, row_revision: i64) -> Option<SyncableConfig> {
    let mut sc = serde_json::from_str::<SyncableConfig>(payload).ok()?;
    sc.migrate_v1(row_revision);
    Some(sc)
}

fn parse_syncable(payload: &str) -> Option<SyncableConfig> {
    parse_syncable_at(payload, 1)
}

/// The plaintext of a server `user_config` payload: unchanged when it isn't
/// encrypted, decrypted when it is and we hold the key, and `None` when it looks
/// encrypted but we cannot read it — a payload we must never overwrite (the
/// locked-device contract).
fn decrypt_server_payload(key: Option<[u8; 32]>, raw: &str) -> Option<String> {
    match (raw, key) {
        (p, _) if !crypt::looks_encrypted(p) => Some(p.to_string()),
        (p, Some(k)) => crypt::decrypt(&k, p),
        _ => None,
    }
}

/// Apply pulled user-config rows. The merge itself lives in
/// `converge_user_config_row`; this is the pull's loop around it plus the
/// high-water advance.
fn apply_user_config(ctx: &SyncCtx, rows: &[Value]) -> Result<(), String> {
    let parsed: Vec<RemoteUserConfig> = rows
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect();
    if parsed.is_empty() {
        return Ok(());
    }
    let mut parse_failed = false;
    let mut max_rev = 0;
    for r in &parsed {
        let remote = if r.deleted_at.is_some() {
            // A remote deletion is never resurrected.
            None
        } else {
            match parse_syncable_at(&r.payload, r.revision) {
                Some(sc) => Some(sc),
                None => {
                    // A bad payload blocks the high-water only when the row is
                    // NEWER than ours — one we would have had to apply. An older
                    // one can neither be absorbed nor overwrite us, so blocking on
                    // it would just re-fetch it (and re-render the UI) every tick.
                    if r.revision > local_row_revision(ctx, &r.id)? {
                        parse_failed = true;
                    }
                    None
                }
            }
        };
        if let Some(remote) = remote {
            converge_user_config_row(ctx, &r.id, &remote, r.revision)?;
        }
        max_rev = max_rev.max(r.revision);
    }
    if !parse_failed {
        let mut conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        advance_watermark(&tx, "user_config", max_rev)?;
        tx.commit().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Our stored revision for a `user_config` row (0 when we have none).
fn local_row_revision(ctx: &SyncCtx<'_>, id: &str) -> Result<i64, String> {
    let conn = ctx.db.0.lock().map_err(|e| e.to_string())?;
    let rev: Option<i64> = conn
        .query_row(
            "SELECT revision FROM user_config WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(rev.unwrap_or(0))
}

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
            if local_rev < r.revision {
                applies.push(true);
                continue;
            }
            // Empty-aware guard: never let an EMPTY local file win over (or
            // block) a NON-empty remote one. A fresh device auto-creates empty
            // core stubs (profile/preferences/goals); if those were ever marked
            // dirty with a high revision they'd beat the real content under plain
            // revision-LWW and silently wipe it from this device — and, once
            // re-pushed, from the mirror too. Deleted rows have no content, so
            // this only ever forces a non-empty write onto an empty local file.
            let remote_nonempty = r
                .content
                .as_deref()
                .map(|c| !c.trim().is_empty())
                .unwrap_or(false);
            let local_empty = memory
                .read(&r.path)
                .map(|c| c.trim().is_empty())
                .unwrap_or(true);
            applies.push(remote_nonempty && local_empty);
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
