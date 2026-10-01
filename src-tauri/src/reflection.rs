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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_openai::types::chat::{ChatCompletionRequestMessage, ChatCompletionRequestSystemMessageArgs};
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
    let Some(api_key) = crate::secrets::get()? else {
        return Ok(());
    };

    let db = app.state::<db::Db>();
    let memory = app.state::<MemoryState>();
    let mcp = app.state::<tools::McpClient>();
    let shell = app.state::<crate::shell::ShellRegistry>();
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

    let conversation = db::get_conversation(&db, conversation_id)?
        .ok_or_else(|| format!("conversation not found: {conversation_id}"))?;
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
    let model_label = crate::pricing::cached_model_name(&db, &cfg.base_url, &cfg.model)
        .unwrap_or_else(|| cfg.model.clone());
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

    let tools_list = tools::reflection_tool_specs();
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
        &cfg,
        &api_key,
        &mcp,
        &shell,
        &memory,
        &approvals,
        conversation_id,
        messages,
        &tools_list,
        flag,
        &sink,
        None,
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
        let pricing = crate::pricing::resolve_for(&db, &cfg, &cfg.model);
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
        &cfg.model,
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
            Some(cfg.model.clone()),
            Some(cfg.base_url.clone()),
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

#[cfg(test)]
mod tests {
    use super::*;

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
