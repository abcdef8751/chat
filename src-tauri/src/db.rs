use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::Serialize;
use uuid::Uuid;

/// Shared database state installed into Tauri via `manage`.
pub struct Db(pub Mutex<Connection>);

/// SQLite schema (conversations, messages, model_prices, memory_reflections).
const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS conversations (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  model TEXT,
  system_prompt TEXT,
  compaction_summary TEXT,
  last_reflected_index INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  role TEXT NOT NULL,
  "index" INTEGER NOT NULL,
  content TEXT NOT NULL,
  model TEXT,
  provider TEXT,
  thinking_level TEXT,
  thinking TEXT,
  usage TEXT,
  stop_reason TEXT,
  attachments TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS model_prices (
  provider TEXT NOT NULL,
  model_id TEXT NOT NULL,
  input_per_million REAL,
  output_per_million REAL,
  cache_read_per_million REAL,
  cache_write_per_million REAL,
  context_window INTEGER,
  name TEXT,
  reasoning INTEGER,
  reasoning_options TEXT,
  fetched_at INTEGER,
  PRIMARY KEY (provider, model_id)
);

CREATE TABLE IF NOT EXISTS memory_reflections (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
  model TEXT,
  usage TEXT,
  cost REAL,
  files TEXT,
  note TEXT,
  created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_messages_conv ON messages(conversation_id, "index");
CREATE INDEX IF NOT EXISTS idx_conversations_updated ON conversations(updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_reflections_conv ON memory_reflections(conversation_id);
"#;

/// Open (or create) the SQLite database at `path` and run the schema migration.
pub fn open(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| format!("open db: {e}"))?;
    conn.execute_batch(SCHEMA).map_err(|e| format!("migrate: {e}"))?;
    // Older databases predate the `messages.thinking` column; add it in place.
    let _ = conn.execute("ALTER TABLE messages ADD COLUMN thinking TEXT", []);
    // Older databases predate `model_prices.context_window`.
    let _ = conn.execute("ALTER TABLE model_prices ADD COLUMN context_window INTEGER", []);
    // Older databases predate the models.dev metadata cached alongside prices.
    let _ = conn.execute("ALTER TABLE model_prices ADD COLUMN name TEXT", []);
    let _ = conn.execute("ALTER TABLE model_prices ADD COLUMN reasoning INTEGER", []);
    let _ = conn.execute("ALTER TABLE model_prices ADD COLUMN reasoning_options TEXT", []);
    // Older databases predate message file attachments.
    let _ = conn.execute("ALTER TABLE messages ADD COLUMN attachments TEXT", []);
    // Older databases predate the per-conversation memory reflection watermark.
    let _ = conn.execute(
        "ALTER TABLE conversations ADD COLUMN last_reflected_index INTEGER",
        [],
    );
    Ok(conn)
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Conversation {
    pub id: String,
    pub title: String,
    pub model: Option<String>,
    pub system_prompt: Option<String>,
    pub compaction_summary: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize)]
pub struct MessageRow {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub index: i64,
    pub content: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub thinking_level: Option<String>,
    pub thinking: Option<String>,
    pub usage: Option<String>,
    pub stop_reason: Option<String>,
    pub attachments: Option<String>,
    pub created_at: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Sidebar title derived from a first user message: first line, truncated.
pub fn derive_title(content: &str) -> String {
    let first_line = content
        .split(['\n', '\r'])
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut title: String = first_line.chars().take(60).collect();
    if first_line.chars().count() > 60 {
        title.push('…');
    }
    if title.is_empty() {
        "New chat".to_string()
    } else {
        title
    }
}

/// Fetch a single conversation row by id, used by the streaming command.
pub fn get_conversation(
    db: &Db,
    conversation_id: &str,
) -> Result<Option<Conversation>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, model, system_prompt, compaction_summary, created_at, updated_at
             FROM conversations WHERE id = ?1",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt
        .query_map([conversation_id], |r| {
            Ok(Conversation {
                id: r.get(0)?,
                title: r.get(1)?,
                model: r.get(2)?,
                system_prompt: r.get(3)?,
                compaction_summary: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    match rows.next() {
        Some(row) => Ok(Some(row.map_err(|e| e.to_string())?)),
        None => Ok(None),
    }
}

/// Read all messages for a conversation in index order.
pub fn read_messages(db: &Db, conversation_id: &str) -> Result<Vec<MessageRow>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, conversation_id, role, \"index\", content, model, provider, thinking_level, thinking, usage, stop_reason, attachments, created_at
             FROM messages WHERE conversation_id = ?1 ORDER BY \"index\" ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([conversation_id], |r| {
            Ok(MessageRow {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                role: r.get(2)?,
                index: r.get(3)?,
                content: r.get(4)?,
                model: r.get(5)?,
                provider: r.get(6)?,
                thinking_level: r.get(7)?,
                thinking: r.get(8)?,
                usage: r.get(9)?,
                stop_reason: r.get(10)?,
                attachments: r.get(11)?,
                created_at: r.get(12)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Insert a message row, assigning the next monotonic index. Used by both the
/// `add_message` command and the streaming command, keeping DB locking short.
#[allow(clippy::too_many_arguments)]
pub fn insert_message(
    db: &Db,
    conversation_id: String,
    role: String,
    content: String,
    model: Option<String>,
    provider: Option<String>,
    thinking_level: Option<String>,
    thinking: Option<String>,
    usage: Option<String>,
    stop_reason: Option<String>,
) -> Result<MessageRow, String> {
    insert_message_full(
        db,
        conversation_id,
        role,
        content,
        model,
        provider,
        thinking_level,
        thinking,
        usage,
        stop_reason,
        None,
    )
}

/// [`insert_message`] plus the serialized attachment list for the turn.
#[allow(clippy::too_many_arguments)]
pub fn insert_message_full(
    db: &Db,
    conversation_id: String,
    role: String,
    content: String,
    model: Option<String>,
    provider: Option<String>,
    thinking_level: Option<String>,
    thinking: Option<String>,
    usage: Option<String>,
    stop_reason: Option<String>,
    attachments: Option<String>,
) -> Result<MessageRow, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let now = now();
    let next_index: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(\"index\"), -1) + 1 FROM messages WHERE conversation_id = ?1",
            [&conversation_id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO messages (id, conversation_id, role, \"index\", content, model, provider, thinking_level, thinking, usage, stop_reason, attachments, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            id,
            conversation_id,
            role,
            next_index,
            content,
            model,
            provider,
            thinking_level,
            thinking,
            usage,
            stop_reason,
            attachments,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    // Keep the conversation fresh in the sidebar order and auto-title the
    // first user message when the conversation is still unnamed. Memory
    // consolidation notes are background bookkeeping, so they don't reorder
    // the sidebar.
    if role == "user" {
        conn.execute(
            "UPDATE conversations
             SET updated_at = ?2,
                 title = CASE WHEN title = 'New chat' OR title = '' THEN ?3 ELSE title END
             WHERE id = ?1",
            params![conversation_id, now, derive_title(&content)],
        )
        .map_err(|e| e.to_string())?;
    } else if role != "memory" {
        conn.execute(
            "UPDATE conversations SET updated_at = ?2 WHERE id = ?1",
            params![conversation_id, now],
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(MessageRow {
        id,
        conversation_id,
        role,
        index: next_index,
        content,
        model,
        provider,
        thinking_level,
        thinking,
        usage,
        stop_reason,
        attachments,
        created_at: now,
    })
}

#[tauri::command]
pub fn list_conversations(db: tauri::State<'_, Db>) -> Result<Vec<Conversation>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, title, model, system_prompt, compaction_summary, created_at, updated_at
             FROM conversations ORDER BY updated_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Conversation {
                id: r.get(0)?,
                title: r.get(1)?,
                model: r.get(2)?,
                system_prompt: r.get(3)?,
                compaction_summary: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

#[tauri::command]
pub fn create_conversation(
    db: tauri::State<'_, Db>,
    title: Option<String>,
) -> Result<Conversation, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    let t = title.unwrap_or_else(|| "New chat".to_string());
    let now = now();
    conn.execute(
        "INSERT INTO conversations (id, title, created_at, updated_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, t, now, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(Conversation {
        id,
        title: t,
        model: None,
        system_prompt: None,
        compaction_summary: None,
        created_at: now,
        updated_at: now,
    })
}

#[tauri::command]
pub fn list_messages(
    db: tauri::State<'_, Db>,
    conversation_id: String,
) -> Result<Vec<MessageRow>, String> {
    read_messages(&db, &conversation_id)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn add_message(
    db: tauri::State<'_, Db>,
    conversation_id: String,
    role: String,
    content: String,
    model: Option<String>,
    provider: Option<String>,
    thinking_level: Option<String>,
    thinking: Option<String>,
    usage: Option<String>,
    stop_reason: Option<String>,
) -> Result<MessageRow, String> {
    insert_message(
        &db, conversation_id, role, content, model, provider, thinking_level, thinking, usage,
        stop_reason,
    )
}

#[tauri::command]
pub fn rename_conversation(
    db: tauri::State<'_, Db>,
    conversation_id: String,
    title: String,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE conversations SET title = ?2, updated_at = ?3 WHERE id = ?1",
        params![conversation_id, title.trim(), now()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_conversation(
    db: tauri::State<'_, Db>,
    shell: tauri::State<'_, crate::shell::ShellRegistry>,
    conversation_id: String,
) -> Result<(), String> {
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        // Messages cascade via FK (foreign_keys pragma is on from open()).
        conn.execute("DELETE FROM conversations WHERE id = ?1", [&conversation_id])
            .map_err(|e| e.to_string())?;
    }
    // Drop the conversation's persistent shell, if any.
    shell.clear(&conversation_id).await;
    Ok(())
}

/// Search conversations by title or message content (case-insensitive
/// substring). Used by the sidebar search box.
#[tauri::command]
pub fn search_conversations(
    db: tauri::State<'_, Db>,
    query: String,
) -> Result<Vec<Conversation>, String> {
    let q = query.trim();
    if q.is_empty() {
        return list_conversations(db);
    }
    let like = format!("%{}%", q.to_lowercase());
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT c.id, c.title, c.model, c.system_prompt, c.compaction_summary, c.created_at, c.updated_at
             FROM conversations c
             LEFT JOIN messages m ON m.conversation_id = c.id
             WHERE lower(c.title) LIKE ?1 OR lower(m.content) LIKE ?1
             ORDER BY c.updated_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([&like], |r| {
            Ok(Conversation {
                id: r.get(0)?,
                title: r.get(1)?,
                model: r.get(2)?,
                system_prompt: r.get(3)?,
                compaction_summary: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Current wall-clock time in milliseconds since the epoch.
pub fn now_ms() -> i64 {
    now()
}

/// Highest index of a *reflectable* message (user/assistant/tool). Memory
/// consolidation notes are excluded so they never make a conversation look
/// dirty for reflection.
pub fn max_reflectable_index(db: &Db, conversation_id: &str) -> Result<Option<i64>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT MAX(\"index\") FROM messages WHERE conversation_id = ?1 AND role != 'memory'",
        [conversation_id],
        |r| r.get::<_, Option<i64>>(0),
    )
    .map_err(|e| e.to_string())
}

/// The message index already consolidated into memory, if any.
pub fn last_reflected_index(db: &Db, conversation_id: &str) -> Result<Option<i64>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT last_reflected_index FROM conversations WHERE id = ?1",
        [conversation_id],
        |r| r.get::<_, Option<i64>>(0),
    )
    .map_err(|e| e.to_string())
}

pub fn set_last_reflected_index(db: &Db, conversation_id: &str, index: i64) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE conversations SET last_reflected_index = ?2 WHERE id = ?1",
        params![conversation_id, index],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Conversations idle since `cutoff_ms` that have messages newer than their
/// reflection watermark, newest first. Empty conversations are excluded.
pub fn conversations_due_for_reflection(db: &Db, cutoff_ms: i64) -> Result<Vec<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT c.id FROM conversations c
             WHERE c.updated_at <= ?1
               AND COALESCE(c.last_reflected_index, -1) <
                   COALESCE((SELECT MAX(m.\"index\") FROM messages m
                             WHERE m.conversation_id = c.id AND m.role != 'memory'), -1)
             ORDER BY c.updated_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([cutoff_ms], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Record one memory-consolidation run (usage/cost kept separate from chat cost).
#[allow(clippy::too_many_arguments)]
pub fn insert_reflection(
    db: &Db,
    conversation_id: &str,
    model: &str,
    usage: Option<&str>,
    cost: Option<f64>,
    files: &[String],
    note: &str,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let id = Uuid::new_v4().to_string();
    conn.execute(
        "INSERT INTO memory_reflections
         (id, conversation_id, model, usage, cost, files, note, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            id,
            conversation_id,
            model,
            usage,
            cost,
            serde_json::to_string(files).unwrap_or_else(|_| "[]".into()),
            note,
            now(),
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Aggregate reflection spend for the header tooltip + Memory tab.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionStats {
    pub count: i64,
    pub cost: f64,
    pub last_at: i64,
}

#[tauri::command]
pub fn memory_reflection_stats(db: tauri::State<'_, Db>) -> Result<ReflectionStats, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(cost), 0), COALESCE(MAX(created_at), 0)
         FROM memory_reflections",
        [],
        |r| {
            Ok(ReflectionStats {
                count: r.get(0)?,
                cost: r.get(1)?,
                last_at: r.get(2)?,
            })
        },
    )
    .map_err(|e| e.to_string())
}

/// One past consolidation, surfaced to the reflection prompt so the model can
/// avoid re-doing work it already reported.
pub struct ReflectionRecord {
    pub note: String,
    pub created_at: i64,
}

/// The most recent consolidation notes across all conversations, newest first.
pub fn recent_reflections(db: &Db, limit: i64) -> Result<Vec<ReflectionRecord>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT note, created_at FROM memory_reflections
             ORDER BY created_at DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([limit], |r| {
            Ok(ReflectionRecord {
                note: r.get(0)?,
                created_at: r.get(1)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::derive_title;

    #[test]
    fn derive_title_uses_first_line_truncated() {
        assert_eq!(derive_title("Hello there\nsecond line"), "Hello there");
        let long = "x".repeat(100);
        assert_eq!(derive_title(&long), format!("{}…", "x".repeat(60)));
        assert_eq!(derive_title("  \n  spaced  "), "spaced");
        assert_eq!(derive_title(""), "New chat");
    }
}
