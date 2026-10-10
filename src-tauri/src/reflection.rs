//! Idle memory reflection.
//!
//! After a conversation has been quiet for a while, a background pass hands the
//! model its transcript plus the current memory files and asks it to consolidate
//! durable facts (via the memory tools). This keeps the always-injected core
//! files curated instead of letting the live inbox grow unbounded, and it runs
//! while the conversation context is still "warm" at the provider. Each run's
//! cost is recorded separately from chat cost, and a small note is left in the
//! conversation when files change.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_openai::types::chat::{
    ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs,
    ChatCompletionRequestUserMessageArgs,
};
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::chat::{self, EventSink, StreamEvent};
use crate::config::ConfigState;
use crate::db;
use crate::memory::MemoryState;
use crate::tools;

/// Sink used when a pass runs without a UI channel: events are discarded.
struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _ev: StreamEvent) {}
}

/// Start the periodic reflection scheduler. It wakes roughly once a minute and
/// reflects at most one due conversation per tick.
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Let startup (catalog fetch, UI) settle before the first pass.
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            if let Err(e) = tick(&app).await {
                eprintln!("memory reflection: {e}");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

async fn tick(app: &AppHandle) -> Result<(), String> {
    let cfg = app.state::<ConfigState>().get();
    if !cfg.memory_reflection_enabled || cfg.model.trim().is_empty() {
        return Ok(());
    }
    let idle_ms = (cfg.memory_reflection_idle_minutes.max(1) as i64) * 60_000;
    let cutoff = db::now_ms() - idle_ms;

    let due = {
        let db = app.state::<db::Db>();
        db::conversations_due_for_reflection(&db, cutoff)?
    };
    for conversation_id in due {
        if app
            .state::<chat::StreamRegistry>()
            .is_streaming(&conversation_id)
        {
            continue;
        }
        reflect_conversation(app, &conversation_id).await?;
        break; // one per tick
    }
    Ok(())
}

/// Run one reflection pass over a conversation. Public so the "Consolidate now"
/// button / a test can trigger it directly.
pub async fn reflect_conversation(app: &AppHandle, conversation_id: &str) -> Result<(), String> {
    let cfg = app.state::<ConfigState>().get();
    if cfg.model.trim().is_empty() {
        return Ok(());
    }
    let db = app.state::<db::Db>();

    // The conversation's own provider drives reflection (which endpoint/model a
    // chat actually runs on), falling back to the active provider.
    let conversation = db::get_conversation(&db, conversation_id)?
        .ok_or_else(|| format!("conversation not found: {conversation_id}"))?;
    let provider = cfg.provider_for(conversation.provider_id.as_deref().unwrap_or(""));
    let model = conversation
        .model
        .clone()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| cfg.model.clone());
    let Some(api_key) = crate::providers::resolve(&provider.id)? else {
        return Ok(());
    };
    // Override the turn-local config so run_tool_loop (which reads cfg.base_url
    // / cfg.model) targets this conversation's provider + model.
    let mut turn_cfg = cfg.clone();
    turn_cfg.base_url = provider.base_url.clone();
    turn_cfg.model = model.clone();

    let memory = app.state::<MemoryState>();
    let brave = app.state::<tools::BraveSearch>();
    let shell = app.state::<crate::shell::ShellExecutor>();
    let approvals = app.state::<tools::ApprovalRegistry>();

    // Snapshot the reflectable message range and skip when nothing is new.
    let snapshot_max = match db::max_reflectable_index(&db, conversation_id)? {
        Some(idx) => idx,
        None => return Ok(()),
    };
    let already = db::last_reflected_index(&db, conversation_id)?.unwrap_or(-1);
    if snapshot_max <= already {
        return Ok(());
    }

    let history = db::read_messages(&db, conversation_id)?;
    let before = snapshot_files(&memory);
    // Recent consolidation notes (across chats) so the model doesn't repeat work.
    let recent = db::recent_reflections(&db, 8).unwrap_or_default();

    // Reuse the SAME leading system prompt as a live chat turn so the shared
    // conversation prefix stays cacheable. The maintenance task goes in a
    // trailing system message — after the prefix, so it doesn't affect caching.
    let base_prompt = conversation
        .system_prompt
        .clone()
        .unwrap_or_else(|| chat::DEFAULT_SYSTEM_PROMPT.to_string());
    let model_label =
        crate::pricing::cached_model_name(&db, &turn_cfg.base_url, &turn_cfg.model)
            .unwrap_or_else(|| turn_cfg.model.clone());
    let system_prompt =
        chat::build_system_prompt(&base_prompt, &model_label, &cfg.preferences, &memory);

    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    messages.push(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(system_prompt)
            .build()
            .map_err(|e| e.to_string())?
            .into(),
    );
    messages.extend(chat::build_history_messages(&history)?);
    messages.push(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(reflection_tail(&recent))
            .build()
            .map_err(|e| e.to_string())?
            .into(),
    );

    let tools_list = tools::tool_specs(tools::brave_available());
    let flag = Arc::new(AtomicBool::new(false));

    // Abort the pass if a real chat turn starts on this conversation.
    let watcher = {
        let app = app.clone();
        let cid = conversation_id.to_string();
        let watcher_flag = flag.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if watcher_flag.load(Ordering::SeqCst) {
                    break;
                }
                if app
                    .state::<chat::StreamRegistry>()
                    .is_streaming(&cid)
                {
                    watcher_flag.store(true, Ordering::SeqCst);
                    break;
                }
            }
        })
    };

    let sink = NullSink;
    let result = chat::run_tool_loop(
        &turn_cfg,
        &api_key,
        &brave,
        &shell,
        &memory,
        &approvals,
        conversation_id,
        messages,
        &tools_list,
        flag,
        &sink,
        None,
        chat::ToolMode::Reflection,
    )
    .await;
    watcher.abort();
    let result = result?;

    // Don't advance the watermark if the pass was interrupted or failed.
    if result.stop_reason == "aborted" || result.stop_reason == "error" {
        return Ok(());
    }

    let after = snapshot_files(&memory);
    let changed = diff_files(&before, &after);

    let cost = {
        let pricing =
            crate::pricing::resolve_for(&db, &turn_cfg, &turn_cfg.model);
        crate::pricing::cost_of_usage(&result.usage, &pricing)
    };
    let note = if changed.is_empty() {
        "Memory consolidation: nothing to save.".to_string()
    } else {
        format!("Memory consolidated: {}", changed.join(", "))
    };

    db::insert_reflection(
        &db,
        conversation_id,
        &turn_cfg.model,
        Some(&result.usage.to_string()),
        cost,
        &changed,
        &note,
    )?;

    // Leave a small, low-key note in the conversation when something changed.
    if !changed.is_empty() {
        db::insert_message(
            &db,
            conversation_id.to_string(),
            "memory".into(),
            note,
            Some(turn_cfg.model.clone()),
            Some(turn_cfg.base_url.clone()),
            None,
            None,
            None,
            None,
        )?;
    }

    db::set_last_reflected_index(&db, conversation_id, snapshot_max)?;

    // Let an open UI refresh the conversation (to reveal the note) and its
    // reflection-cost readout.
    let _ = app.emit(
        "memory-reflection",
        json!({ "conversationId": conversation_id, "changed": changed }),
    );
    Ok(())
}

const REFLECTION_INSTRUCTION: &str = "You are now acting as the long-term memory maintainer. \
Review the conversation above and update the shared Markdown memory files shown in your system \
prompt so durable facts are preserved. Use `read_memory` to see a file's full contents before \
rewriting it.\n\n\
Guidance:\n\
- Save only stable, durable facts (identity, preferences, goals, ongoing projects, and \
notable details). Ignore transient chit-chat and one-off questions.\n\
- Route facts to the right file: identity/background → `profile.md`, preferences → \
`preferences.md`, goals → `goals.md`. Create a new file only for a substantial, recurring \
subject (e.g. `project-atlas.md`), never for a one-off fact.\n\
- Use `write_memory` to rewrite a file when merging or deduplicating; use `save_memory` only \
to append a brand-new fact.\n\
- Keep core files concise. Do not duplicate or contradict the explicit user preferences; do \
not invent facts.\n\
- If nothing is worth remembering, make no tool calls. When done, reply with one short line \
describing what you changed, or \"nothing to save\".";

/// Trailing system message for the reflection pass. It sits *after* the
/// conversation history, so the shared prefix (system prompt + history) stays
/// cacheable; it carries the maintenance task plus the recent consolidation
/// notes so the model doesn't repeat work.
fn reflection_tail(recent: &[db::ReflectionRecord]) -> String {
    let mut out = String::from(REFLECTION_INSTRUCTION);
    if !recent.is_empty() {
        out.push_str(
            "\n\nRecent consolidation activity (newest first) — facts already recorded below are \
             already in memory; do not re-save them or re-report them:\n",
        );
        for record in recent {
            out.push_str(&format!(
                "- {} — {}\n",
                format_day(record.created_at),
                record.note
            ));
        }
    }
    out
}

/// Format a millisecond epoch timestamp as `YYYY-MM-DD` (UTC, no chrono dep).
fn format_day(ms: i64) -> String {
    let days = (ms / 1000).div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

fn snapshot_files(memory: &MemoryState) -> HashMap<String, String> {
    memory
        .file_names()
        .into_iter()
        .map(|name| {
            let content = memory.read(&name).unwrap_or_default();
            (name, content)
        })
        .collect()
}

/// Names of files whose contents were created, changed, or removed. Core files
/// present but empty in both snapshots are ignored.
fn diff_files(before: &HashMap<String, String>, after: &HashMap<String, String>) -> Vec<String> {
    let mut changed: Vec<String> = Vec::new();
    for (name, content) in after {
        match before.get(name) {
            Some(prev) if prev == content => {}
            Some(_) => changed.push(name.clone()),
            None if content.trim().is_empty() => {}
            None => changed.push(name.clone()),
        }
    }
    for name in before.keys() {
        if !after.contains_key(name) {
            changed.push(name.clone());
        }
    }
    changed.sort();
    changed.dedup();
    changed
}

#[tauri::command]
pub async fn reflect_now(app: AppHandle, conversation_id: String) -> Result<(), String> {
    reflect_conversation(&app, &conversation_id).await
}

// ---------------------------------------------------------------------------
// Backfill: map (parallel, read-only) then reduce (serial, single writer)
//
// Reflecting over an imported archive one conversation per 60s tick is both slow
// (~20h for 1200 chats) and unsafe to parallelize naively: every pass is a
// read-modify-write on the same Markdown files, and `reflection_tail`'s "recent
// consolidation notes" only deduplicates because passes are serial.
//
// So the work is split in two. The *map* phase runs many extraction passes
// concurrently, each with `read_memory` only, staging a plain-text summary in
// `memory_extractions` — never touching the memory files. The *reduce* phase is
// a single serial writer that folds those summaries into memory through the
// ordinary reflection tool loop.
//
// A side benefit of freezing memory during the map phase: the leading system
// prompt is byte-identical across every extraction pass, so a provider with
// prompt caching charges for that prefix once instead of once per conversation.
// ---------------------------------------------------------------------------

/// Default floor for a conversation to be worth extracting.
///
/// This is a *noise* filter, not a cost control. It was originally 2000, on the
/// theory that short chats are most of the passes but a rounding error of the
/// content — but the passes are cheap (the extraction system prompt is ~60
/// tokens rather than the live turn's ~1900, and empty payloads are filtered
/// before the reduce), and the short tail is not empty of signal: a 1.9k-char
/// chat about audio gear named the user's existing IEMs and their EQ habit.
/// 200 chars drops the "." and "hm" conversations and keeps everything else.
pub const DEFAULT_MIN_CHARS: i64 = 200;
/// Default *starting* concurrency for a backfill. AIMD adjusts from here.
pub const DEFAULT_CONCURRENCY: usize = 8;
/// Hard ceiling on in-flight extraction passes. AIMD settles below this on its
/// own when the account is throttled; this is only a safety rail.
const MAX_CONCURRENCY: usize = 64;
/// Floor, so a throttled run still makes progress.
const MIN_CONCURRENCY: usize = 1;
/// Attempts per pass before it is counted as failed.
const PASS_ATTEMPTS: u32 = 4;
/// Fraction of the model's context window the staged summaries may occupy before
/// the reduce is split into several passes. One pass is preferred: it sees every
/// summary at once and resolves contradictions better than a fold. Chunking is a
/// fallback for when the payload would not comfortably fit.
const CONTEXT_BUDGET: f64 = 0.8;
/// Rough chars-per-token used to size the reduce payload.
const CHARS_PER_TOKEN: f64 = 4.0;

/// Progress + cancel state for a running backfill, shared with the UI.
#[derive(Default)]
pub struct Backfill {
    running: AtomicBool,
    cancel: AtomicBool,
    total: AtomicUsize,
    done: AtomicUsize,
    failed: AtomicUsize,
    /// The AIMD limit currently in force, for display.
    concurrency: AtomicUsize,
    /// The most recent failure, so a run that ends with failures is diagnosable
    /// from the UI instead of only from stderr.
    last_error: Mutex<Option<String>>,
    phase: Mutex<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackfillStatus {
    pub running: bool,
    pub phase: String,
    pub total: usize,
    pub done: usize,
    pub failed: usize,
    /// In-flight passes allowed right now; AIMD moves this during the run.
    pub concurrency: usize,
    pub last_error: Option<String>,
}

impl Backfill {
    pub fn snapshot(&self) -> BackfillStatus {
        BackfillStatus {
            running: self.running.load(Ordering::SeqCst),
            phase: self.phase.lock().map(|p| p.clone()).unwrap_or_default(),
            total: self.total.load(Ordering::SeqCst),
            done: self.done.load(Ordering::SeqCst),
            failed: self.failed.load(Ordering::SeqCst),
            concurrency: self.concurrency.load(Ordering::SeqCst),
            last_error: self.last_error.lock().ok().and_then(|e| e.clone()),
        }
    }

    fn set_last_error(&self, error: &str) {
        if let Ok(mut slot) = self.last_error.lock() {
            *slot = (!error.is_empty()).then(|| error.to_string());
        }
    }

    fn set_phase(&self, phase: &str) {
        if let Ok(mut p) = self.phase.lock() {
            *p = phase.to_string();
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// Whether a provider failure is worth retrying.
///
/// Fireworks enforces *adaptive* token-per-minute limits rather than a
/// concurrency cap, and its docs are explicit that ramping up too quickly draws
/// 429s. A cold burst of extraction passes is exactly that, so the 429s clear on
/// their own and must not be recorded as permanent failures. 503 means the
/// service is briefly overloaded.
fn is_transient(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    [
        "429",
        "503",
        "rate limit",
        "too many requests",
        "overloaded",
        "timeout",
        "timed out",
    ]
    .iter()
    .any(|needle| e.contains(needle))
}

/// Retry a pass on transient provider errors with exponential backoff. Permanent
/// failures (a 400, an abort) return immediately.
///
/// `on_throttle` fires on *every* transient failure, not just the final one. That
/// matters: waiting until a pass gives up means the concurrency controller learns
/// about a rate limit ~14s late, after a wave of passes has already exhausted
/// their retries. Backing off on the first 429 drops the pressure while the
/// retries still have budget left.
async fn with_retry<F, Fut, T>(
    mut attempt_fn: F,
    attempts: u32,
    on_throttle: impl Fn(),
) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let attempts = attempts.max(1);
    let mut delay = Duration::from_secs(2);
    let mut last = String::new();
    for attempt in 1..=attempts {
        match attempt_fn().await {
            Ok(value) => return Ok(value),
            Err(e) => {
                if !is_transient(&e) {
                    return Err(e);
                }
                on_throttle();
                if attempt == attempts {
                    return Err(e);
                }
                eprintln!("memory backfill: transient failure ({e}); retrying in {delay:?}");
                last = e;
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(60));
            }
        }
    }
    Err(last)
}

/// AIMD policy, split out so it can be tested without a runtime.
///
/// Additive increase: one permit per full window of clean completions, so the
/// ramp is additive in round-trips rather than in requests — growing per request
/// would jump from 8 to 700 in seconds. Multiplicative decrease: halve on a
/// throttle, which is what makes the loop back off fast and recover slowly.
fn aimd_increase(limit: usize, since_increase: usize, max: usize) -> (usize, usize) {
    if limit >= max {
        return (limit, since_increase);
    }
    let seen = since_increase + 1;
    if seen >= limit {
        (limit + 1, 0)
    } else {
        (limit, seen)
    }
}

fn aimd_decrease(limit: usize, min: usize) -> usize {
    (limit / 2).max(min)
}

/// Adaptive concurrency for the map phase.
///
/// Fireworks enforces *adaptive* token-per-minute limits rather than a
/// concurrency cap, and the ceiling depends on the account tier and the model's
/// size tier — so any fixed number is either too timid or over-drives into
/// sustained 429s. AIMD converges on whatever the account actually allows,
/// which also makes the setting safe to leave alone across accounts and models.
struct Aimd {
    sem: Arc<tokio::sync::Semaphore>,
    /// `(limit, clean completions since the last increase)`. Behind a mutex
    /// because `on_throttle` is called from the worker tasks as well as from the
    /// completion loop.
    state: Mutex<(usize, usize)>,
    min: usize,
    max: usize,
}

impl Aimd {
    fn new(initial: usize, min: usize, max: usize) -> Self {
        let initial = initial.clamp(min, max);
        Self {
            sem: Arc::new(tokio::sync::Semaphore::new(initial)),
            state: Mutex::new((initial, 0)),
            min,
            max,
        }
    }

    fn limit(&self) -> usize {
        self.state.lock().map(|s| s.0).unwrap_or(self.min)
    }

    /// A permit from the hard ceiling. The adaptive limit *is* the permit count,
    /// so acquiring is all the gating there is.
    async fn acquire(&self) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
        self.sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())
    }

    fn on_success(&self) {
        let grow = {
            let Ok(mut s) = self.state.lock() else { return };
            let (next, since) = aimd_increase(s.0, s.1, self.max);
            let grow = next - s.0;
            *s = (next, since);
            grow
        };
        if grow > 0 {
            self.sem.add_permits(grow);
        }
    }

    fn on_throttle(&self) {
        let shrink = {
            let Ok(mut s) = self.state.lock() else { return };
            let next = aimd_decrease(s.0, self.min);
            let shrink = s.0 - next;
            *s = (next, 0);
            shrink
        };
        if shrink == 0 {
            return;
        }
        // Retire the difference. `forget_permits` only takes permits that are
        // free right now, so a shrink that outruns the in-flight passes is
        // finished off as they release.
        let sem = self.sem.clone();
        let mut remaining = shrink;
        remaining -= sem.forget_permits(remaining);
        if remaining > 0 {
            tokio::spawn(async move {
                for _ in 0..remaining {
                    match sem.acquire().await {
                        Ok(permit) => permit.forget(),
                        Err(_) => break,
                    }
                }
            });
        }
    }
}

fn system_message(content: String) -> Result<ChatCompletionRequestMessage, String> {
    Ok(ChatCompletionRequestSystemMessageArgs::default()
        .content(content)
        .build()
        .map_err(|e| e.to_string())?
        .into())
}

fn user_message(content: String) -> Result<ChatCompletionRequestMessage, String> {
    Ok(ChatCompletionRequestUserMessageArgs::default()
        .content(content)
        .build()
        .map_err(|e| e.to_string())?
        .into())
}

/// Current backfill progress (for polling UIs).
#[tauri::command]
pub fn backfill_status(state: tauri::State<'_, Backfill>) -> BackfillStatus {
    state.snapshot()
}

/// Ask a running backfill to stop. It finishes the passes already in flight and
/// skips the reduce, leaving staged extractions intact for a later run.
#[tauri::command]
pub fn cancel_backfill(state: tauri::State<'_, Backfill>) {
    state.cancel.store(true, Ordering::SeqCst);
}

/// How many conversations are staged vs. still awaiting extraction.
#[tauri::command]
pub fn memory_extraction_stats(
    db: tauri::State<'_, db::Db>,
) -> Result<db::ExtractionStats, String> {
    db::extraction_stats(&db, DEFAULT_MIN_CHARS)
}

/// Drop staged extractions that have not been folded into memory. Folded rows
/// are kept — they are the watermark that stops an already-extracted
/// conversation being extracted again.
#[tauri::command]
pub fn clear_extractions(db: tauri::State<'_, db::Db>) -> Result<usize, String> {
    db::discard_staged_extractions(&db)
}

/// Start a backfill in the background and return immediately. Progress is
/// reported through the `memory-backfill` event and [`backfill_status`].
#[tauri::command]
pub async fn backfill_memories(
    app: AppHandle,
    min_chars: Option<i64>,
    concurrency: Option<usize>,
) -> Result<BackfillStatus, String> {
    {
        let state = app.state::<Backfill>();
        if state.running.swap(true, Ordering::SeqCst) {
            return Err("a memory backfill is already running".into());
        }
        state.cancel.store(false, Ordering::SeqCst);
        state.total.store(0, Ordering::SeqCst);
        state.done.store(0, Ordering::SeqCst);
        state.failed.store(0, Ordering::SeqCst);
        state.concurrency.store(0, Ordering::SeqCst);
        state.set_last_error("");
        state.set_phase("extracting");
    }
    let initial = app.state::<Backfill>().snapshot();
    let _ = app.emit("memory-backfill", initial.clone());

    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let min = min_chars.unwrap_or(DEFAULT_MIN_CHARS);
        let conc = concurrency.unwrap_or(DEFAULT_CONCURRENCY).max(1);
        let outcome = backfill_inner(&handle, min, conc).await;
        let state = handle.state::<Backfill>();
        match outcome {
            Ok(()) => state.set_phase(if state.cancelled() { "cancelled" } else { "done" }),
            Err(e) => {
                eprintln!("memory backfill: {e}");
                state.set_phase("error");
            }
        }
        state.running.store(false, Ordering::SeqCst);
        let _ = handle.emit("memory-backfill", state.snapshot());
    });

    Ok(initial)
}

async fn backfill_inner(app: &AppHandle, min_chars: i64, concurrency: usize) -> Result<(), String> {
    let pending = {
        let db = app.state::<db::Db>();
        db::conversations_pending_extraction(&db, min_chars)?
    };
    {
        let state = app.state::<Backfill>();
        state.total.store(pending.len(), Ordering::SeqCst);
    }
    let _ = app.emit("memory-backfill", app.state::<Backfill>().snapshot());

    // Map phase: bounded concurrency. Each pass is read-only with respect to
    // memory, so the only shared resource is the SQLite connection, which is
    // locked briefly for reads and never held across an await. Every task is
    // spawned up front and gated on a permit, so `join_next` below reports
    // progress as passes actually finish rather than in one batch at the end.
    let aimd = Arc::new(Aimd::new(concurrency, MIN_CONCURRENCY, MAX_CONCURRENCY));
    let mut set = tokio::task::JoinSet::new();
    for cid in pending {
        let handle = app.clone();
        let aimd = aimd.clone();
        set.spawn(async move {
            if handle.state::<Backfill>().cancelled() {
                return (cid, Ok(()));
            }
            let _permit = match aimd.acquire().await {
                Ok(p) => p,
                Err(_) => return (cid, Err("backfill cancelled".to_string())),
            };
            if handle.state::<Backfill>().cancelled() {
                return (cid, Ok(()));
            }
            let r = with_retry(
                || extract_conversation(&handle, &cid),
                PASS_ATTEMPTS,
                || aimd.on_throttle(),
            )
            .await;
            (cid, r)
        });
    }

    while let Some(joined) = set.join_next().await {
        let state = app.state::<Backfill>();
        match joined {
            Ok((_, Ok(()))) => {
                state.done.fetch_add(1, Ordering::SeqCst);
                // A cancelled task also returns Ok; it must not feed the ramp.
                if !state.cancelled() {
                    aimd.on_success();
                }
            }
            Ok((cid, Err(e))) => {
                eprintln!("memory extraction {cid}: {e}");
                state.failed.fetch_add(1, Ordering::SeqCst);
                state.set_last_error(&e);
                // No `on_throttle` here: `with_retry` already reported every
                // transient failure, including the one that exhausted the
                // budget. Reporting again would double-count the backoff.
            }
            Err(e) => {
                eprintln!("memory extraction task: {e}");
                state.failed.fetch_add(1, Ordering::SeqCst);
            }
        }
        state.concurrency.store(aimd.limit(), Ordering::SeqCst);
        let _ = app.emit("memory-backfill", state.snapshot());
    }

    if app.state::<Backfill>().cancelled() {
        return Ok(());
    }

    app.state::<Backfill>().set_phase("consolidating");
    let _ = app.emit("memory-backfill", app.state::<Backfill>().snapshot());
    consolidate_extractions(app).await
}

/// One extraction pass: transcript in, staged summary out. Read-only with
/// respect to the memory files, which is what makes the map phase safe to run
/// concurrently.
async fn extract_conversation(app: &AppHandle, conversation_id: &str) -> Result<(), String> {
    let cfg = app.state::<ConfigState>().get();
    if cfg.model.trim().is_empty() {
        return Ok(());
    }
    let Some(api_key) = crate::providers::resolve(&cfg.active_provider_id())? else {
        return Ok(());
    };

    let db = app.state::<db::Db>();
    let memory = app.state::<MemoryState>();
    let brave = app.state::<tools::BraveSearch>();
    let shell = app.state::<crate::shell::ShellExecutor>();
    let approvals = app.state::<tools::ApprovalRegistry>();

    let snapshot_max = match db::max_reflectable_index(&db, conversation_id)? {
        Some(idx) => idx,
        None => return Ok(()),
    };
    let history = db::read_messages(&db, conversation_id)?;

    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    messages.push(system_message(EXTRACTION_SYSTEM_PROMPT.to_string())?);
    messages.extend(chat::build_history_messages(&history)?);
    messages.push(system_message(EXTRACTION_INSTRUCTION.to_string())?);

    // No tools: extraction is pure summarization. Dropping them also drops the
    // tool rounds, so the map phase is one request per conversation instead of
    // roughly three.
    let tools_list: Vec<async_openai::types::chat::ChatCompletionTools> = Vec::new();
    let flag = Arc::new(AtomicBool::new(false));
    let sink = NullSink;
    let result = chat::run_tool_loop(
        &cfg,
        &api_key,
        &brave,
        &shell,
        &memory,
        &approvals,
        conversation_id,
        messages,
        &tools_list,
        flag,
        &sink,
        None,
        chat::ToolMode::Extract,
    )
    .await?;

    if result.stop_reason == "aborted" {
        return Err("extraction aborted".to_string());
    }
    if result.stop_reason == "error" {
        return Err(result
            .error
            .unwrap_or_else(|| "extraction failed".to_string()));
    }

    let cost = {
        let pricing = crate::pricing::resolve_for(&db, &cfg, &cfg.model);
        crate::pricing::cost_of_usage(&result.usage, &pricing)
    };
    // Record even an empty payload: the watermark is what stops this
    // conversation being re-extracted on every run.
    let payload = result.text.trim();
    let payload = if is_nothing(payload) { "" } else { payload };
    db::upsert_extraction(
        &db,
        conversation_id,
        snapshot_max,
        &cfg.model,
        Some(&result.usage.to_string()),
        cost,
        payload,
    )?;
    Ok(())
}

/// Reduce phase: fold staged extractions into memory, oldest first, in batches.
/// Serial by construction — this is the only writer.
async fn consolidate_extractions(app: &AppHandle) -> Result<(), String> {
    let extractions = {
        let db = app.state::<db::Db>();
        db::list_extractions(&db)?
    };
    // Payloads with nothing durable need no consolidation. Retire them
    // immediately so they don't linger in the staged count forever.
    let (empty, usable): (Vec<_>, Vec<_>) = extractions
        .into_iter()
        .partition(|e| e.payload.trim().is_empty() || is_nothing(&e.payload));
    if !empty.is_empty() {
        let ids: Vec<String> = empty.iter().map(|e| e.conversation_id.clone()).collect();
        let db = app.state::<db::Db>();
        db::mark_extractions_folded(&db, &ids)?;
    }

    if usable.is_empty() {
        // Nothing left to fold. Folded rows stay put: they are the extraction
        // watermark, and deleting them would make every conversation look
        // pending again.
        return Ok(());
    }

    let batches = plan_batches(app, &usable);
    let total = batches.len();
    let mut failures = 0usize;
    // Resume where a previous run stopped. `list_extractions` already excludes
    // folded rows, so the batches below are only the outstanding work; the fold
    // date is recovered from what was folded before, so "later wins" still holds
    // across a restart.
    let mut memory_through: Option<i64> = {
        let db = app.state::<db::Db>();
        db::folded_through(&db)?
    };

    for (i, batch) in batches.iter().enumerate() {
        if app.state::<Backfill>().cancelled() {
            return Ok(());
        }
        app.state::<Backfill>()
            .set_phase(&format!("consolidating {}/{total}", i + 1));
        let _ = app.emit("memory-backfill", app.state::<Backfill>().snapshot());
        match with_retry(
            || run_reduce_pass(app, batch, memory_through),
            PASS_ATTEMPTS,
            || {},
        )
        .await
        {
            Ok(()) => {
                // Record the fold before moving on, so an interruption resumes at
                // the next batch rather than redoing this one. A crash in the
                // narrow window between the pass and this write re-folds one
                // batch, which the dedup instruction absorbs.
                let ids: Vec<String> = batch.iter().map(|e| e.conversation_id.clone()).collect();
                let db = app.state::<db::Db>();
                db::mark_extractions_folded(&db, &ids)?;
                memory_through = batch.iter().map(|e| e.created_at).max().or(memory_through);
            }
            Err(e) => {
                eprintln!("memory consolidation batch {}: {e}", i + 1);
                failures += 1;
            }
        }
    }

    // Folded rows are kept as the extraction watermark, so a conversation that
    // was extracted is never extracted again — and the ones that *failed* (no
    // row at all) stay pending, which is exactly the set a re-run should retry.
    if failures > 0 {
        return Err(format!("{failures} of {total} consolidation batches failed"));
    }
    Ok(())
}

/// Split the staged summaries into reduce passes. One pass unless the payload
/// would exceed [`CONTEXT_BUDGET`] of the selected model's context window, in
/// which case it is chunked in chronological order.
fn plan_batches(app: &AppHandle, usable: &[db::ExtractionRow]) -> Vec<Vec<db::ExtractionRow>> {
    let budget = {
        let db = app.state::<db::Db>();
        let cfg = app.state::<ConfigState>().get();
        let pricing = crate::pricing::resolve_for(&db, &cfg, &cfg.model);
        // The remaining 20% covers the system prompt, the tool rounds, and the
        // reply.
        (pricing.context_window as f64 * CONTEXT_BUDGET * CHARS_PER_TOKEN) as usize
    };
    chunk_by_budget(usable, budget)
}

/// Chronological chunking by payload size. Pure so it can be tested without an
/// `AppHandle`.
fn chunk_by_budget(
    usable: &[db::ExtractionRow],
    budget: usize,
) -> Vec<Vec<db::ExtractionRow>> {
    let total: usize = usable.iter().map(|e| e.payload.len()).sum();
    if total <= budget {
        return vec![usable.to_vec()];
    }

    let mut out: Vec<Vec<db::ExtractionRow>> = Vec::new();
    let mut cur: Vec<db::ExtractionRow> = Vec::new();
    let mut cur_chars = 0usize;
    for e in usable {
        // Always keep at least one summary per batch, even when a single
        // conversation alone exceeds the budget.
        if !cur.is_empty() && cur_chars + e.payload.len() > budget {
            out.push(std::mem::take(&mut cur));
            cur_chars = 0;
        }
        cur_chars += e.payload.len();
        cur.push(e.clone());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// One reduce pass: a batch of staged summaries in, memory files updated out.
/// Uses the ordinary reflection tool loop, so `write_memory` is available and
/// the memory files are the only thing written.
async fn run_reduce_pass(
    app: &AppHandle,
    batch: &[db::ExtractionRow],
    memory_through: Option<i64>,
) -> Result<(), String> {
    let cfg = app.state::<ConfigState>().get();
    if cfg.model.trim().is_empty() {
        return Ok(());
    }
    let Some(api_key) = crate::providers::resolve(&cfg.active_provider_id())? else {
        return Ok(());
    };

    let db = app.state::<db::Db>();
    let memory = app.state::<MemoryState>();
    let brave = app.state::<tools::BraveSearch>();
    let shell = app.state::<crate::shell::ShellExecutor>();
    let approvals = app.state::<tools::ApprovalRegistry>();

    let before = snapshot_files(&memory);
    let model_label = crate::pricing::cached_model_name(&db, &cfg.base_url, &cfg.model)
        .unwrap_or_else(|| cfg.model.clone());
    let system_prompt = chat::build_system_prompt(
        chat::DEFAULT_SYSTEM_PROMPT,
        &model_label,
        &cfg.preferences,
        &memory,
    );

    let mut body = String::from("Memory extractions from past conversations, oldest first:\n");
    for e in batch {
        body.push_str(&format!(
            "\n### {} ({})\n{}\n",
            e.title,
            format_day(e.created_at),
            e.payload.trim()
        ));
    }

    let messages: Vec<ChatCompletionRequestMessage> = vec![
        system_message(system_prompt)?,
        user_message(body)?,
        system_message(consolidate_instruction(memory_through))?,
    ];

    let tools_list = tools::tool_specs(tools::brave_available());
    let flag = Arc::new(AtomicBool::new(false));
    let sink = NullSink;
    let result = chat::run_tool_loop(
        &cfg,
        &api_key,
        &brave,
        &shell,
        &memory,
        &approvals,
        "memory-backfill",
        messages,
        &tools_list,
        flag,
        &sink,
        None,
        chat::ToolMode::Reflection,
    )
    .await?;

    if result.stop_reason == "aborted" {
        return Err("consolidation aborted".to_string());
    }
    if result.stop_reason == "error" {
        return Err(result
            .error
            .unwrap_or_else(|| "consolidation failed".to_string()));
    }

    let after = snapshot_files(&memory);
    let changed = diff_files(&before, &after);
    let cost = {
        let pricing = crate::pricing::resolve_for(&db, &cfg, &cfg.model);
        crate::pricing::cost_of_usage(&result.usage, &pricing)
    };
    let note = if changed.is_empty() {
        "Memory backfill: nothing to save.".to_string()
    } else {
        format!("Memory backfill: {}", changed.join(", "))
    };
    db::insert_backfill_run(
        &db,
        &cfg.model,
        Some(&result.usage.to_string()),
        cost,
        &changed,
        &note,
    )?;
    Ok(())
}

/// The extraction prompt asks for the literal sentinel `NOTHING` when a
/// conversation holds no durable facts. That is *not* an empty string, so it has
/// to be recognized explicitly — otherwise the reduce is handed a "summary" that
/// reads `NOTHING` and spends tokens deciding what to do with it.
fn is_nothing(payload: &str) -> bool {
    payload
        .trim()
        .trim_matches(&['.', '!', '*', '`', '"', '\''][..])
        .trim()
        .eq_ignore_ascii_case("nothing")
}

/// The extraction pass gets a minimal, task-specific system prompt rather than
/// the live-turn one.
///
/// The live prompt is wrong for this job three ways over: it documents memory
/// tools that extraction does not advertise, it gives routing guidance that
/// belongs to the reduce, and it inlines every core memory file. That is ~1.9k
/// tokens of context irrelevant to "list the durable facts in this transcript",
/// paid once per conversation — and it actively misleads, describing a
/// maintainer's job to a reader.
///
/// Extraction is also deliberately blind to current memory: the map phase
/// recalls, the reduce phase dedupes. Showing the extractor what is already
/// known invites it to *omit* a fact it judges redundant, which is exactly the
/// judgement the reduce is better placed to make.
const EXTRACTION_SYSTEM_PROMPT: &str = "You are extracting durable long-term memory from a \
conversation transcript. You are a reader, not a participant: report only what the transcript \
states about the user, never what you infer or already believe.";

const EXTRACTION_INSTRUCTION: &str = "You are extracting durable memory from the conversation \
above. Do NOT call any tools — reply with text only.\n\n\
Write a compact list of durable facts about the user, one per line, each prefixed with a \
category tag:\n\
[profile] identity, background, location, work, relationships\n\
[preference] how they like things done, tools, styles, dislikes\n\
[goal] things they are working toward\n\
[project] ongoing projects and their state\n\n\
Rules:\n\
- Only stable, durable facts. Ignore transient chit-chat, one-off questions, and anything \
about the assistant.\n\
- Do not invent or infer beyond what is stated.\n\
- Prefer specifics (names, versions, numbers) over generalities.\n\
- At most 12 lines. If the conversation contains nothing durable, reply exactly: NOTHING";

/// The reduce task, plus — when this is not the first pass — the date memory
/// already covers. Memory entries carry no dates of their own, so this is what
/// lets a later pass know its extractions are newer than what is already saved.
fn consolidate_instruction(memory_through: Option<i64>) -> String {
    let mut out = String::from(CONSOLIDATE_INSTRUCTION);
    if let Some(ts) = memory_through {
        out.push_str(&format!(
            "\n\nNote: the memory files already reflect everything up to {}. Every extraction \
             below is from after that date, so where one conflicts with a fact already in \
             memory, the extraction below is the newer one.",
            format_day(ts)
        ));
    }
    out
}

const CONSOLIDATE_INSTRUCTION: &str = "You are the long-term memory maintainer. Above are \
memory extractions from many past conversations, oldest first, each tagged with its date. \
Merge them into the shared Markdown memory files shown in your system prompt.\n\n\
Use `read_memory` to see a file's full contents before rewriting it, and `write_memory` to \
rewrite it.\n\n\
Guidance:\n\
- Save only stable, durable facts (identity, preferences, goals, ongoing projects).\n\
- Route facts to the right file: identity/background → `profile.md`, preferences → \
`preferences.md`, goals → `goals.md`. Create a new file only for a substantial, recurring \
subject (e.g. `project-atlas.md`), never for a one-off fact.\n\
- Deduplicate aggressively: these extractions overlap heavily. Merge duplicates into one \
statement rather than listing the same fact twice.\n\
- When later extractions contradict earlier ones, keep the later fact.\n\
- Keep core files concise. Do not duplicate or contradict the explicit user preferences; do \
not invent facts.\n\
- If nothing is worth remembering, make no tool calls. When done, reply with one short line \
describing what you changed.";

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, chars: usize, at: i64) -> db::ExtractionRow {
        db::ExtractionRow {
            conversation_id: id.into(),
            title: id.into(),
            watermark: 0,
            payload: "x".repeat(chars),
            created_at: at,
        }
    }

    /// One pass is the default: it sees every summary at once, which resolves
    /// contradictions better than a fold.
    #[test]
    fn chunk_by_budget_keeps_a_single_batch_when_it_fits() {
        let rows = vec![row("a", 100, 1), row("b", 100, 2)];
        let batches = chunk_by_budget(&rows, 1000);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 2);
    }

    #[test]
    fn chunk_by_budget_splits_in_chronological_order() {
        let rows = vec![row("a", 100, 1), row("b", 100, 2), row("c", 100, 3)];
        let batches = chunk_by_budget(&rows, 250);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 2);
        assert_eq!(batches[1].len(), 1);
        assert_eq!(batches[0][0].conversation_id, "a");
        assert_eq!(batches[1][0].conversation_id, "c");
    }

    #[test]
    fn chunk_by_budget_gives_an_oversized_summary_its_own_batch() {
        let rows = vec![row("big", 5000, 1), row("small", 10, 2)];
        let batches = chunk_by_budget(&rows, 100);
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0][0].conversation_id, "big");
        assert_eq!(batches[1][0].conversation_id, "small");
    }

    /// Memory entries carry no dates, so the fold date is the only thing telling
    /// a later pass that its extractions are newer than what is already saved.
    #[test]
    fn consolidate_instruction_carries_the_fold_date_only_after_the_first_pass() {
        assert!(!consolidate_instruction(None).contains("already reflect"));
        let later = consolidate_instruction(Some(1_700_000_000_000));
        assert!(later.contains("already reflect everything up to"));
        assert!(later.contains(&format_day(1_700_000_000_000)));
    }

    /// Fireworks enforces adaptive TPM limits, so a cold burst of extraction
    /// passes draws 429s that clear on their own. Those must be retried; a 400
    /// or an abort must not be.
    #[test]
    fn transient_provider_errors_are_retried_and_permanent_ones_are_not() {
        assert!(is_transient(
            "create stream: HTTP 429 Too Many Requests: rate limit exceeded"
        ));
        assert!(is_transient("create stream: HTTP 503 Service Unavailable"));
        assert!(is_transient("stream error: operation timed out"));
        assert!(is_transient("provider error: overloaded"));

        assert!(!is_transient("create stream: HTTP 400 Bad Request: bad model"));
        assert!(!is_transient("create stream: HTTP 401 Unauthorized"));
        assert!(!is_transient("extraction aborted"));
        assert!(!is_transient("consolidation aborted"));
    }

    #[test]
    fn the_nothing_sentinel_is_recognized_but_real_facts_are_not() {
        assert!(is_nothing("NOTHING"));
        assert!(is_nothing("  nothing  "));
        assert!(is_nothing("Nothing."));
        assert!(is_nothing("**NOTHING**"));

        assert!(!is_nothing(""));
        assert!(!is_nothing("[profile] nothing much, really"));
        assert!(!is_nothing("[goal] finish the importer"));
        // A real fact that merely mentions the word.
        assert!(!is_nothing("[preference] says NOTHING is a bad sentinel"));
    }

    /// Additive increase in round-trips, not requests: growing per request would
    /// jump from 8 to 700 in seconds.
    #[test]
    fn aimd_grows_one_permit_per_full_window() {
        let (mut limit, mut since) = (8usize, 0usize);
        for _ in 0..8 {
            (limit, since) = aimd_increase(limit, since, 64);
        }
        assert_eq!((limit, since), (9, 0), "8 clean completions buy one permit");

        // A partial window does not grow.
        assert_eq!(aimd_increase(9, 3, 64), (9, 4));

        // The ceiling holds.
        assert_eq!(aimd_increase(64, 63, 64), (64, 63));
    }

    /// Back off hard, recover gently — the asymmetry is the whole point.
    #[test]
    fn aimd_halves_on_throttle_and_recovers_slowly() {
        assert_eq!(aimd_decrease(64, 1), 32);
        assert_eq!(aimd_decrease(9, 1), 4);
        assert_eq!(aimd_decrease(2, 1), 1);
        // Floored, so a throttled run still makes progress.
        assert_eq!(aimd_decrease(1, 1), 1);

        let (mut limit, mut since) = (aimd_decrease(32, 1), 0usize);
        assert_eq!(limit, 16);
        for _ in 0..16 {
            (limit, since) = aimd_increase(limit, since, 64);
        }
        assert_eq!(limit, 17, "a full window at the new limit earns one back");
    }

    #[test]
    fn diff_detects_changes_additions_and_removals() {
        let mut before = HashMap::new();
        before.insert("profile.md".to_string(), "a".to_string());
        before.insert("notes.md".to_string(), "b".to_string());
        let mut after = HashMap::new();
        after.insert("profile.md".to_string(), "changed".to_string());
        after.insert("project.md".to_string(), "new".to_string());
        let changed = diff_files(&before, &after);
        assert_eq!(changed, vec!["notes.md", "profile.md", "project.md"]);
    }

    #[test]
    fn diff_ignores_empty_new_files() {
        let before = HashMap::new();
        let mut after = HashMap::new();
        after.insert("profile.md".to_string(), "  \n".to_string());
        assert!(diff_files(&before, &after).is_empty());
    }

    #[test]
    fn reflection_tail_carries_instruction_and_recent_notes() {
        let recent = vec![
            db::ReflectionRecord {
                note: "Memory consolidated: profile.md".into(),
                created_at: 0,
            },
            db::ReflectionRecord {
                note: "Memory consolidation: nothing to save.".into(),
                created_at: 0,
            },
        ];
        let tail = reflection_tail(&recent);
        assert!(tail.contains("memory maintainer"));
        assert!(tail.contains("Review the conversation above"));
        assert!(tail.contains("Recent consolidation activity"));
        assert!(tail.contains("Memory consolidated: profile.md"));
        assert!(tail.contains("nothing to save."));
        assert!(tail.contains("1970-01-01"));
    }

    #[test]
    fn reflection_tail_omits_history_when_empty() {
        let tail = reflection_tail(&[]);
        assert!(tail.contains("Review the conversation above"));
        assert!(!tail.contains("Recent consolidation activity"));
    }

    #[test]
    fn format_day_matches_known_dates() {
        assert_eq!(format_day(0), "1970-01-01");
        // 2026-01-01T00:00:00Z
        assert_eq!(format_day(1_767_225_600_000), "2026-01-01");
    }
}
