//! MySQL and MariaDB sessions. Both speak the same protocol, so one module
//! serves them; where their catalogs differ, the session knows which it has.

mod introspect;

use std::fmt::Write as _;
use std::time::Duration;

use mysql_async::prelude::Queryable;
use mysql_async::{Conn, OptsBuilder, SslOpts};

use crate::Error;
use crate::postgres::{ConnectConfig, SslMode, StatementResult};
use crate::schema::{Database, ForeignKey};

/// Which server answered: their catalogs differ in places.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    MySql,
    MariaDb,
}

/// A connection to a MySQL or MariaDB server.
pub struct Session {
    conn: tokio::sync::Mutex<Conn>,
    /// The server's id for the connection, which `KILL QUERY` takes.
    connection_id: u32,
    server_version: String,
    flavor: Flavor,
    config: ConnectConfig,
    /// A second connection for reading the catalogs, opened on first use, so
    /// introspection never waits for, or lands inside, the user's work.
    metadata: tokio::sync::Mutex<Option<Conn>>,
    /// The default database as last read, for when the session is busy.
    default_schema: std::sync::Mutex<Option<String>>,
}

impl Session {
    pub async fn connect(config: &ConnectConfig) -> Result<Self, Error> {
        let mut conn = open(config).await?;
        let version: Option<String> = conn.query_first("SELECT VERSION()").await?;
        let server_version = version.unwrap_or_default();
        let flavor = if server_version.to_ascii_lowercase().contains("mariadb") {
            Flavor::MariaDb
        } else {
            Flavor::MySql
        };
        Ok(Self {
            connection_id: conn.id(),
            conn: tokio::sync::Mutex::new(conn),
            server_version,
            flavor,
            config: config.clone(),
            metadata: tokio::sync::Mutex::new(None),
            default_schema: std::sync::Mutex::new(
                Some(config.database.clone()).filter(|d| !d.is_empty()),
            ),
        })
    }

    pub fn server_version(&self) -> &str {
        &self.server_version
    }

    pub fn flavor(&self) -> Flavor {
        self.flavor
    }

    pub fn config(&self) -> &ConnectConfig {
        &self.config
    }

    /// Another session to the same server and database, for running
    /// statements apart from this one.
    pub async fn another(&self) -> Result<Session, Error> {
        Session::connect(&self.config).await
    }

    /// Runs one or more `;`-separated statements and returns a result per
    /// statement. At most `max_rows` rows are kept per result; the rest are
    /// only counted. Values come in the server's text format.
    pub async fn execute(&self, sql: &str, max_rows: usize) -> Result<Vec<StatementResult>, Error> {
        let mut conn = self.conn.lock().await;
        let mut result = conn.query_iter(sql).await?;
        let mut results = Vec::new();
        loop {
            let columns = result.columns().filter(|columns| !columns.is_empty());
            match columns {
                Some(columns) => {
                    let binary: Vec<bool> = columns.iter().map(is_binary).collect();
                    let mut rows = Vec::new();
                    let mut total_rows = 0;
                    result
                        .for_each(|row| {
                            total_rows += 1;
                            if rows.len() < max_rows {
                                rows.push(
                                    (0..row.len())
                                        .map(|i| row.as_ref(i).and_then(|v| text(v, binary[i])))
                                        .collect(),
                                );
                            }
                        })
                        .await?;
                    results.push(StatementResult::Rows {
                        columns: columns.iter().map(|c| c.name_str().into_owned()).collect(),
                        rows,
                        total_rows,
                        source: None,
                    });
                }
                None => {
                    results.push(StatementResult::Command {
                        rows_affected: result.affected_rows(),
                    });
                    // Moves on to the next statement's result.
                    result.for_each(|_| {}).await?;
                }
            }
            if result.is_empty() {
                break;
            }
        }
        Ok(results)
    }

    /// Makes the session's transactions read-only, or lets them write again.
    /// A guard against mistakes rather than a permission: SQL can turn it off.
    pub async fn set_read_only(&self, read_only: bool) -> Result<(), Error> {
        let mode = if read_only { "READ ONLY" } else { "READ WRITE" };
        self.conn
            .lock()
            .await
            .query_drop(format!("SET SESSION TRANSACTION {mode}"))
            .await?;
        Ok(())
    }

    /// Makes `schema` the default database, and returns the default database.
    pub async fn set_schema(&self, schema: &str) -> Result<Option<String>, Error> {
        self.conn
            .lock()
            .await
            .query_drop(format!("USE {}", quote_ident(schema)))
            .await?;
        self.current_schema().await
    }

    /// The default database. While the session is busy with a long query,
    /// it's the database last read rather than a wait.
    pub async fn current_schema(&self) -> Result<Option<String>, Error> {
        let read = async {
            let mut conn = self.conn.lock().await;
            conn.query_first::<Option<String>, _>("SELECT DATABASE()")
                .await
        };
        if let Ok(Ok(found)) = tokio::time::timeout(Duration::from_millis(500), read).await {
            *self.default_schema.lock().unwrap() = found.flatten();
        }
        Ok(self.default_schema.lock().unwrap().clone())
    }

    /// Asks the server to stop the statement this session is running, if any.
    pub async fn cancel(&self) -> Result<(), Error> {
        let mut killer = open(&self.config).await?;
        let killed = killer
            .query_drop(format!("KILL QUERY {}", self.connection_id))
            .await;
        killer.disconnect().await.ok();
        killed?;
        Ok(())
    }

    /// The databases' structure, in the model PostgreSQL's is read into:
    /// databases are its schemas.
    pub async fn introspect(&self) -> Result<Database, Error> {
        let mut metadata = self.metadata().await?;
        introspect::introspect(metadata.as_mut().expect("opened"), self.flavor).await
    }

    /// Every foreign key between the databases' tables, for showing relationships.
    pub async fn foreign_keys(&self) -> Result<Vec<ForeignKey>, Error> {
        let mut metadata = self.metadata().await?;
        introspect::foreign_keys(metadata.as_mut().expect("opened")).await
    }

    /// The server's own `CREATE` statement for a table or view.
    pub async fn create_statement(
        &self,
        schema: &str,
        name: &str,
        view: bool,
    ) -> Result<String, Error> {
        let mut metadata = self.metadata().await?;
        let object = if view { "VIEW" } else { "TABLE" };
        let row: Option<mysql_async::Row> = metadata
            .as_mut()
            .expect("opened")
            .query_first(format!("SHOW CREATE {object} {}", qualified(schema, name)))
            .await?;
        let statement = row
            .and_then(|row| row.get_opt::<String, _>(1))
            .and_then(Result::ok)
            .unwrap_or_default();
        Ok(format!("{statement};\n"))
    }

    async fn metadata(&self) -> Result<tokio::sync::MutexGuard<'_, Option<Conn>>, Error> {
        let mut metadata = self.metadata.lock().await;
        let alive = match metadata.as_mut() {
            Some(conn) => conn.ping().await.is_ok(),
            None => false,
        };
        if !alive {
            *metadata = Some(open(&self.config).await?);
        }
        Ok(metadata)
    }
}

/// Opens a connection. MySQL has no "TLS if offered" mode of its own, so
/// `Prefer` tries TLS without checking the certificate and falls back to a
/// plain connection when the server has no TLS.
async fn open(config: &ConnectConfig) -> Result<Conn, Error> {
    let options = |tls: Option<SslOpts>| {
        OptsBuilder::default()
            .ip_or_hostname(config.host.clone())
            .tcp_port(config.port)
            .user(Some(config.user.clone()).filter(|u| !u.is_empty()))
            .pass(config.password.clone())
            .db_name(Some(config.database.clone()).filter(|d| !d.is_empty()))
            .prefer_socket(false)
            .stmt_cache_size(0)
            .ssl_opts(tls)
    };
    let unverified = || {
        SslOpts::default()
            .with_danger_accept_invalid_certs(true)
            .with_danger_skip_domain_validation(true)
    };
    let connect = |opts: OptsBuilder| async move {
        tokio::time::timeout(Duration::from_secs(15), Conn::new(opts))
            .await
            .map_err(|_| Error::Connection {
                message: format!("timed out connecting to {}:{}", config.host, config.port),
            })?
            .map_err(Error::from)
    };
    match config.ssl_mode {
        SslMode::Disable => connect(options(None)).await,
        SslMode::Require => connect(options(Some(unverified()))).await,
        SslMode::VerifyFull => connect(options(Some(SslOpts::default()))).await,
        SslMode::Prefer => match connect(options(Some(unverified()))).await {
            Err(Error::Connection { message })
                if message.contains("TLS") || message.contains("SSL") =>
            {
                connect(options(None)).await
            }
            other => other,
        },
    }
}

impl From<mysql_async::Error> for Error {
    fn from(err: mysql_async::Error) -> Self {
        match err {
            mysql_async::Error::Server(err) => Error::Database {
                message: err.message,
                // The SQLSTATE, as PostgreSQL's errors carry; the server's own
                // number is more familiar to MySQL users, so it leads.
                code: format!("{} ({})", err.code, err.state),
                detail: None,
                hint: None,
                position: None,
            },
            other => {
                // Driver errors often repeat their cause in their own text.
                let mut message = other.to_string();
                let mut source = std::error::Error::source(&other);
                while let Some(cause) = source {
                    let cause_text = cause.to_string();
                    if !message.contains(&cause_text) {
                        let _ = write!(message, ": {cause_text}");
                    }
                    source = cause.source();
                }
                Error::Connection { message }
            }
        }
    }
}

/// Whether a column holds bytes rather than text: a string or blob type in
/// the `binary` character set (63).
fn is_binary(column: &mysql_async::Column) -> bool {
    use mysql_async::consts::ColumnType::*;
    column.character_set() == 63
        && matches!(
            column.column_type(),
            MYSQL_TYPE_STRING
                | MYSQL_TYPE_VAR_STRING
                | MYSQL_TYPE_VARCHAR
                | MYSQL_TYPE_BLOB
                | MYSQL_TYPE_TINY_BLOB
                | MYSQL_TYPE_MEDIUM_BLOB
                | MYSQL_TYPE_LONG_BLOB
                | MYSQL_TYPE_GEOMETRY
        )
}

/// A value as text, the way the grid shows PostgreSQL's: bytes as `\x` hex.
fn text(value: &mysql_async::Value, binary: bool) -> Option<String> {
    use mysql_async::Value;
    Some(match value {
        Value::NULL => return None,
        Value::Bytes(bytes) if binary => hex(bytes),
        Value::Bytes(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => hex(bytes),
        },
        Value::Int(n) => n.to_string(),
        Value::UInt(n) => n.to_string(),
        Value::Float(n) => n.to_string(),
        Value::Double(n) => n.to_string(),
        Value::Date(year, month, day, hour, minute, second, micros) => {
            let mut text =
                format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}");
            if *micros > 0 {
                let _ = write!(text, ".{micros:06}");
            }
            text
        }
        Value::Time(negative, days, hours, minutes, seconds, micros) => {
            let hours = u32::from(*hours) + days * 24;
            let sign = if *negative { "-" } else { "" };
            let mut text = format!("{sign}{hours:02}:{minutes:02}:{seconds:02}");
            if *micros > 0 {
                let _ = write!(text, ".{micros:06}");
            }
            text
        }
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(2 + bytes.len() * 2);
    text.push_str("\\x");
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// A name in backticks, as MySQL quotes identifiers.
pub fn quote_ident(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

/// `schema`.`name`.
pub fn qualified(schema: &str, name: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(name))
}

/// A string literal; backslashes are escapes in MySQL's default mode.
pub(crate) fn quote_literal(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_names_and_strings() {
        assert_eq!(quote_ident("order"), "`order`");
        assert_eq!(quote_ident("a`b"), "`a``b`");
        assert_eq!(qualified("shop", "orders"), "`shop`.`orders`");
        assert_eq!(quote_literal("it's a \\ test"), "'it''s a \\\\ test'");
    }

    #[test]
    fn shows_values_as_text() {
        use mysql_async::Value;
        assert_eq!(text(&Value::NULL, false), None);
        assert_eq!(text(&Value::Bytes(b"abc".to_vec()), false).unwrap(), "abc");
        assert_eq!(text(&Value::Bytes(vec![0, 255]), true).unwrap(), "\\x00ff");
        assert_eq!(text(&Value::Bytes(vec![0xc3]), false).unwrap(), "\\xc3");
        assert_eq!(
            text(&Value::Date(2026, 10, 29, 9, 5, 0, 120), false).unwrap(),
            "2026-10-29 09:05:00.000120"
        );
        assert_eq!(
            text(&Value::Time(true, 1, 2, 3, 4, 0), false).unwrap(),
            "-26:03:04"
        );
    }
}
