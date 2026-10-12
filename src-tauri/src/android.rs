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

use std::path::Path;

use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager};
use uuid::Uuid;

use crate::attachments::Attachment;

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

// --- Content-URI attachment bridge (`tauri-plugin-dialog` returns
// `content://` URIs on Android, which `std::fs` cannot open) ----------------

/// Result echoed back from Kotlin's `FileBridgePlugin.readUri`: the display
/// name + mime + size plus the raw bytes as standard base64.
#[derive(Deserialize)]
pub struct UriAttachment {
    pub name: String,
    pub mime: String,
    pub size: u64,
    /// Standard (RFC 4648) base64 of the file bytes, no line wrapping.
    pub data: String,
}

/// Managed state: the Android plugin handle for the content-URI file bridge.
pub struct FileBridgeHandle(pub PluginHandle<tauri::Wry>);

/// Register Kotlin's `FileBridgePlugin` (a `ContentResolver` wrapper) and store
/// its handle for [`read_attachment_from_uri`].
pub fn file_plugin() -> TauriPlugin<tauri::Wry> {
    // Distinct plugin name from `run_command`: on Android the Kotlin
    // PluginManager routes commands by plugin name, so two plugins must not
    // share one.
    Builder::new("file_bridge")
        .setup(move |app, api| {
            let handle = api.register_android_plugin("com.rp.chat", "FileBridgePlugin")?;
            app.manage(FileBridgeHandle(handle));
            Ok(())
        })
        .build()
}

/// Text files larger than this are attached by name only (mirrors
/// `attachments::MAX_TEXT_BYTES`; that constant is private to the module).
const MAX_TEXT_BYTES: u64 = 1_000_000;

/// Extension → mime, mirroring `attachments::mime_for`. Replicated because the
/// original is private and the task scope forbids editing `attachments.rs`; it
/// is only a fallback for providers that report no mime (see the caller).
fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" | "ini" | "conf" | "toml" | "yaml" | "yml" => "text/plain",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" | "cjs" => "text/javascript",
        "ts" | "tsx" => "text/typescript",
        "rs" => "text/x-rust",
        "py" => "text/x-python",
        "go" => "text/x-go",
        "java" => "text/x-java",
        "sh" | "bash" | "zsh" => "text/x-sh",
        _ => "application/octet-stream",
    }
}

/// Text-like extension check, mirroring `attachments::is_text_like`.
fn is_text_like(ext: &str) -> bool {
    matches!(
        ext,
        "txt"
            | "log"
            | "ini"
            | "conf"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "csv"
            | "md"
            | "markdown"
            | "xml"
            | "html"
            | "htm"
            | "css"
            | "js"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "jsx"
            | "rs"
            | "py"
            | "go"
            | "java"
            | "c"
            | "cc"
            | "cpp"
            | "h"
            | "hpp"
            | "sh"
            | "bash"
            | "zsh"
            | "sql"
            | "env"
            | "gitignore"
    )
}

/// Read an Android `content://` URI into the same [`Attachment`] shape the
/// path-based `attachments::read_attachment` produces: images carry a base64
/// data URL, text-like files carry their decoded contents, anything else keeps
/// only its metadata.
pub async fn read_attachment_from_uri(app: &AppHandle, uri: String) -> Result<Attachment, String> {
    let handle = app.state::<FileBridgeHandle>();
    let out: UriAttachment = handle
        .0
        .run_mobile_plugin_async("readUri", json!({ "uri": uri }))
        .await
        .map_err(|e| format!("read_attachment_from_uri: {e}"))?;

    let ext = Path::new(&out.name)
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    // Trust the resolver's mime; fall back to the extension only when it gives
    // us nothing useful (some providers return "" or octet-stream).
    let mime = if out.mime.is_empty() || out.mime == "application/octet-stream" {
        mime_for(&ext).to_string()
    } else {
        out.mime.clone()
    };
    let size = out.size;

    if mime.starts_with("image/") {
        return Ok(Attachment {
            id: Uuid::new_v4().to_string(),
            name: out.name,
            mime: mime.clone(),
            size,
            kind: "image".into(),
            // Kotlin already produced the base64; no need to decode/re-encode.
            data_url: Some(format!("data:{};base64,{}", mime, out.data)),
            text: None,
        });
    }

    if is_text_like(&ext) && size <= MAX_TEXT_BYTES {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&out.data) {
            if let Ok(text) = String::from_utf8(bytes) {
                return Ok(Attachment {
                    id: Uuid::new_v4().to_string(),
                    name: out.name,
                    mime,
                    size,
                    kind: "file".into(),
                    data_url: None,
                    text: Some(text),
                });
            }
        }
    }

    Ok(Attachment {
        id: Uuid::new_v4().to_string(),
        name: out.name,
        mime,
        size,
        kind: "file".into(),
        data_url: None,
        text: None,
    })
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
