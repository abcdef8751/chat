//! Scheduled chat runs ("cron jobs").
//!
//! A job is a schedule + a prompt + a conversation. Each fire appends a normal
//! user → assistant exchange to that conversation (so the run continues the
//! thread: full history, memory notes, clock note, compaction summary) and
//! records an audit row in `cron_runs`.

pub mod schedule;
