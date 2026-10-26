mod pg;
mod sql;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn app_version() -> &'static str {
    dolmen_core::VERSION
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(pg::Sessions::default())
        .invoke_handler(tauri::generate_handler![
            app_version,
            pg::pg_connect,
            pg::pg_execute,
            pg::pg_schema_outline,
            pg::pg_cancel,
            pg::pg_disconnect,
            sql::sql_statement_at,
            sql::sql_split,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
