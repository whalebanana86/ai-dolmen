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

pub mod ddl;
mod introspect;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const INTROSPECTION_ATTEMPTS: u64 = 5;

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

impl ConnectConfig {
    /// Reads a libpq connection string, as a URL (`postgres://user:pass@host:5432/db?sslmode=require`)
    /// or as `key=value` pairs (`host=db user=app dbname=app`). Like libpq, the user
    /// defaults to the OS user, the database to the user, and the password to `PGPASSWORD`.
    pub fn parse(conninfo: &str) -> Result<Self, Error> {
        let invalid = |message: String| Error::Connection {
            message: format!("invalid connection string: {message}"),
        };
        // tokio-postgres only knows disable, prefer and require, so sslmode is read here.
        let (conninfo, ssl_mode) = take_ssl_mode(conninfo).map_err(invalid)?;
        let config: tokio_postgres::Config =
            conninfo.parse().map_err(|err| invalid(format!("{err}")))?;

        let host = match config.get_hosts().first() {
            Some(tokio_postgres::config::Host::Tcp(host)) => host.clone(),
            #[cfg(unix)]
            Some(tokio_postgres::config::Host::Unix(path)) => path.to_string_lossy().into_owned(),
            None => "localhost".to_owned(),
        };
        let user = match config.get_user() {
            Some(user) => user.to_owned(),
            None => std::env::var("USER")
                .or_else(|_| std::env::var("USERNAME"))
                .map_err(|_| invalid("no user given".into()))?,
        };
        let password = match config.get_password() {
            Some(password) => Some(
                String::from_utf8(password.to_vec())
                    .map_err(|_| invalid("the password isn't UTF-8".into()))?,
            ),
            None => std::env::var("PGPASSWORD").ok(),
        };
        Ok(Self {
            host,
            port: config.get_ports().first().copied().unwrap_or(5432),
            database: config.get_dbname().unwrap_or(&user).to_owned(),
            user,
            password,
            ssl_mode,
        })
    }
}

/// Removes `sslmode` from a connection string and returns its mode (`prefer` when absent).
fn take_ssl_mode(conninfo: &str) -> Result<(String, SslMode), String> {
    let mut ssl_mode = SslMode::Prefer;
    let mut read = |value: &str| -> Result<(), String> {
        ssl_mode = match value {
            "disable" => SslMode::Disable,
            "allow" | "prefer" => SslMode::Prefer,
            "require" => SslMode::Require,
            "verify-full" => SslMode::VerifyFull,
            "verify-ca" => {
                return Err("sslmode=verify-ca isn't supported; use verify-full or require".into());
            }
            other => return Err(format!("unknown sslmode {other:?}")),
        };
        Ok(())
    };

    let is_url = conninfo.starts_with("postgres://") || conninfo.starts_with("postgresql://");
    let rest = if is_url {
        let (base, query) = conninfo.split_once('?').unwrap_or((conninfo, ""));
        let mut kept = Vec::new();
        for param in query.split('&').filter(|p| !p.is_empty()) {
            match param.strip_prefix("sslmode=") {
                Some(value) => read(value)?,
                None => kept.push(param),
            }
        }
        if kept.is_empty() {
            base.to_owned()
        } else {
            format!("{base}?{}", kept.join("&"))
        }
    } else {
        let mut kept = Vec::new();
        for pair in conninfo.split_whitespace() {
            match pair.strip_prefix("sslmode=") {
                Some(value) => read(value)?,
                None => kept.push(pair),
            }
        }
        kept.join(" ")
    };
    Ok((rest, ssl_mode))
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
    config: ConnectConfig,
    /// A second connection for reading the catalogs, opened on first use. Its
    /// `search_path` is only `pg_catalog`, as with pg_dump, so definitions name
    /// every schema whatever the user's session has set, and introspection never
    /// touches the user's transaction.
    metadata: tokio::sync::Mutex<Option<Client>>,
}

impl Session {
    pub async fn connect(config: &ConnectConfig) -> Result<Self, Error> {
        let (client, tls) = open(config, None).await?;
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
            config: config.clone(),
            metadata: tokio::sync::Mutex::new(None),
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
        let mut metadata = self.metadata.lock().await;
        if metadata.as_ref().is_none_or(Client::is_closed) {
            *metadata = Some(
                open(&self.config, Some("-c search_path=pg_catalog"))
                    .await?
                    .0,
            );
        }
        let client = metadata.as_mut().expect("opened above");

        // One read-only snapshot makes all catalog queries agree. DDL running at
        // the same time can still drop an object between reading the catalogs and
        // asking for its definition ("could not open relation", internal error
        // XX000), so try again a few times, waiting a little longer each time.
        let mut attempt = 1;
        loop {
            let transaction = client
                .build_transaction()
                .isolation_level(tokio_postgres::IsolationLevel::RepeatableRead)
                .read_only(true)
                .start()
                .await?;
            match introspect::introspect(&transaction).await {
                Err(Error::Database { code, .. })
                    if code == "XX000" && attempt < INTROSPECTION_ATTEMPTS =>
                {
                    drop(transaction);
                    tokio::time::sleep(Duration::from_millis(50 * attempt)).await;
                    attempt += 1;
                }
                Err(Error::Database {
                    message,
                    code,
                    detail,
                    hint,
                    position,
                }) if code == "XX000" => {
                    return Err(Error::Database {
                        message: format!(
                            "The schema kept changing while it was read; try again. ({message})"
                        ),
                        code,
                        detail,
                        hint,
                        position,
                    });
                }
                result => return result,
            }
        }
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

/// Opens a connection, with extra server `options` such as `-c name=value`.
async fn open(
    config: &ConnectConfig,
    options: Option<&str>,
) -> Result<(Client, MakeTlsConnector), Error> {
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
    if let Some(options) = options {
        pg.options(options);
    }

    let tls = tls_connector(config.ssl_mode)?;
    let (client, connection) = pg.connect(tls.clone()).await?;
    // The connection drives the socket. When it fails, the next call on the
    // client returns the error, so there is nothing to handle here.
    tokio::spawn(connection);
    Ok((client, tls))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls_with_libpq_ssl_modes() {
        let config =
            ConnectConfig::parse("postgres://app:s%40cret@db.example.com:6543/shop?sslmode=verify-full&connect_timeout=5")
                .unwrap();
        assert_eq!(config.host, "db.example.com");
        assert_eq!(config.port, 6543);
        assert_eq!(config.user, "app");
        assert_eq!(config.password.as_deref(), Some("s@cret"));
        assert_eq!(config.database, "shop");
        assert_eq!(config.ssl_mode, SslMode::VerifyFull);
    }

    #[test]
    fn parses_key_value_strings() {
        let config = ConnectConfig::parse(
            "host=localhost port=55432 user=postgres password=pw sslmode=disable",
        )
        .unwrap();
        assert_eq!((config.host.as_str(), config.port), ("localhost", 55432));
        // The database defaults to the user, as in libpq.
        assert_eq!(config.database, "postgres");
        assert_eq!(config.ssl_mode, SslMode::Disable);
    }

    #[test]
    fn rejects_unsupported_ssl_modes() {
        for conninfo in [
            "postgres://u@h/d?sslmode=verify-ca",
            "host=h user=u sslmode=sometimes",
        ] {
            assert!(
                matches!(
                    ConnectConfig::parse(conninfo),
                    Err(Error::Connection { .. })
                ),
                "{conninfo}"
            );
        }
    }
}
