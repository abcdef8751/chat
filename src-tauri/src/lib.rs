mod attachments;
#[cfg(target_os = "android")]
mod android;
mod chat;
mod config;
mod cron;
mod crypt;
mod db;
mod import;
mod memory;
mod models;
mod pricing;
mod providers;
mod reflection;
mod secrets;
mod shell;
mod sync;
mod tools;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init());
    // Android-only native bridges: the host-shell bridge (Termux RUN_COMMAND),
    // the content-URI file bridge (ContentResolver), and the app-owned file
    // picker (whose launcher survives Activity recreation; see PickerPlugin.kt).
    #[cfg(target_os = "android")]
    let builder = builder
        .plugin(crate::android::plugin())
        .plugin(crate::android::file_plugin())
        .plugin(crate::android::picker_plugin());
    // Linux: "close to tray". Clicking the X hides the window instead of quitting,
    // so the process (and its background sync/reflection schedulers) keep running.
    // The tray menu is the only reliable reopen path on Linux (tray click events
    // often don't fire), so it doubles as the "hidden app is still alive" affordance.
    #[cfg(target_os = "linux")]
    let builder = builder.on_window_event(|window, event| {
        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
            let _ = window.hide();
            api.prevent_close();
        }
    });
    builder
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
            std::fs::create_dir_all(&dir)
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
            // Android secrets live in app-private files (the `keyring` crate has
            // no Android backend); seed the store with the app data dir. No-op
            // on every other platform, where the OS keychain is used instead.
            crate::secrets::init_android(dir.clone());
            let conn = db::open(&dir.join("chat.sqlite"))
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(std::io::Error::other(e)) })?;
            let conn = std::sync::Arc::new(std::sync::Mutex::new(conn));
            let db_handle = db::Db(conn.clone());
            app.manage(db_handle);

            let config = config::ConfigState::load(dir.join("config.json"))
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(std::io::Error::other(e)) })?;
            app.manage(config);
            app.manage(chat::StreamRegistry::default());
            app.manage(models::ModelCache::default());
            app.manage(tools::BraveSearch::default());
            app.manage(tools::ApprovalRegistry::default());
            // One-shot host shell; on Android it carries the app handle to reach
            // the Termux bridge (see shell.rs / android.rs).
            #[cfg(target_os = "android")]
            let shell_state = shell::ShellExecutor::new_mobile(app.handle().clone());
            #[cfg(not(target_os = "android"))]
            let shell_state = shell::ShellExecutor::new();
            app.manage(shell_state);
            app.manage(reflection::Backfill::default());
            app.manage(import::ImportState::default());
            app.manage(cron::CronState::default());

            let mut memory = memory::MemoryState::load(dir.join("memory")).map_err(
                |e| -> Box<dyn std::error::Error> { Box::new(std::io::Error::other(e)) },
            )?;
            // Attach the shared DB connection so memory mutations mark rows
            // dirty for the sync push (shares the same Arc'd mutex as `Db`).
            memory.set_db(db::Db(conn));
            app.manage(sync::SyncState::new());
            app.manage(memory);

            // Background idle memory reflection.
            reflection::spawn(app.handle().clone());

            // Background scheduled runs (cron jobs).
            cron::spawn(app.handle().clone());

            // Background sync scheduler (only active when enabled + logged in).
            sync::spawn(app.handle().clone());

            // Linux: system-tray icon with Open/Quit. Required for close-to-tray:
            // once the window is hidden there's no other way back in. The menu
            // (not the icon click) is the supported interaction on Linux.
            #[cfg(target_os = "linux")]
            if let Some(icon) = app.default_window_icon().cloned() {
                use tauri::menu::{Menu, MenuItem};
                use tauri::tray::TrayIconBuilder;
                let open = MenuItem::with_id(app, "open", "Open Pi Chat", true, None::<&str>)?;
                let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
                let menu = Menu::with_items(app, &[&open, &quit])?;
                TrayIconBuilder::with_id("main-tray")
                    .icon(icon)
                    .menu(&menu)
                    .on_menu_event(|app, event| match event.id().as_ref() {
                        "open" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    })
                    .build(app)?;
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            db::list_conversations,
            db::create_conversation,
            db::list_messages,
            db::add_message,
            db::rename_conversation,
            db::set_conversation_provider,
            db::set_conversation_model,
            db::delete_conversation,
            db::search_conversations,
            config::get_config,
            config::set_config,
            config::set_model_overrides,
            providers::list_providers,
            providers::add_provider,
            providers::update_provider,
            providers::remove_provider,
            providers::set_active_provider,
            providers::list_models_dev_providers,
            secrets::has_api_key,
            secrets::set_api_key,
            secrets::has_brave_key_cmd,
            secrets::set_brave_key,
            models::list_models,
            chat::stream_chat,
            chat::stop_chat,
            tools::approve_tool,
            tools::deny_tool,
            memory::list_memory_files,
            memory::write_memory_file,
            memory::delete_memory_file,
            reflection::reflect_now,
            reflection::backfill_memories,
            reflection::backfill_status,
            reflection::cancel_backfill,
            reflection::memory_extraction_stats,
            reflection::clear_extractions,
            cron::list_crons,
            cron::create_cron,
            cron::update_cron,
            cron::delete_cron,
            cron::run_cron_now,
            cron::list_cron_runs,
            cron::list_cron_triggers,
            cron::cron_preview,
            cron::cron_set_active,
            cron::test_notification,
            db::memory_reflection_stats,
            pricing::get_pricing,
            pricing::refresh_pricing,
            pricing::thinking_options,
            attachments::read_attachments,
            read_attachment_from_uri,
            pick_attachment_uris,
            db::edit_user_message,
            import::import_conversations,
            sync::sync_sign_in,
            sync::sync_sign_out,
            sync::sync_sign_up,
            sync::sync_status,
            sync::sync_toggle,
            sync::sync_now,
            sync::sync_set_encryption,
            sync::sync_remove_encryption,
            sync::sync_import_key,
            sync::sync_recovery_code,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        // `_app` is only read on Android (window recreate on resume); the
        // underscore keeps the desktop build warning-free.
        .run(|_app, event| match event {
            #[cfg(target_os = "android")]
            tauri::RunEvent::ExitRequested { api, .. } => {
                // tao maps each Android activity to a window; when the user swipes
                // the app away the last window is destroyed and Tauri's default
                // exit-on-last-window-close calls std::process::exit(0), killing
                // the process (and the Rust sync/reflection schedulers) along with
                // it. Block that so the foreground service keeps them alive.
                api.prevent_exit();
            }
            // Android blank-screen-on-relaunch fix (tauri#15671). This event only
            // reaches us because `tauri-runtime-wry` is vendored and patched with
            // upstream PR #15678 (see [patch.crates-io] in Cargo.toml): upstream
            // 2.11.4 emits `RunEvent::Resumed` only on `StartCause::Poll`, an arm
            // its `ControlFlow::Wait` loop never hits, so on Android the resume
            // was silently dropped. It matters here because swipe-away does NOT
            // exit: `ExitRequested` calls `prevent_exit()` and `KeepAliveService`
            // keeps the process (and its sync/reflection schedulers) alive. The
            // relaunched activity therefore resumes with no window at all → a
            // blank screen until the process is killed. The guard keeps this a
            // no-op when a window already exists; recreate the main window
            // otherwise. The label matches `tauri.conf.json` so Tauri's window
            // registry stays consistent.
            #[cfg(target_os = "android")]
            tauri::RunEvent::Resumed if _app.webview_windows().is_empty() => {
                match tauri::WebviewWindowBuilder::new(
                    _app,
                    "main",
                    tauri::WebviewUrl::default(),
                )
                .title("Pi Chat")
                .build()
                {
                    Ok(_) => eprintln!("android: recreated main window on resume"),
                    Err(e) => {
                        eprintln!("android: failed to recreate main window on resume: {e}")
                    }
                }
            }
            #[cfg(target_os = "linux")]
            tauri::RunEvent::ExitRequested { code: None, api, .. } => {
                // Linux close-to-tray: a window closing (code None) must not quit
                // the app; it stays hidden in the background. The tray's explicit
                // "Quit" calls app.exit(0) → code Some(0), which is not matched
                // here and so is allowed through.
                api.prevent_exit();
            }
            _ => {}
        });
}

/// Read an attachment from an Android `content://` URI — what
/// `tauri-plugin-dialog` hands back there, and what `std::fs` cannot open (so
/// the path-based `attachments::read_attachments` silently dropped every picked
/// file). The Kotlin `ContentResolver` bridge reads the bytes/mime/name and
/// this assembles the same `Attachment` shape. Android only; the stub keeps
/// the command available (and the invoke handler stable) on other platforms.
#[cfg(target_os = "android")]
#[tauri::command]
async fn read_attachment_from_uri(
    app: tauri::AppHandle,
    uri: String,
) -> Result<attachments::Attachment, String> {
    crate::android::read_attachment_from_uri(&app, uri).await
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn read_attachment_from_uri(uri: String) -> Result<attachments::Attachment, String> {
    let _ = uri;
    Err("read_attachment_from_uri is only available on Android".into())
}

/// Open the Android document picker and return the selected `content://` URIs
/// (empty when the user cancelled). Android only — desktop keeps using
/// `tauri-plugin-dialog` via the `open()` call in the frontend; the stub keeps
/// the command registered (and the invoke handler stable) on other platforms.
#[cfg(target_os = "android")]
#[tauri::command]
async fn pick_attachment_uris(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    crate::android::pick_attachment_uris(&app).await
}

#[cfg(not(target_os = "android"))]
#[tauri::command]
async fn pick_attachment_uris() -> Result<Vec<String>, String> {
    Err("pick_attachment_uris is only available on Android".into())
}
