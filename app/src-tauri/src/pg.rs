//! Database session commands. Connecting, running statements, reading the
//! structure and cancelling work for every engine; plans, row editing and the
//! rest are PostgreSQL's alone so far.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use dolmen_core::connections::{ConnectionStore, Engine, Environment};
use dolmen_core::history::Source;
use dolmen_core::postgres::ddl::{self, SchemaObject};
use dolmen_core::postgres::edit::{self, Edit};
use dolmen_core::postgres::migrate::Plan;
use dolmen_core::postgres::rows::{self, RowChanges, SaveResult};
use dolmen_core::postgres::{ConnectConfig, RowEstimate, Session, StatementResult};
use dolmen_core::schema::{Database, ForeignKey};
use dolmen_core::sql::{self, Risk};
use dolmen_core::{Error, Refusal};
use dolmen_core::{mongo, mysql, redis};
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
    /// The sessions exporting or importing data, by an id the UI chose, to cancel.
    jobs: Mutex<HashMap<u64, Arc<Session>>>,
}

struct Open {
    conn: Conn,
    engine: Engine,
    connection: history::Connection,
    /// Whether snapshot plans may change this database.
    apply_allowed: bool,
    /// The session the user connected, which the tabs' sessions were opened from.
    family: u64,
    environment: Option<Environment>,
    read_only: bool,
}

/// A session on one of the engines Dolmen speaks.
#[derive(Clone)]
pub(crate) enum Conn {
    Postgres(Arc<Session>),
    MySql(Arc<mysql::Session>),
    Redis(Arc<redis::Session>),
    Mongo(Arc<mongo::Session>),
}

impl Conn {
    async fn connect(engine: Engine, config: &ConnectConfig) -> Result<Conn, Error> {
        Ok(match engine {
            Engine::Postgres => Conn::Postgres(Arc::new(Session::connect(config).await?)),
            Engine::MySql | Engine::MariaDb => {
                Conn::MySql(Arc::new(mysql::Session::connect(config).await?))
            }
            Engine::Redis => Conn::Redis(Arc::new(redis::Session::connect(config).await?)),
            Engine::MongoDb => Conn::Mongo(Arc::new(
                mongo::Session::connect(config, mongo::Flavor::MongoDb).await?,
            )),
            Engine::DocumentDb => Conn::Mongo(Arc::new(
                mongo::Session::connect(config, mongo::Flavor::DocumentDb).await?,
            )),
        })
    }

    async fn another(&self) -> Result<Conn, Error> {
        Ok(match self {
            Conn::Postgres(session) => Conn::Postgres(Arc::new(session.another().await?)),
            Conn::MySql(session) => Conn::MySql(Arc::new(session.another().await?)),
            Conn::Redis(session) => Conn::Redis(Arc::new(session.another().await?)),
            Conn::Mongo(session) => Conn::Mongo(Arc::new(session.another().await?)),
        })
    }

    fn server_version(&self) -> String {
        match self {
            Conn::Postgres(session) => session.server_version().to_owned(),
            Conn::MySql(session) => session.server_version().to_owned(),
            Conn::Redis(session) => session.server_version().to_owned(),
            Conn::Mongo(session) => session.server_version().to_owned(),
        }
    }

    fn config(&self) -> &ConnectConfig {
        match self {
            Conn::Postgres(session) => session.config(),
            Conn::MySql(session) => session.config(),
            Conn::Redis(session) => session.config(),
            Conn::Mongo(session) => session.config(),
        }
    }

    async fn set_read_only(&self, read_only: bool) -> Result<(), Error> {
        match self {
            Conn::Postgres(session) => session.set_read_only(read_only).await,
            Conn::MySql(session) => session.set_read_only(read_only).await,
            // Redis and MongoDB have no read-only mode of their own; execute
            // refuses writes instead.
            Conn::Redis(_) | Conn::Mongo(_) => Ok(()),
        }
    }

    /// Runs `text`. Engines without a read-only mode of their own are told
    /// `read_only`, and refuse writes themselves.
    async fn execute(&self, text: &str, read_only: bool) -> Result<Vec<StatementResult>, Error> {
        match self {
            Conn::Postgres(session) => session.execute(text, MAX_ROWS).await,
            Conn::MySql(session) => session.execute(text, MAX_ROWS).await,
            Conn::Redis(session) => session.execute(text, MAX_ROWS, read_only).await,
            Conn::Mongo(session) => session.execute(text, MAX_ROWS, read_only).await,
        }
    }

    async fn set_schema(&self, schema: &str) -> Result<Option<String>, Error> {
        match self {
            Conn::Postgres(session) => session.set_schema(schema).await,
            Conn::MySql(session) => session.set_schema(schema).await,
            // Redis's "schemas" are its numbered databases.
            Conn::Redis(session) => {
                let database = schema.parse().map_err(|_| Error::Storage {
                    message: format!("{schema} isn't a database number"),
                })?;
                session.select(database).await?;
                Ok(Some(session.database().to_string()))
            }
            Conn::Mongo(session) => {
                session.set_database(schema);
                Ok(Some(session.database()))
            }
        }
    }

    async fn current_schema(&self) -> Result<Option<String>, Error> {
        match self {
            Conn::Postgres(session) => session.current_schema().await,
            Conn::MySql(session) => session.current_schema().await,
            Conn::Redis(session) => Ok(Some(session.database().to_string())),
            Conn::Mongo(session) => Ok(Some(session.database())),
        }
    }

    async fn cancel(&self) -> Result<(), Error> {
        match self {
            Conn::Postgres(session) => session.cancel().await,
            Conn::MySql(session) => session.cancel().await,
            // Redis commands answer at once, short of blocking ones; MongoDB's
            // driver has no way to stop an operation it sent.
            Conn::Redis(_) | Conn::Mongo(_) => Ok(()),
        }
    }
}

/// Refuses something only PostgreSQL connections can do yet.
pub(crate) fn not_supported(what: &str) -> Error {
    Error::Refused {
        reason: Refusal::NotSupported,
        message: format!("{what} isn't supported for this kind of database yet."),
    }
}

impl Sessions {
    /// The PostgreSQL session `id`; other engines are refused.
    pub(crate) fn get(&self, id: u64) -> Result<Arc<Session>, Error> {
        match self.conn(id)? {
            Conn::Postgres(session) => Ok(session),
            _ => Err(not_supported("This")),
        }
    }

    pub(crate) fn conn(&self, id: u64) -> Result<Conn, Error> {
        self.open
            .lock()
            .unwrap()
            .get(&id)
            .map(|open| open.conn.clone())
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

    /// Opens a session of its own to the database of session `id`, for a
    /// long job the UI can cancel by `job`.
    pub(crate) async fn start_job(&self, id: u64, job: u64) -> Result<Arc<Session>, Error> {
        let worker = Arc::new(self.get(id)?.another().await?);
        self.jobs.lock().unwrap().insert(job, Arc::clone(&worker));
        Ok(worker)
    }

    pub(crate) fn end_job(&self, job: u64) {
        self.jobs.lock().unwrap().remove(&job);
    }

    pub(crate) fn job(&self, job: u64) -> Option<Arc<Session>> {
        self.jobs.lock().unwrap().get(&job).cloned()
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
    engine: Engine,
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
pub async fn db_connect(
    engine: Engine,
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
        saved.engine == engine
            && saved.host == config.host
            && saved.port == config.port
            && saved.user == config.user
            && saved.database == config.database
    });
    if let (None, Some(saved)) = (&config.password, &saved) {
        config.password = connections.password(&saved.id)?;
    }
    let conn = Conn::connect(engine, &config).await?;
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
        conn.set_read_only(true).await?;
    }
    // Only PostgreSQL's snapshots can be applied so far.
    let apply_allowed = engine == Engine::Postgres
        && saved
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
        engine,
        server_version: conn.server_version(),
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
            conn,
            engine,
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
pub async fn db_fork(session_id: u64, sessions: State<'_, Sessions>) -> Result<SessionInfo, Error> {
    let (original, engine, connection, apply_allowed, family, environment, read_only) = {
        let open = sessions.open.lock().unwrap();
        let open = open.get(&session_id).ok_or(Error::Closed)?;
        (
            open.conn.clone(),
            open.engine,
            open.connection.clone(),
            open.apply_allowed,
            open.family,
            open.environment,
            open.read_only,
        )
    };
    let conn = original.another().await?;
    if read_only {
        conn.set_read_only(true).await?;
    }
    let id = sessions.next_id.fetch_add(1, Ordering::Relaxed);
    let info = SessionInfo {
        id,
        engine,
        server_version: conn.server_version(),
        database: original.config().database.clone(),
        saved_id: connection.saved_id.clone(),
        label: connection.label.clone(),
        address: session_address(&original),
        environment,
        read_only,
    };
    sessions.open.lock().unwrap().insert(
        id,
        Open {
            conn,
            engine,
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
pub async fn db_set_read_only(
    session_id: u64,
    read_only: bool,
    sessions: State<'_, Sessions>,
) -> Result<(), Error> {
    let members: Vec<(u64, Conn)> = {
        let open = sessions.open.lock().unwrap();
        let family = open.get(&session_id).ok_or(Error::Closed)?.family;
        open.iter()
            .filter(|(_, o)| o.family == family)
            .map(|(id, o)| (*id, o.conn.clone()))
            .collect()
    };
    for (id, conn) in members {
        conn.set_read_only(read_only).await?;
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
pub async fn db_set_schema(
    session_id: u64,
    schema: String,
    sessions: State<'_, Sessions>,
) -> Result<Option<String>, Error> {
    sessions.conn(session_id)?.set_schema(&schema).await
}

/// The schema the session's unqualified names resolve in first.
#[tauri::command]
pub async fn db_current_schema(
    session_id: u64,
    sessions: State<'_, Sessions>,
) -> Result<Option<String>, Error> {
    sessions.conn(session_id)?.current_schema().await
}

fn session_address(conn: &Conn) -> String {
    let config = conn.config();
    format!(
        "{}@{}:{}/{}",
        config.user, config.host, config.port, config.database
    )
}

#[tauri::command]
pub async fn db_execute(
    session_id: u64,
    sql: String,
    sessions: State<'_, Sessions>,
    history: State<'_, HistoryStore>,
) -> Result<Vec<StatementResult>, Error> {
    let conn = sessions.conn(session_id)?;
    let started = Started::now();
    let mut result = conn.execute(&sql, sessions.read_only(session_id)).await;
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
    // A statement's rows can be changed in the grid when they come from one table.
    if let (Conn::Postgres(session), Ok([StatementResult::Rows { source, .. }])) =
        (&conn, result.as_deref_mut())
    {
        *source = session.row_source(&sql).await;
    }
    result
}

/// The statements that saving `changes` makes, with their values written in,
/// for the user to review.
#[tauri::command]
pub fn rows_preview(changes: RowChanges) -> Vec<String> {
    rows::shown(&changes)
}

/// Saves the changes made to a result's rows in the grid, all of them or
/// none. In a transaction the session has open they join it.
#[tauri::command]
pub async fn rows_save(
    session_id: u64,
    changes: RowChanges,
    sessions: State<'_, Sessions>,
    history: State<'_, HistoryStore>,
) -> Result<SaveResult, Error> {
    sessions.check_writable(session_id)?;
    let session = sessions.get(session_id)?;
    let started = Started::now();
    let result = session.save_rows(&changes).await;
    if let Some(connection) = sessions.connection(session_id) {
        let outcome = match &result {
            Ok(SaveResult::Saved { rows, .. }) => Ok(rows.len() as i64),
            Ok(SaveResult::Stale { .. }) => Err(Error::Refused {
                reason: Refusal::RowChanged,
                message: "A row changed or was deleted since it was read, so nothing was saved."
                    .into(),
            }),
            Ok(SaveResult::Failed { error, .. }) => Err(error.clone()),
            Err(err) => Err(err.clone()),
        };
        history.record(
            &connection,
            &rows::shown(&changes).join("\n"),
            started,
            &outcome,
            |rows| Some(*rows),
            Source::Grid,
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
pub async fn db_introspect(
    session_id: u64,
    sessions: State<'_, Sessions>,
) -> Result<Introspection, Error> {
    match sessions.conn(session_id)? {
        Conn::Postgres(session) => Ok(Introspection {
            default_schema: session.current_schema().await?,
            database: session.introspect().await?,
            foreign_keys: session.foreign_keys().await?,
        }),
        Conn::MySql(session) => Ok(Introspection {
            default_schema: session.current_schema().await?,
            database: session.introspect().await?,
            foreign_keys: session.foreign_keys().await?,
        }),
        // Redis's numbered databases stand in for schemas, so a tab can pick
        // one; its keys are listed by redis_keys.
        Conn::Redis(session) => {
            let count = session.databases().await?.count;
            Ok(Introspection {
                default_schema: Some(session.database().to_string()),
                database: Database {
                    schemas: (0..count).map(|n| empty_schema(n.to_string())).collect(),
                    extensions: Vec::new(),
                },
                foreign_keys: Vec::new(),
            })
        }
        // Databases are schemas and collections tables, with fields sampled
        // from their documents.
        Conn::Mongo(session) => Ok(Introspection {
            default_schema: Some(session.database()),
            database: session.introspect(MONGO_SAMPLE).await?,
            foreign_keys: Vec::new(),
        }),
    }
}

/// How many documents of each collection are read for its fields.
const MONGO_SAMPLE: usize = 100;

fn empty_schema(name: String) -> dolmen_core::schema::Schema {
    dolmen_core::schema::Schema {
        name,
        tables: Vec::new(),
        views: Vec::new(),
        functions: Vec::new(),
        sequences: Vec::new(),
        enums: Vec::new(),
    }
}

/// A Redis key, with the commands that show it and name it, written by the
/// core so names that aren't UTF-8 still reach the same key.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedisKey {
    name: String,
    kind: String,
    /// The command that shows its value for its kind, such as `HGETALL user:1`.
    show: String,
    /// The name as a command argument.
    quoted: String,
}

#[derive(Serialize)]
pub struct RedisKeys {
    keys: Vec<RedisKey>,
    /// False when the limit cut the scan short.
    complete: bool,
}

/// The keys of the session's database that match `pattern`, at most `limit`.
#[tauri::command]
pub async fn redis_keys(
    session_id: u64,
    pattern: String,
    limit: usize,
    sessions: State<'_, Sessions>,
) -> Result<RedisKeys, Error> {
    let Conn::Redis(session) = sessions.conn(session_id)? else {
        return Err(not_supported("Listing keys"));
    };
    let found = session.keys(&pattern, limit.min(100_000)).await?;
    Ok(RedisKeys {
        keys: found
            .keys
            .into_iter()
            .map(|key| RedisKey {
                show: redis::show_command(&key),
                quoted: redis::quote(&key.name),
                name: key.name,
                kind: key.kind,
            })
            .collect(),
        complete: found.complete,
    })
}

/// An object's `CREATE` statement, as the session's server would write it:
/// MySQL's own for tables and views, Dolmen's from the model otherwise.
#[tauri::command]
pub async fn db_create_statement(
    session_id: u64,
    schema: String,
    object: SchemaObject,
    sessions: State<'_, Sessions>,
) -> Result<String, Error> {
    match (sessions.conn(session_id)?, &object) {
        (Conn::Postgres(_), _) => Ok(ddl::create_statement(&schema, &object)),
        (Conn::MySql(session), SchemaObject::Table(table)) => {
            session.create_statement(&schema, &table.name, false).await
        }
        (Conn::MySql(session), SchemaObject::View(view)) => {
            session.create_statement(&schema, &view.name, true).await
        }
        (Conn::MySql(_), SchemaObject::Function(function)) => {
            Ok(format!("{};\n", function.definition.trim_end()))
        }
        (Conn::MySql(_) | Conn::Redis(_) | Conn::Mongo(_), _) => {
            Err(not_supported("This object's DDL"))
        }
    }
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
pub async fn db_cancel(session_id: u64, sessions: State<'_, Sessions>) -> Result<(), Error> {
    let explainer = sessions
        .explaining
        .lock()
        .unwrap()
        .get(&session_id)
        .cloned();
    if let Some(explainer) = explainer {
        explainer.cancel().await?;
    }
    sessions.conn(session_id)?.cancel().await
}

#[tauri::command]
pub fn db_disconnect(session_id: u64, sessions: State<'_, Sessions>) {
    sessions.open.lock().unwrap().remove(&session_id);
}
