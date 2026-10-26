//! PostgreSQL sessions.
//!
//! Each [`Session`] owns one dedicated connection, so session state such as
//! `SET`, temporary tables and open transactions carries over between executions.

use std::fmt;
use std::pin::pin;
use std::time::Duration;

use futures_util::TryStreamExt;
use postgres_native_tls::MakeTlsConnector;
use serde::{Deserialize, Serialize};
use tokio_postgres::{Client, SimpleQueryMessage};

use crate::Error;
use crate::schema::Database;

mod introspect;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How to connect to a PostgreSQL server.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: Option<String>,
    pub database: String,
    pub ssl_mode: SslMode,
}

// Written by hand so the password never ends up in logs.
impl fmt::Debug for ConnectConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("database", &self.database)
            .field("ssl_mode", &self.ssl_mode)
            .finish()
    }
}

/// TLS behaviour, named after libpq's `sslmode` values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    /// Never use TLS.
    Disable,
    /// Use TLS if the server supports it, without verifying its certificate.
    #[default]
    Prefer,
    /// Always use TLS, without verifying the server certificate.
    Require,
    /// Always use TLS and verify the certificate chain and host name.
    VerifyFull,
}

/// The outcome of one statement in an execution.
#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum StatementResult {
    /// A statement that returned rows, such as `SELECT` or `INSERT ... RETURNING`.
    Rows {
        columns: Vec<String>,
        /// Values in PostgreSQL's text format; `None` is SQL `NULL`.
        rows: Vec<Vec<Option<String>>>,
        /// Number of rows the statement returned, which can exceed `rows.len()`.
        total_rows: u64,
    },
    /// A statement that returned no rows, such as `UPDATE` or `CREATE TABLE`.
    Command { rows_affected: u64 },
}

/// A connection to a PostgreSQL server.
pub struct Session {
    client: Client,
    tls: MakeTlsConnector,
    server_version: String,
}

impl Session {
    pub async fn connect(config: &ConnectConfig) -> Result<Self, Error> {
        let mut pg = tokio_postgres::Config::new();
        pg.host(&config.host)
            .port(config.port)
            .user(&config.user)
            .dbname(&config.database)
            .application_name("Dolmen")
            .connect_timeout(CONNECT_TIMEOUT)
            .ssl_mode(match config.ssl_mode {
                SslMode::Disable => tokio_postgres::config::SslMode::Disable,
                SslMode::Prefer => tokio_postgres::config::SslMode::Prefer,
                SslMode::Require | SslMode::VerifyFull => tokio_postgres::config::SslMode::Require,
            });
        if let Some(password) = &config.password {
            pg.password(password);
        }

        let tls = tls_connector(config.ssl_mode)?;
        let (client, connection) = pg.connect(tls.clone()).await?;
        // The connection drives the socket. When it fails, the next call on the
        // client returns the error, so there is nothing to handle here.
        tokio::spawn(connection);

        let server_version = client
            .simple_query("SHOW server_version")
            .await?
            .into_iter()
            .find_map(|message| match message {
                SimpleQueryMessage::Row(row) => row.get(0).map(str::to_owned),
                _ => None,
            })
            .unwrap_or_default();

        Ok(Self {
            client,
            tls,
            server_version,
        })
    }

    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    /// Runs one or more `;`-separated statements and returns a result per statement.
    ///
    /// Statements are sent in a single simple query, so PostgreSQL runs them in one
    /// implicit transaction unless they contain their own `BEGIN`/`COMMIT`.
    /// At most `max_rows` rows are kept per result; the rest are only counted.
    pub async fn execute(&self, sql: &str, max_rows: usize) -> Result<Vec<StatementResult>, Error> {
        let mut stream = pin!(self.client.simple_query_raw(sql).await?);
        let mut results = Vec::new();
        let mut current: Option<RowsBuilder> = None;

        while let Some(message) = stream.try_next().await? {
            match message {
                SimpleQueryMessage::RowDescription(columns) => {
                    current = Some(RowsBuilder {
                        columns: columns.iter().map(|c| c.name().to_owned()).collect(),
                        rows: Vec::new(),
                        total_rows: 0,
                    });
                }
                SimpleQueryMessage::Row(row) => {
                    if let Some(builder) = &mut current {
                        builder.total_rows += 1;
                        if builder.rows.len() < max_rows {
                            builder.rows.push(
                                (0..row.len())
                                    .map(|i| row.get(i).map(str::to_owned))
                                    .collect(),
                            );
                        }
                    }
                }
                // The count in the command tag is not used for row results: tags
                // like `SHOW` carry no count.
                SimpleQueryMessage::CommandComplete(rows_affected) => {
                    results.push(match current.take() {
                        Some(builder) => StatementResult::Rows {
                            columns: builder.columns,
                            rows: builder.rows,
                            total_rows: builder.total_rows,
                        },
                        None => StatementResult::Command { rows_affected },
                    });
                }
                _ => {}
            }
        }
        Ok(results)
    }

    /// Reads the structure of the database: schemas, tables, views, functions and types.
    pub async fn introspect(&self) -> Result<Database, Error> {
        introspect::introspect(&self.client).await
    }

    /// The schema unqualified names resolve to first, usually `public`.
    pub async fn current_schema(&self) -> Result<Option<String>, Error> {
        Ok(self
            .client
            .query_one("SELECT current_schema()", &[])
            .await?
            .get(0))
    }

    /// Asks the server to cancel the statement this session is running, if any.
    pub async fn cancel(&self) -> Result<(), Error> {
        self.client
            .cancel_token()
            .cancel_query(self.tls.clone())
            .await?;
        Ok(())
    }
}

struct RowsBuilder {
    columns: Vec<String>,
    rows: Vec<Vec<Option<String>>>,
    total_rows: u64,
}

/// Like libpq, `prefer` and `require` encrypt without verifying the server
/// certificate; only `verify-full` checks the chain and host name.
fn tls_connector(ssl_mode: SslMode) -> Result<MakeTlsConnector, Error> {
    let verify = ssl_mode == SslMode::VerifyFull;
    let connector = native_tls::TlsConnector::builder()
        .danger_accept_invalid_certs(!verify)
        .danger_accept_invalid_hostnames(!verify)
        .build()
        .map_err(|err| Error::Connection {
            message: err.to_string(),
        })?;
    Ok(MakeTlsConnector::new(connector))
}
