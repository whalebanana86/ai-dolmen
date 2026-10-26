//! PostgreSQL session commands.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dolmen_core::Error;
use dolmen_core::connections::ConnectionStore;
use dolmen_core::postgres::{ConnectConfig, Session, StatementResult};
use dolmen_core::schema::Database;
use serde::Serialize;
use tauri::State;

/// Rows sent to the UI per result set; the rest are counted but not transferred.
/// The result grid only renders the rows in view, so this is bound by memory, not DOM size.
const MAX_ROWS: usize = 10_000;

/// Open database sessions, keyed by an id handed to the UI.
#[derive(Default)]
pub struct Sessions {
    next_id: AtomicU64,
    open: Mutex<HashMap<u64, Arc<Session>>>,
}

impl Sessions {
    pub(crate) fn get(&self, id: u64) -> Result<Arc<Session>, Error> {
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
pub struct SessionInfo {
    id: u64,
    server_version: String,
    database: String,
}

/// Connects with `config`. Without a password in `config`, the stored password of
/// the saved connection `saved_id` is used, so it never has to reach the UI.
#[tauri::command]
pub async fn pg_connect(
    mut config: ConnectConfig,
    saved_id: Option<String>,
    sessions: State<'_, Sessions>,
    connections: State<'_, ConnectionStore>,
) -> Result<SessionInfo, Error> {
    if let (None, Some(id)) = (&config.password, &saved_id) {
        config.password = connections.password(id)?;
    }
    let session = Session::connect(&config).await?;
    let id = sessions.next_id.fetch_add(1, Ordering::Relaxed);
    let info = SessionInfo {
        id,
        server_version: session.server_version().to_owned(),
        database: config.database,
    };
    sessions.open.lock().unwrap().insert(id, Arc::new(session));
    Ok(info)
}

#[tauri::command]
pub async fn pg_execute(
    session_id: u64,
    sql: String,
    sessions: State<'_, Sessions>,
) -> Result<Vec<StatementResult>, Error> {
    sessions.get(session_id)?.execute(&sql, MAX_ROWS).await
}

/// The database structure, for the schema tree and completion.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Introspection {
    /// The schema unqualified names resolve to first, usually `public`.
    default_schema: Option<String>,
    database: Database,
}

#[tauri::command]
pub async fn pg_introspect(
    session_id: u64,
    sessions: State<'_, Sessions>,
) -> Result<Introspection, Error> {
    let session = sessions.get(session_id)?;
    Ok(Introspection {
        default_schema: session.current_schema().await?,
        database: session.introspect().await?,
    })
}

#[tauri::command]
pub async fn pg_cancel(session_id: u64, sessions: State<'_, Sessions>) -> Result<(), Error> {
    sessions.get(session_id)?.cancel().await
}

#[tauri::command]
pub fn pg_disconnect(session_id: u64, sessions: State<'_, Sessions>) {
    sessions.open.lock().unwrap().remove(&session_id);
}
