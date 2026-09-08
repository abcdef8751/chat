use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection};
use serde::Serialize;
use uuid::Uuid;

/// Shared database state installed into Tauri via `manage`.
pub struct Db(pub Mutex<Connection>);

/// Schema as defined in PLANS.md (conversations, messages, model_prices).
const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS conversations (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  model TEXT,
  system_prompt TEXT,
  compaction_summary TEXT,
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
  usage TEXT,
  stop_reason TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS model_prices (
  provider TEXT NOT NULL,
  model_id TEXT NOT NULL,
  input_per_million REAL,
  output_per_million REAL,
  cache_read_per_million REAL,
  cache_write_per_million REAL,
  fetched_at INTEGER,
  PRIMARY KEY (provider, model_id)
);

CREATE INDEX IF NOT EXISTS idx_messages_conv ON messages(conversation_id, "index");
CREATE INDEX IF NOT EXISTS idx_conversations_updated ON conversations(updated_at DESC);
"#;

/// Open (or create) the SQLite database at `path` and run the schema migration.
pub fn open(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|e| format!("open db: {e}"))?;
    conn.execute_batch(SCHEMA).map_err(|e| format!("migrate: {e}"))?;
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
    pub usage: Option<String>,
    pub stop_reason: Option<String>,
    pub created_at: i64,
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, conversation_id, role, \"index\", content, model, provider, thinking_level, usage, stop_reason, created_at
             FROM messages WHERE conversation_id = ?1 ORDER BY \"index\" ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([&conversation_id], |r| {
            Ok(MessageRow {
                id: r.get(0)?,
                conversation_id: r.get(1)?,
                role: r.get(2)?,
                index: r.get(3)?,
                content: r.get(4)?,
                model: r.get(5)?,
                provider: r.get(6)?,
                thinking_level: r.get(7)?,
                usage: r.get(8)?,
                stop_reason: r.get(9)?,
                created_at: r.get(10)?,
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
#[allow(clippy::too_many_arguments)]
pub fn add_message(
    db: tauri::State<'_, Db>,
    conversation_id: String,
    role: String,
    content: String,
    model: Option<String>,
    provider: Option<String>,
    thinking_level: Option<String>,
    usage: Option<String>,
    stop_reason: Option<String>,
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
        "INSERT INTO messages (id, conversation_id, role, \"index\", content, model, provider, thinking_level, usage, stop_reason, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            id,
            conversation_id,
            role,
            next_index,
            content,
            model,
            provider,
            thinking_level,
            usage,
            stop_reason,
            now
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(MessageRow {
        id,
        conversation_id,
        role,
        index: next_index,
        content,
        model,
        provider,
        thinking_level,
        usage,
        stop_reason,
        created_at: now,
    })
}
