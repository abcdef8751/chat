//! Unit + integration tests for the sync engine.
//!
//! Unit tests cover revision bumping / dirty marking, tombstone handling, LWW
//! (local vs remote wins), `(created_at, id)` re-indexing, and the per-entity
//! high-water mark. The integration test drives push + pull end-to-end against a
//! small self-contained mock PostgREST server on a loopback TcpListener — no
//! external network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::*;
use crate::db::{self, Db};
use crate::memory::MemoryState;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn temp_db(name: &str) -> Db {
    let mut p = std::env::temp_dir();
    p.push(format!("pi-chat-sync-{name}-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    Db(Arc::new(Mutex::new(db::open(&p).unwrap())))
}

fn temp_memory(name: &str, db: &Db) -> MemoryState {
    let mut p = std::env::temp_dir();
    p.push(format!("pi-chat-sync-mem-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    let mut m = MemoryState::load(p).unwrap();
    m.set_db(db.clone());
    m
}

fn temp_config(name: &str) -> crate::config::ConfigState {
    let mut p = std::env::temp_dir();
    p.push(format!("pi-chat-sync-cfg-{name}-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&p);
    crate::config::ConfigState::load(p).unwrap()
}

fn rev_of(db: &Db, sql: &str) -> i64 {
    let conn = db.0.lock().unwrap();
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap()
}

fn dirty_of(db: &Db, sql: &str) -> i64 {
    let conn = db.0.lock().unwrap();
    conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap()
}

fn insert_conv(db: &Db) -> String {
    // Mirror `create_conversation` (a tauri command) via its underlying SQL.
    let conn = db.0.lock().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let now = db::now_ms();
    let rev = db::bump_revision_c(&conn).unwrap();
    conn.execute(
        "INSERT INTO conversations (id, title, created_at, updated_at, revision, dirty) VALUES (?1, ?2, ?3, ?3, ?4, 1)",
        rusqlite::params![id, "t", now, rev],
    )
    .unwrap();
    id
}

// ---------------------------------------------------------------------------
// mock PostgREST server
// ---------------------------------------------------------------------------

/// Minimal in-process PostgREST mock. Captures POST bodies per table and serves
/// configured GET payloads per table. Loopback only; no external network.
struct MockRest {
    addr: String,
    posts: Arc<Mutex<HashMap<String, Vec<Value>>>>,
    gets: Arc<Mutex<HashMap<String, Vec<Value>>>>,
}

impl MockRest {
    fn spawn() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let posts: Arc<Mutex<HashMap<String, Vec<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let gets: Arc<Mutex<HashMap<String, Vec<Value>>>> = Arc::new(Mutex::new(HashMap::new()));
        let p = posts.clone();
        let g = gets.clone();
        std::thread::spawn(move || loop {
            let (mut sock, _) = match listener.accept() {
                Ok(c) => c,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                }
            };
            sock.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let p = p.clone();
            let g = g.clone();
            std::thread::spawn(move || {
                sock.set_write_timeout(Some(std::time::Duration::from_secs(5))).ok();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                let header_end;
                loop {
                    match sock.read(&mut tmp) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                header_end = pos + 4;
                                break;
                            }
                        }
                    }
                }
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or("").to_string();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut clen = 0usize;
                for l in lines {
                    if let Some((k, v)) = l.split_once(':') {
                        if k.trim().eq_ignore_ascii_case("content-length") {
                            clen = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                while buf.len() < header_end + clen {
                    match sock.read(&mut tmp) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let body_str =
                    String::from_utf8_lossy(&buf[header_end.min(buf.len())..buf.len().min(header_end + clen)])
                        .to_string();

                let table = path
                    .split("/rest/v1/")
                    .nth(1)
                    .map(|s| s.split('?').next().unwrap_or("").to_string());

                let (status, resp_body, ctype) = if path.contains("/auth/v1/token") {
                    (
                        201,
                        json!({
                            "access_token": "acc-token",
                            "refresh_token": "ref-token",
                            "expires_in": 3600,
                            "user": { "id": "user-123", "email": "u@example.com" }
                        })
                        .to_string(),
                        "application/json",
                    )
                } else if method == "POST" {
                    match &table {
                        Some(t) => {
                            let rows: Vec<Value> =
                                serde_json::from_str(&body_str).unwrap_or_default();
                            p.lock().unwrap().entry(t.clone()).or_default().extend(rows);
                            (201, "[]".to_string(), "application/json")
                        }
                        None => (404, "{}".to_string(), "application/json"),
                    }
                } else {
                    match &table {
                        Some(t) => {
                            let rows = g.lock().unwrap().get(t).cloned().unwrap_or_default();
                            let js = serde_json::to_string(&rows).unwrap_or("[]".into());
                            (200, js, "application/json")
                        }
                        None => (404, "{}".to_string(), "application/json"),
                    }
                };
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\r\n{}",
                    resp_body.len(),
                    resp_body
                );
                let _ = sock.write_all(resp.as_bytes());
                let _ = sock.flush();
            });
        });
        Self { addr, posts, gets }
    }

    fn posted(&self, table: &str) -> Vec<Value> {
        self.posts.lock().unwrap().get(table).cloned().unwrap_or_default()
    }

    fn set_get(&self, table: &str, rows: Vec<Value>) {
        self.gets.lock().unwrap().insert(table.to_string(), rows);
    }
}

use std::io::{Read, Write};

// ---------------------------------------------------------------------------
// revision bump + dirty marking
// ---------------------------------------------------------------------------

#[test]
fn insert_message_marks_real_turns_dirty_and_bumps_monotonic_revision() {
    let db = temp_db("dirty");
    let cid = insert_conv(&db);

    db::insert_message(
        &db,
        cid.clone(),
        "user".into(),
        "hello".into(),
        None,
        None,
        None,
        None,
        None,
        Some("stop".into()),
    )
    .unwrap();

    // Message row is dirty + has a revision.
    let mrev = rev_of(&db, "SELECT revision FROM messages WHERE content = 'hello'");
    assert!(mrev > 0);
    assert_eq!(
        dirty_of(&db, "SELECT dirty FROM messages WHERE content = 'hello'"),
        1
    );

    // Second insert gets a strictly larger revision (monotonic).
    db::insert_message(
        &db,
        cid.clone(),
        "user".into(),
        "second".into(),
        None,
        None,
        None,
        None,
        None,
        Some("stop".into()),
    )
    .unwrap();
    let m2 = rev_of(&db, "SELECT revision FROM messages WHERE content = 'second'");
    assert!(m2 > mrev);

    // The conversation (updated_at changed) is dirty too.
    assert_eq!(
        dirty_of(&db, &format!("SELECT dirty FROM conversations WHERE id = '{cid}'")),
        1
    );
    assert!(
        rev_of(&db, &format!("SELECT revision FROM conversations WHERE id = '{cid}'")) > 0
    );
}

#[test]
fn memory_notes_are_never_marked_for_sync() {
    let db = temp_db("memory-note");
    let cid = insert_conv(&db);
    db::insert_message(
        &db,
        cid,
        "memory".into(),
        "consolidation note".into(),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    assert_eq!(dirty_of(&db, "SELECT dirty FROM messages WHERE role = 'memory'"), 0);
    assert_eq!(rev_of(&db, "SELECT revision FROM messages WHERE role = 'memory'"), 0);
}

#[test]
fn bump_revision_is_monotonic() {
    let db = temp_db("monotonic");
    {
        let conn = db.0.lock().unwrap();
        let a = db::bump_revision_c(&conn).unwrap();
        let b = db::bump_revision_c(&conn).unwrap();
        let c = db::bump_revision_c(&conn).unwrap();
        assert!(a <= b && b <= c);
        assert!(b >= a);
    }
    // Persisted counter keeps monotonicity across a re-open.
    let mut p = std::env::temp_dir();
    p.push(format!("pi-chat-sync-monotonic-{}.sqlite", std::process::id()));
    let db2 = Db(Arc::new(Mutex::new(db::open(&p).unwrap())));
    {
        let conn = db.0.lock().unwrap();
        let last = conn
            .query_row("SELECT last_seen_revision FROM sync_state WHERE entity = ?", ["\u{0}revision"], |r| r.get::<_, i64>(0))
            .unwrap();
        let conn2 = db2.0.lock().unwrap();
        let next = db::bump_revision_c(&conn2).unwrap();
        assert!(next >= last);
    }
    let _ = std::fs::remove_file(&p);
}

#[test]
fn memory_file_mutations_mark_dirty_when_db_attached() {
    let db = temp_db("memdirty");
    let mem = temp_memory("memdirty", &db);
    mem.append("Likes sync.", "profile.md").unwrap();
    let dirty = dirty_of(
        &db,
        "SELECT dirty FROM memory_sync WHERE path = 'profile.md'",
    );
    assert_eq!(dirty, 1);
    // A delete keeps the tombstone row marked dirty.
    let _ = mem.apply_remote_delete("profile.md"); // remove file, then a real delete still marks
    mem.delete_file("profile.md").unwrap();
    assert_eq!(
        dirty_of(&db, "SELECT dirty FROM memory_sync WHERE path = 'profile.md'"),
        1
    );
}

#[test]
fn delete_conversation_records_a_tombstone() {
    let db = temp_db("tomb-rec");
    let cid = insert_conv(&db);
    {
        let conn = db.0.lock().unwrap();
        db::tombstone_conversation(&conn, &cid).unwrap();
        conn.execute("DELETE FROM conversations WHERE id = ?1", [&cid]).unwrap();
    }
    let n: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM sync_tombstones WHERE entity = 'conversations' AND id = ?1",
            [&cid],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(n, 1);
}

// ---------------------------------------------------------------------------
// tombstone handling + LWW on conversations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conversation_tombstone_hard_deletes_locally() {
    let db = temp_db("tomb-apply");
    let cid = insert_conv(&db);
    // Pre-seed a high revision so the tombstone's revision isn't filtered by LWW
    // (the tombstone carries a large revision computed by the deleting device).
    let rows = vec![json!({
        "id": cid, "title": "x", "revision": 100, "deleted_at": 9999,
        "created_at": 1, "updated_at": 1
    })];
    apply_conversations(&db, &rows).await.unwrap();
    let exists: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM conversations WHERE id = ?1", [&cid], |r| r.get(0)).unwrap()
    };
    assert_eq!(exists, 0);
    // High-water advanced past the tombstone so it's not re-fetched.
    let conn = db.0.lock().unwrap();
    let wm: i64 = conn
        .query_row("SELECT last_seen_revision FROM sync_state WHERE entity = 'conversations'", [], |r| r.get(0))
        .unwrap();
    assert!(wm >= 100);
}

#[tokio::test]
async fn lww_remote_wins_when_remote_revision_newer() {
    let db = temp_db("lww-remote");
    let cid = insert_conv(&db);
    // Remote revision exceeds the epoch-seeded local revision, so remote wins.
    let rows = vec![json!({
        "id": cid, "title": "REMOTE-title", "revision": 10_000_000_000_000_000i64,
        "created_at": 1, "updated_at": 2, "deleted_at": Value::Null
    })];
    apply_conversations(&db, &rows).await.unwrap();
    // REMOTE won because local (rev from insert, ~epoch ms) < remote.
    let title: String = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT title FROM conversations WHERE id = ?1", [&cid], |r| r.get(0)).unwrap()
    };
    assert_eq!(title, "REMOTE-title");
}

#[tokio::test]
async fn lww_local_wins_when_local_has_newer_content() {
    let db = temp_db("lww-local-win");
    // Local conversation edited AFTER the remote snapshot: local rev is higher.
    let id = uuid::Uuid::new_v4().to_string();
    let high_good;
    {
        let conn = db.0.lock().unwrap();
        let now = db::now_ms();
        db::bump_revision_c(&conn).unwrap();
        high_good = db::bump_revision_c(&conn).unwrap();
        conn.execute(
            "INSERT INTO conversations (id, title, created_at, updated_at, revision, dirty)
             VALUES (?1, 'LOCAL', ?2, ?2, ?3, 1)",
            rusqlite::params![id, now, high_good],
        )
        .unwrap();
    }
    // Remote is older (lower revision). Local wins.
    let remote_rows = vec![json!({
        "id": id, "title": "REMOTE", "revision": high_good - 1,
        "created_at": 0, "updated_at": 0, "deleted_at": Value::Null
    })];
    apply_conversations(&db, &remote_rows).await.unwrap();
    let title: String = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT title FROM conversations WHERE id = ?1", [&id], |r| r.get(0)).unwrap()
    };
    assert_eq!(title, "LOCAL"); // local wins
}

// ---------------------------------------------------------------------------
// (created_at, id) re-indexing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn message_reindex_orders_by_created_at_then_id() {
    let db = temp_db("reindex");
    let cid = insert_conv(&db);
    // Pre-existing messages with out-of-order created_at timestamps.
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO messages (id, conversation_id, role, \"index\", content, created_at, revision)
             VALUES ('m-old', ?1, 'user', 0, 'old', 5000, 1)",
            [&cid],
        ).unwrap();
        conn.execute(
            "INSERT INTO messages (id, conversation_id, role, \"index\", content, created_at, revision)
             VALUES ('m-new', ?1, 'user', 1, 'newer', 9000, 1)",
            [&cid],
        ).unwrap();
    }
    // Apply a remote message with an intermediate timestamp that must slot
    // between the two.
    let remote = vec![json!({
        "id": "m-mid", "conversation_id": cid, "role": "user", "content": "mid",
        "created_at": 7000, "revision": 2, "deleted_at": Value::Null
    })];
    apply_messages(&db, &remote).await.unwrap();

    let indexes: Vec<(i64, String)> = {
        let conn = db.0.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT \"index\", id FROM messages WHERE conversation_id = ?1 ORDER BY \"index\" ASC")
            .unwrap();
        let rows = stmt
            .query_map([&cid], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    let order: Vec<String> = indexes.into_iter().map(|(_, id)| id).collect();
    // old(5000) < mid(7000) < new(9000)
    assert_eq!(order, vec!["m-old".to_string(), "m-mid".to_string(), "m-new".to_string()]);
    // Indexes are contiguous 0..n
    let mut idxs: Vec<i64> = {
        let conn = db.0.lock().unwrap();
        let mut stmt = conn
            .prepare("SELECT \"index\" FROM messages WHERE conversation_id = ?1 ORDER BY \"index\" ASC")
            .unwrap();
        let rows = stmt.query_map([&cid], |r| r.get::<_, i64>(0)).unwrap();
        rows.map(|r| r.unwrap()).collect()
    };
    idxs.sort_unstable();
    assert_eq!(idxs, vec![0, 1, 2]);
}

// ---------------------------------------------------------------------------
// sync_state high-water mark
// ---------------------------------------------------------------------------

#[tokio::test]
async fn high_water_mark_never_regresses() {
    let db = temp_db("highwater");
    // Apply a row with revision 50.
    let rows1 = vec![json!({
        "id": "c1", "title": "a", "revision": 50, "created_at": 1, "updated_at": 1, "deleted_at": Value::Null
    })];
    apply_conversations(&db, &rows1).await.unwrap();
    let wm: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT last_seen_revision FROM sync_state WHERE entity = 'conversations'", [], |r| r.get(0)).unwrap()
    };
    assert_eq!(wm, 50);

    // A later pull only sees rows > 50; applying a SMALLER max must not lower it.
    let rows2 = vec![json!({
        "id": "c2", "title": "b", "revision": 5, "created_at": 1, "updated_at": 1, "deleted_at": Value::Null
    })];
    apply_conversations(&db, &rows2).await.unwrap();
    let wm2: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT last_seen_revision FROM sync_state WHERE entity = 'conversations'", [], |r| r.get(0)).unwrap()
    };
    assert_eq!(wm2, 50); // unchanged
}

// ---------------------------------------------------------------------------
// push / pull integration against the mock server
// ---------------------------------------------------------------------------

#[tokio::test]
async fn push_then_pull_syncs_end_to_end_against_mock() {
    let srv = MockRest::spawn();
    let db = temp_db("integ");
    let mem = temp_memory("integ", &db);

    // --- Local state destined for push ---
    let cid = insert_conv(&db);
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET provider_id = 'p-x' WHERE id = ?1",
            [&cid],
        )
        .unwrap();
    }
    db::insert_message(
        &db,
        cid.clone(),
        "user".into(),
        "push me".into(),
        None,
        None,
        None,
        None,
        None,
        Some("stop".into()),
    )
    .unwrap();
    mem.append("Durable fact about the user.", "profile.md").unwrap();
    assert!(db::pending_rows(&db).unwrap() > 0);

    let client = reqwest::Client::new();
    let base = srv.addr.clone();
    let cfg = temp_config("integ");
    let ctx = SyncCtx {
        client: &client,
        base_url: &base,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };

    // --- Push ---
    let progress = SyncState::new();
    let n = push(&ctx, "fake-token", &progress).await.unwrap();
    assert!(n > 0);
    // Server received our conversation + message + memory rows.
    let conv_posts = srv.posted("conversations");
    assert!(conv_posts.iter().any(|v| v["id"] == cid));
    // provider_id is a structural (plaintext) column and must round-trip on push.
    assert!(
        conv_posts
            .iter()
            .any(|v| v["id"] == cid && v["provider_id"] == "p-x"),
        "provider_id not pushed for conversation"
    );
    assert!(srv.posted("messages").iter().any(|v| v["content"] == "push me"));
    assert!(srv.posted("memory_files").iter().any(|v| v["path"] == "profile.md"));
    // Local dirty flags cleared after a successful push.
    assert_eq!(db::pending_rows(&db).unwrap(), 0);

    // --- Configure remote rows for pull, simulating device B's changes ---
    let remote_conv_id = "remote-conv".to_string();
    srv.set_get(
        "conversations",
        vec![json!({
            "id": remote_conv_id, "title": "From device B", "provider_id": "p-remote", "revision": 100,
            "created_at": 1, "updated_at": 2, "deleted_at": Value::Null
        })],
    );
    srv.set_get(
        "messages",
        vec![json!({
            "id": "remote-msg", "conversation_id": remote_conv_id, "role": "assistant",
            "content": "Hello from B", "created_at": 3, "revision": 101, "deleted_at": Value::Null
        })],
    );
    srv.set_get(
        "memory_files",
        vec![json!({
            "path": "goals.md", "content": "Ship sync.", "revision": 102, "deleted_at": Value::Null
        })],
    );

    // --- Pull ---
    pull(&ctx, "user-123", "fake-token").await.unwrap();

    // Conversation + message applied locally.
    let conv_title: String = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT title FROM conversations WHERE id = ?1", [&remote_conv_id], |r| r.get(0)).unwrap()
    };
    assert_eq!(conv_title, "From device B");
    let conv_prov: Option<String> = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT provider_id FROM conversations WHERE id = ?1", [&remote_conv_id], |r| r.get(0)).unwrap()
    };
    assert_eq!(conv_prov.as_deref(), Some("p-remote"), "provider_id not applied on pull");
    let msg: String = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT content FROM messages WHERE id = 'remote-msg'", [], |r| r.get(0)).unwrap()
    };
    assert_eq!(msg, "Hello from B");
    // Memory file content written to disk.
    assert!(mem.read("goals.md").unwrap().contains("Ship sync."));

    // High-water marks advanced.
    for entity in ["conversations", "messages", "memory"] {
        let conn = db.0.lock().unwrap();
        let wm: i64 = conn
            .query_row("SELECT last_seen_revision FROM sync_state WHERE entity = ?1", [entity], |r| r.get(0))
            .unwrap();
        assert!(wm > 0, "high-water missing for {entity}");
    }
}

/// Client-side encryption: push uploads ciphertext (Supabase never sees
/// plaintext) and pull restores plaintext into the local DB.
#[tokio::test]
async fn encryption_roundtrips_through_push_and_pull() {
    use crate::crypt;

    let srv = MockRest::spawn();
    let db = temp_db("enc");
    let memstate = temp_memory("enc", &db);
    let cid = insert_conv(&db);
    db::insert_message(
        &db,
        cid.clone(),
        "user".into(),
        "top-secret".into(),
        None,
        None,
        None,
        None,
        None,
        Some("stop".into()),
    )
    .unwrap();
    memstate.append("secret memory", "profile.md").unwrap();

    let key = crypt::new_key();
    let client = reqwest::Client::new();
    let base = srv.addr.clone();
    let cfg = temp_config("enc");
    let ctx = SyncCtx {
        client: &client,
        base_url: &base,
        anon: "anon",
        db: &db,
        memory: &memstate,
        config: &cfg,
        key: Some(key),
    };
    let progress = SyncState::new();
    push(&ctx, "tok", &progress).await.unwrap();

    // The server received ciphertext, not plaintext...
    let conv_title = srv.posted("conversations")[0]["title"].as_str().unwrap().to_string();
    assert!(crypt::looks_encrypted(&conv_title), "title must be ciphertext");
    let msg_content = srv.posted("messages")[0]["content"].as_str().unwrap().to_string();
    assert!(crypt::looks_encrypted(&msg_content), "message must be ciphertext");
    let mem_content = srv.posted("memory_files")[0]["content"].as_str().unwrap().to_string();
    assert!(crypt::looks_encrypted(&mem_content), "memory must be ciphertext");
    // ...and it decrypts back to the original with the key.
    assert_eq!(crypt::decrypt(&key, &msg_content).as_deref(), Some("top-secret"));

    // --- Pull: remote returns encrypted rows; local restores plaintext ---
    let rcid = "remote-conv".to_string();
    srv.set_get(
        "conversations",
        vec![json!({
            "id": rcid, "title": crypt::encrypt(&key, "From device B"),
            "revision": 100, "created_at": 1, "updated_at": 2, "deleted_at": Value::Null
        })],
    );
    srv.set_get(
        "messages",
        vec![json!({
            "id": "remote-msg", "conversation_id": rcid, "role": "assistant",
            "content": crypt::encrypt(&key, "Hello encrypted"), "created_at": 3,
            "revision": 101, "deleted_at": Value::Null
        })],
    );
    srv.set_get(
        "memory_files",
        vec![json!({
            "path": "goals.md", "content": crypt::encrypt(&key, "Encrypted goal"),
            "revision": 102, "deleted_at": Value::Null
        })],
    );
    pull(&ctx, "user-123", "tok").await.unwrap();

    let title: String = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT title FROM conversations WHERE id = ?1", [&rcid], |r| r.get(0)).unwrap()
    };
    assert_eq!(title, "From device B");
    let content: String = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT content FROM messages WHERE id = 'remote-msg'", [], |r| r.get(0)).unwrap()
    };
    assert_eq!(content, "Hello encrypted");
    assert!(memstate.read("goals.md").unwrap().contains("Encrypted goal"));
}

/// Push and pull the syncable user config (preferences/providers/overrides) as
/// a single LWW 'main' row. API keys are never part of the payload.
#[tokio::test]
async fn config_syncs_preferences_providers_and_overrides() {
    use crate::config::{ModelOverride, Provider};

    let srv = MockRest::spawn();
    let db = temp_db("cfg");
    let mem = temp_memory("cfg", &db);
    let cfg = temp_config("cfg");

    // Give the local config syncable content (plus a non-syncable field we must
    // keep) and mirror it into the sync row.
    let mut c = cfg.get();
    c.preferences = "Always answer in Spanish.".into();
    c.providers = vec![Provider {
        id: "prov-1".into(),
        name: "My Provider".into(),
        base_url: "https://example.com/v1".into(),
        default_model: Some("model-a".into()),
        catalog_id: Some("example-ai".into()),
    }];
    let mut ov = HashMap::new();
    ov.insert(
        "model-a".into(),
        ModelOverride {
            context_window: Some(128_000),
            ..Default::default()
        },
    );
    c.model_overrides = ov;
    c.shell_workspace_dir = "/tmp/ws".into();
    c.base_url = "https://legacy.example.com/v1".into();
    c.model = "legacy-model".into();
    cfg.set(c.clone()).unwrap();
    {
        let conn = db.0.lock().unwrap();
        db::sync_user_config(&conn, &cfg.get()).unwrap();
    }

    let client = reqwest::Client::new();
    let base = srv.addr.clone();
    let ctx = SyncCtx {
        client: &client,
        base_url: &base,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };

    // Push → the mirror receives the config row; no API key present.
    let progress = SyncState::new();
    push(&ctx, "tok", &progress).await.unwrap();
    let posts = srv.posted("user_config");
    assert!(!posts.is_empty(), "user_config row must be pushed");
    let payload: Value = serde_json::from_str(posts[0]["payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["preferences"], "Always answer in Spanish.");
    assert_eq!(payload["providers"][0]["id"], "prov-1");
    assert_eq!(payload["modelOverrides"]["model-a"]["contextWindow"], 128_000);
    // Legacy mirrors carry the effective provider for non-multi-provider configs.
    assert_eq!(payload["baseUrl"], "https://legacy.example.com/v1");
    assert_eq!(payload["model"], "legacy-model");
    assert!(db::pending_rows(&db).unwrap() == 0);

    // Pull: an EMPTY config arrives with a much higher revision (e.g. a blank
    // device's default pushed at a faster-clock revision). Because the local
    // config is non-empty, the apply is NON-DESTRUCTIVE: the configured provider,
    // preference, and legacy mirrors must NOT be wiped.
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main",
            "payload": json!({
                "preferences": "",
                "providers": [],
                "activeProviderId": "",
                "baseUrl": "",
                "model": "",
                "modelOverrides": {}
            }).to_string(),
            "revision": 9_000_000_000_000_i64,
            "deleted_at": Value::Null,
        })],
    );
    pull(&ctx, "user-123", "tok").await.unwrap();
    let after = cfg.get();
    // A configured device is never clobbered by an empty remote.
    assert_eq!(after.preferences, "Always answer in Spanish.");
    assert_eq!(after.providers.len(), 1);
    assert_eq!(after.providers[0].id, "prov-1");
    assert_eq!(after.base_url, "https://legacy.example.com/v1");
    assert_eq!(after.model, "legacy-model");
    assert_eq!(after.model_overrides.len(), 1);
    // A non-syncable field is untouched by a config pull.
    assert_eq!(after.shell_workspace_dir, "/tmp/ws");
    // ...and the pulled revision was still absorbed (high-water advances; the row
    // is recorded at the remote revision) so it isn't re-fetched forever.
    assert_eq!(rev_of(&db, "SELECT revision FROM user_config WHERE id='main'"), 9_000_000_000_000_i64);
}

/// A blank device holding a dirty EMPTY user_config row must not push it — that
/// would clobber the canonical config on the mirror under a clock-based revision
/// (any clock skew makes it "newer"). The row's dirty flag is cleared locally
/// instead, so nothing is posted and it isn't re-pushed every tick.
#[tokio::test]
async fn empty_user_config_row_is_never_pushed() {
    let srv = MockRest::spawn();
    let db = temp_db("emptycfg");
    let mem = temp_memory("emptycfg", &db);
    let cfg = temp_config("emptycfg");

    // Plant a dirty user_config row as a real blank device has one: an EMPTY
    // user-owned config with the factory-default base_url/model mirrors filled in
    // (exactly what `syncable_payload()` emits for a default config). The guard
    // must recognize this as "nothing to back up" and NOT push it.
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty) VALUES ('main', ?1, 1, 1)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload, revision = excluded.revision, dirty = 1",
            rusqlite::params![json!({
                "preferences": "", "providers": [], "activeProviderId": "",
                "baseUrl": crate::config::DEFAULT_BASE_URL,
                "model": crate::config::DEFAULT_MODEL,
                "modelOverrides": {}
            }).to_string()],
        )
        .unwrap();
    }
    let client = reqwest::Client::new();
    let ctx = SyncCtx {
        client: &client,
        base_url: &srv.addr,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    push(&ctx, "tok", &SyncState::new()).await.unwrap();

    // The empty config must NOT have been sent to the mirror...
    assert!(srv.posted("user_config").is_empty(), "empty user_config must not be pushed");
    // ...and its local dirty flag was cleared so it isn't re-pushed next tick.
    let dirty: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT dirty FROM user_config WHERE id='main'", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(dirty, 0);
}

/// A legacy (pre-multi-provider) config whose ONLY content is a custom base_url
/// and model must still be recognized as NON-empty and pushed — the empty-guard
/// must not swallow a meaningful custom endpoint (only factory-defaults are empty).
#[tokio::test]
async fn legacy_custom_config_is_still_pushed() {
    let srv = MockRest::spawn();
    let db = temp_db("legcfg");
    let mem = temp_memory("legcfg", &db);
    let cfg = temp_config("legcfg");

    // A legacy config: empty providers/active/prefs/overrides, but a custom
    // self-hosted base_url/model (the only thing this user owns).
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty) VALUES ('main', ?1, 1, 1)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload, revision = excluded.revision, dirty = 1",
            rusqlite::params![json!({
                "preferences": "", "providers": [], "activeProviderId": "",
                "baseUrl": "https://my.selfhosted.example/v1",
                "model": "my-custom-model",
                "modelOverrides": {}
            }).to_string()],
        )
        .unwrap();
    }
    let client = reqwest::Client::new();
    let ctx = SyncCtx {
        client: &client,
        base_url: &srv.addr,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    push(&ctx, "tok", &SyncState::new()).await.unwrap();

    // The legacy custom config MUST be pushed (it is real content).
    let posts = srv.posted("user_config");
    assert!(!posts.is_empty(), "legacy custom config must be pushed");
    let payload: Value =
        serde_json::from_str(posts[0]["payload"].as_str().unwrap()).unwrap();
    assert_eq!(payload["baseUrl"], "https://my.selfhosted.example/v1");
    assert_eq!(payload["model"], "my-custom-model");
}

/// A device that edited config OFFLINE at a STALER (lower) revision than the
/// canonical on the server must NOT push its row. The server's enforce_revision
/// trigger clamps only the revision column, not the payload, so a stale push
/// would overwrite the canonical payload while keeping the higher revision — the
/// clock-skew clobber for non-empty configs. The row stays dirty so the pull /
/// post-pull mirror can reconcile it.
#[tokio::test]
async fn stale_config_is_not_pushed_over_newer_server_row() {
    let srv = MockRest::spawn();
    let db = temp_db("stalecfg");
    let mem = temp_memory("stalecfg", &db);
    let cfg = temp_config("stalecfg");

    // The server already holds a NEWER canonical row (revision 5000).
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main",
            "payload": json!({
                "preferences": "Canonical", "providers": [], "activeProviderId": "",
                "baseUrl": crate::config::DEFAULT_BASE_URL,
                "model": crate::config::DEFAULT_MODEL,
                "modelOverrides": {}
            }).to_string(),
            "revision": 5000, "deleted_at": Value::Null,
        })],
    );

    // A device edited offline at a STALER revision (100 < 5000) with its own
    // non-empty content.
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty) VALUES ('main', ?1, 100, 1)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload, revision = excluded.revision, dirty = 1",
            rusqlite::params![json!({
                "preferences": "stale offline edit", "providers": [], "activeProviderId": "",
                "baseUrl": crate::config::DEFAULT_BASE_URL,
                "model": crate::config::DEFAULT_MODEL,
                "modelOverrides": {}
            }).to_string()],
        )
        .unwrap();
    }
    let client = reqwest::Client::new();
    let ctx = SyncCtx {
        client: &client,
        base_url: &srv.addr,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    push(&ctx, "tok", &SyncState::new()).await.unwrap();

    // The stale row must NOT clobber the canonical on the server.
    assert!(srv.posted("user_config").is_empty(), "stale config must not be pushed");
    // ...and stays dirty so the post-pull mirror can reconcile it.
    let dirty: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT dirty FROM user_config WHERE id='main'", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(dirty, 1, "a stale-skipped row must remain dirty (not lost)");
}

/// The read-before-write guard only blocks STALER rows: a device whose local edit
/// is NEWER than the server's row (higher revision) still pushes normally.
#[tokio::test]
async fn newer_config_is_still_pushed() {
    let srv = MockRest::spawn();
    let db = temp_db("newcfg");
    let mem = temp_memory("newcfg", &db);
    let cfg = temp_config("newcfg");

    // Server holds an older row (revision 100).
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main",
            "payload": "whatever",
            "revision": 100, "deleted_at": Value::Null,
        })],
    );

    // This device's edit is newer (revision 5000 > 100).
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty) VALUES ('main', ?1, 5000, 1)
             ON CONFLICT(id) DO UPDATE SET payload = excluded.payload, revision = excluded.revision, dirty = 1",
            rusqlite::params![json!({
                "preferences": "fresh edit", "providers": [], "activeProviderId": "",
                "baseUrl": crate::config::DEFAULT_BASE_URL,
                "model": crate::config::DEFAULT_MODEL,
                "modelOverrides": {}
            }).to_string()],
        )
        .unwrap();
    }
    let client = reqwest::Client::new();
    let ctx = SyncCtx {
        client: &client,
        base_url: &srv.addr,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    push(&ctx, "tok", &SyncState::new()).await.unwrap();

    // A NEWER config is pushed (guard only blocks staler rows).
    let posts = srv.posted("user_config");
    assert!(!posts.is_empty(), "newer config must be pushed");
}

/// Regression for the reviewer's critical finding: a fresh/second device that
/// syncs must ADOPT the authoritative config from the mirror, not push its own
/// (default) row over it under a clock-based revision. Simulates the do_sync
/// ordering (push → pull → post-pull mirror) for two devices sharing one mirror.
#[tokio::test]
async fn two_device_first_sync_adopts_canonical_config_not_clobber() {
    use crate::config::Provider;

    let srv = MockRest::spawn();

    // --- Device A: the device that actually holds the config. ---
    let db_a = temp_db("2dA");
    let mem_a = temp_memory("2dA", &db_a);
    let cfg_a = temp_config("2dA");
    {
        let mut c = cfg_a.get();
        c.preferences = "From device A".into();
        c.providers = vec![Provider {
            id: "prov-a".into(),
            name: "A".into(),
            base_url: "https://a/v1".into(),
            default_model: None,
            catalog_id: None,
        }];
        c.active_provider_id = "prov-a".into();
        cfg_a.set(c).unwrap();
    }
    let client_a = reqwest::Client::new();
    let ctx_a = SyncCtx {
        client: &client_a,
        base_url: &srv.addr,
        anon: "anon",
        db: &db_a,
        memory: &mem_a,
        config: &cfg_a,
        key: None,
    };
    // A round 1: push (nothing dirty for user_config) + pull (mirror empty) +
    // post-pull mirror → A's config row becomes dirty.
    push(&ctx_a, "tok", &SyncState::new()).await.unwrap();
    pull(&ctx_a, "u", "tok").await.unwrap();
    {
        let conn = db_a.0.lock().unwrap();
        db::sync_user_config(&conn, &cfg_a.get()).unwrap();
    }
    // A round 2: push carries A's config to the mirror.
    push(&ctx_a, "tok", &SyncState::new()).await.unwrap();
    pull(&ctx_a, "u", "tok").await.unwrap();
    let pushed_a = srv.posted("user_config").last().expect("A pushed config").clone();
    let pa: Value = serde_json::from_str(pushed_a["payload"].as_str().unwrap()).unwrap();
    assert_eq!(pa["preferences"], "From device A");
    let a_rev = pushed_a["revision"].as_i64().unwrap();
    // Simulate the server now holding A's row (the mock doesn't echo pushed rows
    // back to GET, so seed the pull source explicitly).
    srv.set_get("user_config", vec![pushed_a]);

    // --- Device B: a fresh phone with a default (empty) config, syncs after A. ---
    let db_b = temp_db("2dB");
    let mem_b = temp_memory("2dB", &db_b);
    let cfg_b = temp_config("2dB");
    let client_b = reqwest::Client::new();
    let ctx_b = SyncCtx {
        client: &client_b,
        base_url: &srv.addr,
        anon: "anon",
        db: &db_b,
        memory: &mem_b,
        config: &cfg_b,
        key: None,
    };
    // B's first sync, matching do_sync order: push first (nothing dirty for
    // user_config on a fresh device), then pull adopts A's canonical config, then
    // the post-pull mirror. Before the fix, B's empty config was pushed ahead of
    // the pull and clobbered A's on the server.
    push(&ctx_b, "tok", &SyncState::new()).await.unwrap();
    pull(&ctx_b, "u", "tok").await.unwrap();
    {
        let conn = db_b.0.lock().unwrap();
        db::sync_user_config(&conn, &cfg_b.get()).unwrap();
    }

    // B adopted A's config, not overwritten it.
    let after_b = cfg_b.get();
    assert_eq!(after_b.preferences, "From device A");
    assert_eq!(after_b.providers.len(), 1);
    assert_eq!(after_b.providers[0].id, "prov-a");
    // The adopted config must NOT be marked dirty (no re-upload / no clobber).
    let (b_payload, b_rev, b_dirty): (String, i64, i64) = {
        let conn = db_b.0.lock().unwrap();
        conn.query_row(
            "SELECT payload, revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(b_dirty, 0, "adopted config must not be dirty");
    assert_eq!(b_rev, a_rev, "adopted config keeps the applied revision");
    assert_eq!(
        serde_json::from_str::<Value>(&b_payload).unwrap()["preferences"],
        "From device A"
    );

    // A real local edit on B must bump above A's applied revision (so it wins on
    // the server instead of being a no-op under LWW).
    {
        let mut c = cfg_b.get();
        c.preferences = "Edited on B".into();
        cfg_b.set(c).unwrap();
    }
    {
        let conn = db_b.0.lock().unwrap();
        db::sync_user_config(&conn, &cfg_b.get()).unwrap();
    }
    let (brev, bdirty): (i64, i64) = {
        let conn = db_b.0.lock().unwrap();
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert!(brev > a_rev, "a local edit must bump above the applied remote revision");
    assert_eq!(bdirty, 1, "the edit must be marked dirty for push");
}

/// The locked-device probe detects an encrypted remote mirror (so a device
/// without the key refuses to sync instead of overwriting ciphertext).
#[tokio::test]
async fn probe_detects_encrypted_remote() {
    use crate::crypt;

    let srv = MockRest::spawn();
    let db = temp_db("probe");
    let mem = temp_memory("probe", &db);
    let client = reqwest::Client::new();
    let base = srv.addr.clone();
    let key = crypt::new_key();
    let cfg = temp_config("probe");
    let ctx = SyncCtx {
        client: &client,
        base_url: &base,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: Some(key),
    };

    // Plaintext remote → no encrypted sample.
    srv.set_get(
        "conversations",
        vec![json!({"id": "c1", "title": "plain", "revision": 1})],
    );
    assert_eq!(fetch_encrypted_sample(&ctx, "t").await.unwrap(), None);

    // Encrypted remote → the sample comes back and is decryptable with the key.
    srv.set_get(
        "conversations",
        vec![json!({"id": "c1", "title": crypt::encrypt(&key, "secret"), "revision": 1})],
    );
    let sample = fetch_encrypted_sample(&ctx, "t").await.unwrap();
    assert!(sample.is_some());
    assert_eq!(
        crypt::decrypt(&key, sample.as_deref().unwrap()).as_deref(),
        Some("secret")
    );
}

/// Advancing the reflection watermark must mark the conversation dirty and bump
/// its revision, or the sync push would never learn the chat was reflected and a
/// second device would re-reflect it (C2).
#[test]
fn set_last_reflected_index_marks_conversation_dirty() {
    let db = temp_db("watermark-dirty");
    let cid = insert_conv(&db);
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET dirty = 0, last_reflected_index = NULL WHERE id = ?1",
            [&cid],
        )
        .unwrap();
    }
    let rev_before = rev_of(
        &db,
        &format!("SELECT revision FROM conversations WHERE id = '{cid}'"),
    );
    db::set_last_reflected_index(&db, &cid, 3).unwrap();
    assert_eq!(
        dirty_of(
            &db,
            &format!("SELECT dirty FROM conversations WHERE id = '{cid}'")
        ),
        1
    );
    assert_eq!(
        rev_of(
            &db,
            &format!("SELECT last_reflected_index FROM conversations WHERE id = '{cid}'")
        ),
        3
    );
    let rev_after = rev_of(
        &db,
        &format!("SELECT revision FROM conversations WHERE id = '{cid}'"),
    );
    assert!(rev_after > rev_before, "watermark update must bump the revision");
}

/// The first sync must treat all pre-existing rows as new (back up full history),
/// and only once — a re-enable must not re-mark everything dirty (C1 + I4).
#[test]
fn seed_initial_sync_marks_full_history_dirty_once() {
    let db = temp_db("seed");
    let cid = insert_conv(&db);
    {
        let conn = db.0.lock().unwrap();
        let rev = db::bump_revision_c(&conn).unwrap();
        conn.execute(
            "INSERT INTO messages (id, conversation_id, role, \"index\", content, created_at, revision, dirty)
             VALUES ('m1', ?1, 'user', 0, 'hi', 1, ?2, 1)",
            rusqlite::params![cid, rev],
        )
        .unwrap();
    }
    db::seed_initial_sync(&db, "user-a", &["profile.md".to_string()]).unwrap();
    assert_eq!(
        dirty_of(&db, &format!("SELECT dirty FROM conversations WHERE id = '{cid}'")),
        1
    );
    assert_eq!(
        dirty_of(&db, "SELECT dirty FROM memory_sync WHERE path = 'profile.md'"),
        1
    );

    // Idempotent: after clearing everything, a second seed for the SAME account
    // must be a no-op.
    {
        let conn = db.0.lock().unwrap();
        conn.execute("UPDATE conversations SET dirty = 0", []).unwrap();
        conn.execute("UPDATE messages SET dirty = 0", []).unwrap();
        conn.execute("UPDATE memory_sync SET dirty = 0", []).unwrap();
    }
    db::seed_initial_sync(&db, "user-a", &["profile.md".to_string()]).unwrap();
    assert_eq!(
        dirty_of(&db, &format!("SELECT dirty FROM conversations WHERE id = '{cid}'")),
        0
    );
    assert_eq!(
        dirty_of(&db, "SELECT dirty FROM memory_sync WHERE path = 'profile.md'"),
        0
    );

    // A DIFFERENT account has its own sentinel, so it gets a fresh full seed.
    db::seed_initial_sync(&db, "user-b", &["profile.md".to_string()]).unwrap();
    assert_eq!(
        dirty_of(&db, &format!("SELECT dirty FROM conversations WHERE id = '{cid}'")),
        1
    );
}

/// Re-indexing a conversation after a pull must remap the reflection watermark to
/// the same logical message, or a cross-device reorder would shift its meaning
/// and cause double/skipped reflection (I3).
#[test]
fn reindex_preserves_reflection_watermark() {
    let db = temp_db("reindex-wm");
    let cid = insert_conv(&db);
    {
        let conn = db.0.lock().unwrap();
        // Out-of-order vs created_at; `index` reflects the current order.
        for (id, idx, created) in [("m1", 0i64, 300i64), ("m2", 1i64, 100i64), ("m3", 2i64, 200i64)] {
            conn.execute(
                "INSERT INTO messages (id, conversation_id, role, \"index\", content, created_at, revision, dirty)
                 VALUES (?1, ?2, 'user', ?3, 'x', ?4, 1, 0)",
                rusqlite::params![id, cid, idx, created],
            )
            .unwrap();
        }
        // Watermark points at index 1 (m2).
        conn.execute(
            "UPDATE conversations SET last_reflected_index = 1 WHERE id = ?1",
            [&cid],
        )
        .unwrap();
    }
    {
        let mut conn = db.0.lock().unwrap();
        let tx = conn.transaction().unwrap();
        reindex_messages(&tx, &cid).unwrap();
        tx.commit().unwrap();
    }
    // New order by (created_at,id): m2=0, m3=1, m1=2. Watermark follows m2 → 0.
    assert_eq!(
        rev_of(&db, &format!("SELECT last_reflected_index FROM conversations WHERE id = '{cid}'")),
        0
    );
    assert_eq!(rev_of(&db, "SELECT \"index\" FROM messages WHERE id = 'm2'"), 0);
    assert_eq!(rev_of(&db, "SELECT \"index\" FROM messages WHERE id = 'm3'"), 1);
    assert_eq!(rev_of(&db, "SELECT \"index\" FROM messages WHERE id = 'm1'"), 2);
}
