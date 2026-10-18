//! Scheduled chat runs ("cron jobs").
//!
//! A job is a schedule + a prompt + a conversation. Each fire appends a normal
//! user → assistant exchange to that conversation (so the run continues the
//! thread: full history, memory notes, clock note, compaction summary) and
//! records an audit row in `cron_runs`.
//!
//! The scheduler mirrors `reflection::spawn`: a background task that ticks often
//! enough for 1-minute jobs to feel right, with one immediate pass at startup so
//! a run missed while the app was closed catches up. Firing is capped in three
//! ways — at most [`MAX_FIRES_PER_TICK`] launches per tick, at most
//! [`MAX_CONCURRENT_RUNS`] in flight overall, and one run per conversation (the
//! same `StreamRegistry` slot a live turn or a reflection pass uses, so the three
//! cannot overlap).
//!
//! Job definitions are **local-only** (never synced); the messages a run produces
//! are ordinary `user`/`assistant` rows and sync normally, so another device just
//! sees the thread continue.

use std::collections::HashSet;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::chat::{self, CancelToken, CronToolPolicy, EventSink, StreamEvent, ToolMode};
use crate::config::ConfigState;
use crate::db;
use crate::tools::{ToolCall, ToolOutput};

pub mod schedule;

#[cfg(test)]
mod tests;

/// How often the scheduler looks for due jobs. Finer than reflection's 60 s so a
/// `* * * * *` job does not visibly drift.
const TICK: Duration = Duration::from_secs(20);
/// Let startup (DB, config, UI) settle before the first pass; the pass then runs
/// immediately so a slot missed while the app was closed catches up.
const STARTUP_DELAY: Duration = Duration::from_secs(3);
/// Hard cap on launches per tick, so a long backlog cannot flood the provider.
const MAX_FIRES_PER_TICK: usize = 5;
/// Hard cap on cron runs in flight at once (each is a whole model turn).
const MAX_CONCURRENT_RUNS: usize = 3;

/// Runtime state for the scheduler: which conversations have a cron run in
/// flight, and which conversation the UI currently shows (used to suppress an OS
/// notification for a run the user is already watching).
#[derive(Default)]
pub struct CronState {
    running: Mutex<HashSet<String>>,
    active_conversation: Mutex<Option<String>>,
}

impl CronState {
    /// Claim the right to run a job in `conversation_id`. False when that chat
    /// already has a cron run, or the global concurrency cap is reached.
    fn try_begin(&self, conversation_id: &str) -> bool {
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        if running.len() >= MAX_CONCURRENT_RUNS {
            return false;
        }
        running.insert(conversation_id.to_string())
    }

    fn end(&self, conversation_id: &str) {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(conversation_id);
    }

    fn set_active(&self, conversation_id: Option<String>) {
        *self
            .active_conversation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = conversation_id;
    }

    fn is_active(&self, conversation_id: &str) -> bool {
        self.active_conversation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            == Some(conversation_id)
    }
}

/// Sink for a run with no UI channel: events are discarded (the thread is
/// reloaded from the DB after the run).
struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _ev: StreamEvent) {}
}

/// The cron-management tools. Kept here so `tools::tool_specs` (the shared list)
/// and dispatch (in `chat.rs`) cannot drift.
pub const CRON_TOOLS: &[&str] = &[
    "create_cron",
    "list_crons",
    "update_cron",
    "delete_cron",
    "run_cron_now",
];

pub fn is_cron_tool(name: &str) -> bool {
    CRON_TOOLS.contains(&name)
}

/// Start the background scheduler. One immediate pass, then every [`TICK`].
pub fn spawn(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        loop {
            if let Err(e) = tick(&app).await {
                eprintln!("cron: tick failed: {e}");
            }
            tokio::time::sleep(TICK).await;
        }
    });
}

async fn tick(app: &AppHandle) -> Result<(), String> {
    let cfg = app.state::<ConfigState>().get();
    if !cfg.cron_enabled {
        return Ok(());
    }
    let now = db::now_ms();
    let due = {
        let db = app.state::<db::Db>();
        db::due_cron_jobs(&db, now)?
    };
    let mut fired = 0;
    for job in due {
        if fired >= MAX_FIRES_PER_TICK {
            break;
        }
        // Interlock: a live turn, a reflection pass or another cron run in this
        // chat defers the job to the next tick (its slot is not advanced).
        if app
            .state::<chat::StreamRegistry>()
            .is_streaming(&job.conversation_id)
        {
            continue;
        }
        match launch(app, &job, job.run_requested) {
            Ok(true) => fired += 1,
            // Occupation raced us or the cap is full: try again next tick.
            Ok(false) => {}
            Err(e) => eprintln!("cron: could not launch {}: {e}", job.id),
        }
    }
    Ok(())
}

/// Register and spawn one run. Returns `false` when the conversation is busy or
/// the concurrency cap is reached (the caller leaves the slot alone).
fn launch(app: &AppHandle, job: &db::CronJob, manual: bool) -> Result<bool, String> {
    let state = app.state::<CronState>();
    if !state.try_begin(&job.conversation_id) {
        return Ok(false);
    }
    // `try_register` (not `register`): a live user turn may have started in the
    // window since the tick's check, and replacing its cancel token would make
    // Stop abort the wrong turn.
    let Some(token) = app
        .state::<chat::StreamRegistry>()
        .try_register(job.conversation_id.clone())
    else {
        state.end(&job.conversation_id);
        return Ok(false);
    };

    let app = app.clone();
    let job = job.clone();
    let tail_token = token.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = run_job(&app, &job, manual, token).await {
            eprintln!("cron: run of {} failed: {e}", job.id);
        }
        app.state::<chat::StreamRegistry>()
            .remove(&job.conversation_id, &tail_token);
        app.state::<CronState>().end(&job.conversation_id);
    });
    Ok(true)
}

/// Execute one scheduled (or manual) run and record it.
async fn run_job(
    app: &AppHandle,
    job: &db::CronJob,
    manual: bool,
    token: CancelToken,
) -> Result<(), String> {
    let cfg = app.state::<ConfigState>().get();
    let db = app.state::<db::Db>();

    let conversation = db::get_conversation(&db, &job.conversation_id)?
        .ok_or_else(|| format!("conversation not found: {}", job.conversation_id))?;
    let provider = cfg.provider_for(conversation.provider_id.as_deref().unwrap_or(""));

    // A queued manual request is one-shot: clear it before any early return so a
    // job with no model/key does not stay "requested" forever.
    if job.run_requested {
        db::clear_cron_run_request(&db, &job.id)?;
    }

    let Some(api_key) = crate::providers::resolve(&provider.id)? else {
        // Nothing to run on. Move the slot forward quietly rather than retrying
        // every tick; no `cron_runs` row, no run counter bump.
        skip_slot(&db, job, manual)?;
        return Ok(());
    };
    let Some(model) = crate::reflection::conversation_model(&cfg, &conversation) else {
        skip_slot(&db, job, manual)?;
        return Ok(());
    };

    // Resolve the model/provider the same way a live turn and reflection do: the
    // conversation's own provider, its pinned model (else the app default), and
    // the reasoning level the chat last recorded.
    let history = db::read_messages(&db, &job.conversation_id)?;
    let mut turn_cfg = cfg.clone();
    turn_cfg.base_url = provider.base_url;
    turn_cfg.model = model;
    turn_cfg.thinking_level = crate::reflection::conversation_thinking_level(&history, &cfg);
    // The job's policy is authoritative: a `readonly` job must not inherit the
    // global YOLO switch, and an `autoapprove` job must not depend on it.
    turn_cfg.auto_approve_tools = job.tool_policy == "autoapprove";
    let policy = if turn_cfg.auto_approve_tools {
        CronToolPolicy::AutoApprove
    } else {
        CronToolPolicy::ReadOnly
    };

    let scheduled_for = if manual { db::now_ms() } else { job.next_run_at };
    let run_id = db::insert_cron_run(&db, job, scheduled_for)?;
    let _ = app.emit(
        "cron-updated",
        json!({
            "jobId": job.id,
            "conversationId": job.conversation_id,
            "status": "started",
        }),
    );

    let brave = app.state::<crate::tools::BraveSearch>();
    let shell = app.state::<crate::shell::ShellExecutor>();
    let memory = app.state::<crate::memory::MemoryState>();
    let approvals = app.state::<crate::tools::ApprovalRegistry>();
    let sink = NullSink;

    let result = chat::run_turn_core(
        &db,
        &turn_cfg,
        &api_key,
        &brave,
        &shell,
        &memory,
        &approvals,
        token,
        &sink,
        job.conversation_id.clone(),
        run_prompt(job),
        Vec::new(),
        false,
        ToolMode::Cron(policy),
    )
    .await;
    approvals.deny_all();

    let now = db::now_ms();
    let (stop_reason, error, usage, cost, trigger, reply) = match &result {
        Ok(outcome) => (
            outcome.stop_reason.clone(),
            outcome.error.clone(),
            Some(outcome.usage.to_string()),
            outcome.usage.get("cost").and_then(Value::as_f64),
            outcome.user_message_id.clone(),
            outcome.text.clone(),
        ),
        Err(e) => ("error".to_string(), Some(e.clone()), None, None, None, String::new()),
    };
    db::finish_cron_run(
        &db,
        &run_id,
        trigger.as_deref(),
        &stop_reason,
        error.as_deref(),
        usage.as_deref(),
        cost,
    )?;
    advance(&db, job, now, manual)?;

    let failed = stop_reason == "error";
    let body = if failed {
        format!(
            "Run failed: {}",
            error.as_deref().unwrap_or("unknown error")
        )
    } else if stop_reason == "aborted" {
        "Run stopped before it finished.".to_string()
    } else {
        snippet(&reply)
    };
    notify(app, job, &body);
    let _ = app.emit(
        "cron-updated",
        json!({
            "jobId": job.id,
            "conversationId": job.conversation_id,
            "status": if failed { "error" } else { "finished" },
        }),
    );
    // A failed turn is already recorded and notified; report Ok so the scheduler
    // does not double-log it as a scheduler error.
    Ok(())
}

/// The text a fired job sends as its user turn: the prompt plus an autonomous
/// footer, so the model AND anyone reading the thread know why it appeared.
fn run_prompt(job: &db::CronJob) -> String {
    format!(
        "{}\n\n[Automated scheduled run of \"{}\" (cron job {}). Fired by the schedule `{}` — \
         carry out the instruction above. You may read_memory first if useful.]",
        job.prompt.trim(),
        job.title,
        job.id,
        job.schedule
    )
}

/// Advance a job's schedule after a run.
///
/// Catch-up is max one: the fire that just happened satisfies any backlog, and
/// the next slot is computed from **now** (so a device that was asleep for a day
/// resyncs forward instead of replaying every missed slot).
fn advance(db: &db::Db, job: &db::CronJob, now: i64, manual: bool) -> Result<(), String> {
    if manual {
        // A manual fire does not advance the schedule. It does, however, satisfy
        // a slot that was already overdue (otherwise the scheduler would fire the
        // same overdue slot again on its next tick and double-run the job).
        let next = if job.next_run_at <= now {
            schedule::next_after_ms(&job.schedule, now).unwrap_or(job.next_run_at)
        } else {
            job.next_run_at
        };
        return db::set_cron_job_run_state(db, &job.id, next, now, false);
    }
    match schedule::next_after_ms(&job.schedule, now) {
        Ok(next) => db::set_cron_job_run_state(db, &job.id, next, now, true),
        Err(e) => {
            // A stored schedule was normalized on write, so this is
            // unreachable in practice. Disable rather than fire every tick.
            db::set_cron_job_enabled(db, &job.id, false)?;
            Err(format!("invalid stored schedule for {}: {e}", job.id))
        }
    }
}

/// Move a due job's slot forward without recording a run (no model / no key).
fn skip_slot(db: &db::Db, job: &db::CronJob, manual: bool) -> Result<(), String> {
    if manual {
        return Ok(());
    }
    match schedule::next_after_ms(&job.schedule, db::now_ms()) {
        Ok(next) => db::set_cron_job_next_run(db, &job.id, next),
        Err(e) => {
            db::set_cron_job_enabled(db, &job.id, false)?;
            Err(format!("invalid stored schedule for {}: {e}", job.id))
        }
    }
}

/// One-line notification body: the reply's first ~200 characters, flattened.
fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(200).collect();
    if flat.chars().count() > 200 {
        out.push('…');
    }
    if out.is_empty() {
        "Run finished with no reply.".to_string()
    } else {
        out
    }
}

/// Send an OS notification for a finished run. Suppressed while the app is
/// focused on that exact conversation (the user is already watching it).
fn notify(app: &AppHandle, job: &db::CronJob, body: &str) {
    if !job.notify {
        return;
    }
    let focused = app
        .get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if focused && app.state::<CronState>().is_active(&job.conversation_id) {
        return;
    }
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = app
        .notification()
        .builder()
        .title(&job.title)
        .body(body)
        .show()
    {
        eprintln!("cron: notification failed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Agent tools
// ---------------------------------------------------------------------------

/// Dispatch one cron-management tool call. Ungated and app-local (like the
/// memory tools), so it needs only the DB.
pub fn execute_tool(call: &ToolCall, db: &db::Db, source: &str) -> ToolOutput {
    match call.name.as_str() {
        "create_cron" => tool_create(db, &call.arguments, source),
        "list_crons" => tool_list(db),
        "update_cron" => tool_update(db, &call.arguments),
        "delete_cron" => tool_delete(db, &call.arguments),
        "run_cron_now" => tool_run_now(db, &call.arguments),
        other => ToolOutput::err(format!("unknown cron tool: {other}")),
    }
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// Normalize a tool-supplied policy (`readonly` | `autoapprove`, tolerating
/// `auto-approve` / `auto_approve`).
fn normalize_policy(raw: Option<&str>) -> Result<String, String> {
    let raw = raw.unwrap_or("readonly").trim().to_ascii_lowercase();
    match raw.replace(['-', '_'], "").as_str() {
        "" | "readonly" => Ok("readonly".into()),
        "autoapprove" => Ok("autoapprove".into()),
        other => Err(format!(
            "tool policy must be `readonly` or `autoapprove`, got `{other}`"
        )),
    }
}

fn job_json(job: &db::CronJob) -> Value {
    json!({
        "id": job.id,
        "title": job.title,
        "conversationId": job.conversation_id,
        "schedule": job.schedule,
        "scheduleDescription": schedule::describe(&job.schedule),
        "prompt": job.prompt,
        "enabled": job.enabled,
        "toolPolicy": job.tool_policy,
        "notify": job.notify,
        "nextRunAt": job.next_run_at,
        "nextRun": chat::format_date_time(job.next_run_at),
        "lastRunAt": job.last_run_at,
        "lastRun": job.last_run_at.map(chat::format_date_time),
        "runCount": job.run_count,
    })
}

fn tool_create(db: &db::Db, args: &Value, source: &str) -> ToolOutput {
    let Some(prompt) = arg_str(args, "prompt") else {
        return ToolOutput::err("create_cron requires `prompt`".into());
    };
    let Some(raw_schedule) = arg_str(args, "schedule") else {
        return ToolOutput::err("create_cron requires `schedule`".into());
    };
    let normalized = match schedule::normalize(raw_schedule) {
        Ok(s) => s,
        Err(e) => return ToolOutput::err(format!("invalid schedule: {e}")),
    };
    let conversation_id = arg_str(args, "conversation_id")
        .map(str::to_string)
        .unwrap_or_else(|| source.to_string());
    if conversation_id.is_empty() {
        return ToolOutput::err("create_cron requires a conversation".into());
    }
    match db::get_conversation(db, &conversation_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return ToolOutput::err(format!("conversation not found: {conversation_id}"))
        }
        Err(e) => return ToolOutput::err(e),
    }
    let policy = match normalize_policy(arg_str(args, "tool_policy")) {
        Ok(p) => p,
        Err(e) => return ToolOutput::err(e),
    };
    let title = arg_str(args, "title")
        .map(str::to_string)
        .unwrap_or_else(|| db::derive_title(prompt));
    let notify = arg_bool(args, "notify").unwrap_or(true);
    let next_run_at = match schedule::next_after_ms(&normalized, db::now_ms()) {
        Ok(t) => t,
        Err(e) => return ToolOutput::err(format!("invalid schedule: {e}")),
    };
    match db::insert_cron_job(
        db,
        conversation_id,
        title,
        normalized,
        prompt.to_string(),
        policy,
        notify,
        next_run_at,
    ) {
        Ok(job) => ToolOutput::ok(job_json(&job).to_string()),
        Err(e) => ToolOutput::err(e),
    }
}

fn tool_list(db: &db::Db) -> ToolOutput {
    match db::list_cron_jobs(db) {
        Ok(jobs) => {
            let items: Vec<Value> = jobs.iter().map(job_json).collect();
            ToolOutput::ok(json!({ "jobs": items }).to_string())
        }
        Err(e) => ToolOutput::err(e),
    }
}

fn tool_update(db: &db::Db, args: &Value) -> ToolOutput {
    let Some(id) = arg_str(args, "id") else {
        return ToolOutput::err("update_cron requires `id`".into());
    };
    let Some(job) = (match db::get_cron_job(db, id) {
        Ok(j) => j,
        Err(e) => return ToolOutput::err(e),
    }) else {
        return ToolOutput::err(format!("cron job not found: {id}"));
    };
    // `job_json`/create use trimmed values; update accepts the same fields.
    match apply_updates(db, job, args) {
        Ok(job) => ToolOutput::ok(job_json(&job).to_string()),
        Err(e) => ToolOutput::err(e),
    }
}

/// Shared by the agent tool and the UI command: apply optional fields, validate,
/// recompute `next_run_at` if the schedule changed, persist.
fn apply_updates(db: &db::Db, mut job: db::CronJob, args: &Value) -> Result<db::CronJob, String> {
    if let Some(prompt) = arg_str(args, "prompt") {
        job.prompt = prompt.to_string();
    }
    if let Some(title) = arg_str(args, "title") {
        job.title = title.to_string();
    }
    if let Some(raw) = arg_str(args, "schedule") {
        let normalized = schedule::normalize(raw)?;
        if normalized != job.schedule {
            job.schedule = normalized;
            job.next_run_at = schedule::next_after_ms(&job.schedule, db::now_ms())?;
        }
    }
    if let Some(policy) = arg_str(args, "tool_policy") {
        job.tool_policy = normalize_policy(Some(policy))?;
    }
    if let Some(notify) = arg_bool(args, "notify") {
        job.notify = notify;
    }
    if let Some(enabled) = arg_bool(args, "enabled") {
        job.enabled = enabled;
        // Re-enabling a paused job should not fire immediately for all the slots
        // it missed while paused: resync to the next future slot.
        if enabled {
            job.next_run_at = schedule::next_after_ms(&job.schedule, db::now_ms())?;
        }
    }
    db::update_cron_job(db, &job)?;
    Ok(job)
}

fn tool_delete(db: &db::Db, args: &Value) -> ToolOutput {
    let Some(id) = arg_str(args, "id") else {
        return ToolOutput::err("delete_cron requires `id`".into());
    };
    match db::delete_cron_job(db, id) {
        Ok(true) => ToolOutput::ok(format!("Deleted cron job {id}.")),
        Ok(false) => ToolOutput::err(format!("cron job not found: {id}")),
        Err(e) => ToolOutput::err(e),
    }
}

fn tool_run_now(db: &db::Db, args: &Value) -> ToolOutput {
    let Some(id) = arg_str(args, "id") else {
        return ToolOutput::err("run_cron_now requires `id`".into());
    };
    match db::get_cron_job(db, id) {
        Ok(Some(_)) => {}
        Ok(None) => return ToolOutput::err(format!("cron job not found: {id}")),
        Err(e) => return ToolOutput::err(e),
    }
    match db::request_cron_run(db, id) {
        Ok(()) => ToolOutput::ok(format!(
            "Queued cron job {id} to run now; the scheduler picks it up within ~20 seconds."
        )),
        Err(e) => ToolOutput::err(e),
    }
}

// ---------------------------------------------------------------------------
// Tauri commands (UI)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CronPreview {
    /// The normalized expression, or the raw input when it is invalid.
    pub schedule: String,
    pub description: String,
    pub next_run_at: Option<i64>,
    pub error: Option<String>,
}

/// Validate a schedule as the user types and preview its next fire.
#[tauri::command]
pub fn cron_preview(schedule: String) -> CronPreview {
    match schedule::normalize(&schedule) {
        Ok(normalized) => match schedule::next_after_ms(&normalized, db::now_ms()) {
            Ok(next) => CronPreview {
                description: schedule::describe(&normalized),
                schedule: normalized,
                next_run_at: Some(next),
                error: None,
            },
            Err(e) => CronPreview {
                schedule: normalized,
                description: String::new(),
                next_run_at: None,
                error: Some(e),
            },
        },
        Err(e) => CronPreview {
            schedule,
            description: String::new(),
            next_run_at: None,
            error: Some(e),
        },
    }
}

#[tauri::command]
pub fn list_crons(db: tauri::State<'_, db::Db>) -> Result<Vec<db::CronJob>, String> {
    db::list_cron_jobs(&db)
}

#[tauri::command]
pub fn list_cron_runs(
    db: tauri::State<'_, db::Db>,
    job_id: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<db::CronRunRecord>, String> {
    let limit = limit.unwrap_or(20).clamp(1, 200);
    db::list_cron_runs(&db, job_id.as_deref(), limit)
}

#[tauri::command]
pub fn list_cron_triggers(db: tauri::State<'_, db::Db>) -> Result<Vec<db::CronTrigger>, String> {
    db::list_cron_triggers(&db)
}

/// Tell the backend which chat is on screen, so a finished run in that chat does
/// not raise an OS notification the user can already see.
#[tauri::command]
pub fn cron_set_active(
    state: tauri::State<'_, CronState>,
    conversation_id: Option<String>,
) -> Result<(), String> {
    state.set_active(conversation_id);
    Ok(())
}

#[tauri::command]
pub fn create_cron(
    db: tauri::State<'_, db::Db>,
    conversation_id: Option<String>,
    title: Option<String>,
    schedule: String,
    prompt: String,
    tool_policy: Option<String>,
    notify: Option<bool>,
) -> Result<db::CronJob, String> {
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err("A scheduled run needs a prompt.".into());
    }
    let conversation_id = conversation_id
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "A scheduled run needs a conversation.".to_string())?;
    if db::get_conversation(&db, &conversation_id)?.is_none() {
        return Err(format!("conversation not found: {conversation_id}"));
    }
    let normalized = schedule::normalize(&schedule)?;
    let next_run_at = schedule::next_after_ms(&normalized, db::now_ms())?;
    let policy = normalize_policy(tool_policy.as_deref())?;
    let title = title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| db::derive_title(&prompt));
    db::insert_cron_job(
        &db,
        conversation_id,
        title,
        normalized,
        prompt,
        policy,
        notify.unwrap_or(true),
        next_run_at,
    )
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn update_cron(
    db: tauri::State<'_, db::Db>,
    id: String,
    title: Option<String>,
    schedule: Option<String>,
    prompt: Option<String>,
    enabled: Option<bool>,
    tool_policy: Option<String>,
    notify: Option<bool>,
) -> Result<db::CronJob, String> {
    let job = db::get_cron_job(&db, &id)?.ok_or_else(|| format!("cron job not found: {id}"))?;
    // Reuse the agent-tool path by building the same argument object, so the two
    // entry points cannot validate or recompute differently.
    let mut args = serde_json::Map::new();
    if let Some(title) = title.filter(|t| !t.trim().is_empty()) {
        args.insert("title".into(), json!(title));
    }
    if let Some(schedule) = schedule.filter(|s| !s.trim().is_empty()) {
        args.insert("schedule".into(), json!(schedule));
    }
    if let Some(prompt) = prompt.filter(|p| !p.trim().is_empty()) {
        args.insert("prompt".into(), json!(prompt));
    }
    if let Some(enabled) = enabled {
        args.insert("enabled".into(), json!(enabled));
    }
    if let Some(policy) = tool_policy {
        args.insert("tool_policy".into(), json!(policy));
    }
    if let Some(notify) = notify {
        args.insert("notify".into(), json!(notify));
    }
    apply_updates(&db, job, &Value::Object(args))
}

#[tauri::command]
pub fn delete_cron(db: tauri::State<'_, db::Db>, id: String) -> Result<(), String> {
    if db::delete_cron_job(&db, &id)? {
        Ok(())
    } else {
        Err(format!("cron job not found: {id}"))
    }
}

/// Fire a job immediately (in the background) without touching its schedule.
#[tauri::command]
pub fn run_cron_now(
    app: AppHandle,
    db: tauri::State<'_, db::Db>,
    id: String,
) -> Result<(), String> {
    let job = db::get_cron_job(&db, &id)?.ok_or_else(|| format!("cron job not found: {id}"))?;
    if app
        .state::<chat::StreamRegistry>()
        .is_streaming(&job.conversation_id)
    {
        return Err("A chat turn is already running in that conversation — wait for it to finish."
            .into());
    }
    match launch(&app, &job, true)? {
        true => Ok(()),
        false => Err("A scheduled run is already in flight for that chat.".into()),
    }
}
