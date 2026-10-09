//! Import conversations from an Anthropic-format export.
//!
//! The export is a single `conversations.json` holding an array of
//! conversations. (Anthropic's bulk/directory export is not supported yet.)
//!
//! Two things make an import safe to run over a large archive:
//!
//! - **Timestamps are preserved.** The sidebar sorts by `updated_at DESC`, so
//!   writing the export's own dates is what makes imported chats sort into place
//!   instead of all landing at the top. Nothing here goes through
//!   [`db::insert_message_full`], which would stamp `now` and re-title the chat.
//! - **Imported chats are excluded from the idle scheduler.** Their
//!   `last_reflected_index` is set to their newest message, so the 60s-tick
//!   reflection sweep never wanders into an archive. The memory backfill uses a
//!   separate watermark (`memory_extractions`), so they stay available to it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Manager;

use crate::db;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub conversations: usize,
    pub messages: usize,
    /// Conversations already present (re-import) or with no usable id/messages.
    pub skipped: usize,
}

#[derive(Deserialize)]
struct RawConversation {
    uuid: Option<String>,
    name: Option<String>,
    model: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    #[serde(default)]
    chat_messages: Vec<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    sender: Option<String>,
    created_at: Option<String>,
    #[serde(default)]
    content: Vec<RawBlock>,
    #[serde(default)]
    attachments: Vec<RawFile>,
    #[serde(default)]
    files: Vec<RawFile>,
}

/// The export carries only file *references* — a uuid and a name — never bytes.
#[derive(Deserialize)]
struct RawFile {
    file_name: Option<String>,
}

#[derive(Deserialize)]
struct RawBlock {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    thinking: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
    tool_use_id: Option<String>,
    content: Option<Value>,
    is_error: Option<bool>,
}

/// One row to write. The export flattens a whole tool round-trip into a single
/// assistant message, so one raw message can expand into several rows.
enum Row {
    Text {
        role: &'static str,
        content: String,
        thinking: Option<String>,
    },
    ToolCalls {
        calls: Vec<Call>,
        thinking: Option<String>,
    },
    ToolResult {
        call_id: String,
        name: String,
        error: bool,
        output: String,
    },
}

struct Call {
    id: String,
    name: String,
    arguments: Value,
}

#[tauri::command]
pub async fn import_conversations(
    app: tauri::AppHandle,
    path: String,
) -> Result<ImportReport, String> {
    // A second click on the picker must not start a concurrent import: the
    // inserts are idempotent, but two runs interleaving would make the reported
    // counts meaningless and double the work.
    let state = app.state::<ImportState>();
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("an import is already running".into());
    }
    let result = import_inner(&app, &path);
    app.state::<ImportState>()
        .running
        .store(false, Ordering::SeqCst);
    result
}

/// Re-entrancy guard for [`import_conversations`].
#[derive(Default)]
pub struct ImportState {
    running: AtomicBool,
}

fn import_inner(app: &tauri::AppHandle, path: &str) -> Result<ImportReport, String> {
    let db = app.state::<db::Db>();
    let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
    let conversations = parse_conversations(&text).map_err(|e| format!("parse {path}: {e}"))?;
    let mut report = ImportReport::default();

    // One import run = one batch. Stamping every conversation with the same id
    // lets the backfill scope itself to the newest run instead of the archive.
    let import_batch = db::next_import_batch(&db)?;

    for conversation in &conversations {
        import_one(&db, conversation, import_batch, &mut report)?;
    }

    // Only after everything is in: exclude the archive from the idle sweep.
    db::mark_imported_reflected(&db)?;
    Ok(report)
}

/// Accepts either an array of conversations or a single conversation object.
fn parse_conversations(text: &str) -> Result<Vec<RawConversation>, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    match value {
        Value::Array(items) => items
            .into_iter()
            .map(|v| serde_json::from_value(v).map_err(|e| e.to_string()))
            .collect(),
        Value::Object(_) => Ok(vec![
            serde_json::from_value(value).map_err(|e| e.to_string())?
        ]),
        _ => Err("expected a conversation object or an array of them".into()),
    }
}

fn import_one(
    db: &db::Db,
    conversation: &RawConversation,
    import_batch: i64,
    report: &mut ImportReport,
) -> Result<(), String> {
    let Some(id) = conversation.uuid.clone().filter(|s| !s.is_empty()) else {
        report.skipped += 1;
        return Ok(());
    };

    let created = conversation
        .created_at
        .as_deref()
        .and_then(parse_iso8601)
        .unwrap_or_else(db::now_ms);
    let updated = conversation
        .updated_at
        .as_deref()
        .and_then(parse_iso8601)
        .unwrap_or(created);
    let title = conversation
        .name
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Imported chat".to_string());

    // Build every row first, then hand the whole conversation to the DB in one
    // transaction.
    let mut rows: Vec<db::ImportedMessage> = Vec::new();
    for message in &conversation.chat_messages {
        let at = message
            .created_at
            .as_deref()
            .and_then(parse_iso8601)
            .unwrap_or(created);
        for row in rows_for_message(message) {
            let (role, content, thinking) = match row {
                Row::Text {
                    role,
                    content,
                    thinking,
                } => (role, content, thinking),
                Row::ToolCalls { calls, thinking } => {
                    ("assistant", encode_tool_calls(&calls), thinking)
                }
                Row::ToolResult {
                    call_id,
                    name,
                    error,
                    output,
                } => (
                    "tool",
                    encode_tool_result(&call_id, &name, error, &output),
                    None,
                ),
            };
            let index = rows.len() as i64;
            rows.push(db::ImportedMessage {
                id: format!("{id}:{index}"),
                role: role.to_string(),
                index,
                content,
                thinking,
                created_at: at,
            });
        }
    }

    // A conversation whose messages carry no content imports as an empty shell
    // that can never be extracted from — the export has plenty of these, where
    // every message's `content` array is empty. Skip rather than create dead
    // weight in the sidebar and a permanent no-op in the backfill.
    if rows.is_empty() {
        report.skipped += 1;
        return Ok(());
    }

    if !db::insert_imported_conversation(
        db,
        &id,
        &title,
        conversation.model.as_deref(),
        created,
        updated,
        import_batch,
        &rows,
    )? {
        report.skipped += 1;
        return Ok(());
    }
    report.conversations += 1;
    report.messages += rows.len();
    Ok(())
}

/// Expand one raw message into the app's native row shapes.
///
/// The export interleaves text, thinking, `tool_use` and `tool_result` blocks
/// inside a single assistant message, whereas this app stores a tool round as
/// `assistant(text)` → `assistant({"tool_calls":…})` → `tool({…})`. Splitting on
/// the block boundaries reproduces that, so the activity timeline renders
/// imported tool calls the same way it renders live ones.
fn rows_for_message(message: &RawMessage) -> Vec<Row> {
    let role = match message.sender.as_deref() {
        Some("assistant") => "assistant",
        _ => "user",
    };
    let mut rows: Vec<Row> = Vec::new();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut calls: Vec<Call> = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();

    for block in &message.content {
        match block.kind.as_deref() {
            Some("text") => {
                if let Some(t) = &block.text {
                    text.push_str(t);
                }
            }
            Some("thinking") => {
                if let Some(t) = &block.thinking {
                    thinking.push_str(t);
                }
            }
            Some("tool_use") => {
                if !text.trim().is_empty() || !thinking.trim().is_empty() {
                    rows.push(Row::Text {
                        role,
                        content: std::mem::take(&mut text),
                        thinking: take_thinking(&mut thinking),
                    });
                }
                let call_id = block.id.clone().unwrap_or_default();
                let name = block.name.clone().unwrap_or_else(|| "tool".to_string());
                names.insert(call_id.clone(), name.clone());
                calls.push(Call {
                    id: call_id,
                    name,
                    arguments: block.input.clone().unwrap_or(Value::Null),
                });
            }
            Some("tool_result") => {
                if !calls.is_empty() {
                    rows.push(Row::ToolCalls {
                        calls: std::mem::take(&mut calls),
                        thinking: take_thinking(&mut thinking),
                    });
                }
                let call_id = block.tool_use_id.clone().unwrap_or_default();
                rows.push(Row::ToolResult {
                    name: names
                        .get(&call_id)
                        .cloned()
                        .unwrap_or_else(|| "tool".to_string()),
                    call_id,
                    error: block.is_error.unwrap_or(false),
                    output: flatten_tool_content(block.content.as_ref()),
                });
            }
            // `injected_prompt_block`, `token_budget`, `flag`, … carry no
            // transcript content.
            _ => {}
        }
    }

    if !calls.is_empty() {
        rows.push(Row::ToolCalls {
            calls,
            thinking: take_thinking(&mut thinking),
        });
    }

    // The export references attachments but does not contain them: the entries
    // are `{file_uuid, file_name}` and the accompanying zip holds nothing but
    // the JSON. Record that something was attached so the transcript is not
    // silently missing context — and so the model is not left wondering what an
    // "as you can see above" refers to.
    let mut names: Vec<String> = message
        .attachments
        .iter()
        .chain(message.files.iter())
        .filter_map(|f| f.file_name.as_deref())
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .collect();
    names.dedup();
    if !names.is_empty() {
        let shown = names.len().min(5);
        let mut list = names[..shown].join(", ");
        if names.len() > shown {
            list.push_str(&format!(" and {} more", names.len() - shown));
        }
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&format!(
            "[attached: {list} — contents are not included in the export]"
        ));
    }

    if !text.trim().is_empty() || !thinking.trim().is_empty() {
        rows.push(Row::Text {
            role,
            content: text,
            thinking: take_thinking(&mut thinking),
        });
    }
    rows
}

fn take_thinking(buf: &mut String) -> Option<String> {
    let trimmed = buf.trim();
    let out = (!trimmed.is_empty()).then(|| trimmed.to_string());
    buf.clear();
    out
}

/// A `tool_result` block's `content` is either a plain string or an array of
/// text blocks.
fn flatten_tool_content(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

/// Mirrors `chat::encode_tool_calls` so the frontend parsers read imported rows
/// unchanged.
fn encode_tool_calls(calls: &[Call]) -> String {
    let arr: Vec<Value> = calls
        .iter()
        .map(|c| serde_json::json!({"id": c.id, "name": c.name, "arguments": c.arguments}))
        .collect();
    serde_json::json!({ "tool_calls": arr }).to_string()
}

/// Mirrors `chat::encode_tool_result`.
fn encode_tool_result(call_id: &str, name: &str, error: bool, output: &str) -> String {
    serde_json::json!({
        "tool_call_id": call_id,
        "name": name,
        "error": error,
        "output": output,
    })
    .to_string()
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`). The inverse of `reflection::format_day`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse an ISO-8601 / RFC-3339 timestamp to epoch milliseconds. Handles the
/// shapes Anthropic's exporter emits: `…Z`, fractional seconds, and `±HH:MM`
/// offsets. Returns `None` rather than guessing.
fn parse_iso8601(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    let second: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut rest = s.get(19..).unwrap_or("");
    let mut millis: i64 = 0;
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits: String = fraction
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        rest = fraction.get(digits.len()..).unwrap_or("");
        let mut padded = digits;
        padded.truncate(3);
        while padded.len() < 3 {
            padded.push('0');
        }
        millis = padded.parse().unwrap_or(0);
    }

    let offset_minutes = if rest.is_empty() || rest.starts_with('Z') {
        0
    } else {
        let sign = if rest.starts_with('-') { -1 } else { 1 };
        let h: i64 = rest.get(1..3).and_then(|v| v.parse().ok()).unwrap_or(0);
        let m: i64 = rest.get(4..6).and_then(|v| v.parse().ok()).unwrap_or(0);
        sign * (h * 60 + m)
    };

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_minutes * 60;
    Some(secs * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn message(json: Value) -> RawMessage {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn parses_the_timestamp_shapes_the_exporter_emits() {
        assert_eq!(parse_iso8601("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601("1970-01-01T00:00:01Z"), Some(1_000));
        assert_eq!(parse_iso8601("1970-01-01T00:00:00.500Z"), Some(500));
        // Sub-millisecond precision is truncated, not rounded.
        assert_eq!(parse_iso8601("1970-01-01T00:00:00.072863Z"), Some(72));
        // Offsets are applied.
        assert_eq!(parse_iso8601("1970-01-01T01:00:00+01:00"), Some(0));
        assert_eq!(parse_iso8601("1969-12-31T23:00:00-01:00"), Some(0));
        // A real value from the export.
        assert_eq!(
            parse_iso8601("2026-09-17T11:57:49.072863Z"),
            Some(1_789_646_269_072)
        );
        assert_eq!(parse_iso8601("not a date"), None);
        assert_eq!(parse_iso8601(""), None);
    }

    #[test]
    fn accepts_an_array_or_a_single_conversation() {
        let one = r#"{"uuid":"a","chat_messages":[]}"#;
        assert_eq!(parse_conversations(one).unwrap().len(), 1);
        let many = r#"[{"uuid":"a","chat_messages":[]},{"uuid":"b","chat_messages":[]}]"#;
        assert_eq!(parse_conversations(many).unwrap().len(), 2);
        assert!(parse_conversations("42").is_err());
    }

    #[test]
    fn plain_text_message_becomes_one_row() {
        let m = message(serde_json::json!({
            "sender": "human",
            "content": [{"type": "text", "text": "hello"}]
        }));
        let rows = rows_for_message(&m);
        assert_eq!(rows.len(), 1);
        match &rows[0] {
            Row::Text { role, content, .. } => {
                assert_eq!(*role, "user");
                assert_eq!(content, "hello");
            }
            _ => panic!("expected a text row"),
        }
    }

    /// The export flattens a tool round-trip into one assistant message; it has
    /// to come back out as the three rows the timeline expects.
    #[test]
    fn a_tool_round_trip_expands_into_preamble_calls_and_result() {
        let m = message(serde_json::json!({
            "sender": "assistant",
            "content": [
                {"type": "thinking", "thinking": "let me look"},
                {"type": "text", "text": "Searching."},
                {"type": "tool_use", "id": "t1", "name": "image_search",
                 "input": {"query": "undercut"}},
                {"type": "tool_result", "tool_use_id": "t1",
                 "content": [{"type": "text", "text": "3 results"}]},
                {"type": "text", "text": "Here you go."}
            ]
        }));
        let rows = rows_for_message(&m);
        assert_eq!(rows.len(), 4);

        match &rows[0] {
            Row::Text {
                content, thinking, ..
            } => {
                assert_eq!(content, "Searching.");
                assert_eq!(thinking.as_deref(), Some("let me look"));
            }
            _ => panic!("expected the preamble text row"),
        }
        match &rows[1] {
            Row::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].id, "t1");
                assert_eq!(calls[0].name, "image_search");
            }
            _ => panic!("expected a tool-calls row"),
        }
        match &rows[2] {
            Row::ToolResult {
                call_id,
                name,
                error,
                output,
            } => {
                assert_eq!(call_id, "t1");
                // The name is recovered from the matching tool_use block.
                assert_eq!(name, "image_search");
                assert!(!error);
                assert_eq!(output, "3 results");
            }
            _ => panic!("expected a tool-result row"),
        }
        match &rows[3] {
            Row::Text { content, .. } => assert_eq!(content, "Here you go."),
            _ => panic!("expected the trailing text row"),
        }
    }

    #[test]
    fn unknown_block_types_are_ignored() {
        let m = message(serde_json::json!({
            "sender": "assistant",
            "content": [
                {"type": "injected_prompt_block", "text": "ignore me"},
                {"type": "token_budget", "text": "ignore me too"},
                {"type": "text", "text": "kept"}
            ]
        }));
        let rows = rows_for_message(&m);
        assert_eq!(rows.len(), 1);
        match &rows[0] {
            Row::Text { content, .. } => assert_eq!(content, "kept"),
            _ => panic!("expected a text row"),
        }
    }

    /// A conversation is written atomically: either it and all its rows land, or
    /// nothing does. This matters because a re-import skips conversations that
    /// already exist, so a half-written chat would stay half-written forever.
    #[test]
    fn a_conversation_is_written_atomically_and_re_import_is_a_no_op() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-import-tx-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let db = db::Db(std::sync::Arc::new(std::sync::Mutex::new(db::open(&p).unwrap())));

        let conversation: RawConversation = serde_json::from_value(serde_json::json!({
            "uuid": "conv-1",
            "name": "Test",
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z",
            "chat_messages": [
                {"sender": "human", "content": [{"type": "text", "text": "hi"}]},
                {"sender": "assistant", "content": [{"type": "text", "text": "hello"}]}
            ]
        }))
        .unwrap();

        let mut report = ImportReport::default();
        import_one(&db, &conversation, 1, &mut report).unwrap();
        assert_eq!(report.conversations, 1);
        assert_eq!(report.messages, 2);

        {
            let conn = db.0.lock().unwrap();
            let rows: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE conversation_id = 'conv-1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(rows, 2);
            let (created, updated, imported): (i64, i64, i64) = conn
                .query_row(
                    "SELECT created_at, updated_at, imported FROM conversations WHERE id = 'conv-1'",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .unwrap();
            // The export's own timestamps, not `now`.
            assert_eq!(created, 1_767_225_600_000);
            assert_eq!(updated, 1_767_312_000_000);
            assert_eq!(imported, 1);
        }

        // Re-importing skips it entirely — no duplicate rows.
        let mut again = ImportReport::default();
        import_one(&db, &conversation, 1, &mut again).unwrap();
        assert_eq!(again.conversations, 0);
        assert_eq!(again.messages, 0);
        assert_eq!(again.skipped, 1);
        {
            let conn = db.0.lock().unwrap();
            let rows: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM messages WHERE conversation_id = 'conv-1'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(rows, 2, "re-import must not duplicate rows");
        }

        let _ = std::fs::remove_file(&p);
    }

    /// Opt-in end-to-end run against a real export:
    /// `cargo test --lib -- --ignored imports_a_real_export`
    /// (override the path with `PI_IMPORT_PATH`).
    ///
    /// Set `PI_IMPORT_DB` to run against an existing database instead of a fresh
    /// temp one — useful for checking what a re-import does to real data.
    #[test]
    #[ignore = "reads a real export from disk"]
    fn imports_a_real_export() {
        let path = std::env::var("PI_IMPORT_PATH")
            .unwrap_or_else(|_| "/home/rp/Downloads/conversations.json".to_string());
        let text = std::fs::read_to_string(&path).expect("read export");
        let conversations = parse_conversations(&text).expect("parse export");

        let existing = std::env::var("PI_IMPORT_DB").ok();
        let mut db_path = std::env::temp_dir();
        db_path.push(format!("pi-chat-import-{}.sqlite", std::process::id()));
        let db_path = match &existing {
            Some(p) => PathBuf::from(p),
            None => {
                let _ = std::fs::remove_file(&db_path);
                db_path
            }
        };
        let db = db::Db(std::sync::Arc::new(std::sync::Mutex::new(db::open(&db_path).unwrap())));

        let import_batch = db::next_import_batch(&db).unwrap();
        let before = counts(&db);
        let mut report = ImportReport::default();
        for conversation in &conversations {
            import_one(&db, conversation, import_batch, &mut report).unwrap();
        }
        db::mark_imported_reflected(&db).unwrap();
        let after = counts(&db);
        println!("before: {before:?}");
        println!("report: {report:#?}");
        println!("after:  {after:?}");

        if existing.is_some() {
            // Re-importing into a populated database must change nothing at all.
            assert_eq!(report.conversations, 0, "re-import added conversations");
            assert_eq!(report.messages, 0, "re-import added messages");
            assert_eq!(before, after, "re-import changed the database");
            return;
        }

        assert!(report.conversations > 1000, "{report:#?}");
        assert!(report.messages > 40_000, "{report:#?}");

        // The whole point: an archive must never enter the idle sweep...
        let due = db::conversations_due_for_reflection(&db, db::now_ms()).unwrap();
        assert!(due.is_empty(), "imported chats were scheduled: {due:?}");

        // ...but stays available to the backfill.
        let pending =
            db::conversations_pending_extraction(&db, crate::reflection::DEFAULT_MIN_CHARS).unwrap();
        assert!(!pending.is_empty());
        println!("backfill would extract {} conversations", pending.len());

        // Re-importing is a no-op, not a clobber.
        let mut again = ImportReport::default();
        for conversation in &conversations {
            import_one(&db, conversation, import_batch, &mut again).unwrap();
        }
        assert_eq!(again.conversations, 0);
        assert_eq!(again.messages, 0);

        let _ = std::fs::remove_file(&db_path);
    }

    /// `(conversations, messages)` — enough to prove a re-import changed nothing.
    fn counts(db: &db::Db) -> (i64, i64) {
        let conn = db.0.lock().unwrap();
        let c = conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        let m = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        (c, m)
    }

    #[test]
    fn attachments_are_marked_but_not_invented() {
        let m = message(serde_json::json!({
            "sender": "human",
            "content": [{"type": "text", "text": "look at this"}],
            "files": [
                {"file_uuid": "u1", "file_name": "shot.png"},
                {"file_uuid": "u2", "file_name": null}
            ]
        }));
        let rows = rows_for_message(&m);
        assert_eq!(rows.len(), 1);
        match &rows[0] {
            Row::Text { content, .. } => {
                assert!(content.starts_with("look at this"));
                assert!(content.contains("[attached: shot.png"));
                assert!(content.contains("not included in the export"));
                // A nameless reference contributes nothing.
                assert!(!content.contains("u2"));
            }
            _ => panic!("expected a text row"),
        }
    }

    #[test]
    fn a_message_with_only_an_attachment_still_produces_a_row() {
        let m = message(serde_json::json!({
            "sender": "human",
            "content": [],
            "files": [{"file_uuid": "u1", "file_name": "shot.png"}]
        }));
        let rows = rows_for_message(&m);
        assert_eq!(rows.len(), 1);
        match &rows[0] {
            Row::Text { content, .. } => assert!(content.contains("shot.png")),
            _ => panic!("expected a text row"),
        }
    }

    #[test]
    fn encoded_rows_match_the_shapes_the_frontend_parses() {
        let calls = vec![Call {
            id: "t1".into(),
            name: "image_search".into(),
            arguments: serde_json::json!({"query": "x"}),
        }];
        let encoded: Value = serde_json::from_str(&encode_tool_calls(&calls)).unwrap();
        assert_eq!(encoded["tool_calls"][0]["id"], "t1");
        assert_eq!(encoded["tool_calls"][0]["name"], "image_search");
        assert_eq!(encoded["tool_calls"][0]["arguments"]["query"], "x");

        let encoded: Value =
            serde_json::from_str(&encode_tool_result("t1", "image_search", true, "boom")).unwrap();
        assert_eq!(encoded["tool_call_id"], "t1");
        assert_eq!(encoded["name"], "image_search");
        assert_eq!(encoded["error"], true);
        assert_eq!(encoded["output"], "boom");
    }
}
