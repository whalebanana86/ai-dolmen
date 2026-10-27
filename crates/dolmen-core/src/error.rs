use serde::Serialize;

/// Errors returned by database sessions.
///
/// Serialized with a `kind` tag so the app can show server errors (with their
/// SQLSTATE code and position) differently from connection problems.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Error {
    /// The server rejected a statement or a login, e.g. a syntax error or a wrong password.
    #[error("{message}")]
    Database {
        message: String,
        /// SQLSTATE code, e.g. `42601` for a syntax error.
        code: String,
        detail: Option<String>,
        hint: Option<String>,
        /// 1-based character position of the error in the submitted SQL.
        position: Option<u32>,
    },
    /// Connecting to or communicating with the server failed.
    #[error("{message}")]
    Connection { message: String },
    /// The session is closed.
    #[error("connection is closed")]
    Closed,
    /// Reading or writing saved connections or their passwords failed.
    #[error("{message}")]
    Storage { message: String },
    /// A safety rule stopped the action, such as applying a plan to a database
    /// that isn't local. The reason lets the app word it in the user's language.
    #[error("{message}")]
    Refused { reason: Refusal, message: String },
    /// Asking the AI failed. The reason lets the app word it in the user's language.
    #[error("{message}")]
    Ai { reason: AiFailure, message: String },
}

/// Why asking the AI failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AiFailure {
    /// No API key is saved, and `ANTHROPIC_API_KEY` isn't set.
    NoApiKey,
    /// The API didn't accept the key.
    BadApiKey,
    /// Too many requests for the account's rate limits.
    RateLimited,
    /// The API is overloaded or failing.
    Unavailable,
    /// The API's safety checks declined the request.
    Declined,
    /// The API couldn't be reached, or the connection broke.
    Network,
    /// The user stopped the answer.
    Cancelled,
    /// Anything else the API rejected; the message says what.
    Other,
}

/// Why an action was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Refusal {
    /// The connection doesn't allow snapshot plans to change its database.
    NotAllowed,
    /// The database or snapshot changed since the plan was reviewed.
    PlanChanged,
    /// The plan has changes Dolmen can't make.
    PlanBlocked,
    /// The plan drops data and that wasn't allowed.
    DestructiveNotAllowed,
    /// The snapshot has no schema model, so it can't be applied.
    NoModel,
    /// A snapshot folder path inside a repository isn't a plain relative path.
    BadSnapshotPath,
    /// Switching branches would carry the snapshot folder's uncommitted changes along.
    UncommittedChanges,
}

impl From<keyring_core::Error> for Error {
    fn from(err: keyring_core::Error) -> Self {
        Error::Storage {
            message: format!("credential store: {err}"),
        }
    }
}

impl From<tokio_postgres::Error> for Error {
    fn from(err: tokio_postgres::Error) -> Self {
        if let Some(db) = err.as_db_error() {
            return Error::Database {
                message: db.message().to_owned(),
                code: db.code().code().to_owned(),
                detail: db.detail().map(str::to_owned),
                hint: db.hint().map(str::to_owned),
                position: match db.position() {
                    Some(tokio_postgres::error::ErrorPosition::Original(position)) => {
                        Some(*position)
                    }
                    _ => None,
                },
            };
        }
        if err.is_closed() {
            return Error::Closed;
        }
        Error::Connection {
            message: error_chain(&err),
        }
    }
}

/// Joins an error with its sources, e.g. "error connecting to server: Connection refused".
///
/// tokio-postgres errors only describe their kind, so the cause is needed to be useful.
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}
