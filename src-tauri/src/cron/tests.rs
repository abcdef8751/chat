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

fn job(db: &Db, cid: &str, schedule: &str, next: i64) -> db::CronJob {
    db::insert_cron_job(
        db,
        cid.to_string(),
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

    let older = job(&db, "c1", "0 * * * *", past);
    let newer = job(&db, "c1", "0 * * * *", later);
    let future_job = job(&db, "c1", "0 * * * *", future);
    let paused = job(&db, "c1", "0 * * * *", past);
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
    let j = job(&db, "c1", "0 * * * *", 1_000);

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
    insert_conv(&db, "c1");
    let j = job(&db, "c1", "0 * * * *", 1_000);

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
    assert_eq!(db::list_cron_triggers(&db, Some("c1")).unwrap().len(), 1);
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

/// Deleting a conversation cascades to its jobs (and their runs), so a deleted
/// chat cannot leave a scheduler entry pointing at a missing thread.
#[test]
fn conversation_delete_cascades_jobs_and_runs() {
    let db = open_db("cascade");
    insert_conv(&db, "c1");
    let j = job(&db, "c1", "0 * * * *", 1_000);
    db::insert_cron_run(&db, &j, 1_000).unwrap();

    {
        let conn = db.0.lock().unwrap();
        conn.execute("DELETE FROM conversations WHERE id = 'c1'", [])
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
    let j = job(&db, "c1", "0 * * * *", now - 2 * 3_600_000);

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
    let j = job(&db, "c1", "0 * * * *", now - 3_600_000);

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
    let j = job(&db, "c1", "0 * * * *", 1_000);
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
    let j = job(&db, "c1", "30 8 * * *", 1_000);
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
    insert_conv(&db, "c1");

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
        "c1",
    );
    assert!(!created.is_error, "create failed: {}", created.content);
    let job: Value = serde_json::from_str(&created.content).unwrap();
    assert_eq!(job["schedule"], "30 8 * * *");
    assert_eq!(job["toolPolicy"], "autoapprove");
    assert_eq!(job["conversationId"], "c1");
    assert_eq!(job["enabled"], true);
    // The title defaults to the prompt.
    assert_eq!(job["title"], "Morning briefing");
    let id = job["id"].as_str().unwrap().to_string();

    let listed = execute_tool(&tool_call("list_crons", json!({})), &db, "c1");
    let listed: Value = serde_json::from_str(&listed.content).unwrap();
    assert_eq!(listed["jobs"].as_array().unwrap().len(), 1);
    assert!(listed["jobs"][0]["nextRun"].as_str().is_some());

    let updated = execute_tool(
        &tool_call(
            "update_cron",
            json!({ "id": id, "enabled": false, "title": "Paused briefing" }),
        ),
        &db,
        "c1",
    );
    assert!(!updated.is_error, "update failed: {}", updated.content);
    let updated: Value = serde_json::from_str(&updated.content).unwrap();
    assert_eq!(updated["enabled"], false);
    assert_eq!(updated["title"], "Paused briefing");

    let queued = execute_tool(&tool_call("run_cron_now", json!({ "id": id })), &db, "c1");
    assert!(!queued.is_error);
    assert!(
        db::get_cron_job(&db, &id).unwrap().unwrap().run_requested,
        "run_cron_now must queue a one-shot fire"
    );

    let deleted = execute_tool(&tool_call("delete_cron", json!({ "id": id })), &db, "c1");
    assert!(!deleted.is_error);
    assert!(db::list_cron_jobs(&db).unwrap().is_empty());

    // Invalid input is reported as a tool error, not a panic.
    let bad = execute_tool(
        &tool_call(
            "create_cron",
            json!({ "prompt": "x", "schedule": "0 8 * * * 0" }),
        ),
        &db,
        "c1",
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
