//! Per-model pricing and context windows.
//!
//! OpenAI-compatible `/models` returns no token rates, so prices are resolved:
//!   1. a per-model user override from `config.json`, else
//!   2. a fetched row cached in `model_prices`, else
//!   3. the bundled fallback table below.
//!
//! Fetching reads the free, public models.dev catalog (`api.json`), which keys
//! models by their exact API id and carries `cost` (USD per 1M tokens) and
//! `limit.context`. The provider is matched to the configured base URL.

use rusqlite::params;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use tauri::Manager;

use crate::config::ConfigState;
use crate::db::Db;

/// Fallback context window when neither an override, a fetched row, nor the
/// bundle knows a model.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// Public catalog with per-provider model pricing + limits (no key required).
const MODELS_DEV_API: &str = "https://models.dev/api.json";

/// A bundled entry: model-id substring plus its context window and rates
/// (USD per 1M tokens). Used until prices are fetched. Rates are approximate.
struct Bundled {
    pattern: &'static str,
    context_window: u64,
    input: Option<f64>,
    output: Option<f64>,
}

/// Ordered specific → generic: `find` returns the first matching pattern.
const BUNDLED: &[Bundled] = &[
    // --- Fireworks ---
    Bundled { pattern: "llama-v3p1-405b", context_window: 131_072, input: Some(3.00), output: Some(3.00) },
    Bundled { pattern: "llama-v3p1-70b", context_window: 131_072, input: Some(0.90), output: Some(0.90) },
    Bundled { pattern: "llama-v3p3-70b", context_window: 131_072, input: Some(0.90), output: Some(0.90) },
    Bundled { pattern: "llama-v3p1-8b", context_window: 131_072, input: Some(0.20), output: Some(0.20) },
    Bundled { pattern: "llama-v3p2-3b", context_window: 131_072, input: Some(0.10), output: Some(0.10) },
    Bundled { pattern: "llama-v3p2-1b", context_window: 131_072, input: Some(0.10), output: Some(0.10) },
    Bundled { pattern: "mixtral-8x22b", context_window: 65_536, input: Some(1.20), output: Some(1.20) },
    Bundled { pattern: "mixtral-8x7b", context_window: 32_768, input: Some(0.50), output: Some(0.50) },
    Bundled { pattern: "qwen2p5-coder-32b", context_window: 32_768, input: Some(0.90), output: Some(0.90) },
    Bundled { pattern: "qwen2p5-72b", context_window: 32_768, input: Some(0.90), output: Some(0.90) },
    Bundled { pattern: "qwen2p5-7b", context_window: 32_768, input: Some(0.20), output: Some(0.20) },
    Bundled { pattern: "deepseek-r1", context_window: 163_840, input: Some(3.00), output: Some(8.00) },
    Bundled { pattern: "deepseek-v3", context_window: 163_840, input: Some(0.90), output: Some(0.90) },
    Bundled { pattern: "firefunction-v2", context_window: 32_768, input: Some(0.90), output: Some(0.90) },
    Bundled { pattern: "kimi-k2", context_window: 131_072, input: Some(0.60), output: Some(2.50) },
    Bundled { pattern: "codestral", context_window: 32_768, input: Some(0.20), output: Some(0.60) },
    // --- OpenAI-compatible ---
    Bundled { pattern: "gpt-4o-mini", context_window: 128_000, input: Some(0.15), output: Some(0.60) },
    Bundled { pattern: "gpt-4o", context_window: 128_000, input: Some(2.50), output: Some(10.00) },
    Bundled { pattern: "gpt-4.1-mini", context_window: 1_047_576, input: Some(0.40), output: Some(1.60) },
    Bundled { pattern: "gpt-4.1", context_window: 1_047_576, input: Some(2.00), output: Some(8.00) },
    Bundled { pattern: "gpt-4-turbo", context_window: 128_000, input: Some(10.00), output: Some(30.00) },
    Bundled { pattern: "gpt-3.5-turbo", context_window: 16_385, input: Some(0.50), output: Some(1.50) },
    Bundled { pattern: "o1-mini", context_window: 128_000, input: Some(1.10), output: Some(4.40) },
    Bundled { pattern: "o3-mini", context_window: 200_000, input: Some(1.10), output: Some(4.40) },
    Bundled { pattern: "o1", context_window: 200_000, input: Some(15.00), output: Some(60.00) },
    // --- Family fallbacks (context only where rates are fuzzy) ---
    Bundled { pattern: "llama", context_window: 131_072, input: None, output: None },
    Bundled { pattern: "qwen", context_window: 32_768, input: None, output: None },
    Bundled { pattern: "deepseek", context_window: 163_840, input: None, output: None },
    Bundled { pattern: "mixtral", context_window: 32_768, input: None, output: None },
    Bundled { pattern: "gemma", context_window: 8_192, input: None, output: None },
    Bundled { pattern: "gpt-4", context_window: 128_000, input: None, output: None },
];

fn bundled_for(model_id: &str) -> Option<&'static Bundled> {
    let lower = model_id.to_ascii_lowercase();
    BUNDLED.iter().find(|b| lower.contains(b.pattern))
}

/// A row fetched from the pricing source and cached in `model_prices`.
#[derive(Clone, Debug, Default)]
pub struct CachedPrice {
    pub input: Option<f64>,
    pub output: Option<f64>,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub context_window: Option<u64>,
    /// Human-readable model name from models.dev (e.g. "DeepSeek V4 Pro 0813").
    pub name: Option<String>,
    /// Whether models.dev marks the model as a reasoning model.
    pub reasoning: Option<bool>,
    /// Raw `reasoning_options` JSON array from models.dev, if any.
    pub reasoning_options: Option<String>,
}

/// Model metadata cached from models.dev (name + reasoning support).
#[derive(Clone, Debug, Default)]
pub struct ModelMeta {
    pub name: Option<String>,
    pub reasoning: Option<bool>,
    pub reasoning_options: Option<String>,
}

/// Resolved pricing/context for one model, serialized straight to the frontend.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Pricing {
    pub model_id: String,
    pub context_window: u64,
    pub input_per_million: Option<f64>,
    pub output_per_million: Option<f64>,
    pub cache_read_per_million: Option<f64>,
    pub cache_write_per_million: Option<f64>,
    /// True when a user override contributed to the result.
    pub overridden: bool,
    /// Where the values came from: `override` | `fetched` | `bundled` | `default`.
    pub source: String,
}

/// Result of a price refresh, so the UI can report what happened.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RefreshResult {
    pub count: usize,
    pub provider: String,
    pub fetched_at: i64,
}

/// Resolve a model's pricing: user override wins, then a fetched row, then the
/// bundled table, then defaults.
pub fn resolve_with_cache(
    model_id: &str,
    config: &crate::config::AppConfig,
    cached: Option<CachedPrice>,
) -> Pricing {
    let bundled = bundled_for(model_id);
    let over = config.model_overrides.get(model_id);
    let cached = cached.filter(|c| {
        c.input.is_some()
            || c.output.is_some()
            || c.context_window.is_some()
            || c.cache_read.is_some()
            || c.cache_write.is_some()
    });

    let context_window = over
        .and_then(|o| o.context_window)
        .or_else(|| cached.as_ref().and_then(|c| c.context_window))
        .or_else(|| bundled.map(|b| b.context_window))
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);
    let input_per_million = over
        .and_then(|o| o.input_per_million)
        .or_else(|| cached.as_ref().and_then(|c| c.input))
        .or_else(|| bundled.and_then(|b| b.input));
    let output_per_million = over
        .and_then(|o| o.output_per_million)
        .or_else(|| cached.as_ref().and_then(|c| c.output))
        .or_else(|| bundled.and_then(|b| b.output));
    let cache_read_per_million = over
        .and_then(|o| o.cache_read_per_million)
        .or_else(|| cached.as_ref().and_then(|c| c.cache_read));
    let cache_write_per_million = over
        .and_then(|o| o.cache_write_per_million)
        .or_else(|| cached.as_ref().and_then(|c| c.cache_write));

    let source = if over.is_some() {
        "override"
    } else if cached.is_some() {
        "fetched"
    } else if bundled.is_some() {
        "bundled"
    } else {
        "default"
    };

    Pricing {
        model_id: model_id.to_string(),
        context_window,
        input_per_million,
        output_per_million,
        cache_read_per_million,
        cache_write_per_million,
        overridden: over.is_some(),
        source: source.to_string(),
    }
}

/// Resolve a model's pricing against the configured provider's cached row,
/// then user overrides, then the bundled table. Shared by the `get_pricing`
/// command and the chat loop (which freezes a turn's cost with it).
pub fn resolve_for(db: &Db, config: &crate::config::AppConfig, model_id: &str) -> Pricing {
    let cached = get_cached(db, &provider_key(&config.base_url), model_id);
    resolve_with_cache(model_id, config, cached)
}

/// Dollar cost of one turn's summed `usage` at the given rates. Returns `None`
/// when the model has no input/output rate to bill against.
///
/// Cached prompt tokens bill at the cache-read rate and cache-write tokens at
/// the cache-write rate; both fall back to the input rate when unset. Prompt
/// tokens are split so the three buckets never overlap.
pub fn cost_of_usage(usage: &Value, pricing: &Pricing) -> Option<f64> {
    let input = pricing.input_per_million?;
    let output = pricing.output_per_million?;
    let cache_read = pricing.cache_read_per_million.unwrap_or(input);
    let cache_write = pricing.cache_write_per_million.unwrap_or(input);

    let tokens = |key: &str| usage.get(key).and_then(Value::as_i64).unwrap_or(0).max(0) as f64;
    let prompt = tokens("prompt_tokens");
    let completion = tokens("completion_tokens");
    let cached = tokens("cached_tokens").min(prompt);
    let cache_write_tokens = tokens("cache_write_tokens").min((prompt - cached).max(0.0));
    let uncached = (prompt - cached - cache_write_tokens).max(0.0);

    Some(
        (uncached * input
            + cached * cache_read
            + cache_write_tokens * cache_write
            + completion * output)
            / 1_000_000.0,
    )
}

/// Host portion of a URL (no `url` crate needed): `https://a.b/c` → `a.b`.
fn host_of(url: &str) -> String {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    after_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Find the models.dev provider id matching the configured base URL.
fn provider_id_for(base_url: &str, root: &Value) -> Option<String> {
    let target = base_url.trim_end_matches('/');
    let host = host_of(target);
    let providers = root.as_object()?;

    // Prefer an exact `api` match, then fall back to a host match.
    for (id, provider) in providers {
        if provider.get("api").and_then(Value::as_str).map(|a| a.trim_end_matches('/'))
            == Some(target)
        {
            return Some(id.clone());
        }
    }
    providers
        .iter()
        .find(|(_, provider)| {
            provider
                .get("api")
                .and_then(Value::as_str)
                .map(|a| host_of(a.trim_end_matches('/')) == host)
                .unwrap_or(false)
        })
        .map(|(id, _)| id.clone())
}

/// Extract `(model_id, price)` rows from a models.dev provider object.
fn parse_provider_models(provider: &Value) -> Vec<(String, CachedPrice)> {
    let Some(models) = provider.get("models").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (id, model) in models {
        let cost = model.get("cost");
        let limit = model.get("limit");
        let price = CachedPrice {
            input: cost.and_then(|c| c.get("input")).and_then(Value::as_f64),
            output: cost.and_then(|c| c.get("output")).and_then(Value::as_f64),
            cache_read: cost.and_then(|c| c.get("cache_read")).and_then(Value::as_f64),
            cache_write: cost.and_then(|c| c.get("cache_write")).and_then(Value::as_f64),
            context_window: limit.and_then(|l| l.get("context")).and_then(Value::as_u64),
            name: model.get("name").and_then(Value::as_str).map(str::to_string),
            reasoning: model.get("reasoning").and_then(Value::as_bool),
            reasoning_options: model
                .get("reasoning_options")
                .filter(|v| !v.is_null())
                .map(|v| v.to_string()),
        };
        if price.input.is_some()
            || price.output.is_some()
            || price.context_window.is_some()
            || price.name.is_some()
        {
            out.push((id.clone(), price));
        }
    }
    out
}

fn provider_key(base_url: &str) -> String {
    base_url.trim_end_matches('/').to_string()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn get_cached(db: &Db, provider: &str, model_id: &str) -> Option<CachedPrice> {
    let conn = db.0.lock().ok()?;
    conn.query_row(
        "SELECT input_per_million, output_per_million, cache_read_per_million,
                cache_write_per_million, context_window, name, reasoning, reasoning_options
         FROM model_prices WHERE provider = ?1 AND model_id = ?2",
        params![provider, model_id],
        |r| {
            Ok(CachedPrice {
                input: r.get(0)?,
                output: r.get(1)?,
                cache_read: r.get(2)?,
                cache_write: r.get(3)?,
                context_window: r.get::<_, Option<i64>>(4)?.map(|v| v as u64),
                name: r.get(5)?,
                reasoning: r.get::<_, Option<i64>>(6)?.map(|v| v != 0),
                reasoning_options: r.get(7)?,
            })
        },
    )
    .ok()
}

/// Model metadata (name + reasoning support) cached from models.dev.
pub fn get_meta(db: &Db, provider: &str, model_id: &str) -> Option<ModelMeta> {
    let conn = db.0.lock().ok()?;
    conn.query_row(
        "SELECT name, reasoning, reasoning_options
         FROM model_prices WHERE provider = ?1 AND model_id = ?2",
        params![provider, model_id],
        |r| {
            Ok(ModelMeta {
                name: r.get(0)?,
                reasoning: r.get::<_, Option<i64>>(1)?.map(|v| v != 0),
                reasoning_options: r.get(2)?,
            })
        },
    )
    .ok()
}

/// All cached models.dev display names for the provider at `base_url`.
pub fn names_for_provider(db: &Db, base_url: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(conn) = db.0.lock() else {
        return out;
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT model_id, name FROM model_prices
         WHERE provider = ?1 AND name IS NOT NULL",
    ) else {
        return out;
    };
    let Ok(rows) = stmt.query_map(params![provider_key(base_url)], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    }) else {
        return out;
    };
    for row in rows.flatten() {
        out.insert(row.0, row.1);
    }
    out
}

/// The cached display name for one model, if models.dev provided one.
pub fn cached_model_name(db: &Db, base_url: &str, model_id: &str) -> Option<String> {
    get_meta(db, &provider_key(base_url), model_id).and_then(|m| m.name)
}

/// Every model id cached from models.dev for the provider at `base_url`.
///
/// This is the source for the model picker when the provider is in models.dev:
/// the catalog carries names, context windows, and reasoning metadata, so the
/// list needs no extra `/models` round-trip (or API key).
pub fn provider_model_ids(db: &Db, base_url: &str) -> Vec<String> {
    let Ok(conn) = db.0.lock() else {
        return Vec::new();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT model_id FROM model_prices WHERE provider = ?1")
    else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map(params![provider_key(base_url)], |r| {
        r.get::<_, String>(0)
    }) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

fn upsert_cached(
    db: &Db,
    provider: &str,
    model_id: &str,
    price: &CachedPrice,
    fetched_at: i64,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO model_prices
           (provider, model_id, input_per_million, output_per_million,
            cache_read_per_million, cache_write_per_million, context_window,
            name, reasoning, reasoning_options, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(provider, model_id) DO UPDATE SET
           input_per_million = excluded.input_per_million,
           output_per_million = excluded.output_per_million,
           cache_read_per_million = excluded.cache_read_per_million,
           cache_write_per_million = excluded.cache_write_per_million,
           context_window = COALESCE(excluded.context_window, model_prices.context_window),
           name = COALESCE(excluded.name, model_prices.name),
           reasoning = COALESCE(excluded.reasoning, model_prices.reasoning),
           reasoning_options = COALESCE(excluded.reasoning_options, model_prices.reasoning_options),
           fetched_at = excluded.fetched_at",
        params![
            provider,
            model_id,
            price.input,
            price.output,
            price.cache_read,
            price.cache_write,
            price.context_window.map(|v| v as i64),
            price.name,
            price.reasoning.map(|v| v as i64),
            price.reasoning_options,
            fetched_at
        ],
    )
    .map_err(|e| format!("cache price: {e}"))?;
    Ok(())
}

#[tauri::command]
pub fn get_pricing(
    config: tauri::State<'_, ConfigState>,
    db: tauri::State<'_, Db>,
    model_id: String,
) -> Pricing {
    let config = config.get();
    resolve_for(&db, &config, &model_id)
}

/// Fetch current prices + context windows for the configured provider from
/// models.dev and cache them in `model_prices`.
#[tauri::command]
pub async fn refresh_pricing(app: tauri::AppHandle) -> Result<RefreshResult, String> {
    let config = app.state::<ConfigState>().get();
    let root = fetch_models_dev().await?;
    let db = app.state::<Db>();
    let (provider_id, count, fetched_at) = cache_provider(&db, &config.base_url, &root)?;
    Ok(RefreshResult {
        count,
        provider: provider_id,
        fetched_at,
    })
}

/// Populate `model_prices` from models.dev when nothing is cached for the
/// configured provider. Best-effort: callers ignore errors. This is what makes
/// real model names and reasoning metadata available without a manual refresh.
/// Pass `force` to re-fetch even when a cache exists (e.g. the settings Load).
pub async fn ensure_cached(db: &Db, base_url: &str, force: bool) -> Result<(), String> {
    if !force && has_cached(db, &provider_key(base_url)) {
        return Ok(());
    }
    let root = fetch_models_dev().await?;
    cache_provider(db, base_url, &root).map(|_| ())
}

fn has_cached(db: &Db, provider: &str) -> bool {
    let Ok(conn) = db.0.lock() else {
        return false;
    };
    // Require display names too so databases migrated from an older schema
    // (rows existed but names were NULL) get re-fetched once.
    conn.query_row(
        "SELECT COUNT(*) FROM model_prices WHERE provider = ?1 AND name IS NOT NULL",
        params![provider],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Match the configured base URL to a models.dev provider and upsert its models.
fn cache_provider(db: &Db, base_url: &str, root: &Value) -> Result<(String, usize, i64), String> {
    let provider_id = provider_id_for(base_url, root).ok_or_else(|| {
        format!(
            "No models.dev provider matches {} — prices stay on the bundled defaults.",
            base_url
        )
    })?;
    let provider = root.get(&provider_id).ok_or("provider missing from catalog")?;
    let parsed = parse_provider_models(provider);
    if parsed.is_empty() {
        return Err(format!("models.dev has no priced models for '{provider_id}'."));
    }
    let provider_key = provider_key(base_url);
    let fetched_at = now_millis();
    let mut count = 0usize;
    for (model_id, price) in parsed {
        upsert_cached(db, &provider_key, &model_id, &price, fetched_at)?;
        count += 1;
    }
    Ok((provider_id, count, fetched_at))
}

/// Thinking-level options for a model, derived from cached models.dev data.
///
/// When models.dev lists `effort` reasoning options we show exactly those;
/// otherwise we fall back to the levels the OpenAI API accepts. Models that
/// models.dev marks as non-reasoning expose no options.
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingOptions {
    pub options: Vec<String>,
    /// `models.dev` | `openai` | `none`
    pub source: String,
    pub supports_reasoning: bool,
}

const OPENAI_THINKING_LEVELS: &[&str] = &["minimal", "low", "medium", "high"];

pub fn thinking_options_for(meta: Option<ModelMeta>) -> ThinkingOptions {
    if let Some(meta) = meta {
        if meta.reasoning == Some(false) {
            return ThinkingOptions {
                options: Vec::new(),
                source: "none".into(),
                supports_reasoning: false,
            };
        }
        if let Some(raw) = meta.reasoning_options.as_deref() {
            if let Ok(Value::Array(opts)) = serde_json::from_str::<Value>(raw) {
                let mut values: Vec<String> = Vec::new();
                for opt in &opts {
                    if opt.get("type").and_then(Value::as_str) != Some("effort") {
                        continue;
                    }
                    if let Some(vs) = opt.get("values").and_then(Value::as_array) {
                        for v in vs {
                            if let Some(s) = v.as_str() {
                                if !values.iter().any(|x| x == s) {
                                    values.push(s.to_string());
                                }
                            }
                        }
                    }
                }
                if !values.is_empty() {
                    return ThinkingOptions {
                        options: values,
                        source: "models.dev".into(),
                        supports_reasoning: true,
                    };
                }
            }
        }
    }
    ThinkingOptions {
        options: OPENAI_THINKING_LEVELS.iter().map(|s| s.to_string()).collect(),
        source: "openai".into(),
        supports_reasoning: true,
    }
}

#[tauri::command]
pub fn thinking_options(
    config: tauri::State<'_, ConfigState>,
    db: tauri::State<'_, Db>,
    model_id: String,
) -> ThinkingOptions {
    let config = config.get();
    let meta = get_meta(&db, &provider_key(&config.base_url), &model_id);
    thinking_options_for(meta)
}

async fn fetch_models_dev() -> Result<Value, String> {
    let resp = reqwest::Client::new()
        .get(MODELS_DEV_API)
        .send()
        .await
        .map_err(|e| format!("fetch prices: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("fetch prices: HTTP {}", resp.status()));
    }
    resp.json::<Value>()
        .await
        .map_err(|e| format!("parse prices: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppConfig, ModelOverride};
    use serde_json::json;
    use std::collections::HashMap;

    fn resolve(model_id: &str, config: &AppConfig) -> Pricing {
        resolve_with_cache(model_id, config, None)
    }

    const SAMPLE: &str = r#"{
      "fireworks-ai": {
        "id": "fireworks-ai",
        "api": "https://api.fireworks.ai/inference/v1",
        "models": {
          "accounts/fireworks/models/deepseek-v4-pro-0813": {
            "name": "DeepSeek V4 Pro 0813",
            "reasoning": true,
            "reasoning_options": [
              { "type": "toggle" },
              { "type": "effort", "values": ["high", "max"] }
            ],
            "cost": { "input": 1.32, "output": 3.96, "cache_read": 0.044 },
            "limit": { "context": 1000000, "output": 384000 }
          },
          "accounts/fireworks/routers/kimi-k3-fast": {
            "name": "Kimi K3 Fast",
            "cost": { "input": 4.5, "output": 22.5, "cache_read": 0.45 },
            "limit": { "context": 1048576, "output": 131072 }
          },
          "accounts/fireworks/models/free-but-contextless": {
            "name": "No price or limit"
          }
        }
      }
    }"#;

    #[test]
    fn matches_provider_by_base_url_and_parses_models() {
        let root: Value = serde_json::from_str(SAMPLE).unwrap();
        let id = provider_id_for("https://api.fireworks.ai/inference/v1/", &root).unwrap();
        assert_eq!(id, "fireworks-ai");

        let rows = parse_provider_models(root.get("fireworks-ai").unwrap());
        let by_id: HashMap<String, CachedPrice> = rows.into_iter().collect();
        // Name-only models are cached too (for the picker / system prompt).
        assert_eq!(by_id.len(), 3);
        let ds = &by_id["accounts/fireworks/models/deepseek-v4-pro-0813"];
        assert_eq!(ds.input, Some(1.32));
        assert_eq!(ds.output, Some(3.96));
        assert_eq!(ds.cache_read, Some(0.044));
        assert_eq!(ds.context_window, Some(1_000_000));
        assert_eq!(ds.name.as_deref(), Some("DeepSeek V4 Pro 0813"));
    }

    #[test]
    fn provider_match_falls_back_to_host() {
        let root: Value = serde_json::from_str(SAMPLE).unwrap();
        let id = provider_id_for("https://api.fireworks.ai/v1", &root).unwrap();
        assert_eq!(id, "fireworks-ai");
        assert!(provider_id_for("http://localhost:1234/v1", &root).is_none());
    }

    #[test]
    fn bundled_match_is_case_insensitive_and_specific_first() {
        let p = resolve("accounts/fireworks/models/llama-v3p1-8b-instruct", &AppConfig::default());
        assert_eq!(p.context_window, 131_072);
        assert_eq!(p.input_per_million, Some(0.20));
        assert_eq!(p.source, "bundled");
        assert!(!p.overridden);
    }

    #[test]
    fn unknown_model_gets_default_window_and_no_rates() {
        let p = resolve("some-local/unknown-model", &AppConfig::default());
        assert_eq!(p.context_window, DEFAULT_CONTEXT_WINDOW);
        assert_eq!(p.input_per_million, None);
        assert_eq!(p.source, "default");
    }

    #[test]
    fn override_beats_the_bundle() {
        let mut config = AppConfig::default();
        config.model_overrides.insert(
            "my-model".into(),
            ModelOverride {
                context_window: Some(8_000),
                input_per_million: Some(1.5),
                output_per_million: Some(3.0),
                cache_read_per_million: Some(0.15),
                cache_write_per_million: Some(1.9),
            },
        );
        let p = resolve("my-model", &config);
        assert_eq!(p.context_window, 8_000);
        assert_eq!(p.input_per_million, Some(1.5));
        assert_eq!(p.output_per_million, Some(3.0));
        assert_eq!(p.cache_read_per_million, Some(0.15));
        assert_eq!(p.cache_write_per_million, Some(1.9));
        assert!(p.overridden);
        assert_eq!(p.source, "override");
    }

    #[test]
    fn fetched_cache_beats_bundle_but_loses_to_override() {
        let config = AppConfig::default();
        let cached = CachedPrice {
            input: Some(9.99),
            output: Some(8.88),
            cache_read: Some(1.0),
            cache_write: None,
            context_window: Some(42_000),
            ..Default::default()
        };
        let p = resolve_with_cache(
            "accounts/fireworks/models/llama-v3p1-8b-instruct",
            &config,
            Some(cached.clone()),
        );
        assert_eq!(p.source, "fetched");
        assert_eq!(p.input_per_million, Some(9.99));
        assert_eq!(p.context_window, 42_000);
        assert_eq!(p.cache_read_per_million, Some(1.0));
        assert_eq!(p.cache_write_per_million, None);

        let mut over = AppConfig::default();
        over.model_overrides.insert(
            "accounts/fireworks/models/llama-v3p1-8b-instruct".into(),
            ModelOverride {
                input_per_million: Some(0.01),
                ..Default::default()
            },
        );
        let p = resolve_with_cache(
            "accounts/fireworks/models/llama-v3p1-8b-instruct",
            &over,
            Some(cached),
        );
        assert_eq!(p.source, "override");
        assert_eq!(p.input_per_million, Some(0.01));
        // Output falls through to the fetched row (override only set input).
        assert_eq!(p.output_per_million, Some(8.88));
    }

    #[test]
    fn cost_of_usage_bills_cache_buckets_without_overlap() {
        let pricing = Pricing {
            model_id: "m".into(),
            context_window: 1000,
            input_per_million: Some(1.0),
            output_per_million: Some(2.0),
            cache_read_per_million: Some(0.1),
            cache_write_per_million: Some(1.5),
            overridden: false,
            source: "override".into(),
        };
        // 100 prompt = 40 cache-read + 20 cache-write + 40 uncached, 10 output.
        let usage = json!({
            "prompt_tokens": 100,
            "completion_tokens": 10,
            "cached_tokens": 40,
            "cache_write_tokens": 20,
        });
        let cost = cost_of_usage(&usage, &pricing).unwrap();
        let expected = (40.0 * 1.0 + 40.0 * 0.1 + 20.0 * 1.5 + 10.0 * 2.0) / 1_000_000.0;
        assert!((cost - expected).abs() < 1e-12);
    }

    #[test]
    fn cost_of_usage_is_none_without_rates() {
        let pricing = Pricing {
            model_id: "m".into(),
            context_window: 1000,
            input_per_million: None,
            output_per_million: None,
            cache_read_per_million: None,
            cache_write_per_million: None,
            overridden: false,
            source: "default".into(),
        };
        assert!(cost_of_usage(&json!({ "prompt_tokens": 10 }), &pricing).is_none());
    }

    #[test]
    fn host_parsing() {
        assert_eq!(host_of("https://api.fireworks.ai/inference/v1"), "api.fireworks.ai");
        assert_eq!(host_of("http://localhost:1234/v1"), "localhost:1234");
    }

    #[test]
    fn thinking_options_prefer_models_dev_effort_values() {
        let meta = ModelMeta {
            name: Some("DeepSeek V4 Pro 0813".into()),
            reasoning: Some(true),
            reasoning_options: Some(r#"[{"type":"toggle"},{"type":"effort","values":["high","max"]}]"#.into()),
        };
        let opts = thinking_options_for(Some(meta));
        assert_eq!(opts.source, "models.dev");
        assert_eq!(opts.options, vec!["high", "max"]);
        assert!(opts.supports_reasoning);
    }

    #[test]
    fn thinking_options_fall_back_to_openai_levels() {
        // Known model with no effort values (toggle only) → OpenAI set.
        let toggle = ModelMeta {
            reasoning: Some(true),
            reasoning_options: Some(r#"[{"type":"toggle"}]"#.into()),
            ..Default::default()
        };
        let opts = thinking_options_for(Some(toggle));
        assert_eq!(opts.source, "openai");
        assert_eq!(opts.options, vec!["minimal", "low", "medium", "high"]);

        // Unknown model → OpenAI set.
        let unknown = thinking_options_for(None);
        assert_eq!(unknown.source, "openai");
        assert!(unknown.supports_reasoning);
    }

    #[test]
    fn thinking_options_empty_for_non_reasoning_models() {
        let meta = ModelMeta {
            reasoning: Some(false),
            ..Default::default()
        };
        let opts = thinking_options_for(Some(meta));
        assert_eq!(opts.source, "none");
        assert!(opts.options.is_empty());
        assert!(!opts.supports_reasoning);
    }

    fn temp_db(name: &str) -> crate::db::Db {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-pricing-{name}-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        crate::db::Db(std::sync::Mutex::new(crate::db::open(&p).unwrap()))
    }

    #[test]
    fn provider_ids_and_names_round_trip_through_cache() {
        let db = temp_db("roundtrip");
        let root: Value = serde_json::from_str(SAMPLE).unwrap();
        let base = "https://api.fireworks.ai/inference/v1";
        let (provider, count, _) = cache_provider(&db, base, &root).unwrap();
        assert_eq!(provider, "fireworks-ai");
        assert_eq!(count, 3);

        let mut ids = provider_model_ids(&db, base);
        ids.sort();
        assert_eq!(ids.len(), 3);
        assert!(ids.contains(&"accounts/fireworks/routers/kimi-k3-fast".to_string()));

        let names = names_for_provider(&db, base);
        assert_eq!(
            names.get("accounts/fireworks/models/deepseek-v4-pro-0813").map(String::as_str),
            Some("DeepSeek V4 Pro 0813")
        );
        assert_eq!(
            cached_model_name(&db, base, "accounts/fireworks/routers/kimi-k3-fast").as_deref(),
            Some("Kimi K3 Fast")
        );
    }
}
