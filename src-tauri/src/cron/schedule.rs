//! Cron expression normalization and next-fire computation, in the device's
//! **local wall clock**.
//!
//! The stored schedule is always a normalized 5-field Vixie-style expression
//! (`minute hour day-of-month month day-of-week`). Presets are accepted as
//! shorthand and normalized on input. Sub-minute cadences are rejected, so no
//! schedule can fire more than once a minute.

/// Normalize user input (a preset token or a 5-field expression) into the stored
/// canonical expression. Returns a human-readable error for invalid input.
pub fn normalize(raw: &str) -> Result<String, String> {
    let _ = raw;
    Err("schedule parsing not implemented yet".into())
}

/// Whether `schedule` is a valid *normalized* expression.
pub fn is_valid(schedule: &str) -> bool {
    let _ = schedule;
    false
}

/// The next fire strictly after `from_ms` (epoch milliseconds), as epoch
/// milliseconds in the local time zone.
pub fn next_after_ms(schedule: &str, from_ms: i64) -> Result<i64, String> {
    let _ = (schedule, from_ms);
    Err("schedule parsing not implemented yet".into())
}

/// A short human description of a normalized expression.
pub fn describe(schedule: &str) -> String {
    schedule.to_string()
}
