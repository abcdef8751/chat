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

    let history = db::read_messages(&db, conversation_id)?;
    let before = snapshot_files(&memory);

    let mut messages: Vec<ChatCompletionRequestMessage> = Vec::new();
    messages.push(
        ChatCompletionRequestSystemMessageArgs::default()
            .content(reflection_system_prompt(&memory))
            .build()
            .map_err(|e| e.to_string())?
            .into(),
    );
    messages.extend(chat::build_history_messages(&history)?);
    messages.push(chat::user_request_message(REFLECTION_INSTRUCTION, &[])?);

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

const REFLECTION_INSTRUCTION: &str = "Review the conversation above and update the shared \
long-term memory so durable facts are preserved.\n\n\
Guidance:\n\
- Save only stable, durable facts (identity, preferences, goals, ongoing projects, and \
notable details). Ignore transient chit-chat and one-off questions.\n\
- Route facts to the right file: identity/background → `profile.md`, preferences → \
`preferences.md`, goals → `goals.md`. Create a new file only for a substantial, recurring \
subject (e.g. `project-atlas.md`), never for a one-off fact.\n\
- Prefer the existing files listed above. Use `write_memory` to rewrite a file when merging \
or deduplicating; use `save_memory` only to append a brand-new fact.\n\
- Keep core files concise. Do not duplicate or contradict the explicit user preferences; do \
not invent facts.\n\
- If nothing is worth remembering, make no tool calls. When done, reply with one short line \
describing what you changed, or \"nothing to save\".";

/// System prompt for the reflection pass: instructions plus a full dump of the
/// current memory (the pass needs to see everything to curate it).
fn reflection_system_prompt(memory: &MemoryState) -> String {
    let mut prompt = String::from(
        "You maintain the long-term memory of a personal AI chat assistant. The memory is a set \
         of plain Markdown files shared across all conversations.\n\nExplicit user preferences \
         are set by the user in Settings and are authoritative: never copy them into memory \
         files, and never record anything that contradicts them.\n",
    );
    prompt.push_str("\nCurrent memory files:\n");
    for name in memory.file_names() {
        let content = memory.read(&name).unwrap_or_default();
        let trimmed = content.trim();
        prompt.push_str(&format!("\n### {name}\n"));
        if trimmed.is_empty() {
            prompt.push_str("_(empty)_\n");
        } else {
            prompt.push_str(trimmed);
            prompt.push('\n');
        }
    }
    prompt
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

    fn temp_memory(name: &str) -> MemoryState {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-reflect-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        MemoryState::load(p).unwrap()
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
    fn reflection_prompt_includes_all_files() {
        let memory = temp_memory("prompt");
        memory.write("profile.md", "Name is Ravi.").unwrap();
        memory.write("project-x.md", "Uses Rust.").unwrap();
        let prompt = reflection_system_prompt(&memory);
        assert!(prompt.contains("### profile.md"));
        assert!(prompt.contains("Name is Ravi."));
        assert!(prompt.contains("### project-x.md"));
        assert!(prompt.contains("Uses Rust."));
        assert!(prompt.contains("Explicit user preferences"));
    }
}
