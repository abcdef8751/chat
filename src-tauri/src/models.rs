use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::Manager;

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelObject>,
}

#[derive(Deserialize)]
struct ModelObject {
    id: String,
}

#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    /// Human-readable name from models.dev, when known.
    pub name: Option<String>,
}

/// Substrings identifying clearly non-chat models (embeddings, audio,
/// image-generation, rerankers, guardrails) that servers commonly list
/// alongside chat models. Best-effort heuristic: `/models` carries no
/// capability metadata, so we show everything else and cache the raw list.
const NON_CHAT_HINTS: &[&str] = &[
    "embedding",
    "embed-",
    "whisper",
    "tts",
    "speech",
    "voice",
    "rerank",
    "stable-diffusion",
    "sdxl",
    "sd3",
    "flux",
    "image-gen",
    "moderation",
    "guardrail",
    "guard-",
];

fn is_likely_chat(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    !NON_CHAT_HINTS.iter().any(|hint| lower.contains(hint))
}

/// In-memory cache of the raw `/models` response, keyed by base URL so a
/// base-URL change in Settings invalidates it automatically.
#[derive(Default)]
pub struct ModelCache(Mutex<Option<CachedModels>>);

struct CachedModels {
    base_url: String,
    ids: Vec<String>,
}

impl ModelCache {
    fn get(&self, base_url: &str) -> Option<Vec<String>> {
        let cached = self.0.lock().ok()?;
        let cached = cached.as_ref()?;
        if cached.base_url == base_url {
            Some(
                cached
                    .ids
                    .iter()
                    .filter(|id| is_likely_chat(id))
                    .cloned()
                    .collect(),
            )
        } else {
            None
        }
    }

    fn put(&self, base_url: String, ids: Vec<String>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(CachedModels { base_url, ids });
        }
    }
}

/// Attach cached models.dev display names to a list of model ids.
fn with_names(ids: Vec<String>, names: &HashMap<String, String>) -> Vec<ModelInfo> {
    ids.into_iter()
        .map(|id| ModelInfo {
            name: names.get(&id).cloned(),
            id,
        })
        .collect()
}

/// `GET /models` on the configured endpoint, returning the raw model ids.
async fn fetch_provider_models(base_url: &str, api_key: &str) -> Result<Vec<String>, String> {
    let resp = reqwest::Client::new()
        .get(format!("{base_url}/models"))
        .bearer_auth(api_key)
        .send()
        .await
        .map_err(|e| format!("request models: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("models HTTP {status}: {body}"));
    }
    let models: ModelsResponse = resp
        .json()
        .await
        .map_err(|e| format!("parse models: {e}"))?;
    Ok(models.data.into_iter().map(|m| m.id).collect())
}

/// List chat-capable models for the configured endpoint.
///
/// models.dev is the source of truth when the endpoint matches one of its
/// providers (which also supplies display names, context windows, and
/// reasoning metadata). Self-hosted/unmatched endpoints fall back to their own
/// `GET /models`. The resolved list is cached in memory per base URL; pass
/// `refresh: true` to re-fetch the catalog.
#[tauri::command]
pub async fn list_models(
    app: tauri::AppHandle,
    refresh: Option<bool>,
) -> Result<Vec<ModelInfo>, String> {
    let config = app.state::<crate::config::ConfigState>().get();
    let base_url = config.base_url.trim_end_matches('/').to_string();
    let db = app.state::<crate::db::Db>();
    let force = refresh.unwrap_or(false);

    if !force {
        if let Some(ids) = app.state::<ModelCache>().get(&base_url) {
            let names = crate::pricing::names_for_provider(&db, &base_url);
            return Ok(with_names(ids, &names));
        }
    }

    // Pull/refresh the models.dev catalog for this endpoint, then list from it.
    let _ = crate::pricing::ensure_cached(&db, &base_url, force).await;
    let mut ids: Vec<String> = crate::pricing::provider_model_ids(&db, &base_url)
        .into_iter()
        .filter(|id| is_likely_chat(id))
        .collect();

    // Endpoint isn't in models.dev: ask the endpoint itself (needs a key).
    if ids.is_empty() {
        let api_key = crate::secrets::get()?
            .ok_or_else(|| "API key not set — open Settings and add your key.".to_string())?;
        ids = fetch_provider_models(&base_url, &api_key)
            .await?
            .into_iter()
            .filter(|id| is_likely_chat(id))
            .collect();
    }

    ids.sort();
    ids.dedup();
    app.state::<ModelCache>().put(base_url.clone(), ids.clone());
    let names = crate::pricing::names_for_provider(&db, &base_url);
    Ok(with_names(ids, &names))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_non_chat_models() {
        let ids = [
            "accounts/fireworks/models/llama-v3p1-8b-instruct",
            "accounts/fireworks/models/nomic-embed-text",
            "accounts/fireworks/models/whisper-large-v3",
            "gpt-4o-mini",
        ];
        let chat: Vec<&str> = ids.iter().copied().filter(|id| is_likely_chat(id)).collect();
        assert_eq!(chat, ["accounts/fireworks/models/llama-v3p1-8b-instruct", "gpt-4o-mini"]);
    }

    #[test]
    fn cache_is_scoped_to_base_url() {
        let cache = ModelCache::default();
        cache.put("http://a/v1".into(), vec!["m1".into()]);
        assert_eq!(cache.get("http://a/v1").unwrap(), vec!["m1".to_string()]);
        assert!(cache.get("http://b/v1").is_none());
    }
}
