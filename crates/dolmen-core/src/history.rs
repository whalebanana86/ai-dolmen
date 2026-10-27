//! Query history: the statements run against each connection, kept in a local
//! SQLite file, searchable, with secrets masked and old entries pruned.
//!
//! Only statements and their outcome are kept, never result rows. The same
//! file keeps each connection's editor text, so it comes back on reconnect.

use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regex::Regex;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::Error;

/// Entries older than this go, unless starred.
pub const KEEP_DAYS: u64 = 90;
/// Each connection keeps at most this many entries, besides starred ones.
pub const KEEP_PER_CONNECTION: u32 = 10_000;

/// One statement run, or one reviewed plan applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: i64,
    /// The saved connection, if it was one.
    pub connection_id: Option<String>,
    /// The connection's name, or user@host:port/database.
    pub connection: String,
    pub sql: String,
    /// Milliseconds since the Unix epoch.
    pub started_at: i64,
    pub duration_ms: i64,
    pub status: Status,
    /// Rows returned or affected.
    pub rows: Option<i64>,
    pub error: Option<String>,
    pub source: Source,
    pub starred: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Ok,
    Error,
    Cancelled,
}

/// Where the statement came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    /// The SQL editor, including statements the schema tree put there.
    Editor,
    /// An edit or a new object from the detail panel.
    Detail,
    /// A snapshot applied to the database.
    Snapshot,
}

/// A run to record; the store adds the id and masks secrets.
#[derive(Debug, Clone)]
pub struct NewRun {
    pub connection_id: Option<String>,
    pub connection: String,
    pub sql: String,
    pub started_at: SystemTime,
    pub duration: Duration,
    pub status: Status,
    pub rows: Option<i64>,
    pub error: Option<String>,
    pub source: Source,
}

/// Which runs to list, newest first.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Filter {
    /// Only this saved connection's runs; all connections when unset.
    pub connection_id: Option<String>,
    /// Only runs of the connection with this label, for one that isn't saved.
    pub connection: Option<String>,
    /// Runs whose SQL contains this text, ignoring case.
    pub text: Option<String>,
    pub status: Option<Status>,
    pub starred_only: bool,
    /// Runs older than this id, to page through the list.
    pub before: Option<i64>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Mask passwords and other secrets in recorded SQL.
    pub redact: bool,
}

pub struct History {
    db: Mutex<Connection>,
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS runs (
        id INTEGER PRIMARY KEY,
        connection_id TEXT,
        connection TEXT NOT NULL,
        sql TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        duration_ms INTEGER NOT NULL,
        status TEXT NOT NULL,
        rows INTEGER,
        error TEXT,
        source TEXT NOT NULL,
        starred INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS runs_by_connection ON runs (connection_id, id DESC);
    -- Substring search: trigrams match any three or more characters.
    CREATE VIRTUAL TABLE IF NOT EXISTS runs_text
        USING fts5 (sql, content = 'runs', content_rowid = 'id', tokenize = 'trigram');
    CREATE TRIGGER IF NOT EXISTS runs_text_insert AFTER INSERT ON runs BEGIN
        INSERT INTO runs_text (rowid, sql) VALUES (new.id, new.sql);
    END;
    CREATE TRIGGER IF NOT EXISTS runs_text_delete AFTER DELETE ON runs BEGIN
        INSERT INTO runs_text (runs_text, rowid, sql) VALUES ('delete', old.id, old.sql);
    END;
    CREATE TABLE IF NOT EXISTS editors (
        key TEXT PRIMARY KEY,
        content TEXT NOT NULL,
        updated_at INTEGER NOT NULL
    );
    CREATE TABLE IF NOT EXISTS settings (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
";

impl History {
    /// Opens the history file at `path`, creating it readable by the user only.
    pub fn open(path: &Path) -> Result<Self, Error> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| storage(path, err))?;
        }
        let db = Connection::open(path).map_err(|err| storage(path, err))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|err| storage(path, err))?;
        }
        Self::with(db)
    }

    /// A history kept in memory, for tests.
    pub fn in_memory() -> Result<Self, Error> {
        Self::with(Connection::open_in_memory().map_err(db_error)?)
    }

    fn with(db: Connection) -> Result<Self, Error> {
        db.execute_batch(SCHEMA).map_err(db_error)?;
        let history = Self { db: Mutex::new(db) };
        history.prune(SystemTime::now())?;
        Ok(history)
    }

    pub fn record(&self, run: NewRun) -> Result<i64, Error> {
        let sql = if self.settings()?.redact {
            redact(&run.sql)
        } else {
            run.sql
        };
        let db = self.db.lock().unwrap();
        db.execute(
            "INSERT INTO runs (connection_id, connection, sql, started_at, duration_ms, status, rows, error, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run.connection_id,
                run.connection,
                sql,
                millis(run.started_at),
                run.duration.as_millis() as i64,
                to_text(&run.status),
                run.rows,
                run.error,
                to_text(&run.source),
            ],
        )
        .map_err(db_error)?;
        let id = db.last_insert_rowid();
        // Pruning costs a scan, so do it now and then rather than on every run.
        if id % 100 == 0 {
            drop(db);
            self.prune(SystemTime::now())?;
        }
        Ok(id)
    }

    pub fn list(&self, filter: &Filter) -> Result<Vec<Run>, Error> {
        let mut sql = String::from(
            "SELECT runs.id, connection_id, connection, runs.sql, started_at, duration_ms, status, rows, error, source, starred
             FROM runs",
        );
        let mut conditions = Vec::new();
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        let text = filter
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());
        match text {
            // Trigrams need three characters; shorter text is matched directly.
            Some(text) if text.chars().count() >= 3 => {
                sql.push_str(" JOIN runs_text ON runs_text.rowid = runs.id");
                conditions.push("runs_text MATCH ?".to_owned());
                values.push(format!("\"{}\"", text.replace('"', "\"\"")).into());
            }
            Some(text) => {
                conditions.push("runs.sql LIKE ? ESCAPE '\\'".to_owned());
                let escaped = text
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                values.push(format!("%{escaped}%").into());
            }
            None => {}
        }
        if let Some(id) = &filter.connection_id {
            conditions.push("connection_id = ?".to_owned());
            values.push(id.clone().into());
        }
        if let Some(label) = &filter.connection {
            conditions.push("connection_id IS NULL AND connection = ?".to_owned());
            values.push(label.clone().into());
        }
        if let Some(status) = &filter.status {
            conditions.push("status = ?".to_owned());
            values.push(to_text(status).into());
        }
        if filter.starred_only {
            conditions.push("starred = 1".to_owned());
        }
        if let Some(before) = filter.before {
            conditions.push("runs.id < ?".to_owned());
            values.push(before.into());
        }
        if !conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conditions.join(" AND "));
        }
        sql.push_str(" ORDER BY runs.id DESC LIMIT ?");
        values.push(i64::from(filter.limit.unwrap_or(100).min(500)).into());

        let db = self.db.lock().unwrap();
        let mut statement = db.prepare(&sql).map_err(db_error)?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(values), |row| {
                Ok(Run {
                    id: row.get(0)?,
                    connection_id: row.get(1)?,
                    connection: row.get(2)?,
                    sql: row.get(3)?,
                    started_at: row.get(4)?,
                    duration_ms: row.get(5)?,
                    status: from_text(&row.get::<_, String>(6)?).unwrap_or(Status::Ok),
                    rows: row.get(7)?,
                    error: row.get(8)?,
                    source: from_text(&row.get::<_, String>(9)?).unwrap_or(Source::Editor),
                    starred: row.get(10)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<_, _>>().map_err(db_error)
    }

    pub fn star(&self, id: i64, starred: bool) -> Result<(), Error> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "UPDATE runs SET starred = ?2 WHERE id = ?1",
                params![id, starred],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn delete(&self, ids: &[i64]) -> Result<(), Error> {
        let db = self.db.lock().unwrap();
        for id in ids {
            db.execute("DELETE FROM runs WHERE id = ?1", [id])
                .map_err(db_error)?;
        }
        Ok(())
    }

    /// Deletes the runs that aren't starred, of one saved connection or of all.
    pub fn clear(&self, connection_id: Option<&str>) -> Result<(), Error> {
        let db = self.db.lock().unwrap();
        match connection_id {
            Some(id) => db.execute(
                "DELETE FROM runs WHERE starred = 0 AND connection_id = ?1",
                [id],
            ),
            None => db.execute("DELETE FROM runs WHERE starred = 0", []),
        }
        .map_err(db_error)?;
        Ok(())
    }

    /// Removes unstarred runs older than [`KEEP_DAYS`], and past the newest
    /// [`KEEP_PER_CONNECTION`] of each connection.
    pub fn prune(&self, now: SystemTime) -> Result<(), Error> {
        let cutoff = millis(now) - (KEEP_DAYS * 24 * 60 * 60 * 1000) as i64;
        self.db
            .lock()
            .unwrap()
            .execute(
                "DELETE FROM runs WHERE starred = 0 AND (started_at < ?1 OR id IN (
                     SELECT id FROM (
                         SELECT id, row_number() OVER (
                             PARTITION BY coalesce(connection_id, connection) ORDER BY id DESC
                         ) AS n
                         FROM runs WHERE starred = 0
                     ) WHERE n > ?2
                 ))",
                params![cutoff, KEEP_PER_CONNECTION],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn settings(&self) -> Result<Settings, Error> {
        let redact: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT value FROM settings WHERE key = 'redact'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?;
        Ok(Settings {
            redact: redact.as_deref() != Some("false"),
        })
    }

    pub fn set_settings(&self, settings: &Settings) -> Result<(), Error> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO settings (key, value) VALUES ('redact', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                [settings.redact.to_string()],
            )
            .map_err(db_error)?;
        Ok(())
    }

    /// The editor text last saved under `key`, one per connection.
    pub fn editor(&self, key: &str) -> Result<Option<String>, Error> {
        self.db
            .lock()
            .unwrap()
            .query_row("SELECT content FROM editors WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(db_error)
    }

    pub fn save_editor(&self, key: &str, content: &str) -> Result<(), Error> {
        self.db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO editors (key, content, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (key) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
                params![key, content, millis(SystemTime::now())],
            )
            .map_err(db_error)?;
        Ok(())
    }
}

/// Masks secrets in SQL: the literal after PASSWORD (roles, user mappings,
/// foreign servers), and inside any literal, `password=...` in connection
/// strings and the password in `scheme://user:password@host` URLs.
pub fn redact(sql: &str) -> String {
    static AFTER_PASSWORD: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(password\s+)(?:E?'(?:[^']|'')*'|\$(\w*)\$[\s\S]*?\$\w*\$)").unwrap()
    });
    static LITERAL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"'(?:[^']|'')*'").unwrap());
    static IN_LITERAL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(\bpassword\s*=\s*)[^\s']+|(://[^:/@\s']+:)[^@\s']+@").unwrap()
    });

    let masked = AFTER_PASSWORD.replace_all(sql, "${1}'***'");
    LITERAL
        .replace_all(&masked, |literal: &regex::Captures| {
            IN_LITERAL
                .replace_all(&literal[0], |caps: &regex::Captures| {
                    match (caps.get(1), caps.get(2)) {
                        (Some(key), _) => format!("{}***", key.as_str()),
                        (_, Some(user)) => format!("{}***@", user.as_str()),
                        _ => caps[0].to_owned(),
                    }
                })
                .into_owned()
        })
        .into_owned()
}

fn millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn to_text<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn from_text<T: for<'de> Deserialize<'de>>(text: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(text.to_owned())).ok()
}

fn db_error(err: rusqlite::Error) -> Error {
    Error::Storage {
        message: format!("query history: {err}"),
    }
}

fn storage(path: &Path, err: impl std::fmt::Display) -> Error {
    Error::Storage {
        message: format!("{}: {err}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(connection: &str, sql: &str, at: SystemTime) -> NewRun {
        NewRun {
            connection_id: Some(connection.into()),
            connection: connection.into(),
            sql: sql.into(),
            started_at: at,
            duration: Duration::from_millis(12),
            status: Status::Ok,
            rows: Some(1),
            error: None,
            source: Source::Editor,
        }
    }

    fn sqls(runs: &[Run]) -> Vec<&str> {
        runs.iter().map(|r| r.sql.as_str()).collect()
    }

    #[test]
    fn masks_passwords_and_connection_secrets() {
        let cases = [
            (
                "ALTER ROLE app PASSWORD 'hunter2';",
                "ALTER ROLE app PASSWORD '***';",
            ),
            (
                "CREATE ROLE a LOGIN ENCRYPTED PASSWORD E'it''s secret' VALID UNTIL 'infinity'",
                "CREATE ROLE a LOGIN ENCRYPTED PASSWORD '***' VALID UNTIL 'infinity'",
            ),
            (
                "alter user b with password $p$x$p$",
                "alter user b with password '***'",
            ),
            (
                "CREATE USER MAPPING FOR me SERVER s OPTIONS (user 'me', password 'pw')",
                "CREATE USER MAPPING FOR me SERVER s OPTIONS (user 'me', password '***')",
            ),
            (
                "SELECT dblink_connect('host=db user=me password=pw dbname=x')",
                "SELECT dblink_connect('host=db user=me password=*** dbname=x')",
            ),
            (
                "SELECT 'postgres://me:pw@db:5432/x'",
                "SELECT 'postgres://me:***@db:5432/x'",
            ),
            // Nothing to mask: columns called password are left alone.
            (
                "SELECT password_hash FROM users WHERE id = 1",
                "SELECT password_hash FROM users WHERE id = 1",
            ),
            (
                "SELECT * FROM users WHERE password = crypt('pw', password)",
                "SELECT * FROM users WHERE password = crypt('pw', password)",
            ),
        ];
        for (sql, masked) in cases {
            assert_eq!(redact(sql), masked, "{sql}");
        }
    }

    #[test]
    fn records_lists_and_searches_newest_first() {
        let history = History::in_memory().unwrap();
        let now = SystemTime::now();
        history
            .record(run("a", "SELECT * FROM users", now))
            .unwrap();
        history
            .record(run("b", "SELECT * FROM orders", now))
            .unwrap();
        let failed = NewRun {
            status: Status::Error,
            error: Some("relation \"userz\" does not exist".into()),
            ..run("a", "SELECT * FROM userz", now)
        };
        history.record(failed).unwrap();
        history
            .record(run("a", "ALTER ROLE x PASSWORD 'pw'", now))
            .unwrap();

        let all = history.list(&Filter::default()).unwrap();
        assert_eq!(
            sqls(&all),
            [
                "ALTER ROLE x PASSWORD '***'",
                "SELECT * FROM userz",
                "SELECT * FROM orders",
                "SELECT * FROM users"
            ]
        );
        let search = |filter: Filter| sqls(&history.list(&filter).unwrap()).join(" | ");
        assert_eq!(
            search(Filter {
                text: Some("USER".into()),
                ..Filter::default()
            }),
            "SELECT * FROM userz | SELECT * FROM users"
        );
        assert_eq!(
            search(Filter {
                text: Some("rd".into()),
                ..Filter::default()
            }),
            "ALTER ROLE x PASSWORD '***' | SELECT * FROM orders"
        );
        assert_eq!(
            search(Filter {
                text: Some("100%".into()),
                ..Filter::default()
            }),
            ""
        );
        assert_eq!(
            search(Filter {
                connection_id: Some("a".into()),
                status: Some(Status::Error),
                ..Filter::default()
            }),
            "SELECT * FROM userz"
        );
        let page = history
            .list(&Filter {
                before: Some(all[1].id),
                limit: Some(1),
                ..Filter::default()
            })
            .unwrap();
        assert_eq!(sqls(&page), ["SELECT * FROM orders"]);
    }

    #[test]
    fn keeps_secrets_when_asked_and_restores_editors() {
        let history = History::in_memory().unwrap();
        assert!(history.settings().unwrap().redact);
        history.set_settings(&Settings { redact: false }).unwrap();
        history
            .record(run("a", "ALTER ROLE x PASSWORD 'pw'", SystemTime::now()))
            .unwrap();
        assert_eq!(
            history.list(&Filter::default()).unwrap()[0].sql,
            "ALTER ROLE x PASSWORD 'pw'"
        );

        assert_eq!(history.editor("a").unwrap(), None);
        history.save_editor("a", "SELECT 1;").unwrap();
        history.save_editor("a", "SELECT 2;").unwrap();
        assert_eq!(history.editor("a").unwrap().as_deref(), Some("SELECT 2;"));
    }

    #[test]
    fn prunes_old_and_excess_runs_but_keeps_starred_ones() {
        let history = History::in_memory().unwrap();
        let now = SystemTime::now();
        let old = now - Duration::from_secs((KEEP_DAYS + 1) * 24 * 60 * 60);
        let starred = history
            .record(run("a", "SELECT 'old but starred'", old))
            .unwrap();
        history.star(starred, true).unwrap();
        history.record(run("a", "SELECT 'old'", old)).unwrap();
        history.record(run("a", "SELECT 'new'", now)).unwrap();
        history.prune(now).unwrap();
        assert_eq!(
            sqls(&history.list(&Filter::default()).unwrap()),
            ["SELECT 'new'", "SELECT 'old but starred'"]
        );

        history.clear(Some("a")).unwrap();
        let left = history.list(&Filter::default()).unwrap();
        assert_eq!(sqls(&left), ["SELECT 'old but starred'"]);
        assert!(left[0].starred);
        history.delete(&[left[0].id]).unwrap();
        assert!(history.list(&Filter::default()).unwrap().is_empty());
        // Deleted runs leave the search index too.
        assert!(
            history
                .list(&Filter {
                    text: Some("starred".into()),
                    ..Filter::default()
                })
                .unwrap()
                .is_empty()
        );
    }
}
