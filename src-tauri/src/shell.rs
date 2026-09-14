//! Persistent per-conversation shell sessions.
//!
//! Host `bash` tool calls run against a long-lived `bash` process per
//! conversation so state (`cd`, `export`, shell functions, …) persists across
//! calls. Completion is detected with a random sentinel printed after each
//! command; if a command outruns the timeout or kills the shell, the session is
//! dropped so the next call starts a fresh one.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;

use crate::tools::{truncate, ToolOutput};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

struct ShellSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    timeout: Duration,
}

impl ShellSession {
    fn spawn(timeout: Duration) -> Result<Self, String> {
        let mut child = Command::new("bash")
            .arg("--noprofile")
            .arg("--norc")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn bash: {e}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "bash: no stdin".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "bash: no stdout".to_string())?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            timeout,
        })
    }

    /// Run one command, returning its output and whether the session must be
    /// reset (timeout / shell exit).
    async fn run(&mut self, command: &str) -> (ToolOutput, bool) {
        let sentinel = format!("__PI_DONE_{}__", uuid::Uuid::new_v4().simple());
        // Redirect the command's stdin from /dev/null so it can't swallow the
        // sentinel line — unless it uses a heredoc, whose body is read from the
        // script stream itself (and which a `</dev/null` would override).
        let redirect = if command.contains("<<") { "" } else { " </dev/null" };
        let script = format!(
            "exec 2>&1\n{command}{redirect}\nprintf '\\n{sentinel}%s\\n' \"$?\"\n"
        );

        if let Err(e) = self.stdin.write_all(script.as_bytes()).await {
            return (
                ToolOutput {
                    content: format!("bash: {e}"),
                    is_error: true,
                },
                true,
            );
        }
        if let Err(e) = self.stdin.flush().await {
            return (
                ToolOutput {
                    content: format!("bash: {e}"),
                    is_error: true,
                },
                true,
            );
        }

        let res = tokio::time::timeout(
            self.timeout,
            Self::read_until(&mut self.stdout, &sentinel),
        )
        .await;

        match res {
            Ok(Ok((output, code))) => {
                let mut text = truncate(output.trim_end().to_string());
                if code != 0 {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("[exit: {code}]"));
                }
                (
                    ToolOutput {
                        content: text,
                        is_error: code != 0,
                    },
                    false,
                )
            }
            Ok(Err(e)) => (
                ToolOutput {
                    content: format!("bash: {e}"),
                    is_error: true,
                },
                true,
            ),
            Err(_) => (
                ToolOutput {
                    content: format!("bash: timed out after {}s", self.timeout.as_secs()),
                    is_error: true,
                },
                true,
            ),
        }
    }

    /// Read lines until the sentinel is seen, returning the captured output and
    /// the command's exit code.
    async fn read_until(
        reader: &mut BufReader<ChildStdout>,
        sentinel: &str,
    ) -> Result<(String, i32), String> {
        let mut output = String::new();
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("shell exited".to_string());
            }
            if let Some(rest) = line.strip_prefix(sentinel) {
                let code = rest.trim().parse::<i32>().unwrap_or(-1);
                return Ok((output, code));
            }
            // Cap what we keep in memory; keep draining to stay in sync.
            if output.len() < crate::tools::MAX_OUTPUT_CHARS * 2 {
                output.push_str(&line);
            }
        }
    }
}

/// One live shell per conversation id.
pub struct ShellRegistry {
    sessions: Mutex<HashMap<String, Arc<AsyncMutex<Option<ShellSession>>>>>,
    timeout: Duration,
}

impl Default for ShellRegistry {
    fn default() -> Self {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }
}

impl ShellRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            timeout,
        }
    }

    fn session_for(&self, conversation_id: &str) -> Arc<AsyncMutex<Option<ShellSession>>> {
        let mut map = self.sessions.lock().unwrap();
        map.entry(conversation_id.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(None)))
            .clone()
    }

    /// Run `command` in the conversation's shell, spawning it on first use.
    pub async fn run(&self, conversation_id: &str, command: &str) -> ToolOutput {
        let slot = self.session_for(conversation_id);
        let mut guard = slot.lock().await;
        if guard.is_none() {
            match ShellSession::spawn(self.timeout) {
                Ok(session) => *guard = Some(session),
                Err(e) => {
                    return ToolOutput {
                        content: format!("bash: {e}"),
                        is_error: true,
                    }
                }
            }
        }
        let (output, reset) = guard.as_mut().unwrap().run(command).await;
        if reset {
            if let Some(mut session) = guard.take() {
                let _ = session.child.kill().await;
            }
        }
        output
    }

    /// Drop a conversation's shell (used when the conversation is deleted).
    pub async fn clear(&self, conversation_id: &str) {
        let slot = self.sessions.lock().unwrap().remove(conversation_id);
        if let Some(slot) = slot {
            if let Some(mut session) = slot.lock().await.take() {
                let _ = session.child.kill().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cwd_persists_across_calls() {
        let reg = ShellRegistry::new();
        let out = reg.run("c", "cd /tmp").await;
        assert!(!out.is_error, "{}", out.content);
        let out = reg.run("c", "pwd").await;
        assert_eq!(out.content.trim(), "/tmp");
    }

    #[tokio::test]
    async fn env_persists_across_calls() {
        let reg = ShellRegistry::new();
        let out = reg.run("c", "export PI_SHELL_TEST=hello").await;
        assert!(!out.is_error, "{}", out.content);
        let out = reg.run("c", "echo $PI_SHELL_TEST").await;
        assert_eq!(out.content.trim(), "hello");
    }

    #[tokio::test]
    async fn sessions_are_isolated_per_conversation() {
        let reg = ShellRegistry::new();
        reg.run("a", "export PI_ONLY_A=1").await;
        let out = reg.run("b", "echo ${PI_ONLY_A:-unset}").await;
        assert_eq!(out.content.trim(), "unset");
    }

    #[tokio::test]
    async fn stderr_and_exit_code_are_reported() {
        let reg = ShellRegistry::new();
        let out = reg.run("c", "echo oops >&2; (exit 3)").await;
        assert!(out.is_error);
        assert!(out.content.contains("oops"), "{}", out.content);
        assert!(out.content.contains("[exit: 3]"), "{}", out.content);
    }

    #[tokio::test]
    async fn heredoc_commands_work() {
        let reg = ShellRegistry::new();
        let out = reg.run("c", "cat <<EOF\nhi there\nEOF").await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content.trim(), "hi there");
    }

    #[tokio::test]
    async fn timeout_resets_session() {
        let reg = ShellRegistry::with_timeout(Duration::from_secs(2));
        let out = reg.run("c", "sleep 30").await;
        assert!(out.is_error);
        assert!(out.content.contains("timed out"), "{}", out.content);
        let out = reg.run("c", "echo alive").await;
        assert_eq!(out.content.trim(), "alive");
    }

    #[tokio::test]
    async fn clear_drops_the_session() {
        let reg = ShellRegistry::new();
        reg.run("c", "export PI_CLEAR_TEST=1").await;
        reg.clear("c").await;
        let out = reg.run("c", "echo ${PI_CLEAR_TEST:-unset}").await;
        assert_eq!(out.content.trim(), "unset");
    }
}
