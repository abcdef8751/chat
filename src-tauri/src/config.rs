use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Default OpenAI-compatible endpoint (Fireworks) pre-filled for convenience.
const DEFAULT_BASE_URL: &str = "https://api.fireworks.ai/inference/v1";
/// A common, broadly-available Fireworks model id. Users change this in Settings.
const DEFAULT_MODEL: &str = "accounts/fireworks/models/llama-v3p1-8b-instruct";

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
    /// Free-form user preferences injected at the top of every system prompt.
    pub preferences: String,
    /// Selected reasoning effort (e.g. `low`/`medium`/`high`); empty = provider default.
    pub thinking_level: String,
    /// User-supplied per-model price/context overrides, keyed by model id.
    pub model_overrides: HashMap<String, ModelOverride>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            model: DEFAULT_MODEL.to_string(),
            preferences: String::new(),
            thinking_level: String::new(),
            model_overrides: HashMap::new(),
        }
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
    fn legacy_api_key_field_is_ignored_on_load() {
        let path = temp_path("legacy");
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
}
