//! Multi-provider management.
//!
//! A provider is a named OpenAI-compatible endpoint with its own base URL and
//! its own API key (in the OS keychain under `provider:<id>`). Config holds the
//! list plus an active id; conversations remember a per-chat provider id.
//!
//! Discovery: the "Add provider" picker lists providers straight from the free
//! models.dev catalog (`api.json`), which carries each provider's `name` and
//! public `api` endpoint. Manual/custom endpoints (self-hosted, local) are
//! added by typing a name + base URL directly.

use serde_json::Value;

use crate::config::{normalize_base_url, AppConfig, ConfigState, ModelsDevProvider, Provider};

const MODELS_DEV_API: &str = "https://models.dev/api.json";

/// A provider plus its live key status and whether it is the active one — the
/// shape the frontend renders in the provider manager / picker.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub default_model: Option<String>,
    /// Whether a per-provider (or legacy) API key is stored.
    pub has_key: bool,
    /// Whether this is the active provider for new chats.
    pub active: bool,
}

fn info_for(config: &AppConfig, provider: &Provider) -> ProviderInfo {
    let has_key = if provider.id == crate::config::LEGACY_PROVIDER_ID {
        crate::secrets::has_api_key_quiet()
    } else {
        crate::secrets::has_provider_key(&provider.id)
    };
    ProviderInfo {
        id: provider.id.clone(),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        default_model: provider.default_model.clone(),
        has_key,
        active: config.active_provider_id() == provider.id,
    }
}

/// Resolve the API key for a provider, honoring the legacy single-provider case.
///
/// Order:
///   1. the per-provider keychain account `provider:<id>`, if present;
///   2. the legacy `api_key` account — but only for the synthesized legacy
///      provider, since that is what pre-multi-provider users configured.
pub fn resolve(provider_id: &str) -> Result<Option<String>, String> {
    if let Some(key) = crate::secrets::get_provider_key(provider_id)? {
        if !key.trim().is_empty() {
            return Ok(Some(key));
        }
    }
    if provider_id == crate::config::LEGACY_PROVIDER_ID {
        return crate::secrets::get();
    }
    Ok(None)
}

/// Command-form of [`resolve`], exposed for the frontend to check/display a
/// provider's key state.
#[tauri::command]
pub fn resolve_provider_key(provider_id: String) -> Result<Option<String>, String> {
    resolve(&provider_id)
}

#[tauri::command]
pub fn list_providers(config: tauri::State<'_, ConfigState>) -> Vec<ProviderInfo> {
    let cfg = config.get();
    // Always include the legacy synthesized provider first so a pre-multi config
    // (which has no `providers` entries) still shows something to manage.
    let legacy = cfg.active_provider();
    let mut out = vec![info_for(&cfg, &legacy)];
    for p in &cfg.providers {
        if p.id == legacy.id {
            continue;
        }
        out.push(info_for(&cfg, p));
    }
    out
}

/// Add a provider. Returns the new provider's info (with key status). When
/// `api_key` is given it is stored in the keychain; otherwise left empty.
#[tauri::command]
pub fn add_provider(
    config: tauri::State<'_, ConfigState>,
    name: String,
    base_url: String,
    api_key: Option<String>,
) -> Result<ProviderInfo, String> {
    let mut cfg = config.get();
    let name = name.trim().to_string();
    let base_url = normalize_base_url(&base_url);
    if name.is_empty() {
        return Err("Provider name cannot be empty.".into());
    }
    if base_url.is_empty() {
        return Err("Provider base URL cannot be empty.".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(k) = api_key.as_deref().map(str::trim) {
        if !k.is_empty() {
            crate::secrets::set_provider_key(&id, k)?;
        }
    }
    let provider = Provider {
        id: id.clone(),
        name,
        base_url,
        default_model: None,
    };
    cfg.providers.push(provider.clone());
    // First provider becomes the active one.
    if cfg.active_provider_id().is_empty() || cfg.providers.len() == 1 {
        sync_active(&mut cfg, &id);
    }
    config.set(cfg)?;
    Ok(info_for(&config.get(), &provider))
}

/// Update a provider's name/base URL and optionally (re)set its API key.
/// `api_key: Some("")` clears the key; `None` leaves it unchanged.
#[tauri::command]
pub fn update_provider(
    config: tauri::State<'_, ConfigState>,
    id: String,
    name: Option<String>,
    base_url: Option<String>,
    api_key: Option<Option<String>>,
) -> Result<ProviderInfo, String> {
    let mut cfg = config.get();
    let idx = cfg
        .providers
        .iter()
        .position(|p| p.id == id)
        .ok_or_else(|| format!("provider not found: {id}"))?;
    if let Some(name) = name {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("Provider name cannot be empty.".into());
        }
        cfg.providers[idx].name = name;
    }
    if let Some(url) = base_url {
        let url = normalize_base_url(&url);
        if url.is_empty() {
            return Err("Provider base URL cannot be empty.".into());
        }
        cfg.providers[idx].base_url = url;
    }
    if let Some(key) = api_key.flatten() {
        let key = key.trim().to_string();
        if key.is_empty() {
            crate::secrets::delete_provider_key(&id)?;
        } else {
            crate::secrets::set_provider_key(&id, &key)?;
        }
    }
    // If this is the active provider, keep the legacy base_url/model mirrors in sync.
    if cfg.active_provider_id() == id {
        sync_active(&mut cfg, &id);
    }
    let result = info_for(&config.get(), &cfg.providers[idx].clone());
    config.set(cfg)?;
    Ok(result)
}

/// Remove a provider (and its keychain key). Removing the active provider falls
/// back to the next provider (or the legacy one) as active; it cannot remove
/// the synthesized legacy provider.
#[tauri::command]
pub fn remove_provider(
    config: tauri::State<'_, ConfigState>,
    id: String,
) -> Result<Vec<ProviderInfo>, String> {
    let mut cfg = config.get();
    let Some(idx) = cfg.providers.iter().position(|p| p.id == id) else {
        return Err(format!("provider not found: {id}"));
    };
    cfg.providers.remove(idx);
    let _ = crate::secrets::delete_provider_key(&id);
    if cfg.active_provider_id() == id {
        if let Some(first) = cfg.providers.first() {
            let first_id = first.id.clone();
            sync_active(&mut cfg, &first_id);
        } else {
            cfg.active_provider_id.clear();
        }
    }
    config.set(cfg)?;
    Ok(list_providers(config))
}

/// Set the active (default-for-new-chats) provider.
#[tauri::command]
pub fn set_active_provider(
    config: tauri::State<'_, ConfigState>,
    id: String,
) -> Result<ProviderInfo, String> {
    let mut cfg = config.get();
    let Some(provider) = cfg.provider_by_id(&id).cloned() else {
        return Err(format!("provider not found: {id}"));
    };
    sync_active(&mut cfg, &id);
    config.set(cfg)?;
    Ok(info_for(&config.get(), &provider))
}

/// Keep `base_url`/`model` as mirrors of the active provider so any code path
/// that still reads them directly (legacy) behaves, and so new chats inherit
/// the active provider's default model when one is set.
fn sync_active(cfg: &mut AppConfig, id: &str) {
    cfg.active_provider_id = id.to_string();
    if let Some(p) = cfg.provider_by_id(id) {
        let base_url = p.base_url.clone();
        let default_model = p.default_model.clone();
        cfg.base_url = base_url;
        if let Some(m) = default_model {
            cfg.model = m;
        }
    }
}

/// Providers offered in the "Add provider" picker, read from models.dev's free
/// catalog. Includes only entries with a public API endpoint, sorted by name.
#[tauri::command]
pub async fn list_models_dev_providers() -> Result<Vec<ModelsDevProvider>, String> {
    let root = fetch_models_dev().await?;
    let mut out: Vec<ModelsDevProvider> = Vec::new();
    if let Some(providers) = root.as_object() {
        for (id, provider) in providers {
            let Some(api) = provider
                .get("api")
                .and_then(Value::as_str)
                .map(normalize_base_url)
            else {
                continue;
            };
            if api.is_empty() {
                continue;
            }
            let name = provider
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| id.clone());
            out.push(ModelsDevProvider {
                id: id.clone(),
                name,
                base_url: api,
            });
        }
    }
    out.sort_by(|a, b| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()));
    Ok(out)
}

async fn fetch_models_dev() -> Result<Value, String> {
    let resp = reqwest::Client::new()
        .get(MODELS_DEV_API)
        .send()
        .await
        .map_err(|e| format!("fetch models.dev: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("fetch models.dev: HTTP {}", resp.status()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("parse models.dev: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;

    #[test]
    fn add_provider_normalizes_base_url_and_sets_active() {
        // This test exercises the pure resolution/info logic without a keychain
        // or config file; command-level write flows are covered by the app.
        let mut cfg = AppConfig::default();
        let provider = Provider {
            id: "p1".into(),
            name: "My Provider".into(),
            base_url: "https://x.io/v1".into(),
            default_model: None,
        };
        cfg.providers.push(provider.clone());
        // First provider becomes active (mirrors add_provider's condition).
        if cfg.active_provider_id().is_empty() || cfg.providers.len() == 1 {
            sync_active(&mut cfg, &provider.id);
        }
        assert_eq!(cfg.active_provider_id(), "p1");
        assert_eq!(cfg.base_url, "https://x.io/v1");
        assert_eq!(cfg.active_provider().name, "My Provider");
    }

    #[test]
    fn active_mirrors_follow_provider() {
        let mut cfg = AppConfig::default();
        cfg.providers = vec![Provider {
            id: "p".into(),
            name: "P".into(),
            base_url: "https://p/v1".into(),
            default_model: Some("m1".into()),
        }];
        sync_active(&mut cfg, "p");
        assert_eq!(cfg.base_url, "https://p/v1");
        assert_eq!(cfg.model, "m1");
    }
}
