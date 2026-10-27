//! Query history commands, and recording the statements other commands run.

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use dolmen_core::Error;
use dolmen_core::history::{Filter, History, NewRun, Run, Settings, Source, Status};
use dolmen_core::postgres::StatementResult;
use tauri::State;

/// The history file; when it can't be opened the app works on without history.
pub struct HistoryStore {
    history: Result<Arc<History>, String>,
}

impl HistoryStore {
    pub fn new(history: Result<History, Error>) -> Self {
        Self {
            history: history.map(Arc::new).map_err(|err| err.to_string()),
        }
    }

    fn get(&self) -> Result<&History, Error> {
        self.history.as_deref().map_err(|err| Error::Storage {
            message: format!("the query history isn't available: {err}"),
        })
    }

    /// Records a run in the background. Failing to record is logged, never
    /// reported: it mustn't fail or slow down the statement itself.
    pub fn record<T>(
        &self,
        connection: &Connection,
        sql: &str,
        started: Started,
        result: &Result<T, Error>,
        rows: impl Fn(&T) -> Option<i64>,
        source: Source,
    ) {
        let Ok(history) = &self.history else { return };
        if !connection.record {
            return;
        }
        let (status, rows, error) = match result {
            Ok(value) => (Status::Ok, rows(value), None),
            // query_canceled: the user stopped it.
            Err(Error::Database { code, message, .. }) if code == "57014" => {
                (Status::Cancelled, None, Some(message.clone()))
            }
            Err(Error::Database { code, message, .. }) => (
                Status::Error,
                None,
                Some(format!("{message} (SQLSTATE {code})")),
            ),
            Err(err) => (Status::Error, None, Some(err.to_string())),
        };
        let run = NewRun {
            connection_id: connection.saved_id.clone(),
            connection: connection.label.clone(),
            sql: sql.to_owned(),
            started_at: started.at,
            duration: started.clock.elapsed(),
            status,
            rows,
            error,
            source,
        };
        let history = Arc::clone(history);
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(err) = history.record(run) {
                eprintln!("couldn't record the query: {err}");
            }
        });
    }
}

/// What the history needs to know about a session's connection.
#[derive(Clone)]
pub struct Connection {
    pub saved_id: Option<String>,
    /// The saved connection's name, or user@host:port/database.
    pub label: String,
    pub record: bool,
}

/// When a run started, by the wall clock for the record and a monotonic clock for its duration.
#[derive(Clone, Copy)]
pub struct Started {
    at: SystemTime,
    clock: Instant,
}

impl Started {
    pub fn now() -> Self {
        Self {
            at: SystemTime::now(),
            clock: Instant::now(),
        }
    }
}

/// Rows returned or affected by the statements.
pub fn rows(results: &[StatementResult]) -> Option<i64> {
    Some(
        results
            .iter()
            .map(|result| match result {
                StatementResult::Rows { total_rows, .. } => *total_rows as i64,
                StatementResult::Command { rows_affected } => *rows_affected as i64,
            })
            .sum(),
    )
}

#[tauri::command]
pub async fn history_list(
    filter: Filter,
    store: State<'_, HistoryStore>,
) -> Result<Vec<Run>, Error> {
    store.get()?.list(&filter)
}

#[tauri::command]
pub async fn history_star(
    id: i64,
    starred: bool,
    store: State<'_, HistoryStore>,
) -> Result<(), Error> {
    store.get()?.star(id, starred)
}

#[tauri::command]
pub async fn history_delete(ids: Vec<i64>, store: State<'_, HistoryStore>) -> Result<(), Error> {
    store.get()?.delete(&ids)
}

/// Deletes the unstarred runs of one saved connection, or of all when `connection_id` is null.
#[tauri::command]
pub async fn history_clear(
    connection_id: Option<String>,
    store: State<'_, HistoryStore>,
) -> Result<(), Error> {
    store.get()?.clear(connection_id.as_deref())
}

#[tauri::command]
pub async fn history_settings(store: State<'_, HistoryStore>) -> Result<Settings, Error> {
    store.get()?.settings()
}

#[tauri::command]
pub async fn history_set_settings(
    settings: Settings,
    store: State<'_, HistoryStore>,
) -> Result<(), Error> {
    store.get()?.set_settings(&settings)
}

/// The editor text last saved for a connection, by its saved id or label.
#[tauri::command]
pub async fn editor_load(
    key: String,
    store: State<'_, HistoryStore>,
) -> Result<Option<String>, Error> {
    store.get()?.editor(&key)
}

#[tauri::command]
pub async fn editor_save(
    key: String,
    content: String,
    store: State<'_, HistoryStore>,
) -> Result<(), Error> {
    store.get()?.save_editor(&key, &content)
}
