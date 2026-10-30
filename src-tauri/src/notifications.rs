//! OS notifications for **live** chat turns that finish while the app is in the
//! background.
//!
//! A turn the user is watching needs no notification: the transcript updates in
//! place (and the frontend shows its own per-chat indicator when the user is on
//! another conversation inside a foreground app). The OS notification exists
//! only for the case where the user cannot see the app at all — the window is
//! unfocused/hidden to the tray on desktop, or the Activity is stopped on
//! Android.
//!
//! Suppression is therefore **foreground-only**: unlike a cron run (which is
//! suppressed when the app is in the foreground *and* the run's thread is on
//! screen), a live turn still notifies while the app is in the background even
//! if its conversation happens to be the active one — the user is not watching.
//! The frontend's in-app dots cover the "foreground app, another conversation"
//! case. See [`app_is_foreground`] for why Android uses the Activity lifecycle
//! rather than window focus.
//!
//! The posting path is deliberately the same one `cron::notify` uses
//! ([`crate::cron::notifications_allowed`] + [`crate::cron::send_notification`]):
//! Linux calls notify-rust directly (so a broken D-Bus path returns a real
//! error instead of looking like success), and Android keeps the
//! POST_NOTIFICATIONS runtime-permission check, logging a blocked post rather
//! than pretending it was sent.

use tauri::{AppHandle, Manager};

/// Whether the user can currently see the app.
///
/// **Desktop:** the main window is focused. A hidden (close-to-tray) or
/// unfocused window is not being watched, so a completion should notify.
///
/// **Android:** the Activity is visible (between `onStart` and `onStop`),
/// published through `nativeSetForeground`. `WebviewWindow::is_focused()` is
/// unreliable there — it can report focused while backgrounded (suppressing
/// every notification) or unfocused while visibly open (notifying in the user's
/// face) — so the lifecycle flag is the only dependable signal.
pub(crate) fn app_is_foreground(app: &AppHandle) -> bool {
    #[cfg(target_os = "android")]
    {
        let _ = app;
        crate::android::is_foreground()
    }
    #[cfg(not(target_os = "android"))]
    {
        app.get_webview_window("main")
            .and_then(|w| w.is_focused().ok())
            .unwrap_or(false)
    }
}

/// What a finished turn produced, as far as a notification is concerned.
///
/// Reasoning traces (`thinking`) are never part of this: the notification body
/// is the final assistant text only (or a short provider/turn error).
pub(crate) enum TurnOutcome<'a> {
    /// The final assistant reply text (may be empty on the tool-round cap path).
    Reply(&'a str),
    /// The turn failed; the message is a short error snippet.
    Failed(&'a str),
}

/// Notify that a live turn in `conversation_id` finished, unless the app's main
/// window is focused (the user is watching, so the transcript is the signal).
///
/// `title` is the conversation title (falling back to "Pi Chat" when blank).
pub(crate) fn notify_turn_finished(
    app: &AppHandle,
    conversation_id: &str,
    title: &str,
    outcome: TurnOutcome<'_>,
) {
    // Foreground is the whole predicate: a foreground app means the user can
    // see the in-app indicators (a hidden window or paused Android Activity
    // reports false, which is exactly when an OS notification helps).
    if app_is_foreground(app) {
        return;
    }

    match crate::cron::notifications_allowed(app, false) {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("notify: notification blocked by the OS (permission not granted)");
            return;
        }
        Err(e) => eprintln!("notify: notification permission check failed: {e}"),
    }

    let title = notification_title(title);
    let body = notification_body(outcome);
    match crate::cron::send_notification(app, &title, &body) {
        Ok(()) => eprintln!("notify: turn notification sent for chat {conversation_id}"),
        Err(e) => eprintln!("notify: turn notification failed: {e}"),
    }
}

/// Notification title: the conversation title, or the app name when it is blank.
fn notification_title(title: &str) -> String {
    let t = title.trim();
    if t.is_empty() {
        "Pi Chat".to_string()
    } else {
        t.to_string()
    }
}

/// One-line notification body for a finished turn.
fn notification_body(outcome: TurnOutcome<'_>) -> String {
    match outcome {
        TurnOutcome::Reply(text) => {
            let flat = collapse_whitespace(text);
            if flat.is_empty() {
                "Reply finished with no text.".to_string()
            } else {
                clip(&flat, 200)
            }
        }
        TurnOutcome::Failed(error) => {
            let flat = collapse_whitespace(error);
            if flat.is_empty() {
                "Reply failed.".to_string()
            } else {
                format!("Reply failed: {}", clip(&flat, 180))
            }
        }
    }
}

/// Collapse every run of whitespace (including newlines) to a single space, so
/// a multi-paragraph reply fits in one notification line.
fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` characters, appending an ellipsis when anything was
/// dropped (char-aware, so a multi-byte reply is never split mid-codepoint).
fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_flattens_newlines_and_runs() {
        assert_eq!(collapse_whitespace("a\n\n b\t c  "), "a b c");
        assert_eq!(collapse_whitespace("   "), "");
    }

    #[test]
    fn reply_body_is_truncated_with_ellipsis() {
        let long = "x".repeat(250);
        let body = notification_body(TurnOutcome::Reply(&long));
        assert_eq!(body.chars().count(), 201); // 200 + ellipsis
        assert!(body.ends_with('…'));
        // Multi-byte text must not be split mid-codepoint.
        let wide = "é".repeat(250);
        let body = notification_body(TurnOutcome::Reply(&wide));
        assert_eq!(body.chars().count(), 201);
    }

    #[test]
    fn short_reply_is_unchanged() {
        assert_eq!(
            notification_body(TurnOutcome::Reply("  all done\nthanks  ")),
            "all done thanks"
        );
    }

    #[test]
    fn empty_reply_and_error_get_clear_fallbacks() {
        assert_eq!(
            notification_body(TurnOutcome::Reply("   ")),
            "Reply finished with no text."
        );
        assert_eq!(notification_body(TurnOutcome::Failed("")), "Reply failed.");
        assert_eq!(
            notification_body(TurnOutcome::Failed("stream closed\nunexpectedly")),
            "Reply failed: stream closed unexpectedly"
        );
    }

    #[test]
    fn blank_title_falls_back_to_app_name() {
        assert_eq!(notification_title("  "), "Pi Chat");
        assert_eq!(notification_title(" Audio gear "), "Audio gear");
    }
}
