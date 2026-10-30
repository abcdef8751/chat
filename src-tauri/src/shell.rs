//! One-shot host-shell executor.
//!
//! Every `bash` call is a **fresh** `bash -c <cmd>` process; there is no
//! persistent per-conversation session (see `ANDROID_SHELL.md`). State
//! (`cd`, `export`, shell functions, …) does not survive tool calls, which the
//! agent already assumes. Desktop executes `bash` directly; Android delegates
//! to the user's installed Termux via the `RUN_COMMAND` intent so host tools
//! share one coherent filesystem namespace (`crate::android`).

use std::path::PathBuf;
use std::time::Duration;

#[cfg(not(target_os = "android"))]
use tokio::process::Command;

use crate::tools::ToolOutput;
#[cfg(not(target_os = "android"))]
use crate::tools::truncate;

/// Default per-call timeout (matches the old persistent session's).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The user's home directory, honouring the platform's own variable: `HOME`
/// everywhere, with `USERPROFILE` as the Windows fallback. An empty value is
/// treated as unset (a blank `HOME` must not expand `~/x` to `/x`).
///
/// Unused on Android: the app process's `HOME` is not Termux's, so nothing there
/// expands a `~` (see [`expand_tilde`]).
#[cfg_attr(target_os = "android", allow(dead_code))]
fn home_dir() -> Option<String> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()))
        .map(|v| v.to_string_lossy().into_owned())
}

/// Expand a **leading** `~` to the user's home directory, with terminal
/// semantics.
///
/// Exactly `~`, or a path starting with `~/` or `~\`, expands; `~user/x` is
/// deliberately left **untouched** — resolving another user's home needs the
/// passwd database (and has no meaning on Android), so this helper never
/// guesses. Anything without a leading `~` is returned unchanged, as is `~`
/// itself when no home directory can be resolved.
///
/// This is for **path arguments** only. Command strings handed to `bash` are
/// never expanded here: bash expands a `~` itself, and doing it twice (or in
/// the middle of a quoted command) would corrupt the command.
///
/// **Android: never expand.** Every file operation there runs inside the user's
/// Termux through the `run_command` bridge, and the path is handed over
/// single-quoted (`shq`) — quoting blocks the shell's own `~` expansion, so the
/// string Termux sees must be the literal one the user wrote. The app process
/// has its own `HOME` (the sandboxed data dir), which is not Termux's and is
/// generally unreadable from there; expanding to it would silently point at the
/// wrong place (or overwrite a literal path). Leave the path alone: Termux owns
/// `$HOME`, and its own tools can expand what they are asked to.
#[cfg(target_os = "android")]
pub fn expand_tilde(path: &str) -> String {
    path.to_string()
}

/// Desktop/mobile Rust file operations read the process `HOME` directly, so a
/// leading `~` is expanded here. See the Android note on the alternative
/// definition above.
#[cfg(not(target_os = "android"))]
pub fn expand_tilde(path: &str) -> String {
    expand_tilde_with(path, home_dir().as_deref())
}

/// [`expand_tilde`] with an explicit home, so the behaviour is testable without
/// mutating the process environment (which would race other tests). Unused on
/// Android, where the public helper deliberately never expands.
#[cfg_attr(target_os = "android", allow(dead_code))]
fn expand_tilde_with(path: &str, home: Option<&str>) -> String {
    // Only a *leading* `~` is a home reference, and only when it is the whole
    // path or is immediately followed by a separator.
    let rest = if path == "~" {
        ""
    } else if let Some(rest) = path.strip_prefix("~/") {
        rest
    } else if let Some(rest) = path.strip_prefix("~\\") {
        rest
    } else {
        return path.to_string();
    };
    let Some(home) = home.filter(|h| !h.is_empty()) else {
        return path.to_string();
    };
    if rest.is_empty() {
        home.to_string()
    } else {
        PathBuf::from(home)
            .join(rest)
            .to_string_lossy()
            .into_owned()
    }
}

/// One-shot host-shell executor, platform-agnostic.
///
/// A single managed state so `tools.rs` / `chat.rs` / `reflection.rs` share one
/// instance; it holds no per-conversation state, so it can be a plain value.
#[derive(Clone)]
#[cfg_attr(mobile, allow(dead_code))] // desktop-only fields/constructors on Android
pub struct ShellExecutor {
    timeout: Duration,
    /// Working directory for each invocation (`None` = inherit the host cwd).
    workdir: Option<PathBuf>,
    /// Android only: the app handle used to reach the Termux bridge plugin.
    #[cfg(target_os = "android")]
    app: Option<tauri::AppHandle>,
}

impl Default for ShellExecutor {
    fn default() -> Self {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }
}

#[cfg_attr(mobile, allow(dead_code))] // desktop-only constructors on Android
impl ShellExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            workdir: None,
            #[cfg(target_os = "android")]
            app: None,
        }
    }

    /// Android-only constructor carrying the app handle for the Termux bridge.
    #[cfg(target_os = "android")]
    pub fn new_mobile(app: tauri::AppHandle) -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            workdir: None,
            app: Some(app),
        }
    }

    /// Executor that runs every call in `workdir` (Android: the shared-storage
    /// workspace; desktop: useful for tests).
    #[cfg(test)]
    pub fn with_workdir(timeout: Duration, workdir: PathBuf) -> Self {
        Self {
            timeout,
            workdir: Some(workdir),
            #[cfg(target_os = "android")]
            app: None,
        }
    }

    /// Run one `bash` command, returning captured output and the exit code.
    pub async fn run(&self, command: &str) -> ToolOutput {
        #[cfg(target_os = "android")]
        {
            let Some(app) = &self.app else {
                return ToolOutput::err("Termux bridge not initialized".into());
            };
            crate::android::run_termux_command(app, command).await
        }
        #[cfg(not(target_os = "android"))]
        {
            self.run_desktop(command).await
        }
    }

    /// Read a file as text. Desktop reads the host filesystem directly;
    /// Android routes through Termux so the same namespace as `bash` is used.
    /// A leading `~` is expanded here as well as in `tools.rs`, so every caller
    /// (including the Android `shq`-quoted branch, where single quotes would
    /// otherwise suppress the shell's own expansion) agrees on the real path.
    pub async fn read_file(&self, path: &str) -> ToolOutput {
        let path = expand_tilde(path);
        let path = path.as_str();
        #[cfg(not(target_os = "android"))]
        {
            match tokio::fs::read_to_string(path).await {
                Ok(text) => ToolOutput::ok(truncate(text)),
                Err(e) => ToolOutput::err(format!("read_file: {e}")),
            }
        }
        #[cfg(target_os = "android")]
        {
            self.run(&format!("cat {}", shq(path))).await
        }
    }

    /// Write text to a file. Desktop writes the host filesystem directly;
    /// Android routes through Termux. A leading `~` is expanded first, for the
    /// same reason as [`ShellExecutor::read_file`].
    pub async fn write_file(&self, path: &str, content: &str) -> ToolOutput {
        let path = expand_tilde(path);
        let path = path.as_str();
        #[cfg(not(target_os = "android"))]
        {
            match tokio::fs::write(path, content).await {
                Ok(()) => ToolOutput::ok(format!("wrote {} bytes to {path}", content.len())),
                Err(e) => ToolOutput::err(format!("write_file: {e}")),
            }
        }
        #[cfg(target_os = "android")]
        {
            // Heredoc into the target path so arbitrary content round-trips.
            let script = format!("cat > {} <<'PI_FILE_EOF'\n{}\nPI_FILE_EOF", shq(path), content);
            self.run(&script).await
        }
    }

    #[cfg(not(target_os = "android"))]
    async fn run_desktop(&self, command: &str) -> ToolOutput {
        use std::process::Stdio;
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(command)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(wd) = &self.workdir {
            cmd.current_dir(wd);
        }
        let child = match cmd.kill_on_drop(true).spawn() {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("bash: {e}")),
        };
        // `kill_on_drop` ensures a timed-out future also kills the child.
        let res = tokio::time::timeout(self.timeout, async {
            let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
            Ok::<_, String>(out)
        })
        .await;

        match res {
            Ok(Ok(out)) => {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                let err = String::from_utf8_lossy(&out.stderr).into_owned();
                if !err.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&err);
                }
                let code = out.status.code().unwrap_or(-1);
                let mut text = truncate(text).trim_end().to_string();
                if code != 0 {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&format!("[exit: {code}]"));
                }
                ToolOutput {
                    content: text,
                    is_error: code != 0,
                }
            }
            Ok(Err(e)) => ToolOutput::err(format!("bash: {e}")),
            Err(_) => ToolOutput::err(format!(
                "bash: timed out after {}s",
                self.timeout.as_secs()
            )),
        }
    }
}

/// Single-quote a shell argument (paths) so it survives as one token.
#[cfg(target_os = "android")]
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runs_and_reports_output() {
        let ex = ShellExecutor::new();
        let out = ex.run("echo hello").await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content.trim(), "hello");
    }

    #[tokio::test]
    async fn fresh_env_each_call() {
        let ex = ShellExecutor::new();
        ex.run("export PI_ONE_SHOT=1").await;
        // A fresh process per call: the variable is gone next time.
        let out = ex.run("echo ${PI_ONE_SHOT:-unset}").await;
        assert_eq!(out.content.trim(), "unset");
    }

    #[tokio::test]
    async fn stderr_and_exit_code_are_reported() {
        let ex = ShellExecutor::new();
        let out = ex.run("echo oops >&2; (exit 3)").await;
        assert!(out.is_error);
        assert!(out.content.contains("oops"), "{}", out.content);
        assert!(out.content.contains("[exit: 3]"), "{}", out.content);
    }

    #[tokio::test]
    async fn heredoc_commands_work() {
        let ex = ShellExecutor::new();
        let out = ex.run("cat <<EOF\nhi there\nEOF").await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content.trim(), "hi there");
    }

    #[tokio::test]
    async fn timeout_kills_and_reports() {
        let ex = ShellExecutor::with_timeout(Duration::from_secs(2));
        let out = ex.run("sleep 30").await;
        assert!(out.is_error);
        assert!(out.content.contains("timed out"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_bash_is_a_tool_error() {
        // Point at a path that isn't a shell by abusing `HOME`? bash is found on
        // PATH, so instead confirm a command-not-found is surfaced as an error.
        let ex = ShellExecutor::new();
        let out = ex.run("definitely_not_a_real_command_xyz").await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn workdir_is_respected() {
        let dir = std::env::temp_dir().join(format!("pi-shell-wd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ex = ShellExecutor::with_workdir(Duration::from_secs(10), dir.clone());
        let out = ex.run("pwd").await;
        assert_eq!(out.content.trim(), dir.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- tilde expansion ---------------------------------------------------

    #[test]
    fn expand_tilde_handles_home_prefixes() {
        // `~/x` and a bare `~` resolve against the given home.
        assert_eq!(expand_tilde_with("~/notes.txt", Some("/home/rp")), "/home/rp/notes.txt");
        assert_eq!(expand_tilde_with("~", Some("/home/rp")), "/home/rp");
        // Nested and trailing-slash forms.
        assert_eq!(expand_tilde_with("~/a/b/c", Some("/home/rp")), "/home/rp/a/b/c");
        assert_eq!(expand_tilde_with("~/a/", Some("/home/rp")), "/home/rp/a/");
        // Windows separator form is accepted too.
        assert_eq!(expand_tilde_with("~\\notes.txt", Some("/home/rp")), "/home/rp/notes.txt");
    }

    #[test]
    fn expand_tilde_leaves_absolute_relative_and_user_paths_alone() {
        for path in [
            "/absolute/path",
            "relative/path",
            "./here",
            "../up",
            "",
            // `~user` needs the passwd database: deliberately not expanded.
            "~user/x",
            "~user",
            // A `~` that is not the first character is literal.
            "/tmp/~",
            "/tmp/~/x",
            "a~b",
        ] {
            assert_eq!(expand_tilde_with(path, Some("/home/rp")), path, "{path}");
        }
    }

    #[test]
    fn expand_tilde_without_home_returns_the_input() {
        // Missing HOME (and USERPROFILE) must not turn `~/x` into `/x`.
        assert_eq!(expand_tilde_with("~/x", None), "~/x");
        assert_eq!(expand_tilde_with("~", None), "~");
        // An empty home is treated as unset rather than as the filesystem root.
        assert_eq!(expand_tilde_with("~/x", Some("")), "~/x");
        assert_eq!(expand_tilde_with("~", Some("")), "~");
    }

    #[test]
    fn expand_tilde_uses_the_process_home() {
        // The public helper reads HOME (USERPROFILE as the Windows fallback).
        let Some(home) = home_dir() else {
            // No home in this environment: `~` must survive untouched.
            assert_eq!(expand_tilde("~/x"), "~/x");
            return;
        };
        assert_eq!(expand_tilde("~/x"), format!("{home}/x"));
        assert_eq!(expand_tilde("~"), home);
        assert_eq!(expand_tilde("/abs"), "/abs");
    }

    #[tokio::test]
    async fn read_file_expands_tilde() {
        // End-to-end through the desktop executor: a `~/…` path must resolve to
        // the real file (a literal "~" directory would fail to read).
        let Some(home) = home_dir() else {
            return;
        };
        let name = format!(".pi-tilde-test-{}", std::process::id());
        let full = PathBuf::from(&home).join(&name);
        std::fs::write(&full, "tilde works").unwrap();
        let ex = ShellExecutor::new();
        let out = ex.read_file(&format!("~/{name}")).await;
        let _ = std::fs::remove_file(&full);
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "tilde works");
    }
}
