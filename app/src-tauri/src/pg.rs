//! PostgreSQL session commands.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dolmen_core::connections::ConnectionStore;
use dolmen_core::history::Source;
use dolmen_core::postgres::edit::{self, Edit};
use dolmen_core::postgres::migrate::Plan;
use dolmen_core::postgres::{ConnectConfig, Session, StatementResult};
use dolmen_core::schema::{Database, ForeignKey};
use dolmen_core::{Error, Refusal};
use serde::Serialize;
use tauri::State;

use crate::history::{self, HistoryStore, Started};

/// Rows sent to the UI per result set; the rest are counted but not transferred.
/// The result grid only renders the rows in view, so this is bound by memory, not DOM size.
const MAX_ROWS: usize = 10_000;

/// Open database sessions, keyed by an id handed to the UI.
#[derive(Default)]
pub struct Sessions {
    next_id: AtomicU64,
    open: Mutex<HashMap<u64, Open>>,
}

struct Open {
    session: Arc<Session>,
    connection: history::Connection,
}

impl Sessions {
    pub(crate) fn get(&self, id: u64) -> Result<Arc<Session>, Error> {
        self.open
            .lock()
            .unwrap()
            .get(&id)
            .map(|open| Arc::clone(&open.session))
            .ok_or(Error::Closed)
    }

    /// The session's connection, as the query history records it.
    pub(crate) fn connection(&self, id: u64) -> Option<history::Connection> {
        self.open
            .lock()
            .unwrap()
            .get(&id)
            .map(|open| open.connection.clone())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    id: u64,
    server_version: String,
    database: String,
    /// The saved connection it came from, if any.
    saved_id: Option<String>,
    /// The saved connection's name, or user@host:port/database.
    label: String,
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
    let saved = match &saved_id {
        Some(id) => connections.list()?.into_iter().find(|c| &c.id == id),
        None => None,
    };
    let session = Session::connect(&config).await?;
    let id = sessions.next_id.fetch_add(1, Ordering::Relaxed);
    let connection = history::Connection {
        saved_id: saved.as_ref().map(|c| c.id.clone()),
        label: saved.as_ref().map_or_else(
            || {
                format!(
                    "{}@{}:{}/{}",
                    config.user, config.host, config.port, config.database
                )
            },
            |c| c.name.clone(),
        ),
        record: saved.as_ref().is_none_or(|c| c.record_history),
    };
    let info = SessionInfo {
        id,
        server_version: session.server_version().to_owned(),
        database: config.database,
        saved_id: connection.saved_id.clone(),
        label: connection.label.clone(),
    };
    sessions.open.lock().unwrap().insert(
        id,
        Open {
            session: Arc::new(session),
            connection,
        },
    );
    Ok(info)
}

#[tauri::command]
pub async fn pg_execute(
    session_id: u64,
    sql: String,
    sessions: State<'_, Sessions>,
    history: State<'_, HistoryStore>,
) -> Result<Vec<StatementResult>, Error> {
    let session = sessions.get(session_id)?;
    let started = Started::now();
    let result = session.execute(&sql, MAX_ROWS).await;
    if let Some(connection) = sessions.connection(session_id) {
        history.record(
            &connection,
            &sql,
            started,
            &result,
            |results| history::rows(results),
            Source::Editor,
        );
    }
    result
}

/// The database structure, for the schema tree and completion.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Introspection {
    /// The schema unqualified names resolve to first, usually `public`.
    default_schema: Option<String>,
    database: Database,
    /// The relationships between tables, for the detail view and the ERD.
    foreign_keys: Vec<ForeignKey>,
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
        foreign_keys: session.foreign_keys().await?,
    })
}

/// The statements that make a table, view or function match an edit from the
/// detail view, for the user to review.
#[tauri::command]
pub async fn object_plan(
    session_id: u64,
    edit: Edit,
    sessions: State<'_, Sessions>,
) -> Result<Plan, Error> {
    let session = sessions.get(session_id)?;
    edit::plan(&session.introspect().await?, &edit)
}

/// Applies a reviewed edit in one transaction. The plan is made again here and
/// must match the statements reviewed, so nothing changes behind the user's back.
#[tauri::command]
pub async fn object_apply(
    session_id: u64,
    edit: Edit,
    reviewed: Vec<String>,
    allow_destructive: bool,
    sessions: State<'_, Sessions>,
    history: State<'_, HistoryStore>,
) -> Result<(), Error> {
    let session = sessions.get(session_id)?;
    let plan = edit::plan(&session.introspect().await?, &edit)?;
    let refused = |reason, message: &str| Error::Refused {
        reason,
        message: message.into(),
    };
    if !plan.steps.iter().map(|step| &step.sql).eq(reviewed.iter()) {
        return Err(refused(
            Refusal::PlanChanged,
            "The database changed since you reviewed the statements; review them again.",
        ));
    }
    if !plan.blockers.is_empty() {
        return Err(refused(
            Refusal::PlanBlocked,
            "The change has steps Dolmen can't make; see the statements.",
        ));
    }
    if plan.is_destructive() && !allow_destructive {
        return Err(refused(
            Refusal::DestructiveNotAllowed,
            "The change drops data or changes column types; allow that explicitly to apply it.",
        ));
    }
    let started = Started::now();
    let result = session.apply(&plan).await;
    record_plan(
        &sessions,
        &history,
        session_id,
        &plan,
        started,
        &result,
        Source::Detail,
    );
    result
}

/// Records an applied plan as one run: its statements, in order.
pub(crate) fn record_plan(
    sessions: &Sessions,
    history: &HistoryStore,
    session_id: u64,
    plan: &Plan,
    started: Started,
    result: &Result<(), Error>,
    source: Source,
) {
    if let Some(connection) = sessions.connection(session_id) {
        let sql: Vec<&str> = plan.steps.iter().map(|step| step.sql.as_str()).collect();
        history.record(
            &connection,
            &sql.join("\n"),
            started,
            result,
            |_| None,
            source,
        );
    }
}

#[tauri::command]
pub async fn pg_cancel(session_id: u64, sessions: State<'_, Sessions>) -> Result<(), Error> {
    sessions.get(session_id)?.cancel().await
}

#[tauri::command]
pub fn pg_disconnect(session_id: u64, sessions: State<'_, Sessions>) {
    sessions.open.lock().unwrap().remove(&session_id);
}
