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

#[test]
fn ensure_configured_rejects_the_build_time_fallback_host() {
    // A binary built without a populated `.env` embeds the fallback URL; sync
    // must fail with an actionable message rather than a bogus request.
    assert!(ensure_configured("https://localhost.supabase.co").is_err());
    assert!(ensure_configured("https://localhost.supabase.co/").is_err());
    // A real project URL, or the test override, is fine.
    assert!(ensure_configured("https://abcdefgh.supabase.co").is_ok());
    assert!(ensure_configured("http://127.0.0.1:54321").is_ok());
}

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
                let mut headers: Vec<(String, String)> = Vec::new();
                for l in lines {
                    if let Some((k, v)) = l.split_once(':') {
                        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
                    }
                }
                for (k, v) in &headers {
                    if k == "content-length" {
                        clen = v.parse().unwrap_or(0);
                    }
                }
                // Mimic PostgREST: when asked for an exact count, report the
                // remote row total in the Content-Range header (`0-9/10`).
                let prefer_count = headers
                    .iter()
                    .any(|(k, v)| k == "prefer" && v.contains("count=exact"));
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

                let (status, resp_body, ctype, extra) = if path.contains("/auth/v1/token") {
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
                        String::new(),
                    )
                } else if method == "POST" {
                    match &table {
                        Some(t) => {
                            let rows: Vec<Value> =
                                serde_json::from_str(&body_str).unwrap_or_default();
                            p.lock().unwrap().entry(t.clone()).or_default().extend(rows);
                            (201, "[]".to_string(), "application/json", String::new())
                        }
                        None => (404, "{}".to_string(), "application/json", String::new()),
                    }
                } else {
                    match &table {
                        Some(t) => {
                            // Emulate PostgREST's `revision=gt.<n>` filter so the
                            // returned rows (and the Content-Range total) reflect
                            // the FILTERED set, not the whole table. Absent the
                            // parameter (revision-less probes such as
                            // fetch_user_config_remote) everything matches.
                            let since: i64 = path
                                .split("revision=gt.")
                                .nth(1)
                                .and_then(|s| s.split('&').next())
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(i64::MIN);
                            let rows: Vec<Value> = g
                                .lock()
                                .unwrap()
                                .get(t)
                                .cloned()
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|v| {
                                    v.get("revision").and_then(|r| r.as_i64()).unwrap_or(0) > since
                                })
                                .collect();
                            let js = serde_json::to_string(&rows).unwrap_or("[]".into());
                            let extra = if prefer_count {
                                let n = rows.len();
                                let range = if n > 0 {
                                    format!("0-{}/{}", n - 1, n)
                                } else {
                                    "*/0".into()
                                };
                                format!("Content-Range: {range}\r\n")
                            } else {
                                String::new()
                            };
                            (200, js, "application/json", extra)
                        }
                        None => (404, "{}".to_string(), "application/json", String::new()),
                    }
                };
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nConnection: close\r\nContent-Type: {ctype}\r\n{extra}Content-Length: {}\r\n\r\n{}",
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
    pull(&ctx, "user-123", "fake-token", None).await.unwrap();

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

/// Pull reports progress live: `pulled` climbs per applied page and
/// `pull_total` reflects the remote row counts (from the mock's
/// Content-Range), so the Settings UI can render a real bar instead of an
/// always-zero counter. Regression test for "the sync UI doesn't show pull
/// progress": previously the total was only added after the entire pull
/// finished — and then wiped by `set_success` moments later — so the 1s status
/// poll never observed a non-zero value.
#[tokio::test]
async fn pull_reports_live_pulled_and_pull_total() {
    let srv = MockRest::spawn();
    srv.set_get(
        "conversations",
        vec![json!({
            "id": "r-conv", "title": "From B", "provider_id": "p-remote",
            "revision": 100, "created_at": 1, "updated_at": 2, "deleted_at": Value::Null
        })],
    );
    srv.set_get(
        "messages",
        vec![json!({
            "id": "r-msg", "conversation_id": "r-conv", "role": "assistant",
            "content": "Hi", "created_at": 3, "revision": 101, "deleted_at": Value::Null
        })],
    );
    let db = temp_db("pull-progress");
    let mem = temp_memory("pull-progress", &db);
    let cfg = temp_config("pull-progress");
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
    let progress = SyncState::for_url(&srv.addr, "anon");

    let n = pull(&ctx, "user-123", "tok", Some(&progress)).await.unwrap();
    assert_eq!(n, 2);

    // Live counters: each applied page is counted as it lands...
    let (_, _, _, _, pushed, pulled, pull_total, _) = progress.snapshot();
    assert_eq!(pushed, 0);
    assert_eq!(pulled, 2);
    // ...and the remote totals (1 conversation + 1 message) became the bar's
    // denominator, so the UI can show `↓2/2` at 100%.
    assert_eq!(pull_total, 2);
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
    pull(&ctx, "user-123", "tok", None).await.unwrap();

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
    pull(&ctx, "user-123", "tok", None).await.unwrap();
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
    // ...and the row was recorded ABOVE the (empty, faster-clock) remote
    // revision and DIRTY, so the configured device pushes its non-empty config
    // back and the empty row never freezes it out (the pull high-water still
    // advances past the remote, so the row is not re-fetched every tick).
    let (rev, dirty) = (
        rev_of(&db, "SELECT revision FROM user_config WHERE id='main'"),
        rev_of(&db, "SELECT dirty FROM user_config WHERE id='main'"),
    );
    assert!(rev > 9_000_000_000_000_i64, "must exceed the remote revision");
    assert_eq!(dirty, 1, "diverged content must be pushed back");
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

/// The remaining stale guard: when the server's payload can't be MERGED (an
/// unreadable/legacy payload, or ciphertext we lack the key for), we can't
/// verify what a push would drop — so a staler local row is refused rather than
/// risk a destructive overwrite. (A stale row with a MERGEABLE server payload is
/// now pushed as a union instead; see `stale_dirty_edit_is_pushed_as_a_union`.)
#[tokio::test]
async fn stale_push_is_refused_when_the_server_payload_is_unreadable() {
    let srv = MockRest::spawn();
    let db = temp_db("stalecfg");
    let mem = temp_memory("stalecfg", &db);
    let cfg = temp_config("stalecfg");

    // The server already holds a NEWER canonical row (revision 5000), but its
    // payload is not mergeable JSON (a legacy/garbage blob).
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": "not-mergeable-json", "revision": 5000,
            "deleted_at": Value::Null,
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

/// A pulled user-config row must not ABSORB unpushed local edits. When the live
/// config differs from what the remote actually sent, the local row is stored
/// DIRTY at a revision above the remote's so the next push uploads it —
/// otherwise the edit is marked synced without ever leaving the device
/// (observed in production: a capability toggle sat "clean" at the remote's
/// revision while the server kept the old payload forever).
#[tokio::test]
async fn pulled_config_does_not_absorb_unpushed_local_edits() {
    // Live config carries an edit the remote has never seen, and there is no
    // local row yet (a fresh device, or one whose row is below the remote's).
    let cfg = temp_config("absorb");
    {
        let mut c = cfg.get();
        let mut overrides = c.model_overrides.clone();
        overrides.insert(
            "m-1".into(),
            crate::config::ModelOverride {
                output_modalities: Some(vec!["text".into()]),
                ..Default::default()
            },
        );
        c.model_overrides = overrides;
        c.preferences = "Talk like me.".into();
        cfg.set(c).unwrap();
    }
    let db = temp_db("absorb");
    let mem = temp_memory("absorb", &db);
    let srv = MockRest::spawn();
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
    // Remote row (revision 100) WITHOUT the local override/preferences.
    let row = json!({
        "id": "main",
        "payload": json!({
            "activeProviderId": "", "baseUrl": "", "model": "",
            "modelOverrides": {}, "preferences": "", "providers": [],
        }).to_string(),
        "revision": 100,
        "deleted_at": Value::Null,
    });
    apply_user_config(&ctx, std::slice::from_ref(&row)).unwrap();

    // The row exists at a revision ABOVE the remote's and is DIRTY: the edit
    // will be pushed on the next tick, not silently marked synced.
    let (payload, rev, dirty): (String, i64, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT payload, revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    assert_eq!(dirty, 1);
    assert!(rev > 100, "revision must exceed the remote's");
    assert!(payload.contains("outputModalities"), "local edit must survive");
    assert!(payload.contains("Talk like me"), "local edit must survive");
}

/// A row stuck at the server's revision with a never-pushed payload (the
/// absorption bug's residue, equal-revision — invisible to the pull high-water)
/// is repaired by the post-pull reconcile: it is re-marked dirty at a revision
/// above the server's so the push guard lets the next push upload it.
#[tokio::test]
async fn reconcile_redirties_row_diverged_from_server_at_equal_revision() {
    let srv = MockRest::spawn();
    // Server holds the OLD payload at revision 100.
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main",
            "payload": json!({
                "activeProviderId": "", "baseUrl": "", "model": "",
                "modelOverrides": {}, "preferences": "", "providers": [],
            }).to_string(),
            "revision": 100,
        })],
    );
    let db = temp_db("reconcile");
    let mem = temp_memory("reconcile", &db);
    let cfg = temp_config("reconcile");
    let client = reqwest::Client::new();
    // Local row stuck: NEW payload at the SAME revision, clean. The live config has
    // to AGREE with it — that is the production invariant (every edit path mirrors)
    // — or the pre-merge mirror would read the planted override as a deliberate
    // CLEAR and this test would pass for the wrong reason.
    {
        let mut c = cfg.get();
        c.model_overrides.insert(
            "m-1".into(),
            crate::config::ModelOverride {
                output_modalities: Some(vec!["text".into()]),
                ..Default::default()
            },
        );
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 0)",
            rusqlite::params![json!({
                "activeProviderId": "", "baseUrl": "", "model": "",
                "modelOverrides": {"m-1": {"outputModalities": ["text"]}},
                "preferences": "", "providers": [],
            }).to_string()],
        )
        .unwrap();
    }
    let ctx = SyncCtx {
        client: &client,
        base_url: &srv.addr,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    reconcile_user_config(&ctx, "tok").await.unwrap();

    let (rev, dirty): (i64, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(dirty, 1);
    assert!(rev > 100, "revision must exceed the server's so the push guard passes");
    // …and the never-pushed payload it was written for is still there.
    let payload: String = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT payload FROM user_config WHERE id = 'main'", [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert!(payload.contains("m-1"), "the override survives: {payload}");
    assert!(cfg.get().model_overrides.contains_key("m-1"));
}

/// A row that MATCHES the server is left alone by the reconcile — no spurious
/// re-dirtying and re-pushing every tick.
#[tokio::test]
async fn reconcile_leaves_matching_row_clean() {
    let payload = json!({
        "activeProviderId": "", "baseUrl": "", "model": "",
        "modelOverrides": {}, "preferences": "", "providers": [],
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": payload.clone(), "revision": 100,
        })],
    );
    let db = temp_db("reconcile-clean");
    let mem = temp_memory("reconcile-clean", &db);
    let cfg = temp_config("reconcile-clean");
    let client = reqwest::Client::new();
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 0)",
            [&payload],
        )
        .unwrap();
    }
    let ctx = SyncCtx {
        client: &client,
        base_url: &srv.addr,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    reconcile_user_config(&ctx, "tok").await.unwrap();

    let (rev, dirty): (i64, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(dirty, 0);
    assert_eq!(rev, 100);
}

/// A strictly-newer remote value WINS the conflicting key and is adopted, with
/// nothing pushed — the device converges instead of sitting on its older value
/// forever, and there is no fight because it contributes nothing the remote lacks.
#[tokio::test]
async fn a_newer_remote_value_wins_a_conflict_without_a_push() {
    let prov_b = json!({ "id": "prov-b", "name": "PROV-B", "baseUrl": "https://b.example/v1",
                         "defaultModel": null, "catalogId": null });
    let local = json!({
        "v": 2, "preferences": "Configured on A", "providers": [],
        "activeProviderId": "", "baseUrl": "", "model": "", "modelOverrides": {},
        "revs": { "preferences": 100 },
    })
    .to_string();
    let remote = json!({
        "v": 2, "preferences": "Canonical from B", "providers": [prov_b],
        "activeProviderId": "", "baseUrl": "", "model": "", "modelOverrides": {},
        "revs": { "preferences": 5000, "provider:prov-b": 5000 },
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": remote, "revision": 5000,
            "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("newer-value");
    let mem = temp_memory("newer-value", &db);
    let cfg = temp_config("newer-value");
    {
        // Production invariant: the live config and the row agree, because
        // every edit path mirrors. A fixture that disagrees would make the
        // pre-merge mirror read the planted row as a deliberate CLEAR.
        let mut c = cfg.get();
        c.preferences = "Configured on A".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 0)",
            [&local],
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
    apply_user_config(
        &ctx,
        &[json!({
            "id": "main", "payload": remote, "revision": 5000,
            "deleted_at": Value::Null,
        })],
    )
    .unwrap();
    reconcile_user_config(&ctx, "tok").await.unwrap();

    // Converged on the newer values…
    let live = cfg.get();
    assert_eq!(live.preferences, "Canonical from B");
    assert_eq!(live.providers.len(), 1);
    // …we contributed nothing, so nothing is pushed and the row is clean AT the
    // remote's revision.
    assert!(srv.posted("user_config").is_empty(), "nothing may be pushed");
    let (rev, dirty): (i64, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!((rev, dirty), (5000, 0));
}

/// Reconcile never pushes (plaintext or re-encrypted) over a server payload it
/// cannot decrypt — the locked-device contract. A device without the key that
/// finds an encrypted user_config row leaves the row alone rather than risk
/// destroying ciphertext.
#[tokio::test]
async fn reconcile_skips_undecryptable_server_payload() {
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": "enc:v1:not-a-real-ciphertext", "revision": 100,
        })],
    );
    let db = temp_db("reconcile-enc");
    let mem = temp_memory("reconcile-enc", &db);
    let cfg = temp_config("reconcile-enc");
    let client = reqwest::Client::new();
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 0)",
            rusqlite::params![json!({
                "activeProviderId": "", "baseUrl": "", "model": "",
                "modelOverrides": {}, "preferences": "local", "providers": [],
            }).to_string()],
        )
        .unwrap();
    }
    let ctx = SyncCtx {
        client: &client,
        base_url: &srv.addr,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None, // no key on this device — must not push over ciphertext
    };
    reconcile_user_config(&ctx, "tok").await.unwrap();

    let (rev, dirty): (i64, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(dirty, 0);
    assert_eq!(rev, 100);
    assert!(srv.posted("user_config").is_empty());
}

/// v2 policy, pinned end-to-end: a stale OFFLINE edit still goes out, but what it
/// contributes is decided PER KEY by revision. A key only it has is added; a key
/// BOTH sides set resolves to the NEWER revision — that is what a per-key version
/// means. The push lands above the server's revision, so it is never refused as
/// stale. (The old whole-row rule was "a local edit always wins", which clobbered
/// the other device's newer value rather than converging with it.)
#[tokio::test]
async fn a_stale_dirty_edit_pushes_what_it_alone_has() {
    let prov_a = json!({ "id": "prov-a", "name": "PROV-A", "baseUrl": "https://a.example/v1",
                         "defaultModel": null, "catalogId": null });
    let prov_b = json!({ "id": "prov-b", "name": "PROV-B", "baseUrl": "https://b.example/v1",
                         "defaultModel": null, "catalogId": null });
    // Ours: a provider only we have, plus a preference set at 100.
    let local = json!({
        "v": 2, "preferences": "Offline edit on A", "providers": [prov_a],
        "activeProviderId": "", "baseUrl": "", "model": "", "modelOverrides": {},
        "revs": { "preferences": 100, "provider:prov-a": 100 },
    })
    .to_string();
    // The canonical: its own provider, and the same preference set LATER (5000).
    let server = json!({
        "v": 2, "preferences": "Canonical from B", "providers": [prov_b],
        "activeProviderId": "", "baseUrl": "", "model": "", "modelOverrides": {},
        "revs": { "preferences": 5000, "provider:prov-b": 5000 },
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": server, "revision": 5000,
            "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("stale-edit");
    let mem = temp_memory("stale-edit", &db);
    let cfg = temp_config("stale-edit");
    {
        // Production invariant: the live config and the row agree, because
        // every edit path mirrors. A fixture that disagrees would make the
        // pre-merge mirror read the planted row as a deliberate CLEAR.
        let mut c = cfg.get();
        c.preferences = "Offline edit on A".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 1)",
            [&local],
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

    let posts = srv.posted("user_config");
    assert!(!posts.is_empty(), "the provider only we have must go out");
    let sent = &posts[posts.len() - 1];
    let payload: Value = serde_json::from_str(sent["payload"].as_str().unwrap()).unwrap();
    assert_eq!(
        payload["preferences"], "Canonical from B",
        "the NEWER value wins the conflicting key"
    );
    let ids: Vec<String> = payload["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids.contains(&"prov-a".to_string()) && ids.contains(&"prov-b".to_string()),
        "both providers survive: {ids:?}"
    );
    assert!(
        sent["revision"].as_i64().unwrap() > 5000,
        "the push lands above the canonical revision"
    );
}

/// The non-fight skip leaves a row's revision below the pull high-water; the
/// NEXT local edit must still land ABOVE the watermark (the highest seen server
/// revision) so the push guard accepts it — otherwise a slow-clock device
/// wedges its config edits forever.
#[tokio::test]
async fn config_edit_after_non_fight_skip_lands_above_watermark() {
    let db = temp_db("wm-bump");
    let cfg = temp_config("wm-bump");
    // Row sits at 100 (below the pull watermark of 5000 — the non-fight skip
    // state). A slow-clock device's now() would be < 5000, so bumping from the
    // row revision alone would produce a refuse-able revision.
    {
        let conn = db.0.lock().unwrap();
        let payload = json!({
            "activeProviderId": "", "baseUrl": "", "model": "",
            "modelOverrides": {}, "preferences": "", "providers": [],
        })
        .to_string();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 0)",
            [&payload],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sync_state(entity, last_seen_revision) VALUES ('user_config', 5000)",
            [],
        )
        .unwrap();
    }
    let mut c = cfg.get();
    c.preferences = "New edit after the skip".into();
    cfg.set(c).unwrap();
    let conn = db.0.lock().unwrap();
    db::sync_user_config(&conn, &cfg.get()).unwrap();
    let (rev, dirty): (i64, i64) = {
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(dirty, 1);
    assert!(rev > 5000, "edit must land above the pull high-water");
}

/// Absorption is per key: a remote key we never set is adopted even while our own
/// NEWER key is kept — the "hybrid" the post-pull mirror then folds upward into a
/// dirty row so the next push carries both.
#[tokio::test]
async fn absorbing_a_remote_key_keeps_our_newer_one() {
    let prov_b = json!({ "id": "prov-b", "name": "PROV-B", "baseUrl": "https://b.example/v1",
                         "defaultModel": null, "catalogId": null });
    let local = json!({
        "v": 2, "preferences": "Local preference", "providers": [],
        "activeProviderId": "", "baseUrl": "", "model": "", "modelOverrides": {},
        "revs": { "preferences": 9000 },
    })
    .to_string();
    let remote = json!({
        "v": 2, "preferences": "", "providers": [prov_b],
        "activeProviderId": "prov-b", "baseUrl": "https://b.example/v1",
        "model": "m-b", "modelOverrides": {},
        "revs": { "provider:prov-b": 500, "activeProviderId": 500, "baseUrl": 500 },
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": remote, "revision": 500,
            "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("per-key-absorb");
    let mem = temp_memory("per-key-absorb", &db);
    let cfg = temp_config("per-key-absorb");
    {
        // Production invariant: the live config and the row agree, because
        // every edit path mirrors. A fixture that disagrees would make the
        // pre-merge mirror read the planted row as a deliberate CLEAR.
        let mut c = cfg.get();
        c.preferences = "Local preference".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 9000, 0)",
            [&local],
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
    apply_user_config(
        &ctx,
        &[json!({
            "id": "main", "payload": remote, "revision": 500,
            "deleted_at": Value::Null,
        })],
    )
    .unwrap();

    let live = cfg.get();
    assert_eq!(live.providers.len(), 1, "the remote's provider is adopted");
    assert_eq!(
        live.preferences, "Local preference",
        "our strictly-newer key is kept"
    );
    // We still hold something the remote lacks, so the row is re-armed dirty above
    // the remote's revision for the next push.
    let (rev, dirty): (i64, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(dirty, 1);
    assert!(rev > 500, "re-armed above the remote's revision");
}

/// End-to-end guard for the combined post-pull config tail
/// (`reconcile_config_after_pull`): the mirror and the reconcile run in the
/// order `do_sync` uses them and, together, repair a row stuck at the server's
/// revision (the absorption residue) so the real push path then uploads it.
/// This pins the one seam the individual unit tests can't reach — `do_sync`
/// itself needs an `AppHandle`.
///
/// The planted row payload is built FROM the live config, so the mirror is a
/// no-op by construction and the RECONCILE is necessarily the thing that
/// re-dirties the row (removing the reconcile call would fail this test).
#[tokio::test]
async fn post_pull_tail_repairs_stuck_row_and_pushes_it() {
    let db = temp_db("tail");
    let mem = temp_memory("tail", &db);
    let cfg = temp_config("tail");
    // Live config carries content the server's row LACKS (the server row is
    // empty), so the reconcile's additive rule fires.
    let mut c = cfg.get();
    c.preferences = "Local newer".into();
    cfg.set(c).unwrap();
    let live = cfg.get();
    let local_new = live.syncable_payload().to_string();
    let mut server_cfg = live.clone();
    server_cfg.preferences = String::new();
    server_cfg.providers.clear();
    let server_old = server_cfg.syncable_payload().to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": server_old, "revision": 100,
            "deleted_at": Value::Null,
        })],
    );
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 0)",
            [&local_new],
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
    // The tail: mirror (no-op — row equals the live config) then reconcile
    // (equal-revision divergence → re-dirty above the server).
    reconcile_config_after_pull(&ctx, "tok").await.unwrap();
    let (rev, dirty): (i64, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(dirty, 1);
    assert!(rev > 100);
    // ...and the ordinary push path then uploads the repaired row.
    push(&ctx, "tok", &SyncState::new()).await.unwrap();
    let posts = srv.posted("user_config");
    assert!(!posts.is_empty(), "repaired row must push");
    assert!(posts[posts.len() - 1]["payload"]
        .as_str()
        .unwrap()
        .contains("Local newer"));
}

/// The push UNION invariant: a device whose config is non-empty but PARTIAL
/// (it sets one field, leaves others unset) must not drop the server's content.
/// Here we have a preference but no provider; the server has a provider and no
/// preference. The push must carry both.
#[tokio::test]
async fn push_unions_with_server_and_never_drops_its_content() {
    let local = json!({
        "activeProviderId": "", "baseUrl": "", "model": "",
        "modelOverrides": {}, "preferences": "Only on this device", "providers": [],
    })
    .to_string();
    let server = json!({
        "activeProviderId": "prov-s", "baseUrl": "https://s.example/v1",
        "model": "m-s", "modelOverrides": {},
        "preferences": "",
        "providers": [{
            "id": "prov-s", "name": "S", "baseUrl": "https://s.example/v1",
            "defaultModel": null, "catalogId": null,
        }],
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": server, "revision": 900,
            "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("union-push");
    let mem = temp_memory("union-push", &db);
    let cfg = temp_config("union-push");
    {
        let mut c = cfg.get();
        c.preferences = "Only on this device".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 800, 1)",
            [&local],
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

    let posts = srv.posted("user_config");
    assert!(!posts.is_empty(), "the partial config must push its union");
    let sent: Value = serde_json::from_str(posts[posts.len() - 1]["payload"].as_str().unwrap())
        .unwrap();
    assert_eq!(sent["preferences"], "Only on this device", "our field wins");
    assert_eq!(sent["providers"][0]["id"], "prov-s", "server field survives");
}

/// A configured device stranded below a content-POOR newer server row heals
/// itself: the reconcile's additive rule re-dirties it and the push restores
/// the union, so the server's own fields survive too.
#[tokio::test]
async fn stranded_richer_row_heals_and_pushes_the_union() {
    // Local is rich; the server row is newer but missing our content and holds
    // one provider we lack.
    let local = json!({
        "activeProviderId": "", "baseUrl": "", "model": "",
        "modelOverrides": {}, "preferences": "Local config", "providers": [],
    })
    .to_string();
    let server = json!({
        "activeProviderId": "prov-s", "baseUrl": "https://s.example/v1",
        "model": "m-s", "modelOverrides": {},
        "preferences": "",
        "providers": [{
            "id": "prov-s", "name": "S", "baseUrl": "https://s.example/v1",
            "defaultModel": null, "catalogId": null,
        }],
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": server, "revision": 1000,
            "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("heal");
    let mem = temp_memory("heal", &db);
    let cfg = temp_config("heal");
    {
        let mut c = cfg.get();
        c.preferences = "Local config".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 0)",
            [&local],
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
    // The reconcile sees content the server lacks → re-dirty above it.
    reconcile_user_config(&ctx, "tok").await.unwrap();
    let rev: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT revision FROM user_config WHERE id='main'", [], |r| r.get(0))
            .unwrap()
    };
    assert!(rev > 1000, "must be re-armed above the server");
    // The push restores it — as a union that keeps the server's provider.
    push(&ctx, "tok", &SyncState::new()).await.unwrap();
    let posts = srv.posted("user_config");
    assert!(!posts.is_empty());
    let sent: Value = serde_json::from_str(posts[posts.len() - 1]["payload"].as_str().unwrap())
        .unwrap();
    assert_eq!(sent["preferences"], "Local config");
    assert_eq!(sent["providers"][0]["id"], "prov-s");

    // Convergence / no flap: with the server now holding the union, a second
    // reconcile must NOT re-arm the row again.
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": sent.to_string(), "revision": rev + 1,
            "deleted_at": Value::Null,
        })],
    );
    {
        // Clear dirty as a successful push would, and settle the row payload to
        // the union the push uploaded.
        let conn = db.0.lock().unwrap();
        conn.execute("UPDATE user_config SET dirty = 0", []).unwrap();
    }
    reconcile_user_config(&ctx, "tok").await.unwrap();
    let rev_after: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT revision FROM user_config WHERE id='main'", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(rev_after, rev, "no further escalation once the server has our content");
}

/// A pure conflict (both sides set the same key, different values) converges on
/// the NEWER one — and does NOT push, so two configured devices can never start a
/// per-tick fight over it. (The old rule left both sides on their own value
/// forever, which is divergence, not convergence.)
#[tokio::test]
async fn a_pure_conflict_converges_on_the_newer_value() {
    let local = json!({
        "v": 2, "preferences": "Value A", "providers": [],
        "activeProviderId": "", "baseUrl": "", "model": "", "modelOverrides": {},
        "revs": { "preferences": 100 },
    })
    .to_string();
    let server = json!({
        "v": 2, "preferences": "Value B", "providers": [],
        "activeProviderId": "", "baseUrl": "", "model": "", "modelOverrides": {},
        "revs": { "preferences": 5000 },
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": server, "revision": 5000,
            "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("conflict");
    let mem = temp_memory("conflict", &db);
    let cfg = temp_config("conflict");
    {
        // Production invariant: the live config and the row agree, because
        // every edit path mirrors. A fixture that disagrees would make the
        // pre-merge mirror read the planted row as a deliberate CLEAR.
        let mut c = cfg.get();
        c.preferences = "Value A".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 0)",
            [&local],
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
    reconcile_user_config(&ctx, "tok").await.unwrap();

    assert_eq!(cfg.get().preferences, "Value B", "the newer value wins");
    let (rev, dirty): (i64, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!((rev, dirty), (5000, 0), "clean at the remote's revision");
    assert!(
        srv.posted("user_config").is_empty(),
        "no fight — we add nothing"
    );
}

/// The empty-guard ignores the `model` mirror on the default endpoint: a device
/// that merely picked a model (no provider/preferences bound to it) carries
/// nothing portable and must read as empty — this is the phone-blanked-the-
/// desktop shape. A CUSTOM base_url still reads as non-empty (legacy configs).
#[test]
fn empty_guard_ignores_model_mirror_but_keeps_legacy_base_url() {
    let model_only = json!({
        "preferences": "", "providers": [], "activeProviderId": "",
        "baseUrl": crate::config::DEFAULT_BASE_URL,
        "model": "some-picked-model",
        "modelOverrides": {},
    })
    .to_string();
    assert!(
        syncable_payload_is_empty(&model_only),
        "a model-only config on the default endpoint is empty"
    );

    let legacy = json!({
        "preferences": "", "providers": [], "activeProviderId": "",
        "baseUrl": "https://my.selfhosted.example/v1",
        "model": "my-custom-model",
        "modelOverrides": {},
    })
    .to_string();
    assert!(
        !syncable_payload_is_empty(&legacy),
        "a legacy custom base_url is still real content"
    );
}

/// The union must keep the server's active provider when we have a provider
/// list but no active selection of our own — otherwise our empty id would blank
/// the server's choice (and, for a providers-without-active device, never
/// converge). A server id that isn't in the merged list is not adopted.
#[tokio::test]
async fn union_keeps_server_active_provider_when_local_has_none() {
    let local = json!({
        "activeProviderId": "", "baseUrl": "", "model": "",
        "modelOverrides": {},
        "preferences": "",
        "providers": [{
            "id": "prov-l", "name": "L", "baseUrl": "https://l.example/v1",
            "defaultModel": null, "catalogId": null,
        }],
    })
    .to_string();
    let server = json!({
        "activeProviderId": "prov-s", "baseUrl": "https://s.example/v1",
        "model": "m-s", "modelOverrides": {},
        "preferences": "",
        "providers": [{
            "id": "prov-s", "name": "S", "baseUrl": "https://s.example/v1",
            "defaultModel": null, "catalogId": null,
        }],
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": server, "revision": 900,
            "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("active-union");
    let mem = temp_memory("active-union", &db);
    let cfg = temp_config("active-union");
    {
        // The row carries the provider list via its payload; the live config is
        // left default (the assertion is on what gets pushed).
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 800, 1)",
            [&local],
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

    let posts = srv.posted("user_config");
    assert!(!posts.is_empty(), "the union must push");
    let sent: Value = serde_json::from_str(posts[posts.len() - 1]["payload"].as_str().unwrap())
        .unwrap();
    assert_eq!(sent["activeProviderId"], "prov-s", "server selection survives");
    assert_eq!(sent["providers"].as_array().unwrap().len(), 2, "both providers");
}

/// Regression for the phone wedge, part 1: absorption is ADDITIVE, so it must not
/// be gated on revision order. A blank row that merely got CREATED at a revision
/// above the server's used to parse the canonical config, refuse it as "older"
/// and discard it — permanently, since the high-water then advanced past the row.
#[tokio::test]
async fn pull_absorbs_a_blank_row_that_runs_ahead_of_the_server() {
    let blank = json!({
        "activeProviderId": "", "baseUrl": "https://api.fireworks.ai/inference/v1",
        "model": "deepseek-v4-flash-0731", "modelOverrides": {},
        "preferences": "", "providers": [],
    })
    .to_string();
    let canonical = json!({
        "activeProviderId": "prov-s", "baseUrl": "https://s.example/v1",
        "model": "m-s", "modelOverrides": {},
        "preferences": "Talk like me.\n",
        "providers": [{
            "id": "prov-s", "name": "S", "baseUrl": "https://s.example/v1",
            "defaultModel": null, "catalogId": null,
        }],
    })
    .to_string();
    let db = temp_db("absorb-ahead");
    let mem = temp_memory("absorb-ahead", &db);
    let cfg = temp_config("absorb-ahead");
    {
        let conn = db.0.lock().unwrap();
        // Local row is NEWER than the server's revision, but carries nothing.
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty) VALUES ('main', ?1, 1000, 0)",
            [&blank],
        )
        .unwrap();
    }
    let client = reqwest::Client::new();
    let base = "http://127.0.0.1:1".to_string(); // unused: no request on this path
    let ctx = SyncCtx {
        client: &client,
        base_url: &base,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    apply_user_config(
        &ctx,
        &[json!({
            "id": "main", "payload": canonical, "revision": 900,
            "deleted_at": Value::Null,
        })],
    )
    .unwrap();

    let live = cfg.get();
    assert_eq!(
        live.preferences, "Talk like me.\n",
        "a NEWER blank row must still absorb the canonical config"
    );
    assert_eq!(live.providers.len(), 1);
    assert_eq!(live.active_provider_id, "prov-s");
}

/// Regression for the phone wedge, part 2 — the exact production shape: the blank
/// row sits ABOVE the server's revision AND the high-water has already passed the
/// server row, so the pull can never fetch it again. Only the reconcile can heal
/// it, because it re-reads the row by id. The live config ends up canonical, the
/// row mirrors it, the follow-up push adds nothing (the server already has it),
/// and the next tick is a no-op.
#[tokio::test]
async fn reconcile_absorbs_the_canonical_config_into_a_wedged_blank_row() {
    let blank = json!({
        "activeProviderId": "", "baseUrl": "https://api.fireworks.ai/inference/v1",
        "model": "deepseek-v4-flash-0731", "modelOverrides": {},
        "preferences": "", "providers": [],
    })
    .to_string();
    let canonical = json!({
        "activeProviderId": "prov-s", "baseUrl": "https://s.example/v1",
        "model": "m-s", "modelOverrides": {},
        "preferences": "Talk like me.\n",
        "providers": [{
            "id": "prov-s", "name": "S", "baseUrl": "https://s.example/v1",
            "defaultModel": null, "catalogId": null,
        }],
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": canonical, "revision": 900,
            "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("wedged-blank");
    let mem = temp_memory("wedged-blank", &db);
    let cfg = temp_config("wedged-blank");
    {
        // In production the row and the live config AGREE — both are this
        // device's blank config, with `model` as its one non-default pick. (That
        // matters: the post-pull mirror only dirties the row when they diverge,
        // and the reconcile then defers to the push.)
        let mut c = cfg.get();
        c.model = "deepseek-v4-flash-0731".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty) VALUES ('main', ?1, 1000, 0)",
            [&blank],
        )
        .unwrap();
        // The pull already saw this row and advanced past it.
        conn.execute(
            "INSERT INTO sync_state(entity, last_seen_revision) VALUES ('user_config', 900)",
            [],
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

    // The pull fetches nothing — the high-water already passed the row…
    assert_eq!(pull(&ctx, "user", "tok", None).await.unwrap(), 0);
    assert!(
        cfg.get().preferences.is_empty(),
        "…so the pull alone cannot heal it"
    );

    // …the reconcile re-reads the row by id and absorbs it.
    reconcile_config_after_pull(&ctx, "tok").await.unwrap();
    let live = cfg.get();
    assert_eq!(live.preferences, "Talk like me.\n");
    assert_eq!(live.providers.len(), 1);
    assert_eq!(live.active_provider_id, "prov-s");
    let mirrored: String = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT payload FROM user_config WHERE id = 'main'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(
        mirrored.contains("Talk like me."),
        "the row mirrors the absorbed config: {mirrored}"
    );

    // The follow-up push adds nothing (the server already holds all of it)…
    push(&ctx, "tok", &SyncState::new()).await.unwrap();
    assert!(
        srv.posted("user_config").is_empty(),
        "nothing to add — the server is already canonical"
    );
    // …and the next tick is a no-op: converged.
    reconcile_config_after_pull(&ctx, "tok").await.unwrap();
    let dirty: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT dirty FROM user_config WHERE id = 'main'", [], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!(dirty, 0, "converged — the state settles");
}

/// A blank config must not CREATE the `user_config` row: a revision is an edit
/// claim, and a merely-created row would look newer than real content elsewhere
/// (the wedge above). Clearing an EXISTING config is still recorded — that is a
/// genuine edit, and the push's empty-guard keeps it from being published.
#[test]
fn mirror_does_not_create_a_row_for_a_blank_config() {
    let db = temp_db("blank-mirror");
    let conn = db.0.lock().unwrap();

    // Fresh install: no row, and the default config carries nothing.
    db::sync_user_config(&conn, &crate::config::AppConfig::default()).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM user_config", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0, "a blank config must not create a row");

    // An existing row that carries content IS rewritten when the config clears.
    conn.execute(
        "INSERT INTO user_config(id, payload, revision, dirty)
         VALUES ('main', ?1, 500, 0)",
        [r#"{"preferences":"Keep me","providers":[],"activeProviderId":"","baseUrl":"","model":"","modelOverrides":{}}"#],
    )
    .unwrap();
    db::sync_user_config(&conn, &crate::config::AppConfig::default()).unwrap();
    let (payload, dirty): (String, i64) = conn
        .query_row(
            "SELECT payload, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(!payload.contains("Keep me"), "the clear is recorded: {payload}");
    assert_eq!(dirty, 1);
}

/// A legacy-only remote (providers empty on both sides) reaches a NORMAL blank
/// device through the merge: its v1 payload is migrated with its row revision, so
/// its keys are strictly newer than the device's never-set ones. `base_url` and
/// `model` move as a PAIR — in the legacy shape they define the one provider — so
/// a device can never end up with its own endpoint and the remote's model.
#[test]
fn merge_adopts_a_legacy_remote_endpoint_as_a_pair() {
    let remote = parse_syncable_at(
        &json!({
            "activeProviderId": "", "baseUrl": "https://legacy.example/v1",
            "model": "legacy-model", "modelOverrides": {},
            "preferences": "", "providers": [],
        })
        .to_string(),
        500,
    )
    .unwrap();

    // A factory-default device (its mirrors count as unset) adopts the whole pair.
    let mut fresh = crate::config::AppConfig::default();
    assert_eq!(fresh.base_url, crate::config::DEFAULT_BASE_URL);
    project_merged(&mut fresh, &SyncableConfig::new_v2().merge(&remote));
    assert_eq!(fresh.base_url, "https://legacy.example/v1");
    assert_eq!(fresh.model, "legacy-model");

    // …but a device whose endpoint is NEWER keeps BOTH halves of its own pair.
    let mut mine = SyncableConfig::new_v2();
    mine.base_url = "https://mine.example/v1".into();
    mine.model = "my-model".into();
    mine.revs.insert(KEY_BASE_URL.into(), 900);
    let mut own = crate::config::AppConfig {
        base_url: "https://mine.example/v1".into(),
        model: "my-model".into(),
        ..crate::config::AppConfig::default()
    };
    project_merged(&mut own, &mine.merge(&remote));
    assert_eq!(own.base_url, "https://mine.example/v1");
    assert_eq!(own.model, "my-model", "the pair is never mixed");

    // A device on the FACTORY endpoint that merely picked a model has no endpoint of
    // its own to protect: the pair's revision is 0 on both sides, so the legacy
    // remote's endpoint AND its model are adopted together.
    let mut picked = SyncableConfig::new_v2();
    picked.model = "my-model".into();
    let mut cfg_picked = crate::config::AppConfig {
        model: "my-model".into(),
        ..crate::config::AppConfig::default()
    };
    project_merged(&mut cfg_picked, &picked.merge(&remote));
    assert_eq!(cfg_picked.base_url, "https://legacy.example/v1");
    assert_eq!(cfg_picked.model, "legacy-model");
}

/// A v1 payload is parsed at its OWN row's revision, so two v1 payloads ARE ordered
/// by their row revisions (a newer row's keys win) rather than by "local always
/// wins" — and when the revisions match, the v1 tie rule applies (the side that
/// carries a value; `self` on a tie).
#[test]
fn v1_payloads_are_ordered_by_their_own_row_revision() {
    let v1 = |prefs: &str| {
        json!({
            "preferences": prefs, "providers": [], "activeProviderId": "",
            "baseUrl": "", "model": "", "modelOverrides": {},
        })
        .to_string()
    };
    let older = parse_syncable_at(&v1("From the older row"), 100).unwrap();
    let newer = parse_syncable_at(&v1("From the newer row"), 900).unwrap();
    assert_eq!(older.merge(&newer).preferences, "From the newer row");
    assert_eq!(newer.merge(&older).preferences, "From the newer row");

    let same = parse_syncable_at(&v1("From the same revision"), 900).unwrap();
    assert_eq!(
        newer.merge(&same).preferences,
        "From the newer row",
        "a tie goes to self, as it always did"
    );
}

/// An unparseable remote row that is OLDER than ours must not pin the high-water:
/// we could neither absorb it nor lose to it, so refusing to advance would just
/// re-fetch it — and re-render the UI — every tick. (A *newer* bad row still
/// blocks, so we never eclipse one we'd have had to apply.)
#[tokio::test]
async fn unparseable_older_row_does_not_pin_the_watermark() {
    let db = temp_db("unparseable-older");
    let mem = temp_memory("unparseable-older", &db);
    let cfg = temp_config("unparseable-older");
    {
        let mut c = cfg.get();
        c.preferences = "Mine".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty) VALUES ('main', ?1, 1000, 0)",
            [cfg.get().syncable_payload().to_string()],
        )
        .unwrap();
    }
    let client = reqwest::Client::new();
    let base = "http://127.0.0.1:1".to_string(); // unused: no request on this path
    let ctx = SyncCtx {
        client: &client,
        base_url: &base,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    apply_user_config(
        &ctx,
        &[json!({
            "id": "main", "payload": "{ not the syncable shape",
            "revision": 900, "deleted_at": Value::Null,
        })],
    )
    .unwrap();

    assert_eq!(last_seen(&db, "user_config"), 900);
    assert_eq!(cfg.get().preferences, "Mine", "our config is untouched");
}

/// Pins the documented consequence of absorbing on emptiness: clearing a field is
/// NOT a way to remove it fleet-wide — the canonical value comes back. That is the
/// contract the write-side union already had (an empty local field always adopts
/// the server's), so both directions now agree; removing a value needs the
/// per-field versions noted in AGENTS.md.
#[tokio::test]
async fn a_cleared_field_is_refilled_from_the_canonical_config() {
    let db = temp_db("cleared-field");
    let mem = temp_memory("cleared-field", &db);
    let cfg = temp_config("cleared-field");
    {
        // `preferences` cleared, but an override keeps the payload non-empty — so
        // the write-side union would refill it on push anyway.
        let mut c = cfg.get();
        c.preferences = String::new();
        c.model_overrides
            .insert("m".into(), crate::config::ModelOverride::default());
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty) VALUES ('main', ?1, 1000, 0)",
            [cfg.get().syncable_payload().to_string()],
        )
        .unwrap();
    }
    let client = reqwest::Client::new();
    let base = "http://127.0.0.1:1".to_string(); // unused: no request on this path
    let ctx = SyncCtx {
        client: &client,
        base_url: &base,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    apply_user_config(
        &ctx,
        &[json!({
            "id": "main",
            "payload": json!({
                "activeProviderId": "", "baseUrl": "", "model": "",
                "modelOverrides": {}, "preferences": "Be terse", "providers": [],
            })
            .to_string(),
            "revision": 900,
            "deleted_at": Value::Null,
        })],
    )
    .unwrap();

    assert_eq!(
        cfg.get().preferences,
        "Be terse",
        "an empty local field adopts the server's"
    );
}

/// The converge path must not apply a merge to a row that moved under it — and
/// must not touch the LIVE config either, which is the half that lost an edit: a
/// Settings save landing between the merge and the write owns the row (dirty, and
/// it will push), so projecting the stale merge over it would revert the edit in
/// memory as well.
#[test]
fn apply_converged_row_bails_when_the_row_moved() {
    let db = temp_db("converge-guard");
    let cfg = temp_config("converge-guard");
    let merged = parse_syncable(
        &json!({
            "v": 2, "preferences": "Merged", "providers": [],
            "activeProviderId": "", "baseUrl": "", "model": "", "modelOverrides": {},
            "revs": { "preferences": 500 },
        })
        .to_string(),
    )
    .unwrap();
    let conn = db.0.lock().unwrap();
    // The concurrent edit bumped the row past what the merge was computed against.
    conn.execute(
        "INSERT INTO user_config(id, payload, revision, dirty) VALUES ('main', ?1, 700, 1)",
        [r#"{"v":2,"preferences":"Edited","providers":[],"activeProviderId":"","baseUrl":"","model":"","modelOverrides":{},"revs":{"preferences":700}}"#],
    )
    .unwrap();

    assert!(
        !apply_converged_row(&conn, &cfg, "main", &merged, 500, false, 600).unwrap(),
        "a moved row must not be applied"
    );
    let (payload, rev, dirty): (String, i64, i64) = conn
        .query_row(
            "SELECT payload, revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((rev, dirty), (700, 1), "the edit keeps its revision and dirty flag");
    assert!(payload.contains("Edited"), "the edit survives: {payload}");
    assert!(
        cfg.get().preferences.is_empty(),
        "and the stale merge is NOT projected over the live config"
    );

    // …and it applies normally once the revision matches.
    assert!(apply_converged_row(&conn, &cfg, "main", &merged, 700, false, 600).unwrap());
    let (rev, dirty): (i64, i64) = conn
        .query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((rev, dirty), (700, 0), "absorbed clean, never lowering our revision");
    assert_eq!(cfg.get().preferences, "Merged", "the merged value reaches the config");
}

/// The migration is what makes a v1 payload comparable per key: keys it CARRIES
/// take the row's revision, keys it left empty/default are "never set" (0). That
/// is the distinction v1 could not express, and it is what stopped a
/// merely-created blank row from outranking real content.
#[test]
fn migrate_v1_stamps_only_the_keys_that_carry_something() {
    let blank = parse_syncable_at(
        &json!({
            "preferences": "", "providers": [], "activeProviderId": "",
            "baseUrl": "", "model": "", "modelOverrides": {},
        })
        .to_string(),
        700,
    )
    .unwrap();
    assert_eq!(blank.v, 2, "migrated");
    assert!(!blank.has_content(), "a blank v1 payload has nothing set");

    let prefs = parse_syncable_at(
        &json!({
            "preferences": "Talk like me.", "providers": [], "activeProviderId": "",
            "baseUrl": "", "model": "", "modelOverrides": {},
        })
        .to_string(),
        700,
    )
    .unwrap();
    assert_eq!(prefs.rev(KEY_PREFERENCES), 700);
    assert_eq!(prefs.rev(KEY_BASE_URL), 0, "an empty key stays never-set");
    assert!(prefs.has_content());

    // The `model` mirror is not content on its own: stamping it is what let a
    // model-only device publish over a configured mirror.
    let model_only = json!({
        "preferences": "", "providers": [], "activeProviderId": "",
        "baseUrl": crate::config::DEFAULT_BASE_URL,
        "model": "some-model", "modelOverrides": {},
    })
    .to_string();
    assert!(
        syncable_payload_is_empty(&model_only),
        "a model pick on the default endpoint carries nothing"
    );
    // …but a custom base_url IS legacy content, and the model rides with it.
    let legacy = json!({
        "preferences": "", "providers": [], "activeProviderId": "",
        "baseUrl": "https://legacy.example/v1", "model": "some-model",
        "modelOverrides": {},
    })
    .to_string();
    assert!(!syncable_payload_is_empty(&legacy), "legacy base_url is content");
}

/// Per-key ordering replaces the whole-row `remote_newer` gate: a field we never
/// set is adopted EVEN WHEN our row revision is the higher one — the exact state
/// that wedged the phone.
#[test]
fn per_key_revisions_beat_the_row_revision() {
    let ours = parse_syncable(
        &json!({
            "v": 2, "preferences": "", "providers": [], "activeProviderId": "",
            "baseUrl": "", "model": "", "modelOverrides": {},
            "revs": {},
        })
        .to_string(),
    )
    .unwrap();
    let theirs = parse_syncable(
        &json!({
            "v": 2, "preferences": "Be terse", "providers": [], "activeProviderId": "",
            "baseUrl": "", "model": "", "modelOverrides": {},
            "revs": { "preferences": 500 },
        })
        .to_string(),
    )
    .unwrap();
    let merged = ours.merge(&theirs);
    assert_eq!(merged.preferences, "Be terse", "a field we never set is adopted");
    assert_eq!(merged.rev(KEY_PREFERENCES), 500);
}

/// A CLEAR is set at a revision with an empty value, so it wins outright instead
/// of being refilled — the behaviour v1 could not express, and both sides agree.
#[test]
fn a_clear_wins_over_an_older_value() {
    let ours = parse_syncable(
        &json!({
            "v": 2, "preferences": "", "providers": [], "activeProviderId": "",
            "baseUrl": "", "model": "", "modelOverrides": {},
            "revs": { "preferences": 900 },
        })
        .to_string(),
    )
    .unwrap();
    let theirs = parse_syncable(
        &json!({
            "v": 2, "preferences": "Be terse", "providers": [], "activeProviderId": "",
            "baseUrl": "", "model": "", "modelOverrides": {},
            "revs": { "preferences": 500 },
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(ours.merge(&theirs).preferences, "", "our newer clear stands");
    assert_eq!(theirs.merge(&ours).preferences, "", "…and they agree");

    // A strictly NEWER value still beats the older clear.
    let mut older_clear = ours.clone();
    older_clear.revs.insert(KEY_PREFERENCES.into(), 400);
    assert_eq!(older_clear.merge(&theirs).preferences, "Be terse");
}

/// A removal keeps its key's revision while the value goes away (a per-key
/// tombstone), so a newer removal beats a stale entry on BOTH sides — while a v1
/// peer, which has no revisions at all, still unions it in.
#[test]
fn a_newer_removal_beats_a_stale_provider() {
    let prov_a = json!({ "id": "prov-a", "name": "A", "baseUrl": "https://a/v1",
                         "defaultModel": null, "catalogId": null });
    // We removed `prov-a` at 900; the server still lists it at 500.
    let ours = parse_syncable(
        &json!({
            "v": 2, "preferences": "", "providers": [], "activeProviderId": "",
            "baseUrl": "", "model": "", "modelOverrides": {},
            "revs": { "provider:prov-a": 900 },
        })
        .to_string(),
    )
    .unwrap();
    let theirs = parse_syncable(
        &json!({
            "v": 2, "preferences": "", "providers": [prov_a],
            "activeProviderId": "", "baseUrl": "", "model": "", "modelOverrides": {},
            "revs": { "provider:prov-a": 500 },
        })
        .to_string(),
    )
    .unwrap();
    assert!(ours.merge(&theirs).providers.is_empty(), "our removal stands");
    assert!(
        theirs.merge(&ours).providers.is_empty(),
        "…and it converges for them too"
    );

    let v1_ours = parse_syncable(
        &json!({
            "preferences": "", "providers": [], "activeProviderId": "",
            "baseUrl": "", "model": "", "modelOverrides": {},
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(
        v1_ours.merge(&theirs).providers.len(),
        1,
        "a v1 peer still unions the entry in"
    );
}

/// The union's provider merge is per-id and its active-id adoption never yields
/// a dangling id — even when the server's active id names a provider that exists
/// nowhere in the merged list.
#[test]
fn merge_merges_providers_by_id_and_never_dangles_active() {
    let local = parse_syncable(
        &json!({
            "activeProviderId": "", "baseUrl": "", "model": "",
            "modelOverrides": {}, "preferences": "",
            "providers": [{
                "id": "prov-l", "name": "L", "baseUrl": "https://l.example/v1",
                "defaultModel": null, "catalogId": null,
            }],
        })
        .to_string(),
    )
    .unwrap();

    // Server active id names one of its OWN providers → adopted.
    let server_ok = parse_syncable(
        &json!({
            "activeProviderId": "prov-s", "baseUrl": "", "model": "",
            "modelOverrides": {}, "preferences": "",
            "providers": [{
                "id": "prov-s", "name": "S", "baseUrl": "https://s.example/v1",
                "defaultModel": null, "catalogId": null,
            }],
        })
        .to_string(),
    )
    .unwrap();
    let merged = local.merge(&server_ok);
    assert_eq!(merged.providers.len(), 2, "providers merge per id");
    assert_eq!(merged.active_provider_id, "prov-s");

    // Server active id names a provider present nowhere → NOT adopted (no
    // dangling id), our own list preserved.
    let server_ghost = parse_syncable(
        &json!({
            "activeProviderId": "ghost", "baseUrl": "", "model": "",
            "modelOverrides": {}, "preferences": "",
            "providers": [{
                "id": "prov-s", "name": "S", "baseUrl": "https://s.example/v1",
                "defaultModel": null, "catalogId": null,
            }],
        })
        .to_string(),
    )
    .unwrap();
    let merged = local.merge(&server_ghost);
    assert_eq!(merged.active_provider_id, "", "a dangling server id is not adopted");
}

/// The regression guard for the push's concurrent-edit bail: when the row moved
/// past the revision the push read, `persist_merged_user_config` must touch
/// NOTHING — not the row, and (critically) not the live config, whose clobbering
/// would revert the concurrent edit for good.
#[tokio::test]
async fn persist_bails_and_leaves_live_config_untouched_when_the_row_moved() {
    let db = temp_db("persist-bail");
    let mem = temp_memory("persist-bail", &db);
    let cfg = temp_config("persist-bail");
    {
        // The live config holds a just-saved edit; the row already moved past the
        // stale revision (900) the caller will pass as `expect_rev`.
        let mut c = cfg.get();
        c.preferences = "KEEP me".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 901, 1)",
            rusqlite::params![json!({
                "activeProviderId": "", "baseUrl": "", "model": "",
                "modelOverrides": {}, "preferences": "KEEP me", "providers": [],
            })
            .to_string()],
        )
        .unwrap();
    }
    let client = reqwest::Client::new();
    let base = "http://127.0.0.1:1".to_string(); // unused: this path makes no request
    let ctx = SyncCtx {
        client: &client,
        base_url: &base,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    // A union that would replace the preference if it were (wrongly) adopted.
    let merged = parse_syncable(
        &json!({
            "activeProviderId": "", "baseUrl": "", "model": "",
            "modelOverrides": {}, "preferences": "CLOBBER", "providers": [],
        })
        .to_string(),
    )
    .unwrap();
    // expect_rev 900 != the row's 901 → the guarded UPDATE affects 0 rows.
    let persisted = persist_merged_user_config(&ctx, "main", &merged, 902, 900).unwrap();

    assert!(!persisted, "a moved row must not be persisted");
    let (payload, revision): (String, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT payload, revision FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(revision, 901, "row revision untouched");
    assert!(payload.contains("KEEP me"), "row payload untouched");
    assert_eq!(cfg.get().preferences, "KEEP me", "live config NOT clobbered");
}

/// When the server already holds everything we have, the union adds nothing:
/// the push must NOT post anything and must clear the local dirty flag (no
/// churn), while our config still converges on the pull.
#[tokio::test]
async fn push_clears_dirty_when_the_server_already_has_our_content() {
    let local = json!({
        "activeProviderId": "", "baseUrl": "", "model": "",
        "modelOverrides": {}, "preferences": "Shared", "providers": [],
    })
    .to_string();
    let server = json!({
        "activeProviderId": "prov-s", "baseUrl": "https://s.example/v1",
        "model": "m-s", "modelOverrides": {},
        "preferences": "Shared",
        "providers": [{
            "id": "prov-s", "name": "S", "baseUrl": "https://s.example/v1",
            "defaultModel": null, "catalogId": null,
        }],
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": server, "revision": 900,
            "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("noop-push");
    let mem = temp_memory("noop-push", &db);
    let cfg = temp_config("noop-push");
    {
        let mut c = cfg.get();
        c.preferences = "Shared".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 800, 1)",
            [&local],
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

    assert!(
        srv.posted("user_config").is_empty(),
        "a subset config must not re-post"
    );
    let dirty: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT dirty FROM user_config WHERE id='main'", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(dirty, 0, "nothing pending once the server has our content");
}

/// The union path works on an ENCRYPTED mirror: the server's ciphertext is
/// decrypted to compute the union, and what we push goes back out as ciphertext
/// holding both sides' content.
#[tokio::test]
async fn union_push_round_trips_through_encryption() {
    use crate::crypt;

    let key = [7u8; 32];
    let local = json!({
        "activeProviderId": "", "baseUrl": "", "model": "",
        "modelOverrides": {}, "preferences": "Mine", "providers": [],
    })
    .to_string();
    let server_plain = json!({
        "activeProviderId": "prov-s", "baseUrl": "https://s.example/v1",
        "model": "m-s", "modelOverrides": {},
        "preferences": "",
        "providers": [{
            "id": "prov-s", "name": "S", "baseUrl": "https://s.example/v1",
            "defaultModel": null, "catalogId": null,
        }],
    })
    .to_string();
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": crypt::encrypt(&key, &server_plain),
            "revision": 900, "deleted_at": Value::Null,
        })],
    );
    let db = temp_db("union-enc");
    let mem = temp_memory("union-enc", &db);
    let cfg = temp_config("union-enc");
    {
        let mut c = cfg.get();
        c.preferences = "Mine".into();
        cfg.set(c).unwrap();
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 800, 1)",
            [&local],
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
        key: Some(key),
    };
    push(&ctx, "tok", &SyncState::new()).await.unwrap();

    let posts = srv.posted("user_config");
    assert!(!posts.is_empty(), "the union must be pushed");
    let sent = posts[posts.len() - 1]["payload"].as_str().unwrap();
    assert!(crypt::looks_encrypted(sent), "must go out as ciphertext");
    let plain = crypt::decrypt(&key, sent).expect("decryptable with our key");
    let merged: Value = serde_json::from_str(&plain).unwrap();
    assert_eq!(merged["preferences"], "Mine", "our field wins");
    assert_eq!(merged["providers"][0]["id"], "prov-s", "server field survives");
}

/// A server payload that looks encrypted is skipped even when a key IS present
/// but fails to decrypt it (never push over ciphertext we can't verify).
#[tokio::test]
async fn reconcile_skips_undecryptable_payload_even_with_key() {
    let srv = MockRest::spawn();
    srv.set_get(
        "user_config",
        vec![json!({
            "id": "main", "payload": "enc:v1:definitely-not-valid", "revision": 100,
        })],
    );
    let db = temp_db("reconcile-enc-key");
    let mem = temp_memory("reconcile-enc-key", &db);
    let cfg = temp_config("reconcile-enc-key");
    let client = reqwest::Client::new();
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "INSERT INTO user_config(id, payload, revision, dirty)
             VALUES ('main', ?1, 100, 0)",
            rusqlite::params![json!({
                "activeProviderId": "", "baseUrl": "", "model": "",
                "modelOverrides": {}, "preferences": "local", "providers": [],
            }).to_string()],
        )
        .unwrap();
    }
    let ctx = SyncCtx {
        client: &client,
        base_url: &srv.addr,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: Some([0u8; 32]), // key present, but the blob isn't decryptable
    };
    reconcile_user_config(&ctx, "tok").await.unwrap();

    let (rev, dirty): (i64, i64) = {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT revision, dirty FROM user_config WHERE id = 'main'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(dirty, 0);
    assert_eq!(rev, 100);
    assert!(srv.posted("user_config").is_empty());
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
    pull(&ctx_a, "u", "tok", None).await.unwrap();
    {
        let conn = db_a.0.lock().unwrap();
        db::sync_user_config(&conn, &cfg_a.get()).unwrap();
    }
    // A round 2: push carries A's config to the mirror.
    push(&ctx_a, "tok", &SyncState::new()).await.unwrap();
    pull(&ctx_a, "u", "tok", None).await.unwrap();
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
    pull(&ctx_b, "u", "tok", None).await.unwrap();
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

// ---------------------------------------------------------------------------
// edit-time truncation tombstones
// ---------------------------------------------------------------------------

/// Seed a conversation with three turns and return `(cid, m1, m2, m3)`.
fn seed_edit_turns(db: &Db) -> (String, String, String, String) {
    let cid = insert_conv(db);
    let m1 = db::insert_message(
        db, cid.clone(), "user".into(), "first".into(), None, None, None, None, None, None,
    )
    .unwrap();
    let m2 = db::insert_message(
        db, cid.clone(), "assistant".into(), "second".into(), None, None, None, None, None, None,
    )
    .unwrap();
    let m3 = db::insert_message(
        db, cid.clone(), "user".into(), "third".into(), None, None, None, None, None, None,
    )
    .unwrap();
    (cid, m1.id, m2.id, m3.id)
}

/// Editing a user message truncates every later row. Each truncated row must
/// leave a per-message tombstone so the fork propagates cross-device (otherwise
/// a second synced device keeps the stale tail beside the edited turn).
#[test]
fn edit_truncation_records_message_tombstones() {
    let db = temp_db("edit-tomb");
    let (cid, m1, m2, m3) = seed_edit_turns(&db);

    db::apply_user_message_edit(&db, &cid, &m1, "edited", None).unwrap();

    let conn = db.0.lock().unwrap();
    let count = |id: &str| -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM sync_tombstones WHERE entity = 'messages' AND id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(count(&m2), 1, "truncated assistant row must be tombstoned");
    assert_eq!(count(&m3), 1, "truncated user row must be tombstoned");
    assert_eq!(count(&m1), 0, "the edited row survives and is not tombstoned");
    let stored_cid: String = conn
        .query_row(
            "SELECT conversation_id FROM sync_tombstones WHERE entity = 'messages' AND id = ?1",
            [&m2],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored_cid, cid, "tombstone carries the conversation id for push");
}

/// A remote message tombstone (e.g. from another device's edit) must hard-delete
/// the stale local row, matching how conversation tombstones behave.
#[tokio::test]
async fn remote_message_tombstone_deletes_the_stale_tail() {
    let db = temp_db("msg-tomb-pull");
    let cid = insert_conv(&db);
    let stale = db::insert_message(
        &db, cid.clone(), "assistant".into(), "stale".into(), None, None, None, None, None, None,
    )
    .unwrap();

    let remote = vec![json!({
        "id": stale.id, "conversation_id": cid, "role": "assistant", "content": "",
        "seq": 0, "created_at": 1, "revision": 9_000_000_000_i64, "deleted_at": 9_000_000_001_i64
    })];
    apply_messages(&db, &remote).await.unwrap();

    let remaining: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM messages WHERE id = ?1", [&stale.id], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(remaining, 0, "the tombstoned message must be deleted locally");
}

/// The push must carry those tombstones to the remote as rows with a non-null
/// `deleted_at`, then retire them locally.
#[tokio::test]
async fn push_emits_message_tombstones_for_edit_truncation() {
    let srv = MockRest::spawn();
    let db = temp_db("edit-tomb-push");
    let mem = temp_memory("edit-tomb-push", &db);
    let (cid, m1, m2, m3) = seed_edit_turns(&db);
    db::apply_user_message_edit(&db, &cid, &m1, "edited", None).unwrap();

    let client = reqwest::Client::new();
    let base = srv.addr.clone();
    let cfg = temp_config("edit-tomb-push");
    let ctx = SyncCtx {
        client: &client,
        base_url: &base,
        anon: "anon",
        db: &db,
        memory: &mem,
        config: &cfg,
        key: None,
    };
    push(&ctx, "tok", &SyncState::new()).await.unwrap();

    let posted = srv.posted("messages");
    let find = |id: &str| posted.iter().find(|v| v["id"] == id).cloned();
    let t2 = find(&m2).expect("truncated assistant row must be pushed");
    assert!(
        t2["deleted_at"].as_i64().is_some(),
        "truncated row must be a tombstone: {t2}"
    );
    assert_eq!(t2["conversation_id"], cid);
    let t3 = find(&m3).expect("truncated user row must be pushed");
    assert!(t3["deleted_at"].as_i64().is_some(), "truncated row must be a tombstone");
    // The edited row is still live (no deleted_at).
    let live = find(&m1).expect("edited row must be pushed");
    assert!(live["deleted_at"].is_null(), "edited row must stay live");

    // Successful push retires the tombstones and clears every dirty flag.
    assert_eq!(db::pending_rows(&db).unwrap(), 0);
}
