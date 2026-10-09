mod attachments;
#[cfg(target_os = "android")]
mod android;
mod chat;
mod config;
mod crypt;
mod db;
mod import;
mod memory;
mod models;
mod pricing;
mod reflection;
mod secrets;
mod shell;
mod sync;
mod tools;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());
    // Android-only host-shell bridge (Termux RUN_COMMAND).
    #[cfg(target_os = "android")]
    let builder = builder.plugin(crate::android::plugin());
    builder
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
            std::fs::create_dir_all(&dir)
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
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

            // Background sync scheduler (only active when enabled + logged in).
            sync::spawn(app.handle().clone());

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            db::list_conversations,
            db::create_conversation,
            db::list_messages,
            db::add_message,
            db::rename_conversation,
            db::delete_conversation,
            db::search_conversations,
            config::get_config,
            config::set_config,
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
            db::memory_reflection_stats,
            pricing::get_pricing,
            pricing::refresh_pricing,
            pricing::thinking_options,
            attachments::read_attachments,
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
