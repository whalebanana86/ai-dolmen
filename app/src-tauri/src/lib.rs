use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dolmen_core::Error;
use dolmen_core::postgres::{ConnectConfig, Session, StatementResult};
use serde::Serialize;
use tauri::State;

/// Rows sent to the UI per result set; the rest are counted but not transferred.
const MAX_ROWS: usize = 1000;

/// Open database sessions, keyed by an id handed to the UI.
#[derive(Default)]
struct Sessions {
    next_id: AtomicU64,
    open: Mutex<HashMap<u64, Arc<Session>>>,
}

impl Sessions {
    fn get(&self, id: u64) -> Result<Arc<Session>, Error> {
        self.open
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(Error::Closed)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionInfo {
    id: u64,
    server_version: String,
}

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn app_version() -> &'static str {
    dolmen_core::VERSION
}

#[tauri::command]
async fn pg_connect(
    config: ConnectConfig,
    sessions: State<'_, Sessions>,
) -> Result<SessionInfo, Error> {
    let session = Session::connect(&config).await?;
    let id = sessions.next_id.fetch_add(1, Ordering::Relaxed);
    let info = SessionInfo {
        id,
        server_version: session.server_version().to_owned(),
    };
    sessions.open.lock().unwrap().insert(id, Arc::new(session));
    Ok(info)
}

#[tauri::command]
async fn pg_execute(
    session_id: u64,
    sql: String,
    sessions: State<'_, Sessions>,
) -> Result<Vec<StatementResult>, Error> {
    sessions.get(session_id)?.execute(&sql, MAX_ROWS).await
}

#[tauri::command]
async fn pg_cancel(session_id: u64, sessions: State<'_, Sessions>) -> Result<(), Error> {
    sessions.get(session_id)?.cancel().await
}

#[tauri::command]
fn pg_disconnect(session_id: u64, sessions: State<'_, Sessions>) {
    sessions.open.lock().unwrap().remove(&session_id);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(Sessions::default())
        .invoke_handler(tauri::generate_handler![
            app_version,
            pg_connect,
            pg_execute,
            pg_cancel,
            pg_disconnect
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
