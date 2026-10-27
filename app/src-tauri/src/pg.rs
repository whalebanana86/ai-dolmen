//! PostgreSQL session commands.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dolmen_core::connections::{ConnectionStore, Environment};
use dolmen_core::history::Source;
use dolmen_core::postgres::edit::{self, Edit};
use dolmen_core::postgres::migrate::Plan;
use dolmen_core::postgres::{ConnectConfig, RowEstimate, Session, StatementResult};
use dolmen_core::schema::{Database, ForeignKey};
use dolmen_core::sql::{self, Risk};
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
    /// The sessions explaining a statement for an open session, by its id, to cancel.
    explaining: Mutex<HashMap<u64, Arc<Session>>>,
}

struct Open {
    session: Arc<Session>,
    connection: history::Connection,
    /// Whether snapshot plans may change this database.
    apply_allowed: bool,
    /// The session the user connected, which the tabs' sessions were opened from.
    family: u64,
    environment: Option<Environment>,
    read_only: bool,
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

    /// Whether snapshot plans may change the session's database: as its saved
    /// connection says, else only for a server on this machine.
    pub(crate) fn apply_allowed(&self, id: u64) -> bool {
        self.open
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|open| open.apply_allowed)
    }

    fn read_only(&self, id: u64) -> bool {
        self.open
            .lock()
            .unwrap()
            .get(&id)
            .is_some_and(|open| open.read_only)
    }

    /// Refuses changes while the session is read-only.
    pub(crate) fn check_writable(&self, id: u64) -> Result<(), Error> {
        if self.read_only(id) {
            return Err(Error::Refused {
                reason: Refusal::ReadOnly,
                message: "The connection is read-only; allow writing from the toolbar first."
                    .into(),
            });
        }
        Ok(())
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
    /// user@host:port/database, to say which server something will change.
    address: String,
    environment: Option<Environment>,
    /// Whether its transactions are read-only for now.
    read_only: bool,
}

/// Connects with `config`. The saved connection `saved_id` lends its settings
/// (environment, read-only, history, snapshots) when `config` is still its
/// server, and its stored password when `config` has none, so the password
/// never has to reach the UI.
#[tauri::command]
pub async fn pg_connect(
    mut config: ConnectConfig,
    saved_id: Option<String>,
    sessions: State<'_, Sessions>,
    connections: State<'_, ConnectionStore>,
) -> Result<SessionInfo, Error> {
    let saved = match &saved_id {
        Some(id) => connections.list()?.into_iter().find(|c| &c.id == id),
        None => None,
    };
    // The stored password and settings go only with the server they were saved
    // for: a form edited without saving could otherwise send the password
    // somewhere else, or let snapshots change a database they shouldn't.
    let saved = saved.filter(|saved| {
        saved.host == config.host
            && saved.port == config.port
            && saved.user == config.user
            && saved.database == config.database
    });
    if let (None, Some(saved)) = (&config.password, &saved) {
        config.password = connections.password(&saved.id)?;
    }
    let session = Session::connect(&config).await?;
    let id = sessions.next_id.fetch_add(1, Ordering::Relaxed);
    let environment = saved.as_ref().and_then(|c| c.environment);
    let production = environment == Some(Environment::Production);
    // Production is read-only and takes no snapshot plans unless its connection
    // says otherwise.
    let read_only = saved
        .as_ref()
        .and_then(|c| c.read_only)
        .unwrap_or(production);
    if read_only {
        session.set_read_only(true).await?;
    }
    let apply_allowed = saved
        .as_ref()
        .and_then(|c| c.allow_snapshot_apply)
        .unwrap_or_else(|| config.is_local() && !production);
    let address = format!(
        "{}@{}:{}/{}",
        config.user, config.host, config.port, config.database
    );
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
        address,
        environment,
        read_only,
    };
    sessions.open.lock().unwrap().insert(
        id,
        Open {
            session: Arc::new(session),
            connection,
            apply_allowed,
            family: id,
            environment,
            read_only,
        },
    );
    Ok(info)
}

/// Opens another session to the session's database, for an editor tab of its
/// own: separate transactions, search path and cancelling. It shares the
/// session's read-only state.
#[tauri::command]
pub async fn pg_fork(session_id: u64, sessions: State<'_, Sessions>) -> Result<SessionInfo, Error> {
    let (original, connection, apply_allowed, family, environment, read_only) = {
        let open = sessions.open.lock().unwrap();
        let open = open.get(&session_id).ok_or(Error::Closed)?;
        (
            Arc::clone(&open.session),
            open.connection.clone(),
            open.apply_allowed,
            open.family,
            open.environment,
            open.read_only,
        )
    };
    let session = original.another().await?;
    if read_only {
        session.set_read_only(true).await?;
    }
    let id = sessions.next_id.fetch_add(1, Ordering::Relaxed);
    let info = SessionInfo {
        id,
        server_version: session.server_version().to_owned(),
        database: session_database(&original),
        saved_id: connection.saved_id.clone(),
        label: connection.label.clone(),
        address: session_address(&original),
        environment,
        read_only,
    };
    sessions.open.lock().unwrap().insert(
        id,
        Open {
            session: Arc::new(session),
            connection,
            apply_allowed,
            family,
            environment,
            read_only,
        },
    );
    Ok(info)
}

/// Makes the session and every session opened from the same connection (each
/// tab's) read-only, or lets them write again.
#[tauri::command]
pub async fn pg_set_read_only(
    session_id: u64,
    read_only: bool,
    sessions: State<'_, Sessions>,
) -> Result<(), Error> {
    let members: Vec<(u64, Arc<Session>)> = {
        let open = sessions.open.lock().unwrap();
        let family = open.get(&session_id).ok_or(Error::Closed)?.family;
        open.iter()
            .filter(|(_, o)| o.family == family)
            .map(|(id, o)| (*id, Arc::clone(&o.session)))
            .collect()
    };
    for (id, session) in members {
        session.set_read_only(read_only).await?;
        if let Some(open) = sessions.open.lock().unwrap().get_mut(&id) {
            open.read_only = read_only;
        }
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assessment {
    risk: Risk,
    /// About how many rows it touches, when that can be told.
    estimate: Option<RowEstimate>,
}

/// What each statement could destroy, with about how many rows, to ask before
/// running them. Nothing is run but EXPLAIN and row counts.
#[tauri::command]
pub async fn pg_assess(
    session_id: u64,
    statements: Vec<String>,
    sessions: State<'_, Sessions>,
) -> Result<Vec<Option<Assessment>>, Error> {
    let session = sessions.get(session_id)?;
    let mut assessments = Vec::with_capacity(statements.len());
    for statement in &statements {
        let assessment = match sql::risk(statement) {
            Some(risk) => Some(Assessment {
                estimate: session.estimate(statement, &risk).await,
                risk,
            }),
            None => None,
        };
        assessments.push(assessment);
    }
    Ok(assessments)
}

/// Sets the schema the session's unqualified names resolve in, and returns it.
#[tauri::command]
pub async fn pg_set_schema(
    session_id: u64,
    schema: String,
    sessions: State<'_, Sessions>,
) -> Result<Option<String>, Error> {
    sessions.get(session_id)?.set_schema(&schema).await
}

/// The schema the session's unqualified names resolve in first.
#[tauri::command]
pub async fn pg_current_schema(
    session_id: u64,
    sessions: State<'_, Sessions>,
) -> Result<Option<String>, Error> {
    sessions.get(session_id)?.current_schema().await
}

fn session_database(session: &Session) -> String {
    session.config().database.clone()
}

fn session_address(session: &Session) -> String {
    let config = session.config();
    format!(
        "{}@{}:{}/{}",
        config.user, config.host, config.port, config.database
    )
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

/// The plan for `statement` as EXPLAIN's JSON; with `analyze`, measured by
/// running it and rolling back. It runs on a session of its own with the same
/// search path and read-only state, so the session's open transaction, if
/// any, is left alone.
#[tauri::command]
pub async fn pg_explain(
    session_id: u64,
    statement: String,
    analyze: bool,
    sessions: State<'_, Sessions>,
    history: State<'_, HistoryStore>,
) -> Result<serde_json::Value, Error> {
    let session = sessions.get(session_id)?;
    let explainer = Arc::new(session.another().await?);
    if sessions.read_only(session_id) {
        explainer.set_read_only(true).await?;
    }
    explainer
        .set_search_path(&session.search_path().await?)
        .await?;
    sessions
        .explaining
        .lock()
        .unwrap()
        .insert(session_id, Arc::clone(&explainer));
    let started = Started::now();
    let result = explainer.explain(&statement, analyze).await;
    sessions.explaining.lock().unwrap().remove(&session_id);
    if analyze && let Some(connection) = sessions.connection(session_id) {
        // Only ANALYZE runs the statement; a plain EXPLAIN changes nothing worth keeping.
        history.record(
            &connection,
            &format!("EXPLAIN ANALYZE {}", statement.trim()),
            started,
            &result,
            |_| None,
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
    sessions.check_writable(session_id)?;
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
    let result = session.apply_edit(&plan, edit.schema()).await;
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
    let explainer = sessions
        .explaining
        .lock()
        .unwrap()
        .get(&session_id)
        .cloned();
    if let Some(explainer) = explainer {
        explainer.cancel().await?;
    }
    sessions.get(session_id)?.cancel().await
}

#[tauri::command]
pub fn pg_disconnect(session_id: u64, sessions: State<'_, Sessions>) {
    sessions.open.lock().unwrap().remove(&session_id);
}
