#![cfg(target_os = "android")]
//! Android host-shell bridge.
//!
//! Executes `bash` / `read_file` / `write_file` **inside the user's installed
//! Termux** through Termux's public `RUN_COMMAND` intent. We cannot open
//! another app's data dir (per-app UID + SELinux), so every shell-side action
//! runs in Termux's own process via the intent; the working directory lives on
//! visible shared storage (`/storage/emulated/0/...`) so the agent works
//! directly on the user's real files. See `ANDROID_SHELL.md`.
//!
//! This module owns the Rust half of a small Tauri mobile plugin named
//! `run_command`; the Kotlin half (`RunCommandPlugin`) lives in
//! `src-tauri/gen/android/.../RunCommandPlugin.kt` and performs the
//! `RUN_COMMAND` transport + result `PendingIntent`/`BroadcastReceiver`.

use serde::Deserialize;
use serde_json::json;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager};

/// Initialize `rustls-platform-verifier` with Android's system trust store
/// before the first TLS handshake.
///
/// reqwest 0.13 routes every HTTPS request through `rustls-platform-verifier`.
/// On Android that verifier cannot reach the OS trust store on its own — it
/// needs a JNI env + a `Context`, and without them the *first* handshake (the
/// models.dev catalog fetch, a stream, Brave, or sync — whatever runs first on
/// a `tokio-rt-worker`) hard-aborts the whole process with
/// `Expect rustls-platform-verifier to be initialized` (SIGABRT). Desktop is
/// unaffected because the verifier reads the OS store directly there; only
/// Android needs this explicit init.
///
/// This is the JNI export for the Kotlin `MainActivity.initRustls` external
/// method. It is invoked once from `MainActivity.onCreate` (see
/// `gen/android/.../MainActivity.kt`), where the Android `Activity` (itself a
/// `Context`) is passed as the `this` receiver.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_rp_chat_MainActivity_initRustls(
    mut env: jni::EnvUnowned,
    activity: jni::objects::JObject,
) {
    // `init_with_env` wants a full `Env`; `with_env` upgrades the FFI-safe
    // unowned env and runs the closure inside its local JNI frame.
    let _ = env.with_env(|env| {
        let result = rustls_platform_verifier::android::init_with_env(env, activity);
        match &result {
            Ok(()) => eprintln!("rustls-platform-verifier initialized"),
            Err(e) => eprintln!("rustls-platform-verifier init failed: {e:?}"),
        }
        result
    });
}

use crate::tools::ToolOutput;

/// Default working directory under shared storage, per ANDROID_SHELL.md
/// (overridable via `config.shell_workspace_dir`).
pub const DEFAULT_WORKSPACE: &str = "/storage/emulated/0/PiChat";

/// Result bundle echoed back from Kotlin's `BroadcastReceiver` for one call.
#[derive(Deserialize)]
pub struct TermuxOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// Managed state: the live Android plugin handle used to reach `RunCommandPlugin`.
pub struct RunCommandHandle(pub PluginHandle<tauri::Wry>);

/// Register the Kotlin plugin and store the handle so `run_termux_command` can
/// reach it. `register_android_plugin` builds `package/SimpleClass`, so the
/// first arg is the dotted package and the second the simple Kotlin class name.
pub fn plugin() -> TauriPlugin<tauri::Wry> {
    Builder::new("run_command")
        .setup(move |app, api| {
            let handle = api.register_android_plugin("com.rp.chat", "RunCommandPlugin")?;
            app.manage(RunCommandHandle(handle));
            Ok(())
        })
        .build()
}

/// Resolve the working directory for a call: the user's configured workspace,
/// else the documented shared-storage default.
fn workspace(app: &AppHandle) -> String {
    let cfg = app.state::<crate::config::ConfigState>().get();
    let w = cfg.shell_workspace_dir.trim();
    if w.is_empty() {
        DEFAULT_WORKSPACE.to_string()
    } else {
        w.to_string()
    }
}

/// Execute `command` inside the user's Termux and return its output.
pub async fn run_termux_command(app: &AppHandle, command: &str) -> ToolOutput {
    let handle = app.state::<RunCommandHandle>();
    let payload = json!({ "command": command, "workdir": workspace(app) });
    // Match the desktop executor's per-call timeout: if Termux never delivers a
    // result (e.g. `allow-external-apps` is off, or the command hangs), the
    // chat's tool loop must not block forever.
    let result = tokio::time::timeout(
        crate::shell::DEFAULT_TIMEOUT,
        handle
            .0
            .run_mobile_plugin_async::<TermuxOutput>("run", payload),
    )
    .await;
    let result: Result<TermuxOutput, _> = match result {
        Err(_) => return ToolOutput::err("termux: timed out".into()),
        Ok(inner) => inner,
    };
    match result {
        Ok(out) => {
            let mut text = out.stdout;
            if !out.stderr.is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&out.stderr);
            }
            let mut text = crate::tools::truncate(text).trim_end().to_string();
            if out.exit_code != 0 {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&format!("[exit: {}]", out.exit_code));
            }
            ToolOutput {
                content: text,
                is_error: out.exit_code != 0,
            }
        }
        Err(e) => ToolOutput::err(format!("termux: {e}")),
    }
}
