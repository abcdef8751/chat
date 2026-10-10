use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Default OpenAI-compatible endpoint (Fireworks) pre-filled for convenience.
const DEFAULT_BASE_URL: &str = "https://api.fireworks.ai/inference/v1";
/// A common, broadly-available Fireworks model id. Users change this in Settings.
const DEFAULT_MODEL: &str = "accounts/fireworks/models/llama-v3p1-8b-instruct";
/// Stable id used for the single provider synthesized from a legacy config that
/// predates multiple-provider support. Its API key resolves via the legacy
/// `api_key` keychain entry (see `secrets` / `providers::resolve_provider_key`).
pub const LEGACY_PROVIDER_ID: &str = "legacy";

/// Per-model override for pricing/context values that `/models` doesn't carry.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelOverride {
    pub context_window: Option<u64>,
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
}

/// One OpenAI-compatible provider: its own base URL and (optionally) a favorite
/// default model. The per-provider API key never lives here — it is stored in
/// the OS keychain under a per-provider account (see `secrets` / `providers`).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Provider {
    /// Stable unique id (uuid), never changes after creation.
    pub id: String,
    /// Display name (e.g. "Fireworks").
    pub name: String,
    /// OpenAI-compatible base URL, trimmed of a trailing slash on input.
    pub base_url: String,
    /// Optional per-provider favorite model shown first in the model picker.
    pub default_model: Option<String>,
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            base_url: DEFAULT_BASE_URL.to_string(),
            default_model: None,
        }
    }
}

/// A models.dev provider entry offered in the "Add provider" picker.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsDevProvider {
    /// models.dev provider id (e.g. `fireworks-ai`).
    pub id: String,
    /// Display name from the catalog.
    pub name: String,
    /// The public API endpoint (base URL).
    pub base_url: String,
}

/// Runtime configuration for the OpenAI-compatible client.
///
/// Serialized to a JSON file in the app data directory. The API key is NOT
/// stored here: it lives in the OS keychain (see [`crate::secrets`]).
/// A legacy plaintext `apiKey` field is migrated to the keychain on load.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    pub base_url: String,
    pub model: String,
    /// All configured providers. Empty on configs that predate multi-provider
    /// support; `active_provider()` synthesizes the legacy single provider from
    /// `base_url` in that case.
    pub providers: Vec<Provider>,
    /// Which provider new chats default to. Must match a `providers` id; when
    /// empty/unknown the active provider resolves to the first, or the legacy one.
    pub active_provider_id: String,
    /// Free-form user preferences injected at the top of every system prompt.
    pub preferences: String,
    /// Selected reasoning effort (e.g. `low`/`medium`/`high`); empty = provider default.
    pub thinking_level: String,
    /// Echo the current turn's reasoning trace back on its assistant tool-call
    /// message. DeepSeek's thinking mode requires this (HTTP 400 otherwise);
    /// some providers reject the unknown field, so it can be turned off.
    pub echo_reasoning_content: bool,
    /// Whether to run the idle memory-consolidation pass after conversations
    /// go quiet (spends tokens in the background).
    pub memory_reflection_enabled: bool,
    /// Minutes a conversation must be idle before reflection runs over it.
    pub memory_reflection_idle_minutes: u32,
    /// User-supplied per-model price/context overrides, keyed by model id.
    pub model_overrides: HashMap<String, ModelOverride>,
    /// Working directory for host shell tools (Android: a directory under
    /// shared storage, e.g. `/storage/emulated/0/PiChat`). Empty resolves to
    /// the platform default (desktop: inherit the host cwd). See ANDROID_SHELL.md.
    pub shell_workspace_dir: String,
    /// Opt-in Supabase backup + sync. Off by default; when off (or logged out)
    /// the whole sync module is idle and never touches the network.
    pub sync_enabled: bool,
    /// ms epoch of the last successful sync, persisted so the "last sync"
    /// readout survives restarts (the live value in `SyncState` is in-memory
    /// only and otherwise resets to "never" on each launch).
    pub sync_last_sync_at: Option<i64>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            providers: Vec::new(),
            active_provider_id: String::new(),
            preferences: String::new(),
            thinking_level: String::new(),
            echo_reasoning_content: true,
            memory_reflection_enabled: true,
            memory_reflection_idle_minutes: 30,
            model_overrides: HashMap::new(),
            shell_workspace_dir: String::new(),
            sync_enabled: false,
            sync_last_sync_at: None,
        }
    }
}

/// Normalize a base URL: trim whitespace and a single trailing slash.
pub fn normalize_base_url(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

/// Extract the host portion of a URL for naming a provider derived from a URL:
/// `https://a.b/c` → `a.b`. Cheap split, no `url` crate needed.
fn host_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .to_owned()
}

impl AppConfig {
    /// The provider to use for new chats: the one named by `active_provider_id`,
    /// else the first configured, else a single provider synthesized from the
    /// legacy `base_url` (back-compat for pre-multi-provider configs). When the
    /// synthesized legacy provider is returned its id is [`LEGACY_PROVIDER_ID`],
    /// whose API key resolves via the legacy `api_key` keychain entry.
    pub fn active_provider(&self) -> Provider {
        if let Some(p) = self.provider_by_id(&self.active_provider_id) {
            return p.clone();
        }
        if let Some(first) = self.providers.first() {
            return first.clone();
        }
        let host = {
            let h = host_of(self.base_url.trim_end_matches('/'));
            if h.is_empty() { "provider".to_string() } else { h }
        };
        Provider {
            id: LEGACY_PROVIDER_ID.to_string(),
            name: host,
            base_url: normalize_base_url(&self.base_url),
            default_model: None,
        }
    }

    pub fn provider_by_id(&self, id: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// The id of the provider `active_provider()` would return. Providers with
    /// no default_model still get an id, so this is cheaply callable where only
    /// the id is needed.
    pub fn active_provider_id(&self) -> String {
        self.active_provider().id
    }

    /// Resolve a conversation's provider by its stored id, falling back to the
    /// active provider when the id is empty/unknown.
    pub fn provider_for(&self, provider_id: &str) -> Provider {
        if provider_id.is_empty() {
            return self.active_provider();
        }
        self.provider_by_id(provider_id)
            .cloned()
            .unwrap_or_else(|| self.active_provider())
    }
}

/// Global config state installed into Tauri. Holds the config file path and an
/// in-memory copy guarded by a mutex to serialize writes.
pub struct ConfigState {
    path: PathBuf,
    data: Mutex<AppConfig>,
}

impl ConfigState {
    /// Load config from `path`, falling back to defaults when the file is
    /// missing or malformed. Migrates a legacy plaintext `apiKey` to the OS
    /// keychain (best-effort; the file is rewritten without the key on success).
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let data = if path.exists() {
            let text = std::fs::read_to_string(&path).map_err(|e| format!("read config: {e}"))?;
            let value: serde_json::Value =
                serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
            let data: AppConfig =
                serde_json::from_value(value.clone()).unwrap_or_else(|_| AppConfig::default());
            migrate_legacy_api_key(&path, &value, &data);
            data
        } else {
            AppConfig::default()
        };
        Ok(Self {
            path,
            data: Mutex::new(data),
        })
    }

    pub fn get(&self) -> AppConfig {
        self.data.lock().map(|d| d.clone()).unwrap_or_default()
    }

    pub fn set(&self, config: AppConfig) -> Result<(), String> {
        {
            *self.data.lock().map_err(|e| e.to_string())? = config.clone();
        }
        let text = serde_json::to_string_pretty(&config).map_err(|e| format!("serialize: {e}"))?;
        std::fs::write(&self.path, text).map_err(|e| format!("write config: {e}"))?;
        Ok(())
    }
}

/// Move a legacy plaintext `apiKey` from `config.json` into the OS keychain.
///
/// Only runs when the keychain has no entry yet (first migration). On success
/// the config file is rewritten without the key so the secret stops living on
/// disk. On keychain failure the file is left untouched.
fn migrate_legacy_api_key(path: &std::path::Path, raw: &serde_json::Value, data: &AppConfig) {
    let Some(key) = raw.get("apiKey").and_then(|v| v.as_str()) else {
        return;
    };
    if key.trim().is_empty() {
        return;
    }
    let already_set = crate::secrets::get().ok().flatten().is_some();
    if !already_set && crate::secrets::set(key).is_ok() {
        if let Ok(text) = serde_json::to_string_pretty(data) {
            let _ = std::fs::write(path, text);
        }
    }
}

#[tauri::command]
pub fn get_config(state: tauri::State<'_, ConfigState>) -> AppConfig {
    state.get()
}

#[tauri::command]
pub fn set_config(state: tauri::State<'_, ConfigState>, config: AppConfig) -> Result<(), String> {
    state.set(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-config-test-{name}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn missing_file_uses_defaults() {
        let state = ConfigState::load(temp_path("missing")).unwrap();
        let cfg = state.get();
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        assert_eq!(cfg.model, DEFAULT_MODEL);
    }

    #[test]
    fn roundtrip_set_get() {
        let path = temp_path("roundtrip");
        let state = ConfigState::load(path.clone()).unwrap();
        state
            .set(AppConfig {
                base_url: "http://localhost:1/v1".into(),
                model: "m".into(),
                ..Default::default()
            })
            .unwrap();
        let reloaded = ConfigState::load(path.clone()).unwrap();
        assert_eq!(reloaded.get().model, "m");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sync_last_sync_at_roundtrips_and_defaults_for_old_config() {
        let path = temp_path("sync-last");
        // An old config file without the new field must load with None (backwards
        // compat), so "last sync" simply shows "never" rather than erroring.
        std::fs::write(&path, r#"{"baseUrl":"http://x/v1","model":"m"}"#).unwrap();
        let state = ConfigState::load(path.clone()).unwrap();
        assert_eq!(state.get().sync_last_sync_at, None);
        // And a value we set must survive a write -> reload round-trip.
        let mut cfg = state.get();
        cfg.sync_last_sync_at = Some(1_700_000_000_000);
        state.set(cfg).unwrap();
        let reloaded = ConfigState::load(path.clone()).unwrap();
        assert_eq!(reloaded.get().sync_last_sync_at, Some(1_700_000_000_000));
        assert_eq!(reloaded.get().model, "m"); // other fields preserved
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_api_key_field_is_ignored_on_load() {        let path = temp_path("legacy");
        std::fs::write(
            &path,
            r#"{"baseUrl":"http://x/v1","apiKey":"fw_legacy","model":"m"}"#,
        )
        .unwrap();
        let state = ConfigState::load(path.clone()).unwrap();
        assert_eq!(state.get().model, "m");
        // a rewritten config never contains the key in plaintext
        state.set(state.get()).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("apiKey"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_config_synthesizes_a_single_provider() {
        let cfg = AppConfig::default();
        // No providers configured (legacy): active_provider derives from base_url.
        let p = cfg.active_provider();
        assert_eq!(p.id, LEGACY_PROVIDER_ID);
        assert_eq!(p.base_url, normalize_base_url(&DEFAULT_BASE_URL));
        // provider_for with an unknown id falls back to the active provider.
        assert_eq!(cfg.provider_for("nope").id, LEGACY_PROVIDER_ID);
        // empty id → active provider too.
        assert_eq!(cfg.provider_for("").id, LEGACY_PROVIDER_ID);
    }

    #[test]
    fn active_provider_prefers_selected_then_first() {
        let mut cfg = AppConfig::default();
        cfg.providers = vec![
            Provider {
                id: "a".into(),
                name: "A".into(),
                base_url: "https://a/v1/".into(),
                default_model: None,
            },
            Provider {
                id: "b".into(),
                name: "B".into(),
                base_url: "https://b/v1/".into(),
                default_model: Some("m-b".into()),
            },
        ];
        // No active id → first provider.
        assert_eq!(cfg.active_provider().id, "a");
        // Explicit active id → that provider.
        cfg.active_provider_id = "b".into();
        assert_eq!(cfg.active_provider().name, "B");
        assert_eq!(cfg.active_provider_id(), "b");
        // Unknown active id → falls back to first.
        cfg.active_provider_id = "zzz".into();
        assert_eq!(cfg.active_provider().id, "a");
        // provider_for resolves a stored id.
        assert_eq!(cfg.provider_for("b").base_url, "https://b/v1/");
    }

    #[test]
    fn normalize_base_url_trims_trailing_slashes() {
        assert_eq!(normalize_base_url(" https://x/v1/ "), "https://x/v1");
        assert_eq!(normalize_base_url("https://x/v1"), "https://x/v1");
    }
}
