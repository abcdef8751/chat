use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::Manager;

use crate::pricing::ModelMeta;

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ModelObject>,
}

/// One entry from the provider's `GET /models`. Besides the required `id`, we
/// capture every unknown field so a per-model context length can be read
/// regardless of which key the server uses — OpenAI-compatible servers disagree
/// (`context_length`, `max_model_len`, `max_input_tokens`, …), and flattening
/// keeps a stray non-numeric field from failing the whole deserialize.
#[derive(Deserialize)]
struct ModelObject {
    id: String,
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// Best-effort context length from a `/models` entry. Tolerates a number or a
/// numeric string. Keys are tried in order of how unambiguous they are.
fn context_length_of(extra: &serde_json::Map<String, serde_json::Value>) -> Option<u64> {
    const KEYS: &[&str] = &[
        "context_length",
        "context_window",
        "max_context_length",
        "max_model_len",
        "max_input_tokens",
        "context",
    ];
    for key in KEYS {
        let Some(value) = extra.get(*key) else {
            continue;
        };
        if let Some(n) = value.as_u64() {
            return Some(n);
        }
        if let Some(n) = value.as_str().and_then(|s| s.parse::<u64>().ok()) {
            return Some(n);
        }
    }
    None
}

#[derive(Serialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    /// Human-readable name from models.dev, when known.
    pub name: Option<String>,
    /// Whether the model can accept image input, resolved from the user's override
    /// (override → models.dev → text-only), so effectively never `None`. The
    /// composer re-resolves this client-side from the raw fields below
    /// (`src/lib/capabilities.ts`, mirroring `pricing::resolve_capabilities`)
    /// because this bit is baked at list-fetch time and goes stale the moment a
    /// capability is toggled in Settings — the list isn't refetched then.
    pub vision: Option<bool>,
    /// models.dev `reasoning` flag; `None` when the provider isn't in the catalog.
    pub reasoning: Option<bool>,
    /// models.dev coarse `attachment` flag (documents + images).
    pub attachment: Option<bool>,
    /// Raw models.dev `modalities` JSON (input/output lists); `None` when unknown.
    pub modalities: Option<String>,
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

/// Attach cached models.dev metadata to a list of model ids.
///
/// `vision` is resolved through [`crate::pricing::resolve_capabilities`] so a
/// user's modality override (needed for custom providers, which have no
/// models.dev data) is applied to the canonical bit. The raw
/// `reasoning`/`attachment`/`modalities` fields are passed through untouched so
/// the Settings editor can show the underlying catalog values, and the frontend
/// re-resolves vision from them plus the override for the composer's image
/// warning (see `src/lib/capabilities.ts`) — the baked-in `vision` bit would go
/// stale the moment a capability is toggled without a list refresh.
fn with_meta(
    ids: Vec<String>,
    meta: &HashMap<String, ModelMeta>,
    config: &crate::config::AppConfig,
) -> Vec<ModelInfo> {
    ids.into_iter()
        .map(|id| {
            let m = meta.get(&id);
            let caps = crate::pricing::resolve_capabilities(config, &id, m);
            ModelInfo {
                name: m.and_then(|m| m.name.clone()),
                vision: Some(caps.vision()),
                reasoning: m.and_then(|m| m.reasoning),
                attachment: m.and_then(|m| m.attachment),
                modalities: m.and_then(|m| m.modalities.clone()),
                id,
            }
        })
        .collect()
}

/// `GET /models` on the configured endpoint, returning each model's id and the
/// context length it advertises (when it advertises one).
async fn fetch_provider_models(
    base_url: &str,
    api_key: &str,
) -> Result<Vec<(String, Option<u64>)>, String> {
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
    Ok(models
        .data
        .into_iter()
        .map(|m| {
            let ctx = context_length_of(&m.extra);
            (m.id, ctx)
        })
        .collect())
}

/// List chat-capable models for a provider's endpoint.
///
/// models.dev is the source of truth when the endpoint matches one of its
/// providers (which also supplies display names, context windows, and
/// reasoning metadata). Self-hosted/unmatched endpoints fall back to their own
/// `GET /models`. The resolved list is cached in memory per base URL; pass
/// `refresh: true` to re-fetch the catalog. `provider_id` selects a specific
/// provider (base URL + key); when `None`/unknown the active provider is used.
#[tauri::command]
pub async fn list_models(
    app: tauri::AppHandle,
    refresh: Option<bool>,
    provider_id: Option<String>,
) -> Result<Vec<ModelInfo>, String> {
    let config = app.state::<crate::config::ConfigState>().get();
    let provider = config.provider_for(provider_id.as_deref().unwrap_or(""));
    let base_url = provider.base_url.trim_end_matches('/').to_string();
    let db = app.state::<crate::db::Db>();
    let force = refresh.unwrap_or(false);

    if !force {
        if let Some(ids) = app.state::<ModelCache>().get(&base_url) {
            let meta = crate::pricing::meta_for_provider(&db, &base_url);
            return Ok(with_meta(ids, &meta, &config));
        }
    }

    // models.dev is used ONLY when the user explicitly picked this provider from
    // the catalog (catalog_id is set). Manual/custom providers always list via
    // the provider's own GET /models — models.dev is never auto-matched by host.
    let use_catalog = crate::pricing::uses_catalog(&provider);
    let mut ids: Vec<String> = Vec::new();
    if use_catalog {
        let _ = crate::pricing::ensure_cached(&db, &base_url, force).await;
        ids = crate::pricing::provider_model_ids(&db, &base_url)
            .into_iter()
            .filter(|id| is_likely_chat(id))
            .collect();
    }

    // No catalog list (manual provider, or the catalog had no chat models): ask
    // the endpoint itself (needs a key).
    if ids.is_empty() {
        let api_key = crate::providers::resolve(&provider.id)?.ok_or_else(|| {
            "API key not set for this provider — set it in Settings.".to_string()
        })?;
        let fetched = fetch_provider_models(&base_url, &api_key).await?;
        // Some OpenAI-compatible servers advertise a per-model context length;
        // persist it so the pricing resolver finds it as a cached value (it beats
        // any bundled family guess). Servers that don't are left alone — a custom
        // provider's models then default to 200k rather than a wrong family guess.
        for (id, ctx) in &fetched {
            if let (Some(ctx), true) = (ctx, is_likely_chat(id)) {
                crate::pricing::cache_context_window(&db, &base_url, id, *ctx);
            }
        }
        ids = fetched
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| is_likely_chat(id))
            .collect();
    }

    ids.sort();
    ids.dedup();
    app.state::<ModelCache>().put(base_url.clone(), ids.clone());
    let meta = crate::pricing::meta_for_provider(&db, &base_url);
    Ok(with_meta(ids, &meta, &config))
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

    #[test]
    fn parses_context_length_from_models_response() {
        // Different servers name the field differently; the id-only model must
        // still deserialize, and a non-numeric value must not fail the parse.
        let raw = r#"{"data":[
            {"id":"deepseek-v4.1-flash","context_length":1048576},
            {"id":"Qwen3.8-27B-FP8","max_model_len":262000},
            {"id":"agents-a1","context_window":"262000"},
            {"id":"plain-model"},
            {"id":"weird-model","context_length":"not-a-number"}
        ]}"#;
        let resp: ModelsResponse = serde_json::from_str(raw).unwrap();
        let got: Vec<(String, Option<u64>)> = resp
            .data
            .into_iter()
            .map(|m| (m.id, context_length_of(&m.extra)))
            .collect();
        assert_eq!(
            got,
            vec![
                ("deepseek-v4.1-flash".to_string(), Some(1_048_576)),
                ("Qwen3.8-27B-FP8".to_string(), Some(262_000)),
                ("agents-a1".to_string(), Some(262_000)),
                ("plain-model".to_string(), None),
                ("weird-model".to_string(), None),
            ]
        );
    }
}
