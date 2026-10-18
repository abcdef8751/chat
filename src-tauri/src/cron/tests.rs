//! Cron DB/scheduler unit tests. Schedule parsing/computation lives in
//! `cron::schedule` and is tested there; the turn-level integration (a mocked
//! run persisting rows, tool-policy gating) is in `chat::tests`.

use super::*;
use crate::db::Db;
use std::sync::{Arc, Mutex};

fn open_db(name: &str) -> Db {
    let mut p = std::env::temp_dir();
    p.push(format!("pi-chat-cron-{name}-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let conn = crate::db::open(&p).unwrap();
    Db(Arc::new(Mutex::new(conn)))
}

fn insert_conv(db: &Db, id: &str) {
    let conn = db.0.lock().unwrap();
    conn.execute(
        "INSERT INTO conversations (id, title, created_at, updated_at) VALUES (?1, 'test', 0, 0)",
        [id],
    )
    .unwrap();
}

fn job(db: &Db, schedule: &str, next: i64) -> db::CronJob {
    db::insert_cron_job(
        db,
        "Test job".into(),
        schedule.into(),
        "do the thing".into(),
        "readonly".into(),
        true,
        next,
    )
    .unwrap()
}

/// Due selection is `enabled = 1 AND (run_requested = 1 OR next_run_at <= now)`,
/// oldest slot first, and a paused job is never selected even when overdue.
#[test]
fn due_jobs_selects_enabled_and_ordered_by_slot() {
    let db = open_db("due");
    insert_conv(&db, "c1");
    let past = 1_000;
    let later = 2_000;
    let future = 9_000;

    let older = job(&db, "0 * * * *", past);
    let newer = job(&db, "0 * * * *", later);
    let future_job = job(&db, "0 * * * *", future);
    let paused = job(&db, "0 * * * *", past);
    db::set_cron_job_enabled(&db, &paused.id, false).unwrap();

    let due = db::due_cron_jobs(&db, 5_000).unwrap();
    let ids: Vec<&str> = due.iter().map(|j| j.id.as_str()).collect();
    assert_eq!(ids, vec![older.id.as_str(), newer.id.as_str()]);
    assert!(!ids.contains(&future_job.id.as_str()));
    assert!(!ids.contains(&paused.id.as_str()));

    // An explicit one-shot request fires even when the slot is in the future.
    db::request_cron_run(&db, &future_job.id).unwrap();
    let due = db::due_cron_jobs(&db, 5_000).unwrap();
    assert!(due
        .iter()
        .any(|j| j.id == future_job.id && j.run_requested));

    // ...and on a PAUSED job: "Run now" must not silently do nothing.
    db::request_cron_run(&db, &paused.id).unwrap();
    let due = db::due_cron_jobs(&db, 5_000).unwrap();
    assert!(due.iter().any(|j| j.id == paused.id && j.run_requested));
}

/// Firing advances the slot, stamps `last_run_at`, and bumps the counter only
/// for a real scheduled fire (a manual fire must not inflate `run_count`).
#[test]
fn run_state_advances_slot_and_counts_only_scheduled_fires() {
    let db = open_db("state");
    insert_conv(&db, "c1");
    let j = job(&db, "0 * * * *", 1_000);

    db::set_cron_job_run_state(&db, &j.id, 5_000, 4_000, true).unwrap();
    let after = db::get_cron_job(&db, &j.id).unwrap().unwrap();
    assert_eq!(after.next_run_at, 5_000);
    assert_eq!(after.last_run_at, Some(4_000));
    assert_eq!(after.run_count, 1);

    db::set_cron_job_run_state(&db, &j.id, 9_000, 8_000, false).unwrap();
    let after = db::get_cron_job(&db, &j.id).unwrap().unwrap();
    assert_eq!(after.next_run_at, 9_000);
    assert_eq!(after.last_run_at, Some(8_000));
    assert_eq!(after.run_count, 1, "a manual run must not bump the counter");
}

/// A run row starts with an empty trigger id and gains it when the turn's user
/// row lands; `list_cron_triggers` then maps that message to the job title (the
/// timeline "scheduled" chip) and skips unfinished runs.
#[test]
fn run_rows_and_triggers() {
    let db = open_db("runs");
    let j = job(&db, "0 * * * *", 1_000);

    let finished = db::insert_cron_run(&db, &j, 1_000).unwrap();
    db::finish_cron_run(
        &db,
        &finished,
        Some("msg-1"),
        "stop",
        None,
        Some(r#"{"prompt_tokens":1}"#),
        Some(0.002),
    )
    .unwrap();
    let pending = db::insert_cron_run(&db, &j, 1_000).unwrap();

    let triggers = db::list_cron_triggers(&db, None).unwrap();
    assert_eq!(triggers.len(), 1, "only finished runs carry a trigger");
    assert_eq!(triggers[0].message_id, "msg-1");
    assert_eq!(triggers[0].job_id, j.id);
    assert_eq!(triggers[0].title, "Test job");
    // The transcript chip is per conversation: another chat sees nothing.
    assert_eq!(
        db::list_cron_triggers(&db, Some(&j.conversation_id))
            .unwrap()
            .len(),
        1
    );
    assert!(db::list_cron_triggers(&db, Some("other")).unwrap().is_empty());

    let runs = db::list_cron_runs(&db, Some(&j.id), 10).unwrap();
    assert_eq!(runs.len(), 2);
    let finished_row = runs.iter().find(|r| r.id == finished).unwrap();
    assert_eq!(finished_row.stop_reason.as_deref(), Some("stop"));
    assert_eq!(finished_row.cost, Some(0.002));
    assert!(finished_row.finished_at.is_some());
    let pending_row = runs.iter().find(|r| r.id == pending).unwrap();
    assert!(pending_row.finished_at.is_none());
    assert_eq!(pending_row.trigger_message_id, "");
}

/// Deleting a job's thread cascades to the job (and its runs), so removing the
/// thread from the sidebar cannot leave a scheduler entry pointing at a missing
/// conversation.
#[test]
fn conversation_delete_cascades_jobs_and_runs() {
    let db = open_db("cascade");
    let j = job(&db, "0 * * * *", 1_000);
    db::insert_cron_run(&db, &j, 1_000).unwrap();

    {
        let conn = db.0.lock().unwrap();
        conn.execute("DELETE FROM conversations WHERE id = ?1", [&j.conversation_id])
            .unwrap();
    }
    assert!(db::list_cron_jobs(&db).unwrap().is_empty());
    assert!(db::list_cron_runs(&db, None, 10).unwrap().is_empty());
}

/// Catch-up is max one: a job whose slot passed while the app was closed is
/// selected once, and `advance` resyncs the next slot into the future instead of
/// replaying every missed slot.
#[test]
fn catch_up_fires_once_and_resyncs_forward() {
    let db = open_db("catchup");
    insert_conv(&db, "c1");
    let now = db::now_ms();
    // An hourly slot two hours in the past: two thresholds were crossed.
    let j = job(&db, "0 * * * *", now - 2 * 3_600_000);

    let due = db::due_cron_jobs(&db, now).unwrap();
    assert_eq!(due.len(), 1, "a missed backlog must collapse to one fire");

    advance(&db, &j, now, false).unwrap();
    let after = db::get_cron_job(&db, &j.id).unwrap().unwrap();
    assert!(
        after.next_run_at > now,
        "the schedule must resync forward, got {} (now {now})",
        after.next_run_at
    );
    assert_eq!(after.run_count, 1);
    assert!(db::due_cron_jobs(&db, now + 1).unwrap().is_empty());
}

/// A manual fire never bumps the run counter, and it must satisfy an already
/// overdue slot — otherwise the scheduler would fire the same slot on its next
/// tick and the job would run twice.
#[test]
fn manual_advance_satisfies_an_overdue_slot() {
    let db = open_db("manual-advance");
    insert_conv(&db, "c1");
    let now = db::now_ms();
    let j = job(&db, "0 * * * *", now - 3_600_000);

    advance(&db, &j, now, true).unwrap();
    let after = db::get_cron_job(&db, &j.id).unwrap().unwrap();
    assert!(
        after.next_run_at > now,
        "an overdue slot must be satisfied, not left due"
    );
    assert_eq!(after.run_count, 0, "a manual fire is not a scheduled run");
    assert_eq!(after.last_run_at, Some(now));
    assert!(db::due_cron_jobs(&db, now + 1).unwrap().is_empty());
}

/// The per-conversation claim and the global concurrency cap: a duplicate claim
/// fails, the cap holds, and releasing a slot admits a new conversation.
#[test]
fn cron_state_claims_are_exclusive_and_capped() {
    let state = CronState::default();
    assert!(state.try_begin("a"));
    assert!(!state.try_begin("a"), "a chat must have at most one cron run");
    for i in 1..MAX_CONCURRENT_RUNS {
        assert!(state.try_begin(&format!("c{i}")), "slot {i} should be free");
    }
    assert!(
        !state.try_begin("overflow"),
        "the global cap must refuse an extra chat"
    );
    state.end("a");
    assert!(state.try_begin("fresh"), "a released slot is reusable");
}

/// A panic in the run task leaves its `cron_runs` row open. The task's RAII
/// guard closes it, so the audit trail never shows a run "in progress" forever;
/// a second sweep is a no-op.
#[test]
fn abnormal_exit_closes_an_open_run_row() {
    let db = open_db("abnormal");
    insert_conv(&db, "c1");
    let j = job(&db, "0 * * * *", 1_000);
    let run = db::insert_cron_run(&db, &j, 1_000).unwrap();

    assert_eq!(
        db::finish_open_cron_run(&db, &j.id, "run terminated unexpectedly").unwrap(),
        1
    );
    let rows = db::list_cron_runs(&db, Some(&j.id), 10).unwrap();
    let row = rows.iter().find(|r| r.id == run).unwrap();
    assert!(row.finished_at.is_some());
    assert_eq!(row.stop_reason.as_deref(), Some("error"));
    assert_eq!(row.error.as_deref(), Some("run terminated unexpectedly"));

    assert_eq!(db::finish_open_cron_run(&db, &j.id, "again").unwrap(), 0);
}

/// The fired user turn carries the prompt plus an autonomous footer naming the
/// job, so both the model and a reader know why it appeared.
#[test]
fn run_prompt_has_autonomous_footer() {
    let db = open_db("prompt");
    insert_conv(&db, "c1");
    let j = job(&db, "30 8 * * *", 1_000);
    let text = run_prompt(&j);
    assert!(text.starts_with("do the thing"));
    assert!(text.contains("Automated scheduled run of \"Test job\""));
    assert!(text.contains("30 8 * * *"));
}

/// Policy parsing tolerates the spellings an agent may emit, and rejects
/// anything else instead of silently defaulting.
#[test]
fn policy_normalization() {
    assert_eq!(normalize_policy(None).unwrap(), "readonly");
    assert_eq!(normalize_policy(Some("autoapprove")).unwrap(), "autoapprove");
    assert_eq!(normalize_policy(Some("auto-approve")).unwrap(), "autoapprove");
    assert_eq!(normalize_policy(Some("auto_approve")).unwrap(), "autoapprove");
    assert_eq!(normalize_policy(Some("readonly")).unwrap(), "readonly");
    assert!(normalize_policy(Some("yolo")).is_err());
}

#[test]
fn snippet_flattens_and_caps() {
    assert_eq!(snippet("  hello\n\nworld  "), "hello world");
    assert_eq!(snippet("   "), "Run finished with no reply.");
    let long = "x".repeat(300);
    let out = snippet(&long);
    assert_eq!(out.chars().count(), 201); // 200 + ellipsis
    assert!(out.ends_with('…'));
}

#[test]
fn cron_tool_names_are_recognized() {
    for name in CRON_TOOLS {
        assert!(is_cron_tool(name));
    }
    assert!(!is_cron_tool("bash"));
    assert!(!is_cron_tool("save_memory"));
}

fn tool_call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: format!("call-{name}"),
        name: name.into(),
        arguments,
    }
}

/// The agent's cron surface end to end (DB only): create with a preset, list,
/// pause, queue a manual fire, delete. This also pins the preset→cron
/// normalization boundary the model relies on.
#[test]
fn agent_tools_create_list_update_and_delete() {
    let db = open_db("agent-tools");

    let created = execute_tool(
        &tool_call(
            "create_cron",
            json!({
                "prompt": "Morning briefing",
                "schedule": "daily@08:30",
                "tool_policy": "auto-approve",
            }),
        ),
        &db,
    );
    assert!(!created.is_error, "create failed: {}", created.content);
    let job: Value = serde_json::from_str(&created.content).unwrap();
    assert_eq!(job["schedule"], "30 8 * * *");
    assert_eq!(job["toolPolicy"], "autoapprove");
    assert_eq!(job["enabled"], true);
    // The title defaults to the prompt.
    assert_eq!(job["title"], "Morning briefing");
    let id = job["id"].as_str().unwrap().to_string();
    // The job owns a fresh thread; no conversation had to be supplied.
    let conv_id = job["conversationId"].as_str().unwrap();
    let thread = db::get_conversation(&db, conv_id).unwrap().unwrap();
    assert_eq!(thread.kind, "scheduled");
    assert_eq!(thread.title, "Morning briefing");

    let listed = execute_tool(&tool_call("list_crons", json!({})), &db);
    let listed: Value = serde_json::from_str(&listed.content).unwrap();
    assert_eq!(listed["jobs"].as_array().unwrap().len(), 1);
    assert!(listed["jobs"][0]["nextRun"].as_str().is_some());

    let updated = execute_tool(
        &tool_call(
            "update_cron",
            json!({ "id": id, "enabled": false, "title": "Paused briefing" }),
        ),
        &db,
    );
    assert!(!updated.is_error, "update failed: {}", updated.content);
    let updated: Value = serde_json::from_str(&updated.content).unwrap();
    assert_eq!(updated["enabled"], false);
    assert_eq!(updated["title"], "Paused briefing");
    // The job's thread follows the title while the user has not renamed it.
    assert_eq!(
        db::get_conversation(&db, conv_id).unwrap().unwrap().title,
        "Paused briefing"
    );

    let queued = execute_tool(&tool_call("run_cron_now", json!({ "id": id })), &db);
    assert!(!queued.is_error);
    assert!(
        db::get_cron_job(&db, &id).unwrap().unwrap().run_requested,
        "run_cron_now must queue a one-shot fire"
    );

    let deleted = execute_tool(&tool_call("delete_cron", json!({ "id": id })), &db);
    assert!(!deleted.is_error);
    assert!(db::list_cron_jobs(&db).unwrap().is_empty());
    assert!(
        db::get_conversation(&db, conv_id).unwrap().is_none(),
        "deleting a job must delete its own thread"
    );

    // Invalid input is reported as a tool error, not a panic.
    let bad = execute_tool(
        &tool_call(
            "create_cron",
            json!({ "prompt": "x", "schedule": "0 8 * * * 0" }),
        ),
        &db,
    );
    assert!(bad.is_error);
    assert!(bad.content.contains("sub-minute"));
}

/// `cron_preview` validates for the UI and resolves the next fire; invalid input
/// comes back as an `error` field rather than an invoke failure.
#[test]
fn preview_reports_next_run_and_errors() {
    let ok = cron_preview("daily@08:30".into());
    assert!(ok.error.is_none());
    assert_eq!(ok.schedule, "30 8 * * *");
    assert_eq!(ok.description, "Daily at 08:30");
    assert!(ok.next_run_at.is_some());
    assert!(ok.next_run_at.unwrap() > db::now_ms() - 1000);

    let bad = cron_preview("61 * * * *".into());
    assert!(bad.error.is_some());
    assert!(bad.next_run_at.is_none());
}

/// A job owns its thread from birth: no conversation is supplied, the thread is
/// flagged `scheduled` (so the sidebar keeps it out of the chat list), and it is
/// deleted with the job (covered above).
#[test]
fn a_job_owns_a_scheduled_thread() {
    let db = open_db("thread-kind");
    insert_conv(&db, "chat-1");
    let j = job(&db, "0 * * * *", 1_000);

    let thread = db::get_conversation(&db, &j.conversation_id).unwrap().unwrap();
    assert_eq!(thread.kind, "scheduled");
    assert_eq!(thread.title, "Test job");
    assert_ne!(thread.id, "chat-1");
    assert_eq!(
        db::get_conversation(&db, "chat-1").unwrap().unwrap().kind,
        "chat"
    );
}

/// Renaming the job renames its thread, unless the user renamed the thread
/// themselves in the sidebar (that edit wins).
#[test]
fn job_rename_does_not_clobber_a_user_renamed_thread() {
    let db = open_db("rename-thread");
    let j = job(&db, "0 * * * *", 1_000);
    {
        let conn = db.0.lock().unwrap();
        conn.execute(
            "UPDATE conversations SET title = 'My own title' WHERE id = ?1",
            [&j.conversation_id],
        )
        .unwrap();
    }

    let updated = execute_tool(
        &tool_call("update_cron", json!({ "id": j.id, "title": "Job title" })),
        &db,
    );
    assert!(!updated.is_error, "update failed: {}", updated.content);
    assert_eq!(
        db::get_conversation(&db, &j.conversation_id)
            .unwrap()
            .unwrap()
            .title,
        "My own title"
    );
}

/// Scheduled threads are local-only, like the job definitions that own them: the
/// sync seed must not mark them (or their messages) for upload, and deleting one
/// must not leave a tombstone for a row the mirror never saw.
#[test]
fn sync_seed_and_tombstones_skip_scheduled_threads() {
    let db = open_db("sync-skip");
    insert_conv(&db, "chat-1");
    let j = job(&db, "0 * * * *", 1_000);
    db::seed_initial_sync(&db, "user-1", &[]).unwrap();

    let dirty = |id: &str| -> i64 {
        let conn = db.0.lock().unwrap();
        conn.query_row(
            "SELECT dirty FROM conversations WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(dirty("chat-1"), 1, "chats still back up");
    assert_eq!(dirty(&j.conversation_id), 0, "scheduled threads never sync");

    let rev = {
        let conn = db.0.lock().unwrap();
        db::tombstone_conversation(&conn, &j.conversation_id).unwrap()
    };
    assert_eq!(rev, 0);
    let tombstones: i64 = {
        let conn = db.0.lock().unwrap();
        conn.query_row("SELECT COUNT(*) FROM sync_tombstones", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(tombstones, 0);
}

/// Databases from before job-owned threads had jobs pinned to a user chat. On
/// open, each such job is moved to a fresh `scheduled` thread (once), and the
/// old chat is left alone.
#[test]
fn opening_an_old_db_moves_jobs_off_user_chats() {
    let mut p = std::env::temp_dir();
    p.push(format!("pi-chat-cron-migrate-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    {
        let conn = crate::db::open(&p).unwrap();
        conn.execute(
            "INSERT INTO conversations (id, title, created_at, updated_at)
             VALUES ('old-chat', 'Old chat', 0, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cron_jobs
               (id, conversation_id, title, schedule, prompt, enabled, tool_policy,
                notify, next_run_at, run_count, run_requested, created_at)
             VALUES ('j1', 'old-chat', 'Say hi', '* * * * *', 'say hi', 1,
                     'readonly', 1, 1000, 0, 0, 0)",
            [],
        )
        .unwrap();
    }

    let db = Db(Arc::new(Mutex::new(crate::db::open(&p).unwrap())));
    let job = db::get_cron_job(&db, "j1").unwrap().unwrap();
    assert_ne!(job.conversation_id, "old-chat");
    let thread = db::get_conversation(&db, &job.conversation_id).unwrap().unwrap();
    assert_eq!(thread.kind, "scheduled");
    assert_eq!(thread.title, "Say hi");
    assert_eq!(
        db::get_conversation(&db, "old-chat").unwrap().unwrap().kind,
        "chat",
        "the user's chat must not be converted or deleted"
    );

    // Idempotent: opening again does not create a second thread.
    let moved = job.conversation_id.clone();
    drop(db);
    let db = Db(Arc::new(Mutex::new(crate::db::open(&p).unwrap())));
    assert_eq!(
        db::get_cron_job(&db, "j1").unwrap().unwrap().conversation_id,
        moved
    );
}
