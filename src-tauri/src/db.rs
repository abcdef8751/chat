use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use uuid::Uuid;

/// Shared database state installed into Tauri via `manage`.
///
/// The connection lives behind an `Arc` so the memory module can share the very
/// same mutex (and thus serialize on the same lock) without holding a second
/// wrapper around the same connection.
#[derive(Clone)]
pub struct Db(pub Arc<Mutex<Connection>>);

/// SQLite schema (conversations, messages, model_prices, memory_reflections).
const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS conversations (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  model TEXT,
  provider_id TEXT,
  system_prompt TEXT,
  compaction_summary TEXT,
  last_reflected_index INTEGER,
  imported INTEGER NOT NULL DEFAULT 0,
  import_batch INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  revision BIGINT NOT NULL DEFAULT 0,
  dirty INTEGER NOT NULL DEFAULT 0
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
  created_at INTEGER NOT NULL,
  revision BIGINT NOT NULL DEFAULT 0,
  dirty INTEGER NOT NULL DEFAULT 0
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
  attachment INTEGER,
  modalities TEXT,
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

CREATE TABLE IF NOT EXISTS memory_extractions (
  conversation_id TEXT PRIMARY KEY REFERENCES conversations(id) ON DELETE CASCADE,
  watermark INTEGER NOT NULL,
  model TEXT,
  usage TEXT,
  cost REAL,
  payload TEXT NOT NULL,
  folded INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS memory_backfill_runs (
  id TEXT PRIMARY KEY,
  model TEXT,
  usage TEXT,
  cost REAL,
  files TEXT,
  note TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_state (
  entity TEXT PRIMARY KEY,
  last_seen_revision BIGINT NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS memory_sync (
  path TEXT PRIMARY KEY,
  revision BIGINT NOT NULL DEFAULT 0,
  dirty INTEGER NOT NULL DEFAULT 0,
  last_local_at BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS sync_tombstones (
  entity TEXT NOT NULL,
  id TEXT NOT NULL,
  revision BIGINT NOT NULL,
  PRIMARY KEY (entity, id)
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
    // Older databases predate the models.dev capability metadata used to warn
    // when an image is attached to a model that cannot see it.
    let _ = conn.execute("ALTER TABLE model_prices ADD COLUMN attachment INTEGER", []);
    let _ = conn.execute("ALTER TABLE model_prices ADD COLUMN modalities TEXT", []);
    // Older databases predate message file attachments.
    let _ = conn.execute("ALTER TABLE messages ADD COLUMN attachments TEXT", []);
    // Older databases predate the per-conversation memory reflection watermark.
    let _ = conn.execute(
        "ALTER TABLE conversations ADD COLUMN last_reflected_index INTEGER",
        [],
    );
    // Older databases predate imported-conversation tracking.
    let _ = conn.execute(
        "ALTER TABLE conversations ADD COLUMN imported INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // Older databases predate import-batch scoping.
    let _ = conn.execute(
        "ALTER TABLE conversations ADD COLUMN import_batch INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // Older databases predate per-conversation provider selection.
    let _ = conn.execute(
        "ALTER TABLE conversations ADD COLUMN provider_id TEXT",
        [],
    );
    // Older databases predate resumable consolidation.
    let _ = conn.execute(
        "ALTER TABLE memory_extractions ADD COLUMN folded INTEGER NOT NULL DEFAULT 0",
        [],
    );
    // Older databases predate sync: add per-row revision + dirty flags whose
    // defaults (0) make every pre-existing row a fresh, unpushed change. The
    // sync-engine tables are created (idempotently) for databases of any age.
    let _ = conn.execute(
        "ALTER TABLE conversations ADD COLUMN revision BIGINT NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE conversations ADD COLUMN dirty INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE messages ADD COLUMN revision BIGINT NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE messages ADD COLUMN dirty INTEGER NOT NULL DEFAULT 0",
        [],
    );
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS sync_state (
           entity TEXT PRIMARY KEY,
           last_seen_revision BIGINT NOT NULL DEFAULT 0
         )",
        [],
    );
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS memory_sync (
           path TEXT PRIMARY KEY,
           revision BIGINT NOT NULL DEFAULT 0,
           dirty INTEGER NOT NULL DEFAULT 0,
           last_local_at BIGINT NOT NULL
         )",
        [],
    );
    let _ = conn.execute(
        "CREATE TABLE IF NOT EXISTS sync_tombstones (
           entity TEXT NOT NULL,
           id TEXT NOT NULL,
           revision BIGINT NOT NULL,
           PRIMARY KEY (entity, id)
         )",
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
    pub provider_id: Option<String>,
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
            "SELECT id, title, model, provider_id, system_prompt, compaction_summary, created_at, updated_at
             FROM conversations WHERE id = ?1",
        )
        .map_err(|e| e.to_string())?;
    let mut rows = stmt
        .query_map([conversation_id], |r| {
            Ok(Conversation {
                id: r.get(0)?,
                title: r.get(1)?,
                model: r.get(2)?,
                provider_id: r.get(3)?,
                system_prompt: r.get(4)?,
                compaction_summary: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
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
    // Only real turns are syncable; memory consolidation notes are background
    // bookkeeping and are never pushed (so they keep revision 0 / dirty 0).
    let rev = if role == "memory" {
        0
    } else {
        bump_revision_c(&conn)?
    };
    conn.execute(
        "INSERT INTO messages (id, conversation_id, role, \"index\", content, model, provider, thinking_level, thinking, usage, stop_reason, attachments, created_at, revision, dirty)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
            now,
            rev,
            if role == "memory" { 0 } else { 1 }
        ],
    )
    .map_err(|e| e.to_string())?;
    // Keep the conversation fresh in the sidebar order and auto-title the
    // first user message when the conversation is still unnamed. Memory
    // consolidation notes are background bookkeeping, so they don't reorder
    // the sidebar. Only real turns bump the conversation's sync revision.
    if role == "user" {
        conn.execute(
            "UPDATE conversations
             SET updated_at = ?2,
                 title = CASE WHEN title = 'New chat' OR title = '' THEN ?3 ELSE title END,
                 revision = ?4,
                 dirty = 1
             WHERE id = ?1",
            params![conversation_id, now, derive_title(&content), rev],
        )
        .map_err(|e| e.to_string())?;
    } else if role != "memory" {
        conn.execute(
            "UPDATE conversations SET updated_at = ?2, revision = ?3, dirty = 1 WHERE id = ?1",
            params![conversation_id, now, rev],
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
            "SELECT id, title, model, provider_id, system_prompt, compaction_summary, created_at, updated_at
             FROM conversations ORDER BY updated_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Conversation {
                id: r.get(0)?,
                title: r.get(1)?,
                model: r.get(2)?,
                provider_id: r.get(3)?,
                system_prompt: r.get(4)?,
                compaction_summary: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
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
    let rev = bump_revision_c(&conn)?;
    conn.execute(
        "INSERT INTO conversations (id, title, created_at, updated_at, revision, dirty)
         VALUES (?1, ?2, ?3, ?4, ?5, 1)",
        params![id, t, now, now, rev],
    )
    .map_err(|e| e.to_string())?;
    Ok(Conversation {
        id,
        title: t,
        model: None,
        provider_id: None,
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
    let rev = bump_revision_c(&conn)?;
    conn.execute(
        "UPDATE conversations SET title = ?2, updated_at = ?3, revision = ?4, dirty = 1 WHERE id = ?1",
        params![conversation_id, title.trim(), now(), rev],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Set (or clear) a conversation's provider. `provider_id: None` clears it back
/// to the active provider's default. Marks the row dirty for sync.
#[tauri::command]
pub fn set_conversation_provider(
    db: tauri::State<'_, Db>,
    conversation_id: String,
    provider_id: Option<String>,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let rev = bump_revision_c(&conn)?;
    let pid = provider_id.filter(|p| !p.is_empty());
    conn.execute(
        "UPDATE conversations SET provider_id = ?2, updated_at = ?3, revision = ?4, dirty = 1 WHERE id = ?1",
        params![conversation_id, pid, now(), rev],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Set a conversation's model (used by the per-chat model picker).
#[tauri::command]
pub fn set_conversation_model(
    db: tauri::State<'_, Db>,
    conversation_id: String,
    model: String,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let rev = bump_revision_c(&conn)?;
    conn.execute(
        "UPDATE conversations SET model = ?2, updated_at = ?3, revision = ?4, dirty = 1 WHERE id = ?1",
        params![conversation_id, model, now(), rev],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_conversation(
    db: tauri::State<'_, Db>,
    conversation_id: String,
) -> Result<(), String> {
    {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        // Record tombstones so the sync push can delete the conversation (and
        // its messages) on other devices, then hard-delete locally. Messages
        // cascade via FK (foreign_keys pragma is on from open()).
        crate::db::tombstone_conversation(&conn, &conversation_id)?;
        conn.execute("DELETE FROM conversations WHERE id = ?1", [&conversation_id])
            .map_err(|e| e.to_string())?;
    }
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
            "SELECT DISTINCT c.id, c.title, c.model, c.provider_id, c.system_prompt, c.compaction_summary, c.created_at, c.updated_at
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
                provider_id: r.get(3)?,
                system_prompt: r.get(4)?,
                compaction_summary: r.get(5)?,
                created_at: r.get(6)?,
                updated_at: r.get(7)?,
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

/// Next monotonic sync revision, seeded from the clock but guaranteed strictly
/// increasing so two changes within the same millisecond never collide.
///
/// The low-water counter is persisted in `sync_state` under a synthetic entity
/// (which the entity-scoped pull high-water marks never touch), so the sequence
/// survives restarts. Takes a `&Connection` so it can be called while the caller
/// already holds the DB lock (the mutators do) without re-entering the mutex.
pub fn bump_revision_c(conn: &Connection) -> Result<i64, String> {
    const REV: &str = "\u{0}revision";
    let cur: i64 = conn
        .query_row(
            "SELECT last_seen_revision FROM sync_state WHERE entity = ?1",
            [REV],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let now = now();
    let next = if now > cur { now } else { cur + 1 };
    conn.execute(
        "INSERT INTO sync_state(entity, last_seen_revision) VALUES (?1, ?2)
         ON CONFLICT(entity) DO UPDATE SET last_seen_revision = excluded.last_seen_revision",
        params![REV, next],
    )
    .map_err(|e| e.to_string())?;
    Ok(next)
}

/// Mark a memory file as changed (append/replace) or deleted (tombstone) so the
/// next push mirrors it. Identity is by path within the user namespace; the push
/// inspects whether the file still exists to decide live-vs-tombstone. Takes a
/// `&Connection` for the same lock-safe reason as [`bump_revision_c`].
pub fn mark_memory_dirty(conn: &Connection, path: &str) -> Result<(), String> {
    let rev = bump_revision_c(conn)?;
    conn.execute(
        "INSERT INTO memory_sync(path, revision, dirty, last_local_at)
         VALUES (?1, ?2, 1, ?3)
         ON CONFLICT(path) DO UPDATE SET
           revision = excluded.revision,
           dirty = 1,
           last_local_at = excluded.last_local_at",
        params![path, rev, now_ms()],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// One-time seeding for the first sync: mark every pre-existing row (and every
/// on-disk memory file) dirty so the initial push mirrors full history — the
/// "backup" slice. Without this, rows that predate the feature (default revision
/// 0 / dirty 0) would never be pushed and enabling sync on an existing user
/// would back up nothing.
///
/// Guarded by a sentinel row in `sync_state`, scoped **per account** by
/// `user_id` (entity `\0seeded:<user_id>`), so it runs once per Supabase user
/// and re-enabling sync — or signing into a *different* account — never skips a
/// needed full backup. `memory_files` is the list of on-disk memory file names
/// (the caller collects them from `MemoryState` so this module stays free of a
/// dependency on `memory`).
pub fn seed_initial_sync(db: &Db, user_id: &str, memory_files: &[String]) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let seeded_entity = format!("\u{0}seeded:{}", user_id);
    let already: Option<i64> = conn
        .query_row(
            "SELECT last_seen_revision FROM sync_state WHERE entity = ?1",
            [&seeded_entity],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if already.is_some() {
        return Ok(());
    }
    conn.execute("UPDATE conversations SET dirty = 1", [])
        .map_err(|e| e.to_string())?;
    // Memory consolidation notes (`role = 'memory'`) are background bookkeeping —
    // never pushed — so the seed must exclude them, matching the insert path.
    conn.execute("UPDATE messages SET dirty = 1 WHERE role != 'memory'", [])
        .map_err(|e| e.to_string())?;
    for path in memory_files {
        mark_memory_dirty(&conn, path)?;
    }
    conn.execute(
        "INSERT INTO sync_state(entity, last_seen_revision) VALUES (?1, 1)
         ON CONFLICT(entity) DO NOTHING",
        [&seeded_entity],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Record a locally hard-deleted row as a tombstone to push. Only the
/// conversation itself is tombstoned (its messages cascade-delete locally and on
/// consumers, so per-message tombstones would carry no row data anyway).
pub fn tombstone_conversation(conn: &Connection, conversation_id: &str) -> Result<i64, String> {
    let rev = bump_revision_c(conn)?;
    conn.execute(
        "INSERT OR REPLACE INTO sync_tombstones(entity, id, revision)
         VALUES ('conversations', ?1, ?2)",
        params![conversation_id, rev],
    )
    .map_err(|e| e.to_string())?;
    Ok(rev)
}

/// Number of rows locally awaiting a push (dirty rows + pending tombstones).
pub fn pending_rows(db: &Db) -> Result<u64, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let count = |stmt: &str| -> i64 {
        conn.query_row(stmt, [], |r| r.get(0)).unwrap_or(0)
    };
    let conv = count(
        "SELECT COUNT(*) FROM conversations WHERE dirty = 1",
    );
    let msg = count("SELECT COUNT(*) FROM messages WHERE dirty = 1");
    let mem = count("SELECT COUNT(*) FROM memory_sync WHERE dirty = 1");
    let tomb = count("SELECT COUNT(*) FROM sync_tombstones");
    Ok((conv + msg + mem + tomb) as u64)
}

/// Mark every syncable row dirty so the next push re-uploads it. Used when
/// toggling client-side encryption: enabling re-uploads everything encrypted,
/// disabling re-uploads everything plaintext. Memory-role notes are excluded
/// (they are never synced by design).
pub fn mark_all_dirty_for_resync(db: &Db) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute("UPDATE conversations SET dirty = 1", [])
        .map_err(|e| e.to_string())?;
    conn.execute("UPDATE messages SET dirty = 1 WHERE role != 'memory'", [])
        .map_err(|e| e.to_string())?;
    conn.execute("UPDATE memory_sync SET dirty = 1", [])
        .map_err(|e| e.to_string())?;
    Ok(())
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
    // The watermark rides on the conversation row for sync, so advancing it must
    // mark the row dirty and bump its revision — otherwise the sync push never
    // learns the conversation was reflected and a second device re-reflects it.
    let rev = bump_revision_c(&conn)?;
    conn.execute(
        "UPDATE conversations SET last_reflected_index = ?2, revision = ?3, dirty = 1 WHERE id = ?1",
        params![conversation_id, index, rev],
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

/// Record one reduce pass of a memory backfill. Kept in its own table because a
/// reduce pass spans many conversations and so has no single `conversation_id`
/// to hang off `memory_reflections` (which is FK-bound to `conversations`).
pub fn insert_backfill_run(
    db: &Db,
    model: &str,
    usage: Option<&str>,
    cost: Option<f64>,
    files: &[String],
    note: &str,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO memory_backfill_runs (id, model, usage, cost, files, note, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            Uuid::new_v4().to_string(),
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

/// Aggregate reflection spend for the header tooltip + Memory tab. Includes
/// backfill reduce passes so the readout covers all memory-maintenance spend.
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
        "SELECT COUNT(*), COALESCE(SUM(cost), 0), COALESCE(MAX(created_at), 0) FROM (
           SELECT cost, created_at FROM memory_reflections
           UNION ALL
           SELECT cost, created_at FROM memory_backfill_runs
         )",
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

/// One staged memory extraction — the "map" half of a backfill. It is kept
/// deliberately *outside* the memory files: the reduce pass is the only writer,
/// so parallel extraction passes can never race on shared Markdown.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionRow {
    pub conversation_id: String,
    pub title: String,
    pub watermark: i64,
    pub payload: String,
    /// When the *conversation* happened — not when it was extracted. A backfill
    /// stages every conversation at once, so the extraction's own timestamp
    /// would date all of them "today" and carry no chronology at all.
    pub created_at: i64,
}

/// Insert or replace the staged extraction for a conversation. Re-staging
/// resets `folded`, because a conversation that grew new messages needs its
/// fresh summary consolidated again.
#[allow(clippy::too_many_arguments)]
pub fn upsert_extraction(
    db: &Db,
    conversation_id: &str,
    watermark: i64,
    model: &str,
    usage: Option<&str>,
    cost: Option<f64>,
    payload: &str,
) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO memory_extractions
           (conversation_id, watermark, model, usage, cost, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(conversation_id) DO UPDATE SET
           watermark = excluded.watermark,
           model = excluded.model,
           usage = excluded.usage,
           cost = excluded.cost,
           payload = excluded.payload,
           folded = 0,
           created_at = excluded.created_at",
        params![
            conversation_id,
            watermark,
            model,
            usage,
            cost,
            payload,
            now()
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Conversations still needing a staged extraction: they have reflectable
/// messages, at least `min_chars` of them, and no extraction covering the newest
/// message. Newest first.
///
/// Restricted to **imported** conversations. Chats the user actually had are
/// handled by the ordinary sequential reflection pass; the backfill exists to
/// mine an archive that was never lived through in this app.
///
/// The `min_chars` floor is the cheap big win on a bulk import: short chats are
/// most of the *passes* (and therefore most of the fixed per-pass system-prompt
/// cost) but a rounding error of the actual content.
pub fn conversations_pending_extraction(db: &Db, min_chars: i64) -> Result<Vec<String>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT c.id FROM conversations c
             WHERE c.imported = 1
               AND c.import_batch = (
                   SELECT COALESCE(MAX(import_batch), 0) FROM conversations WHERE imported = 1
               )
               AND COALESCE((SELECT MAX(m.\"index\") FROM messages m
                             WHERE m.conversation_id = c.id AND m.role != 'memory'), -1) >= 0
               AND (SELECT COALESCE(SUM(LENGTH(m.content)), 0) FROM messages m
                    WHERE m.conversation_id = c.id AND m.role != 'memory') >= ?1
               AND COALESCE((SELECT e.watermark FROM memory_extractions e
                             WHERE e.conversation_id = c.id), -1)
                   < COALESCE((SELECT MAX(m.\"index\") FROM messages m
                               WHERE m.conversation_id = c.id AND m.role != 'memory'), -1)
             ORDER BY c.updated_at DESC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([min_chars], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Staged extractions that have not yet been folded into memory, in
/// chronological order so the reduce folds history forward and later facts
/// naturally win over earlier ones.
///
/// Folded rows are excluded, which is what makes an interrupted consolidation
/// resumable: a re-run picks up at the first batch that did not land.
pub fn list_extractions(db: &Db) -> Result<Vec<ExtractionRow>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT e.conversation_id, COALESCE(c.title, ''), e.watermark, e.payload,
                    COALESCE(c.created_at, e.created_at)
             FROM memory_extractions e
             LEFT JOIN conversations c ON c.id = e.conversation_id
             WHERE e.folded = 0
             ORDER BY c.created_at ASC, e.created_at ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ExtractionRow {
                conversation_id: r.get(0)?,
                title: r.get(1)?,
                watermark: r.get(2)?,
                payload: r.get(3)?,
                created_at: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Drop every staged extraction (the reduce has consumed them, or the user
/// wants to start over).
/// Drop staged extractions that have not been folded into memory — the
/// "Discard staged" action.
///
/// Folded rows are deliberately kept: they are the watermark that stops an
/// already-extracted conversation being extracted again. Deleting them would
/// make every conversation look pending, so the next run would redo the whole
/// archive and re-fold it into memory.
pub fn discard_staged_extractions(db: &Db) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM memory_extractions WHERE folded = 0", [])
        .map_err(|e| e.to_string())
}

/// Mark a batch's staged extractions as folded into memory. Called after each
/// reduce pass succeeds, so an interrupted consolidation resumes at the first
/// batch that did not land instead of re-folding everything.
pub fn mark_extractions_folded(db: &Db, conversation_ids: &[String]) -> Result<(), String> {
    if conversation_ids.is_empty() {
        return Ok(());
    }
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("UPDATE memory_extractions SET folded = 1 WHERE conversation_id = ?1")
        .map_err(|e| e.to_string())?;
    for id in conversation_ids {
        stmt.execute([id]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The newest conversation date already folded into memory. A resumed
/// consolidation uses this to tell the model what memory already covers, so
/// "later wins" survives a restart even though memory entries are undated.
pub fn folded_through(db: &Db) -> Result<Option<i64>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT MAX(COALESCE(c.created_at, e.created_at))
         FROM memory_extractions e
         LEFT JOIN conversations c ON c.id = e.conversation_id
         WHERE e.folded = 1",
        [],
        |r| r.get::<_, Option<i64>>(0),
    )
    .map_err(|e| e.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionStats {
    pub staged: i64,
    pub pending: i64,
}

/// One row of an imported conversation, ready to write.
pub struct ImportedMessage {
    pub id: String,
    pub role: String,
    pub index: i64,
    pub content: String,
    pub thinking: Option<String>,
    pub created_at: i64,
}

/// The batch id to stamp on the conversations of the next import run. One import
/// run = one batch, so the backfill can scope itself to the newest one.
pub fn next_import_batch(db: &Db) -> Result<i64, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT COALESCE(MAX(import_batch), 0) + 1 FROM conversations WHERE imported = 1",
        [],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

/// Insert an imported conversation and all of its rows in a single transaction.
///
/// The caller supplies the timestamps, so imported chats sort into the sidebar
/// by their real dates instead of all landing at the top. Unlike
/// [`insert_message_full`] this never touches the conversation's `updated_at` or
/// title, so an import cannot reorder the sidebar.
///
/// Atomic per conversation, which matters twice over: an interrupted import can
/// never leave a chat holding half its messages, and a re-import skips
/// conversations that already exist — so a half-written chat would stay
/// half-written forever. It is also far faster than a statement per row, since
/// each of those would otherwise be its own WAL commit.
///
/// `import_batch` stamps the run that wrote the conversation, so the memory
/// backfill can scope itself to the newest import.
///
/// Returns `false` when the conversation already exists, which makes re-importing
/// a no-op rather than a clobber.
#[allow(clippy::too_many_arguments)]
pub fn insert_imported_conversation(
    db: &Db,
    id: &str,
    title: &str,
    model: Option<&str>,
    created_at: i64,
    updated_at: i64,
    import_batch: i64,
    messages: &[ImportedMessage],
) -> Result<bool, String> {
    let mut conn = db.0.lock().map_err(|e| e.to_string())?;
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO conversations
               (id, title, model, system_prompt, compaction_summary, last_reflected_index,
                imported, import_batch, created_at, updated_at)
             VALUES (?1, ?2, ?3, NULL, NULL, NULL, 1, ?6, ?4, ?5)",
            params![id, title, model, created_at, updated_at, import_batch],
        )
        .map_err(|e| e.to_string())?
        > 0;
    if !inserted {
        return Ok(false);
    }

    {
        let mut stmt = tx
            .prepare(
                "INSERT OR IGNORE INTO messages
                   (id, conversation_id, role, \"index\", content, model, provider,
                    thinking_level, thinking, usage, stop_reason, attachments, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, NULL, ?8, NULL, ?9)",
            )
            .map_err(|e| e.to_string())?;
        for m in messages {
            let stop_reason = if m.role == "assistant" { Some("stop") } else { None };
            stmt.execute(params![
                m.id,
                id,
                m.role,
                m.index,
                m.content,
                model,
                m.thinking,
                stop_reason,
                m.created_at
            ])
            .map_err(|e| e.to_string())?;
        }
    }

    tx.commit().map_err(|e| e.to_string())?;
    Ok(true)
}

/// Mark every imported conversation as already reflected, so the idle scheduler
/// never sweeps an archive. The backfill uses its own watermark
/// (`memory_extractions`), so imported chats stay available to it.
pub fn mark_imported_reflected(db: &Db) -> Result<usize, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "UPDATE conversations SET last_reflected_index = COALESCE(
           (SELECT MAX(m.\"index\") FROM messages m
            WHERE m.conversation_id = conversations.id AND m.role != 'memory'), -1)
         WHERE imported = 1",
        [],
    )
    .map_err(|e| e.to_string())
}

pub fn extraction_stats(db: &Db, min_chars: i64) -> Result<ExtractionStats, String> {
    let staged: i64 = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        // Only unfolded rows: a folded extraction is already in memory, so it is
        // no longer "staged" from the user's point of view.
        conn.query_row(
            "SELECT COUNT(*) FROM memory_extractions WHERE folded = 0",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?
    };
    let pending = conversations_pending_extraction(db, min_chars)?.len() as i64;
    Ok(ExtractionStats { staged, pending })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_title_uses_first_line_truncated() {
        assert_eq!(derive_title("Hello there\nsecond line"), "Hello there");
        let long = "x".repeat(100);
        assert_eq!(derive_title(&long), format!("{}…", "x".repeat(60)));
        assert_eq!(derive_title("  \n  spaced  "), "spaced");
        assert_eq!(derive_title(""), "New chat");
    }

    /// An interrupted consolidation must resume at the first unfolded batch, and
    /// must still know what date memory already covers.
    #[test]
    fn folding_makes_consolidation_resumable() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-fold-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let db = Db(Arc::new(Mutex::new(open(&p).unwrap())));

        let mk = |id: &str, at: i64| {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO conversations (id, title, imported, created_at, updated_at)
                 VALUES (?1, ?1, 1, ?2, ?2)",
                params![id, at],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (id, conversation_id, role, \"index\", content, created_at)
                 VALUES (?1, ?2, 'user', 0, 'x', ?3)",
                params![format!("{id}-m"), id, at],
            )
            .unwrap();
        };

        mk("a", 1_000);
        mk("b", 2_000);
        mk("c", 3_000);
        for id in ["a", "b", "c"] {
            upsert_extraction(&db, id, 0, "m", None, None, &format!("[{id}] fact")).unwrap();
        }

        assert_eq!(list_extractions(&db).unwrap().len(), 3);
        assert_eq!(folded_through(&db).unwrap(), None);

        // Fold the first batch, then "crash".
        mark_extractions_folded(&db, &["a".to_string()]).unwrap();

        // Only the outstanding work remains, and the fold date is recoverable.
        let rest = list_extractions(&db).unwrap();
        assert_eq!(rest.len(), 2);
        assert_eq!(rest[0].conversation_id, "b");
        assert_eq!(folded_through(&db).unwrap(), Some(1_000));
        assert_eq!(extraction_stats(&db, 0).unwrap().staged, 2);

        // Folding is idempotent.
        mark_extractions_folded(&db, &["a".to_string()]).unwrap();
        assert_eq!(list_extractions(&db).unwrap().len(), 2);

        // A conversation that grows new messages is re-staged and must be
        // folded again, not left marked as done.
        upsert_extraction(&db, "a", 1, "m", None, None, "[a] newer fact").unwrap();
        let restaged = list_extractions(&db).unwrap();
        assert_eq!(restaged.len(), 3);
        assert!(restaged.iter().any(|e| e.conversation_id == "a"));

        // Finishing clears everything, folded rows included.
        let all: Vec<String> = list_extractions(&db)
            .unwrap()
            .into_iter()
            .map(|e| e.conversation_id)
            .collect();
        mark_extractions_folded(&db, &all).unwrap();
        assert!(list_extractions(&db).unwrap().is_empty());
        assert_eq!(folded_through(&db).unwrap(), Some(3_000));

        // Discarding drops only unfolded work. The folded rows stay as the
        // extraction watermark — deleting them would make every conversation
        // look pending again and redo the whole archive.
        assert_eq!(discard_staged_extractions(&db).unwrap(), 0);
        assert_eq!(folded_through(&db).unwrap(), Some(3_000));

        let _ = std::fs::remove_file(&p);
    }

    /// The point of keeping folded rows: after a run, the conversations that
    /// succeeded are no longer pending and the ones that *failed* still are — so
    /// a re-run retries exactly the failures instead of redoing the archive.
    #[test]
    fn a_failed_extraction_stays_pending_after_a_successful_run() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-pending-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let db = Db(Arc::new(Mutex::new(open(&p).unwrap())));
        let now = now_ms();

        let mk = |id: &str| {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO conversations (id, title, imported, created_at, updated_at)
                 VALUES (?1, ?1, 1, ?2, ?2)",
                params![id, now],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (id, conversation_id, role, \"index\", content, created_at)
                 VALUES (?1, ?2, 'user', 0, ?3, ?4)",
                params![format!("{id}-m"), id, "x".repeat(3000), now],
            )
            .unwrap();
        };
        mk("ok1");
        mk("ok2");
        mk("failed");

        // Two extracted, one not — the failure leaves no row at all.
        upsert_extraction(&db, "ok1", 0, "m", None, None, "[profile] a").unwrap();
        upsert_extraction(&db, "ok2", 0, "m", None, None, "[profile] b").unwrap();

        // The reduce folds them.
        mark_extractions_folded(&db, &["ok1".to_string(), "ok2".to_string()]).unwrap();

        assert_eq!(
            conversations_pending_extraction(&db, 2000).unwrap(),
            vec!["failed".to_string()]
        );
        // Nothing left to consolidate.
        assert_eq!(extraction_stats(&db, 2000).unwrap().staged, 0);

        let _ = std::fs::remove_file(&p);
    }

    /// The staging area is what keeps a bulk import from being re-reflected on
    /// every scheduler tick: the watermark, not the timestamp, is the gate.
    #[test]
    fn pending_extraction_honors_min_chars_and_watermark() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-extract-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let db = Db(Arc::new(Mutex::new(open(&p).unwrap())));
        let now = now_ms();

        let mk = |id: &str, body: &str, imported: i64| {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO conversations (id, title, imported, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![id, id, imported, now],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (id, conversation_id, role, \"index\", content, created_at)
                 VALUES (?1, ?2, 'user', 0, ?3, ?4)",
                params![format!("{id}-m"), id, body, now],
            )
            .unwrap();
        };

        mk("short", "hi", 1);
        mk("long", &"x".repeat(3000), 1);
        // A chat the user actually had is the sequential pass's job, never the
        // backfill's.
        mk("native", &"x".repeat(3000), 0);

        // Below the floor, and not imported: neither is extracted.
        assert_eq!(
            conversations_pending_extraction(&db, 2000).unwrap(),
            vec!["long".to_string()]
        );

        // Staging an extraction covering the newest message clears it.
        upsert_extraction(&db, "long", 0, "m", None, None, "[profile] x").unwrap();
        assert!(conversations_pending_extraction(&db, 2000).unwrap().is_empty());

        // A newer message makes it pending again.
        {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO messages (id, conversation_id, role, \"index\", content, created_at)
                 VALUES ('long-m2', 'long', 'user', 1, 'more', ?1)",
                params![now],
            )
            .unwrap();
        }
        assert_eq!(
            conversations_pending_extraction(&db, 2000).unwrap(),
            vec!["long".to_string()]
        );

        let stats = extraction_stats(&db, 2000).unwrap();
        assert_eq!(stats.staged, 1);
        assert_eq!(stats.pending, 1);

        // Re-staging replaces rather than duplicating.
        upsert_extraction(&db, "long", 1, "m", None, None, "[profile] y").unwrap();
        let rows = list_extractions(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].payload, "[profile] y");
        assert_eq!(rows[0].watermark, 1);

        assert_eq!(discard_staged_extractions(&db).unwrap(), 1);
        assert!(list_extractions(&db).unwrap().is_empty());

        let _ = std::fs::remove_file(&p);
    }

    /// The backfill is scoped to the newest import batch: importing a second
    /// archive must not re-sweep the first one, even though its conversations
    /// are still `imported = 1` with no extraction watermark.
    #[test]
    fn pending_extraction_is_scoped_to_the_newest_import_batch() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-batch-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let db = Db(Arc::new(Mutex::new(open(&p).unwrap())));
        let now = now_ms();

        let mk = |id: &str, batch: i64| {
            let conn = db.0.lock().unwrap();
            conn.execute(
                "INSERT INTO conversations
                   (id, title, imported, import_batch, created_at, updated_at)
                 VALUES (?1, ?1, 1, ?2, ?3, ?3)",
                params![id, batch, now],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO messages (id, conversation_id, role, \"index\", content, created_at)
                 VALUES (?1, ?2, 'user', 0, ?3, ?4)",
                params![format!("{id}-m"), id, "x".repeat(3000), now],
            )
            .unwrap();
        };

        mk("older", 1);
        mk("newest", 2);

        // Both are pending by content, but only the newest batch is in scope.
        assert_eq!(
            conversations_pending_extraction(&db, 2000).unwrap(),
            vec!["newest".to_string()]
        );

        // The next import run gets a fresh, higher batch id.
        assert_eq!(next_import_batch(&db).unwrap(), 3);

        let _ = std::fs::remove_file(&p);
    }

    /// A database written before import-batch scoping existed must migrate
    /// cleanly: the new column defaults existing imported chats to batch 0, and
    /// with no newer import that batch is still the one the backfill targets.
    #[test]
    fn import_batch_migration_defaults_existing_imports() {
        let mut p = std::env::temp_dir();
        p.push(format!("pi-chat-batch-migrate-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);

        // A database with the pre-`import_batch` conversations schema.
        {
            let conn = Connection::open(&p).unwrap();
            conn.execute_batch(
                "CREATE TABLE conversations (
                   id TEXT PRIMARY KEY, title TEXT NOT NULL, model TEXT,
                   system_prompt TEXT, compaction_summary TEXT,
                   last_reflected_index INTEGER,
                   imported INTEGER NOT NULL DEFAULT 0,
                   created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
                 );
                 INSERT INTO conversations (id, title, imported, created_at, updated_at)
                 VALUES ('legacy', 'legacy', 1, 1, 1);",
            )
            .unwrap();
        }

        let db = Db(Arc::new(Mutex::new(open(&p).unwrap())));
        {
            let conn = db.0.lock().unwrap();
            let batch: i64 = conn
                .query_row(
                    "SELECT import_batch FROM conversations WHERE id = 'legacy'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(batch, 0);
        }
        assert_eq!(next_import_batch(&db).unwrap(), 1);

        let _ = std::fs::remove_file(&p);
    }
}
