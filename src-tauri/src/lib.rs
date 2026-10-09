mod attachments;
#[cfg(target_os = "android")]
mod android;
mod chat;
mod config;
mod db;
mod import;
mod memory;
mod models;
mod pricing;
mod reflection;
mod secrets;
mod shell;
mod tools;

use std::sync::Mutex;

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
            app.manage(db::Db(Mutex::new(conn)));

            let config = config::ConfigState::load(dir.join("config.json"))
                .map_err(|e| -> Box<dyn std::error::Error> { Box::new(std::io::Error::other(e)) })?;
            app.manage(config);
            app.manage(chat::StreamRegistry::default());
            app.manage(models::ModelCache::default());
            app.manage(tools::McpClient::new());
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

            let memory = memory::MemoryState::load(dir.join("memory")).map_err(
                |e| -> Box<dyn std::error::Error> { Box::new(std::io::Error::other(e)) },
            )?;
            app.manage(memory);

            // Background idle memory reflection.
            reflection::spawn(app.handle().clone());

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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
