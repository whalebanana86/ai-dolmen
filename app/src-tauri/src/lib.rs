mod ai;
mod connections;
mod pg;
mod snapshot;
mod sql;

use dolmen_core::ai::ApiKeys;
use dolmen_core::connections::{ConnectionStore, native_credential_store};
use tauri::Manager;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn app_version() -> &'static str {
    dolmen_core::VERSION
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let path = app.path().app_config_dir()?.join("connections.json");
            // Without a credential store the app still works; passwords and the
            // API key just can't be saved.
            let credentials = native_credential_store()
                .inspect_err(|err| eprintln!("passwords can't be saved: {err}"))
                .ok();
            app.manage(ConnectionStore::new(path, credentials.clone()));
            app.manage(ai::Ai::new(ApiKeys::new(credentials)));
            Ok(())
        })
        .manage(pg::Sessions::default())
        .invoke_handler(tauri::generate_handler![
            app_version,
            connections::connections_list,
            connections::connections_save,
            connections::connections_delete,
            pg::pg_connect,
            pg::pg_execute,
            pg::pg_introspect,
            pg::pg_cancel,
            pg::pg_disconnect,
            pg::object_plan,
            pg::object_apply,
            snapshot::snapshot_checkout,
            snapshot::snapshot_ssh_alternatives,
            snapshot::snapshot_create_branch,
            snapshot::snapshot_switch_branch,
            snapshot::snapshot_status,
            snapshot::snapshot_save,
            snapshot::snapshot_push,
            snapshot::snapshot_history,
            snapshot::snapshot_compare,
            snapshot::snapshot_plan,
            snapshot::snapshot_apply,
            snapshot::ddl_create_statement,
            sql::sql_statement_at,
            sql::sql_split,
            ai::ai_key_status,
            ai::ai_key_set,
            ai::ai_key_forget,
            ai::ai_ask,
            ai::ai_cancel,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
