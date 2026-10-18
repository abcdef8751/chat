//! Cron expression normalization and next-fire computation, in the device's
//! **local wall clock**.
//!
//! The stored schedule is always a normalized 5-field Vixie-style expression
//! (`minute hour day-of-month month day-of-week`). Presets are accepted as
//! shorthand and normalized on input. Sub-minute cadences are rejected, so no
//! schedule can fire more than once a minute.
//!
//! Accepted presets:
//! - `hourly` → `0 * * * *`
//! - `daily@HH:MM` → `MM HH * * *`
//! - `weekly@DDD:HH:MM` → `MM HH * * DDD` (`DDD` = `MON`…`SUN`)

use std::str::FromStr as _;

use chrono::{DateTime, Local};
use croner::Cron;

/// Normalize user input (a preset token or a 5-field expression) into the stored
/// canonical expression. Returns a human-readable error for invalid input.
pub fn normalize(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("schedule cannot be empty".to_string());
    }

    let lower = trimmed.to_ascii_lowercase();
    if lower == "hourly" {
        return Ok("0 * * * *".to_string());
    }
    if let Some(rest) = lower.strip_prefix("daily@") {
        let (hour, minute) = parse_hhmm(rest)?;
        return Ok(format!("{minute} {hour} * * *"));
    }
    if let Some(rest) = lower.strip_prefix("weekly@") {
        let (day, time) = match rest.split_once(':') {
            Some((day, time)) => (day, time),
            None => return Err(format!("invalid weekly@DDD:HH:MM preset: {raw}")),
        };
        let (hour, minute) = parse_hhmm(time)?;
        return Ok(format!("{minute} {hour} * * {}", parse_weekday(day)?));
    }

    // Raw expression: exactly 5 fields.
    let fields: Vec<&str> = trimmed.split_whitespace().collect();
    if fields.len() > 5 {
        return Err(
            "sub-minute schedules (a seconds field) are not supported; the floor is once per minute"
                .to_string(),
        );
    }
    if fields.len() < 5 {
        return Err(format!(
            "expected a cron expression with 5 fields, found {}",
            fields.len()
        ));
    }

    let expr = fields.join(" ");
    // Validate with croner; any parse error becomes the error.
    Cron::from_str(&expr).map_err(|e| e.to_string())?;
    Ok(expr)
}

/// Whether `schedule` is a valid *normalized* expression.
///
/// Part of the module's small public surface (the UI validates through
/// `cron_preview`, so this is exercised by tests today); retained because it is
/// the natural predicate for callers that already have a normalized string.
#[allow(dead_code)]
pub fn is_valid(schedule: &str) -> bool {
    normalize(schedule).is_ok()
}

/// The next fire strictly after `from_ms` (epoch milliseconds), as epoch
/// milliseconds in the local time zone.
pub fn next_after_ms(schedule: &str, from_ms: i64) -> Result<i64, String> {
    let cron = Cron::from_str(schedule).map_err(|e| e.to_string())?;
    // Construct the instant from the epoch timestamp, then resolve it in the
    // device's local wall clock (DST-aware via `with_timezone`).
    let from = DateTime::<chrono::Utc>::from_timestamp_millis(from_ms)
        .ok_or_else(|| format!("invalid timestamp: {from_ms} ms"))?
        .with_timezone(&Local);
    let next = cron
        .find_next_occurrence(&from, false)
        .map_err(|e| e.to_string())?;
    Ok(next.timestamp_millis())
}

/// A short human description of a normalized expression.
pub fn describe(schedule: &str) -> String {
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() == 5 {
        let (min, hour, dom, mon, dow) = (fields[0], fields[1], fields[2], fields[3], fields[4]);

        // `0 * * * *` → Every hour
        if min == "0" && hour == "*" && dom == "*" && mon == "*" && dow == "*" {
            return "Every hour".to_string();
        }

        // `M H * * *` → Daily at HH:MM
        if dom == "*" && mon == "*" && dow == "*" {
            if let Some(hhmm) = describe_hhmm(min, hour) {
                return format!("Daily at {hhmm}");
            }
        }

        // `M H * * DDD` → Weekly on <Weekday> at HH:MM
        if dom == "*" && mon == "*" {
            if let Some(weekday) = describe_weekday(dow) {
                if let Some(hhmm) = describe_hhmm(min, hour) {
                    return format!("Weekly on {weekday} at {hhmm}");
                }
            }
        }
    }

    match Cron::from_str(schedule) {
        Ok(cron) => cron.describe(),
        Err(_) => schedule.to_string(),
    }
}

/// Parse a `HH:MM` preset time (24h clock; zero-padding optional).
fn parse_hhmm(s: &str) -> Result<(u32, u32), String> {
    let mut parts = s.split(':');
    let hour = parts.next().unwrap_or("");
    let minute = match parts.next() {
        Some(m) => m,
        None => return Err(format!("invalid time \"{s}\" (expected HH:MM)")),
    };
    if parts.next().is_some() {
        return Err(format!("invalid time \"{s}\" (expected HH:MM)"));
    }
    let hour: u32 = hour
        .parse()
        .map_err(|_| format!("invalid time \"{s}\" (expected HH:MM)"))?;
    let minute: u32 = minute
        .parse()
        .map_err(|_| format!("invalid time \"{s}\" (expected HH:MM)"))?;
    if hour > 23 {
        return Err(format!("hour {hour} out of range 0-23 in \"{s}\""));
    }
    if minute > 59 {
        return Err(format!("minute {minute} out of range 0-59 in \"{s}\""));
    }
    Ok((hour, minute))
}

/// Case-insensitive 3-letter weekday name → canonical uppercase token.
fn parse_weekday(s: &str) -> Result<&'static str, String> {
    match s {
        "mon" => Ok("MON"),
        "tue" => Ok("TUE"),
        "wed" => Ok("WED"),
        "thu" => Ok("THU"),
        "fri" => Ok("FRI"),
        "sat" => Ok("SAT"),
        "sun" => Ok("SUN"),
        _ => Err(format!("invalid weekday \"{s}\" (expected mon..sun)")),
    }
}

/// `HH:MM` for a fixed numeric minute + hour, else `None`.
fn describe_hhmm(min: &str, hour: &str) -> Option<String> {
    let min: u32 = min.parse().ok()?;
    let hour: u32 = hour.parse().ok()?;
    if min > 59 || hour > 23 {
        return None;
    }
    Some(format!("{hour:02}:{min:02}"))
}

/// Full weekday name for a single-value day-of-week field (3-letter name or
/// `0`-`7`, where `0` and `7` are Sunday), else `None`.
fn describe_weekday(dow: &str) -> Option<&'static str> {
    match dow.to_ascii_lowercase().as_str() {
        "sun" | "0" | "7" => Some("Sunday"),
        "mon" | "1" => Some("Monday"),
        "tue" | "2" => Some("Tuesday"),
        "wed" | "3" => Some("Wednesday"),
        "thu" | "4" => Some("Thursday"),
        "fri" | "5" => Some("Friday"),
        "sat" | "6" => Some("Saturday"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Datelike, TimeDelta, TimeZone, Timelike, Utc, Weekday};

    /// Decode an epoch-millis timestamp into the local wall clock.
    fn local_dt(millis: i64) -> DateTime<Local> {
        DateTime::<chrono::Utc>::from_timestamp_millis(millis)
            .expect("valid timestamp")
            .with_timezone(&Local)
    }

    #[test]
    fn normalize_presets() {
        assert_eq!(normalize("hourly").unwrap(), "0 * * * *");
        assert_eq!(normalize("daily@8:05").unwrap(), "5 8 * * *");
        assert_eq!(normalize("daily@08:05").unwrap(), "5 8 * * *");
        assert_eq!(normalize("daily@0:0").unwrap(), "0 0 * * *");
        assert_eq!(normalize("weekly@mon:8:05").unwrap(), "5 8 * * MON");
        assert_eq!(normalize("weekly@SUN:23:59").unwrap(), "59 23 * * SUN");
        // Preset names are matched case-insensitively too.
        assert_eq!(normalize("HOURLY").unwrap(), "0 * * * *");
        assert_eq!(normalize("Daily@8:05").unwrap(), "5 8 * * *");
    }

    #[test]
    fn normalize_rejects_invalid_input() {
        assert!(normalize("").is_err());
        assert!(normalize("   ").is_err());
        // Too few fields.
        let err = normalize("0 8 * *").unwrap_err();
        assert!(err.contains("5 fields"), "unexpected error: {err}");
        // 6+ fields: sub-minute (seconds) rejected.
        for expr in ["0 8 * * * 0", "0 8 * * * 2026", " 0   8 * * * 0 "] {
            let err = normalize(expr).unwrap_err();
            assert!(
                err.contains("sub-minute"),
                "expected sub-minute error for {expr:?}, got: {err}"
            );
        }
        // Nonsense / bad values.
        assert!(normalize("nonsense").is_err());
        assert!(normalize("61 8 * * *").is_err());
        assert!(normalize("0 25 * * *").is_err());
        assert!(normalize("0 8 * * abc").is_err());
        // Bad presets.
        assert!(normalize("daily@25:00").is_err());
        assert!(normalize("daily@8:60").is_err());
        assert!(normalize("daily@8").is_err());
        assert!(normalize("daily@8:05:30").is_err());
        assert!(normalize("weekly@foo:8:05").is_err());
        assert!(normalize("weekly@8:05").is_err());
        assert!(normalize("weekly@mon").is_err());
        assert!(normalize("weekly@mon:8").is_err());
    }

    #[test]
    fn normalize_raw_trims_and_keeps_spelling() {
        // Extra whitespace is collapsed to single spaces; field spelling/case is kept.
        assert_eq!(normalize("  30   8   *   *   *  ").unwrap(), "30 8 * * *");
        assert_eq!(normalize("0 8 * * mon").unwrap(), "0 8 * * mon");
        assert_eq!(normalize("*/15 8-17 * * 1-5").unwrap(), "*/15 8-17 * * 1-5");
        assert!(is_valid("0 * * * *"));
        assert!(is_valid("hourly"));
        assert!(is_valid("daily@6:30"));
        assert!(!is_valid("61 * * * *"));
        assert!(!is_valid("0 8 * * * 0"));
        assert!(!is_valid(""));
    }

    #[test]
    fn next_after_lands_on_830_local() {
        // Deterministic June date (no DST transition anywhere in June).
        let from = Local
            .with_ymd_and_hms(2026, 6, 15, 7, 0, 0)
            .single()
            .expect("valid local time");
        let next = next_after_ms("30 8 * * *", from.timestamp_millis()).unwrap();
        let next_dt = local_dt(next);
        assert_eq!(
            (next_dt.hour(), next_dt.minute(), next_dt.second()),
            (8, 30, 0)
        );
        assert_eq!(next_dt.date_naive(), from.date_naive());

        // Starting after 08:30 → the fire lands the following day.
        let from2 = Local
            .with_ymd_and_hms(2026, 6, 15, 10, 0, 0)
            .single()
            .expect("valid local time");
        let next2 = next_after_ms("30 8 * * *", from2.timestamp_millis()).unwrap();
        let next2_dt = local_dt(next2);
        assert_eq!(
            (next2_dt.hour(), next2_dt.minute(), next2_dt.second()),
            (8, 30, 0)
        );
        assert_eq!(next2_dt.date_naive(), from.date_naive().succ_opt().unwrap());
    }

    #[test]
    fn next_after_is_strictly_after_fire_instant() {
        // from_ms is exactly a fire time → the result is the *following*
        // occurrence (the next day at 08:30), not the same instant.
        let from = Local
            .with_ymd_and_hms(2026, 6, 15, 8, 30, 0)
            .single()
            .expect("valid local time");
        let next = next_after_ms("30 8 * * *", from.timestamp_millis()).unwrap();
        assert!(next > from.timestamp_millis());
        let next_dt = local_dt(next);
        assert_eq!(
            (next_dt.hour(), next_dt.minute(), next_dt.second()),
            (8, 30, 0)
        );
        assert_eq!(next_dt.date_naive(), from.date_naive().succ_opt().unwrap());
    }

    #[test]
    fn hourly_next_is_within_60_minutes_and_strictly_after() {
        let from = Local
            .with_ymd_and_hms(2026, 6, 15, 12, 34, 56)
            .single()
            .expect("valid local time");
        let from_ms = from.timestamp_millis();
        let next = next_after_ms("0 * * * *", from_ms).unwrap();
        assert!(next > from_ms);
        assert!(
            next - from_ms <= 3_600_000,
            "next not within 60 minutes: {next}"
        );
        let next_dt = local_dt(next);
        assert_eq!(
            (next_dt.hour(), next_dt.minute(), next_dt.second()),
            (13, 0, 0)
        );
    }

    #[test]
    fn weekly_preset_lands_on_requested_weekday() {
        // 2026-06-10 is a Wednesday; the schedule fires on Friday 09:15 local.
        let from = Local
            .with_ymd_and_hms(2026, 6, 10, 12, 0, 0)
            .single()
            .expect("valid local time");
        assert_eq!(from.weekday(), Weekday::Wed, "test precondition");
        let next = next_after_ms("15 9 * * FRI", from.timestamp_millis()).unwrap();
        let next_dt = local_dt(next);
        assert_eq!(next_dt.weekday(), Weekday::Fri);
        assert_eq!(
            next_dt.date_naive(),
            from.date_naive().succ_opt().unwrap().succ_opt().unwrap()
        );
        assert_eq!(
            (next_dt.hour(), next_dt.minute(), next_dt.second()),
            (9, 15, 0)
        );
        assert!(next > from.timestamp_millis());
    }

    #[test]
    fn dst_transition_no_panic_and_strictly_after() {
        // Scan forward for the next instant where the local UTC offset changes
        // (a DST transition) and take a timestamp within a couple of hours of
        // it. On a time zone with no DST the scan never fires — fall back to
        // "now"; the assertions below still hold either way.
        let mut probe = Utc::now();
        let mut prev = Local
            .from_utc_datetime(&probe.naive_utc())
            .offset()
            .local_minus_utc();
        let mut from_ms = probe.timestamp_millis();
        for _ in 0..4000 {
            probe += TimeDelta::hours(6);
            let cur = Local
                .from_utc_datetime(&probe.naive_utc())
                .offset()
                .local_minus_utc();
            if cur != prev {
                // Transition happened somewhere in (prev_probe, probe]; the
                // timestamp two hours after the previous probe is close to it.
                from_ms = (probe - TimeDelta::hours(4)).timestamp_millis();
                break;
            }
            prev = cur;
        }
        let next = next_after_ms("0 3 * * *", from_ms).expect("must not panic near DST");
        assert!(next > from_ms, "next fire must be strictly after from_ms");
    }

    #[test]
    fn describe_readable_strings() {
        assert_eq!(describe("0 * * * *"), "Every hour");
        assert_eq!(describe("5 8 * * *"), "Daily at 08:05");
        assert_eq!(describe("0 0 * * *"), "Daily at 00:00");
        assert_eq!(describe("15 9 * * FRI"), "Weekly on Friday at 09:15");
        // Numeric day-of-week (0 and 7 are Sunday).
        assert_eq!(describe("30 6 * * 1"), "Weekly on Monday at 06:30");
        assert_eq!(describe("5 8 * * 0"), "Weekly on Sunday at 08:05");
        assert_eq!(describe("59 23 * * 7"), "Weekly on Sunday at 23:59");
        // Range/ranges fall back to croner's own description.
        assert!(!describe("*/15 8 * * 1-5").is_empty());
        // Unparseable input → the input itself.
        assert_eq!(describe("not a cron"), "not a cron");
    }

    #[test]
    fn next_after_rejects_invalid_input() {
        assert!(next_after_ms("garbage", 0).is_err());
        // Out-of-range epoch (i64::MAX ms is far beyond chrono's representable
        // range) → the timestamp conversion returns None → error.
        assert!(next_after_ms("0 * * * *", i64::MAX).is_err());
        assert!(next_after_ms("0 * * * *", i64::MIN).is_err());
    }
}
