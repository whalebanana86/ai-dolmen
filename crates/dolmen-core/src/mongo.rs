//! MongoDB and DocumentDB sessions.
//!
//! Statements are written as in mongosh (`db.users.find({ age: 30 })`, `show
//! dbs`, `use shop`), read by [`shell`] and run through the official driver.
//! Results come back in PostgreSQL's shapes, so the app shows them the same
//! way: documents as rows, with a column per top-level field.
//!
//! Unlike a PostgreSQL session, a session here doesn't own a connection: the
//! driver keeps a pool, and every database on the server is reachable through
//! it. The current database is only a name, which `use` changes.

use std::sync::Mutex;
use std::time::Duration;

use futures_util::TryStreamExt;
use mongodb::bson::{Bson, Document, doc};
use mongodb::options::{
    ClientOptions, Credential, FindOneOptions, FindOptions, ServerAddress, Tls, TlsOptions,
    UpdateModifications,
};
use mongodb::{Client, Cursor};
use serde::{Deserialize, Serialize};

use crate::postgres::{ConnectConfig, SslMode, StatementResult};
use crate::schema::Database;
use crate::{Error, Refusal};

mod introspect;
mod shell;

pub use shell::statement_ranges;
use shell::{CollectionMethod, DatabaseMethod, Operation, Statement, Update};

/// How long connecting, and finding the server at all, may take; a wrong
/// address fails after this.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The database statements run in when the connection names none, as in mongosh.
const DEFAULT_DATABASE: &str = "test";

/// How many documents [`Session::show_statement`] shows.
const SHOW_LIMIT: u32 = 1000;

/// Commands `runCommand` and `adminCommand` may run on a read-only
/// connection, because they only read. Any other command is refused.
const READ_ONLY_COMMANDS: &[&str] = &[
    "find",
    "aggregate",
    "count",
    "distinct",
    "listCollections",
    "listIndexes",
    "listDatabases",
    "dbStats",
    "collStats",
    "serverStatus",
    "ping",
    "buildInfo",
    "hello",
    "isMaster",
    "connectionStatus",
    "hostInfo",
    "explain",
    "getParameter",
    "getCmdLineOpts",
    "currentOp",
    "validate",
];

/// Which server speaks the MongoDB protocol, for the few things done differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Flavor {
    MongoDb,
    /// AWS DocumentDB, or Microsoft's open-source DocumentDB.
    DocumentDb,
}

/// A connection to a MongoDB or DocumentDB server.
pub struct Session {
    client: Client,
    /// The database `db` means, which `use` changes.
    database: Mutex<String>,
    server_version: String,
    config: ConnectConfig,
    flavor: Flavor,
}

impl Session {
    /// Connects straight to `config.host:port`, without discovering the rest
    /// of a replica set, so the address given is the server used. The user,
    /// if any, signs in against `admin`, where MongoDB keeps users by default.
    ///
    /// MongoDB can't negotiate TLS on the same port the way PostgreSQL does,
    /// so `prefer` means no TLS; `require` encrypts without verifying the
    /// certificate, and `verify-full` verifies it and the host name.
    pub async fn connect(config: &ConnectConfig, flavor: Flavor) -> Result<Session, Error> {
        let tls = match config.ssl_mode {
            SslMode::Disable | SslMode::Prefer => Tls::Disabled,
            SslMode::Require => Tls::Enabled(
                TlsOptions::builder()
                    .allow_invalid_certificates(true)
                    .build(),
            ),
            SslMode::VerifyFull => Tls::Enabled(TlsOptions::default()),
        };
        let credential = (!config.user.is_empty()).then(|| {
            Credential::builder()
                .username(config.user.clone())
                .password(config.password.clone())
                .source("admin".to_owned())
                .build()
        });
        let options = ClientOptions::builder()
            .hosts(vec![ServerAddress::Tcp {
                host: config.host.clone(),
                port: Some(config.port),
            }])
            .direct_connection(true)
            .app_name("Dolmen".to_owned())
            .connect_timeout(CONNECT_TIMEOUT)
            .server_selection_timeout(CONNECT_TIMEOUT)
            .credential(credential)
            .tls(tls)
            // AWS DocumentDB has no retryable writes, and refuses writes that ask for them.
            .retry_writes((flavor == Flavor::DocumentDb).then_some(false))
            .build();
        let client = Client::with_options(options)?;

        // The driver connects lazily; ping so a wrong address or password fails here.
        let admin = client.database("admin");
        admin.run_command(doc! { "ping": 1 }).await?;
        // DocumentDB answers with the MongoDB version it is compatible with.
        let server_version = admin
            .run_command(doc! { "buildInfo": 1 })
            .await
            .ok()
            .and_then(|info| info.get_str("version").ok().map(str::to_owned))
            .unwrap_or_default();

        let database = if config.database.is_empty() {
            DEFAULT_DATABASE.to_owned()
        } else {
            config.database.clone()
        };
        Ok(Session {
            client,
            database: Mutex::new(database),
            server_version,
            config: config.clone(),
            flavor,
        })
    }

    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    /// The settings the session connected with.
    pub fn config(&self) -> &ConnectConfig {
        &self.config
    }

    pub fn flavor(&self) -> Flavor {
        self.flavor
    }

    /// Another session on the same connection pool, starting in this
    /// session's current database; `use` in one doesn't change the other.
    pub async fn another(&self) -> Result<Session, Error> {
        Ok(Session {
            client: self.client.clone(),
            database: Mutex::new(self.database()),
            server_version: self.server_version.clone(),
            config: self.config.clone(),
            flavor: self.flavor,
        })
    }

    /// The database `db` means in statements.
    pub fn database(&self) -> String {
        self.database.lock().unwrap().clone()
    }

    pub fn set_database(&self, name: &str) {
        *self.database.lock().unwrap() = name.to_owned();
    }

    /// Runs mongosh statements in order and returns a result per statement.
    ///
    /// The whole text is read first, so a syntax error, or on a `read_only`
    /// connection a statement that writes, stops it before anything runs.
    /// Then the first statement to fail stops the rest; MongoDB has no
    /// transaction to undo the ones before it. At most `max_rows` documents
    /// are kept per result; reading stops one past that, so `total_rows` of
    /// `max_rows + 1` means "more than `max_rows`".
    pub async fn execute(
        &self,
        text: &str,
        max_rows: usize,
        read_only: bool,
    ) -> Result<Vec<StatementResult>, Error> {
        let statements = shell::parse(text)?;
        if read_only {
            for statement in &statements {
                refuse_writes(&statement.operation)?;
            }
        }
        let mut results = Vec::with_capacity(statements.len());
        for statement in statements {
            results.push(self.run(statement, max_rows).await?);
        }
        Ok(results)
    }

    /// Reads every database's collections and views, with columns sampled
    /// from up to `sample` documents each; see [`introspect`].
    pub async fn introspect(&self, sample: usize) -> Result<Database, Error> {
        introspect::introspect(self, sample).await
    }

    /// The statement that shows a collection's documents.
    pub fn show_statement(collection: &str) -> String {
        format!(
            "db.getCollection({}).find({{}}).limit({SHOW_LIMIT})",
            shell::quote(collection)
        )
    }

    /// The statement that shows the documents of a collection in another
    /// database than the current one.
    pub fn show_statement_in(database: &str, collection: &str) -> String {
        format!(
            "db.getSiblingDB({}).getCollection({}).find({{}}).limit({SHOW_LIMIT})",
            shell::quote(database),
            shell::quote(collection)
        )
    }

    async fn run(&self, statement: Statement, max_rows: usize) -> Result<StatementResult, Error> {
        let database = self
            .client
            .database(&statement.database.unwrap_or_else(|| self.database()));
        match statement.operation {
            Operation::ShowDatabases => {
                let reply = self
                    .client
                    .database("admin")
                    .run_command(doc! { "listDatabases": 1 })
                    .await?;
                let databases = documents_in(&reply, "databases");
                Ok(rows(
                    vec!["name".into(), "sizeOnDisk".into(), "empty".into()],
                    &databases,
                    databases.len() as u64,
                ))
            }
            Operation::Use(name) => {
                self.set_database(&name);
                Ok(StatementResult::Command { rows_affected: 0 })
            }
            Operation::Database(method) => self.run_on_database(&database, method, max_rows).await,
            Operation::Collection { name, method } => {
                self.run_on_collection(&database, &name, method, max_rows)
                    .await
            }
        }
    }

    async fn run_on_database(
        &self,
        database: &mongodb::Database,
        method: DatabaseMethod,
        max_rows: usize,
    ) -> Result<StatementResult, Error> {
        let done = StatementResult::Command { rows_affected: 0 };
        match method {
            DatabaseMethod::RunCommand { command, admin } => {
                let reply = if admin {
                    self.client.database("admin").run_command(command).await?
                } else {
                    database.run_command(command).await?
                };
                Ok(documents(&[reply], 1))
            }
            DatabaseMethod::GetCollectionNames => {
                let mut names = database.list_collection_names().await?;
                names.sort();
                Ok(values(
                    "name",
                    names.into_iter().map(Bson::String),
                    max_rows,
                ))
            }
            DatabaseMethod::CreateCollection { name, options } => {
                let mut command = doc! { "create": name };
                command.extend(options);
                database.run_command(command).await?;
                Ok(done)
            }
            DatabaseMethod::CreateView {
                name,
                source,
                pipeline,
                options,
            } => {
                let mut command = doc! { "create": name, "viewOn": source, "pipeline": pipeline };
                command.extend(options);
                database.run_command(command).await?;
                Ok(done)
            }
            DatabaseMethod::DropDatabase => {
                database.drop().await?;
                Ok(done)
            }
            DatabaseMethod::Stats => {
                let reply = database.run_command(doc! { "dbStats": 1 }).await?;
                Ok(documents(&[reply], 1))
            }
            DatabaseMethod::GetName => Ok(values(
                "name",
                [Bson::String(database.name().to_owned())],
                max_rows,
            )),
        }
    }

    async fn run_on_collection(
        &self,
        database: &mongodb::Database,
        name: &str,
        method: CollectionMethod,
        max_rows: usize,
    ) -> Result<StatementResult, Error> {
        let collection = database.collection::<Document>(name);
        let done = StatementResult::Command { rows_affected: 0 };
        // Ask for no more than is kept, plus the one that shows there are more.
        let batch_size = u32::try_from(max_rows.saturating_add(1)).unwrap_or(u32::MAX);
        match method {
            CollectionMethod::Find(find) => {
                if find.count {
                    let count = collection.count_documents(find.filter).await?;
                    return Ok(count_of(count));
                }
                let options = FindOptions::builder()
                    .projection(find.projection)
                    .sort(find.sort)
                    .limit(find.limit)
                    .skip(find.skip)
                    .batch_size(batch_size)
                    .build();
                read(
                    collection.find(find.filter).with_options(options).await?,
                    max_rows,
                )
                .await
            }
            CollectionMethod::FindOne { filter, projection } => {
                let options = FindOneOptions::builder().projection(projection).build();
                let found = collection.find_one(filter).with_options(options).await?;
                let found: Vec<Document> = found.into_iter().collect();
                Ok(documents(&found, found.len() as u64))
            }
            CollectionMethod::CountDocuments { filter, options } => Ok(count_of(
                collection
                    .count_documents(filter)
                    .with_options(options)
                    .await?,
            )),
            CollectionMethod::EstimatedDocumentCount(options) => Ok(count_of(
                collection
                    .estimated_document_count()
                    .with_options(options)
                    .await?,
            )),
            CollectionMethod::Distinct {
                field,
                filter,
                options,
            } => {
                let found = collection
                    .distinct(field, filter)
                    .with_options(options)
                    .await?;
                Ok(values("value", found, max_rows))
            }
            CollectionMethod::Aggregate { pipeline, options } => {
                let mut options = options.unwrap_or_default();
                options.batch_size.get_or_insert(batch_size);
                read(
                    collection.aggregate(pipeline).with_options(options).await?,
                    max_rows,
                )
                .await
            }
            CollectionMethod::InsertOne { document, options } => {
                collection
                    .insert_one(document)
                    .with_options(options)
                    .await?;
                Ok(StatementResult::Command { rows_affected: 1 })
            }
            CollectionMethod::InsertMany { documents, options } => {
                let result = collection
                    .insert_many(documents)
                    .with_options(options)
                    .await?;
                Ok(StatementResult::Command {
                    rows_affected: result.inserted_ids.len() as u64,
                })
            }
            // Updates count the documents changed, plus one inserted by an
            // upsert: a document matched but left as it was isn't counted.
            CollectionMethod::Update {
                filter,
                update,
                options,
                many,
            } => {
                let update = match update {
                    Update::Document(document) => UpdateModifications::Document(document),
                    Update::Pipeline(pipeline) => UpdateModifications::Pipeline(pipeline),
                };
                let result = if many {
                    collection
                        .update_many(filter, update)
                        .with_options(options)
                        .await?
                } else {
                    collection
                        .update_one(filter, update)
                        .with_options(options)
                        .await?
                };
                Ok(StatementResult::Command {
                    rows_affected: result.modified_count + u64::from(result.upserted_id.is_some()),
                })
            }
            CollectionMethod::ReplaceOne {
                filter,
                replacement,
                options,
            } => {
                let result = collection
                    .replace_one(filter, replacement)
                    .with_options(options)
                    .await?;
                Ok(StatementResult::Command {
                    rows_affected: result.modified_count + u64::from(result.upserted_id.is_some()),
                })
            }
            CollectionMethod::Delete {
                filter,
                options,
                many,
            } => {
                let result = if many {
                    collection.delete_many(filter).with_options(options).await?
                } else {
                    collection.delete_one(filter).with_options(options).await?
                };
                Ok(StatementResult::Command {
                    rows_affected: result.deleted_count,
                })
            }
            // Run as the createIndexes command, as mongosh does, so the server
            // checks every option rather than the driver dropping unknown ones.
            CollectionMethod::CreateIndex { keys, options } => {
                let index_name = match options.get_str("name") {
                    Ok(index_name) => index_name.to_owned(),
                    Err(_) => default_index_name(&keys),
                };
                let mut index = doc! { "key": keys, "name": index_name };
                index.extend(options.into_iter().filter(|(key, _)| key != "name"));
                database
                    .run_command(doc! { "createIndexes": name, "indexes": [index] })
                    .await?;
                Ok(done)
            }
            CollectionMethod::DropIndex(index) => {
                database
                    .run_command(doc! { "dropIndexes": name, "index": index })
                    .await?;
                Ok(done)
            }
            CollectionMethod::GetIndexes => {
                let indexes: Vec<Document> = database
                    .run_cursor_command(doc! { "listIndexes": name })
                    .await?
                    .try_collect()
                    .await?;
                Ok(index_rows(&indexes))
            }
            CollectionMethod::Drop => {
                collection.drop().await?;
                Ok(done)
            }
            CollectionMethod::RenameCollection { to, drop_target } => {
                let database = database.name();
                self.client
                    .database("admin")
                    .run_command(doc! {
                        "renameCollection": format!("{database}.{name}"),
                        "to": format!("{database}.{to}"),
                        "dropTarget": drop_target,
                    })
                    .await?;
                Ok(done)
            }
        }
    }
}

/// Refuses a statement that writes, for a read-only connection. Commands
/// pass only when known to read, and pipelines only without `$out` or `$merge`.
fn refuse_writes(operation: &Operation) -> Result<(), Error> {
    let reason = match (operation.write_method(), operation) {
        (Some(method), _) => Some(format!("{method}() changes the database")),
        (
            None,
            Operation::Collection {
                method: CollectionMethod::Aggregate { pipeline, .. },
                ..
            },
        ) => writing_stage(pipeline.iter()).map(pipeline_writes),
        (None, Operation::Database(DatabaseMethod::RunCommand { command, .. })) => {
            command_writes(command)
        }
        (None, _) => None,
    };
    match reason {
        Some(reason) => Err(Error::Refused {
            reason: Refusal::ReadOnly,
            message: format!("The connection is read-only, and {reason}."),
        }),
        None => Ok(()),
    }
}

fn pipeline_writes(stage: &str) -> String {
    format!("the pipeline's {stage} stage writes to a collection")
}

/// The `$out` or `$merge` stage of a pipeline, which write its result to a
/// collection. Neither may nest in another stage, so the top level is enough.
fn writing_stage<'a>(pipeline: impl IntoIterator<Item = &'a Document>) -> Option<&'static str> {
    pipeline.into_iter().find_map(|stage| {
        ["$out", "$merge"]
            .into_iter()
            .find(|name| stage.contains_key(name))
    })
}

/// Why a command can't run on a read-only connection, if it can't.
fn command_writes(command: &Document) -> Option<String> {
    let name = command
        .keys()
        .next()
        .map(String::as_str)
        .unwrap_or_default();
    if !READ_ONLY_COMMANDS
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(name))
    {
        return Some(format!("the {name} command isn't known to only read"));
    }
    // validate can repair a collection, which writes.
    if name.eq_ignore_ascii_case("validate") && command.get_bool("repair") == Ok(true) {
        return Some("validate with repair writes to the collection".into());
    }
    // An aggregate, or one explain describes, may end in $out or $merge.
    let aggregate = match command.get(name) {
        Some(Bson::Document(inner)) if name.eq_ignore_ascii_case("explain") => inner,
        _ => command,
    };
    let is_aggregate = aggregate
        .keys()
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("aggregate"));
    let pipeline = match aggregate.get("pipeline") {
        Some(Bson::Array(stages)) if is_aggregate => stages,
        _ => return None,
    };
    writing_stage(pipeline.iter().filter_map(Bson::as_document)).map(pipeline_writes)
}

/// The name MongoDB gives an index by default, e.g. `email_1_name_-1`.
fn default_index_name(keys: &Document) -> String {
    keys.iter()
        .map(|(field, kind)| {
            let kind = match kind {
                Bson::String(kind) => kind.clone(),
                other => cell(other).unwrap_or_default(),
            };
            format!("{field}_{kind}")
        })
        .collect::<Vec<_>>()
        .join("_")
}

/// Reads up to `max_rows` documents, and one more to tell that there are more.
async fn read(mut cursor: Cursor<Document>, max_rows: usize) -> Result<StatementResult, Error> {
    let mut kept = Vec::new();
    let mut total_rows = 0;
    while let Some(document) = cursor.try_next().await? {
        total_rows += 1;
        if kept.len() == max_rows {
            break;
        }
        kept.push(document);
    }
    Ok(documents(&kept, total_rows))
}

/// Documents as rows: a column per top-level field in the order fields
/// first appear, `_id` first.
fn documents(documents: &[Document], total_rows: u64) -> StatementResult {
    rows(columns(documents, &["_id"]), documents, total_rows)
}

/// The top-level fields of `documents` in the order they first appear,
/// with those of `leading` that appear moved to the front.
fn columns(documents: &[Document], leading: &[&str]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut columns = Vec::new();
    for key in documents.iter().flat_map(Document::keys) {
        if seen.insert(key.as_str()) {
            columns.push(key.clone());
        }
    }
    let mut front: Vec<String> = leading
        .iter()
        .filter(|name| seen.contains(**name))
        .map(|name| (*name).to_owned())
        .collect();
    columns.retain(|column| !leading.contains(&column.as_str()));
    front.append(&mut columns);
    front
}

fn rows(columns: Vec<String>, documents: &[Document], total_rows: u64) -> StatementResult {
    let rows = documents
        .iter()
        .map(|document| {
            columns
                .iter()
                .map(|column| document.get(column).and_then(cell))
                .collect()
        })
        .collect();
    StatementResult::Rows {
        columns,
        rows,
        total_rows,
        source: None,
    }
}

/// One column of values, at most `max_rows` of them.
fn values(
    column: &str,
    values: impl IntoIterator<Item = Bson>,
    max_rows: usize,
) -> StatementResult {
    let mut rows = Vec::new();
    let mut total_rows = 0;
    for value in values {
        total_rows += 1;
        if rows.len() < max_rows {
            rows.push(vec![cell(&value)]);
        }
    }
    StatementResult::Rows {
        columns: vec![column.to_owned()],
        rows,
        total_rows,
        source: None,
    }
}

fn count_of(count: u64) -> StatementResult {
    StatementResult::Rows {
        columns: vec!["count".into()],
        rows: vec![vec![Some(count.to_string())]],
        total_rows: 1,
        source: None,
    }
}

/// The documents of an array field in a command's reply.
fn documents_in(reply: &Document, field: &str) -> Vec<Document> {
    reply
        .get_array(field)
        .map(|values| {
            values
                .iter()
                .filter_map(Bson::as_document)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// `getIndexes()` as rows: name, keys and whether unique, then the other
/// options any index has. The index format version `v` is left out.
fn index_rows(indexes: &[Document]) -> StatementResult {
    let shown: Vec<Document> = indexes
        .iter()
        .map(|index| {
            let mut shown = index.clone();
            shown.remove("v");
            shown.remove("ns");
            if !shown.contains_key("unique") {
                shown.insert("unique", false);
            }
            shown
        })
        .collect();
    let columns = columns(&shown, &["name", "key", "unique"]);
    rows(columns, &shown, shown.len() as u64)
}

/// A value as a grid cell. Strings, numbers, booleans, ObjectIds (as hex)
/// and dates (as ISO-8601 UTC) read as themselves; anything else, such as a
/// document, an array or binary data, as compact relaxed Extended JSON.
/// `null` is `None`, like a missing field.
fn cell(value: &Bson) -> Option<String> {
    Some(match value {
        Bson::Null | Bson::Undefined => return None,
        Bson::String(text) => text.clone(),
        Bson::Int32(n) => n.to_string(),
        Bson::Int64(n) => n.to_string(),
        Bson::Double(n) => shell::format_double(*n),
        Bson::Boolean(b) => b.to_string(),
        Bson::ObjectId(id) => id.to_hex(),
        Bson::DateTime(date) => match shell::format_date(*date) {
            Some(text) => text,
            None => value.clone().into_relaxed_extjson().to_string(),
        },
        other => other.clone().into_relaxed_extjson().to_string(),
    })
}

impl From<mongodb::error::Error> for Error {
    fn from(err: mongodb::error::Error) -> Self {
        use mongodb::error::{ErrorKind, WriteFailure};

        let database = |code: String, message: &str, details: Option<&Document>| Error::Database {
            message: message.to_owned(),
            code,
            detail: details.map(|details| {
                Bson::Document(details.clone())
                    .into_relaxed_extjson()
                    .to_string()
            }),
            hint: None,
            position: None,
        };
        match err.kind.as_ref() {
            ErrorKind::Command(error) => database(
                code_name(error.code, Some(&error.code_name)),
                &error.message,
                None,
            ),
            ErrorKind::Write(WriteFailure::WriteError(error)) => database(
                code_name(error.code, error.code_name.as_deref()),
                &error.message,
                error.details.as_ref(),
            ),
            ErrorKind::Write(WriteFailure::WriteConcernError(error)) => database(
                code_name(error.code, Some(&error.code_name)),
                &error.message,
                error.details.as_ref(),
            ),
            ErrorKind::InsertMany(error) => {
                match (&error.write_errors, &error.write_concern_error) {
                    (Some(errors), _) if !errors.is_empty() => database(
                        code_name(errors[0].code, errors[0].code_name.as_deref()),
                        &errors[0].message,
                        errors[0].details.as_ref(),
                    ),
                    (_, Some(error)) => database(
                        code_name(error.code, Some(&error.code_name)),
                        &error.message,
                        error.details.as_ref(),
                    ),
                    _ => database("WriteError".into(), &err.kind.to_string(), None),
                }
            }
            ErrorKind::Authentication { message, .. } => {
                database("AuthenticationFailed".into(), message, None)
            }
            // A mistake in the statement the driver caught before sending it,
            // such as an update document without operators.
            ErrorKind::InvalidArgument { message, .. } => {
                database("InvalidArgument".into(), message, None)
            }
            ErrorKind::ServerSelection { message, .. } => Error::Connection {
                message: unreachable_message(message),
            },
            ErrorKind::Shutdown => Error::Closed,
            _ => Error::Connection {
                message: err.kind.to_string(),
            },
        }
    }
}

/// Why the server couldn't be reached, e.g. "Couldn't reach the server: I/O
/// error: Connection refused (os error 61)". The driver's message wraps the
/// cause in a description of every server it knows; with a direct
/// connection there is one, so its error is the cause.
fn unreachable_message(message: &str) -> String {
    let cause = message
        .split_once("Error: Kind: ")
        .and_then(|(_, rest)| rest.split_once(", labels:"))
        .map(|(cause, _)| cause);
    match cause {
        Some(cause) => format!("Couldn't reach the server: {cause}"),
        None => message.to_owned(),
    }
}

/// The server's name for an error code, such as `DuplicateKey`, or the code
/// itself. Write errors often come without a name.
fn code_name(code: i32, name: Option<&str>) -> String {
    match (name, code) {
        (Some(name), _) if !name.is_empty() => name.to_owned(),
        (_, 11000) => "DuplicateKey".into(),
        (_, 121) => "DocumentValidationFailure".into(),
        _ => code.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use mongodb::bson::{DateTime, oid::ObjectId};

    use super::*;

    fn refusal(text: &str) -> Option<String> {
        let mut refused = None;
        for statement in shell::parse(text).unwrap() {
            if let Err(Error::Refused {
                reason: Refusal::ReadOnly,
                message,
            }) = refuse_writes(&statement.operation)
            {
                refused = Some(message);
            }
        }
        refused
    }

    #[test]
    fn refuses_writes_on_read_only_connections() {
        for text in [
            "db.c.insertOne({})",
            "db.c.insertMany([{}])",
            "db.c.updateOne({}, { $set: { a: 1 } })",
            "db.c.updateMany({}, { $set: { a: 1 } })",
            "db.c.replaceOne({}, {})",
            "db.c.deleteOne({})",
            "db.c.deleteMany({})",
            "db.c.createIndex({ a: 1 })",
            "db.c.dropIndex('a_1')",
            "db.c.drop()",
            "db.c.renameCollection('d')",
            "db.createCollection('c')",
            "db.createView('v', 'c', [])",
            "db.dropDatabase()",
            "db.getSiblingDB('shop').dropDatabase()",
            "db.getSiblingDB('other').c.insertOne({})",
            "db.c.aggregate([{ $match: {} }, { $out: 'copy' }])",
            "db.c.aggregate({ $merge: { into: 'copy' } })",
            "db.runCommand({ dropDatabase: 1 })",
            "db.adminCommand({ shutdown: 1 })",
            "db.runCommand({ aggregate: 'c', pipeline: [{ $out: 'copy' }], cursor: {} })",
            "db.runCommand({ explain: { aggregate: 'c', pipeline: [{ $merge: 'x' }] } })",
            "db.runCommand({ validate: 'c', repair: true })",
            "db.runCommand('drop')",
        ] {
            assert!(refusal(text).is_some(), "{text} should be refused");
        }
        assert_eq!(
            refusal("db.c.insertOne({})").unwrap(),
            "The connection is read-only, and insertOne() changes the database."
        );
    }

    #[test]
    fn lets_reads_run_on_read_only_connections() {
        for text in [
            "db.c.find({ a: 1 }).sort({ a: 1 }).limit(5)",
            "db.c.findOne()",
            "db.c.countDocuments()",
            "db.c.estimatedDocumentCount()",
            "db.c.distinct('a')",
            "db.c.aggregate([{ $group: { _id: '$a', n: { $mergeObjects: '$b' } } }])",
            "db.c.getIndexes()",
            "db.getCollectionNames()",
            "db.stats()",
            "db.getName()",
            "show dbs",
            "show collections",
            "use other",
            "db.runCommand({ ping: 1 })",
            "db.runCommand('buildInfo')",
            "db.adminCommand({ listDatabases: 1 })",
            "db.runCommand({ ismaster: 1 })",
            "db.runCommand({ aggregate: 'c', pipeline: [{ $match: {} }], cursor: {} })",
            "db.runCommand({ validate: 'c' })",
        ] {
            assert_eq!(refusal(text), None, "{text} should run");
        }
    }

    #[test]
    fn writes_cells_as_text() {
        let id = ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap();
        let cells: Vec<_> = [
            Bson::String("text".into()),
            Bson::Int32(1),
            Bson::Int64(9_007_199_254_740_993),
            Bson::Double(2.0),
            Bson::Double(0.1),
            Bson::Boolean(true),
            Bson::ObjectId(id),
            Bson::DateTime(DateTime::from_millis(1_714_557_600_123)),
            Bson::Null,
            Bson::Document(doc! { "a": [1, "x"] }),
            Bson::Decimal128("1.50".parse().unwrap()),
        ]
        .iter()
        .map(cell)
        .collect();
        assert_eq!(
            cells,
            [
                Some("text".into()),
                Some("1".into()),
                Some("9007199254740993".into()),
                Some("2".into()),
                Some("0.1".into()),
                Some("true".into()),
                Some("507f1f77bcf86cd799439011".into()),
                Some("2024-05-01T10:00:00.123Z".into()),
                None,
                Some(r#"{"a":[1,"x"]}"#.into()),
                Some(r#"{"$numberDecimal":"1.50"}"#.into()),
            ]
        );
    }

    #[test]
    fn makes_columns_from_fields_in_first_seen_order() {
        let result = documents(
            &[
                doc! { "name": "a", "_id": 1 },
                doc! { "_id": 2, "age": 3, "name": null },
            ],
            2,
        );
        let StatementResult::Rows { columns, rows, .. } = result else {
            panic!("expected rows");
        };
        assert_eq!(columns, ["_id", "name", "age"]);
        assert_eq!(
            rows,
            [
                vec![Some("1".into()), Some("a".into()), None],
                vec![Some("2".into()), None, Some("3".into())],
            ]
        );
    }

    #[test]
    fn names_indexes_as_mongodb_does() {
        assert_eq!(
            default_index_name(&doc! { "email": 1, "createdAt": -1.0, "body": "text" }),
            "email_1_createdAt_-1_body_text"
        );
    }

    #[test]
    fn writes_show_statements_that_parse() {
        let shown = Session::show_statement("odd \"name\"");
        assert_eq!(
            shown,
            r#"db.getCollection("odd \"name\"").find({}).limit(1000)"#
        );
        let shown_in = Session::show_statement_in("shop", "orders");
        assert_eq!(
            shown_in,
            r#"db.getSiblingDB("shop").getCollection("orders").find({}).limit(1000)"#
        );
        for text in [shown, shown_in] {
            assert_eq!(shell::parse(&text).unwrap().len(), 1);
        }
    }

    #[test]
    fn reduces_server_selection_errors_to_their_cause() {
        let message = "Server selection timeout: No available servers. Topology: { Type: Single, \
                       Servers: [ { Address: db:27017, Type: Unknown, Error: Kind: I/O error: \
                       Connection refused (os error 61), labels: {}, source: None } ] }";
        assert_eq!(
            unreachable_message(message),
            "Couldn't reach the server: I/O error: Connection refused (os error 61)"
        );
        assert_eq!(unreachable_message("something else"), "something else");
    }

    /// The app shares sessions between Tokio's threads and runs their work there.
    #[test]
    fn sessions_and_their_futures_cross_threads() {
        fn shared<T: Send + Sync>() {}
        fn send<T: Send>(_: &T) {}
        shared::<Session>();
        let _ = |session: &Session, config: &ConnectConfig| {
            send(&Session::connect(config, Flavor::MongoDb));
            send(&session.execute("", 1, false));
            send(&session.introspect(1));
            send(&session.another());
        };
    }
}
