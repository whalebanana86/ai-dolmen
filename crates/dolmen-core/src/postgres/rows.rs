//! Changing the rows of a query's result: which table they come from, and
//! saving edits to them by primary key.
//!
//! A result can be changed when every column it reads from a table reads the
//! same one, and that table's primary key is among them. Changes are saved
//! together or not at all, and an update only touches a row whose edited
//! cells still hold the values read, so nobody else's change is overwritten.

use std::fmt::Write;

use bytes::BytesMut;
use serde::{Deserialize, Serialize};
use tokio_postgres::Client;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::{Format, IsNull, ToSql, Type, to_sql_checked};

use crate::postgres::Session;
use crate::postgres::ddl::{qualified, quote_ident, quote_literal};
use crate::{Error, Refusal};

/// Where a result's rows come from, and whether they can be changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RowSource {
    /// Rows of one table, told apart by its primary key.
    Table {
        schema: String,
        table: String,
        /// For each result column, the table column it shows; `None` for a
        /// value the query computes, or a column shown a second time.
        columns: Vec<Option<TableColumn>>,
        /// The result columns that hold the primary key, in key order.
        key: Vec<usize>,
    },
    /// The rows can't be changed.
    ReadOnly { reason: ReadOnlyReason },
}

/// A table column a result shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TableColumn {
    pub name: String,
    /// As PostgreSQL writes it, e.g. `character varying(255)`.
    pub data_type: String,
    pub nullable: bool,
    /// Computed from other columns, so it can't be given a value.
    pub generated: bool,
    /// An identity column only the database gives values (`GENERATED ALWAYS`).
    pub identity_always: bool,
}

/// Why a result's rows can't be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReadOnlyReason {
    /// No column comes from a table: the query computes them all.
    NoTable,
    /// The columns come from a view.
    View,
    /// The columns come from more than one table, as in a join.
    SeveralTables,
    /// The table has no primary key, so a row can't be told from an identical one.
    NoPrimaryKey,
    /// The table's primary key isn't among the result's columns.
    KeyNotSelected,
}

/// Changes to a table's rows, saved together.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowChanges {
    pub schema: String,
    pub table: String,
    /// Made in this order.
    pub changes: Vec<RowChange>,
    /// The table columns whose values come back for each updated or added row.
    pub returning: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RowChange {
    /// Gives cells of the row with `key` new values, if they still hold the old ones.
    Update {
        key: Vec<Value>,
        cells: Vec<CellEdit>,
    },
    /// Adds a row; columns left out get their defaults.
    Insert { cells: Vec<Value> },
    /// Deletes the row with `key`.
    Delete { key: Vec<Value> },
}

/// A column's value in PostgreSQL's text format; `None` is NULL.
#[derive(Debug, Clone, Deserialize)]
pub struct Value {
    pub column: String,
    pub value: Option<String>,
}

/// A cell's value as read, and the one it gets.
#[derive(Debug, Clone, Deserialize)]
pub struct CellEdit {
    pub column: String,
    pub old: Option<String>,
    pub new: Option<String>,
}

/// How saving went.
#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SaveResult {
    /// Every change was made.
    Saved {
        /// False when the changes joined a transaction the session had open,
        /// which still has to be committed.
        committed: bool,
        /// For each change in order, the row's `returning` values now, or
        /// `None` for a delete.
        rows: Vec<Option<Vec<Option<String>>>>,
    },
    /// Change `change` found its row changed or gone since it was read, so
    /// nothing was saved.
    Stale { change: usize },
    /// Change `change` failed, so nothing was saved.
    Failed { change: usize, error: Error },
}

impl Session {
    /// Where the rows of `statement` come from, to tell whether they can be
    /// changed. The statement is prepared, not run, in this session, so its
    /// names resolve as they did when it ran. `None` when it can't be
    /// prepared, as inside a failed transaction.
    pub async fn row_source(&self, statement: &str) -> Option<RowSource> {
        let statement = crate::sql::without_terminator(statement.trim());
        let prepared = self.client.prepare(statement).await.ok()?;
        let origins: Vec<(Option<u32>, Option<i16>)> = prepared
            .columns()
            .iter()
            .map(|c| (c.table_oid(), c.column_id()))
            .collect();
        let mut tables: Vec<u32> = origins.iter().filter_map(|(table, _)| *table).collect();
        tables.sort_unstable();
        tables.dedup();
        let read_only = |reason| Some(RowSource::ReadOnly { reason });
        let oid = match tables[..] {
            [] => return read_only(ReadOnlyReason::NoTable),
            [oid] => oid,
            _ => return read_only(ReadOnlyReason::SeveralTables),
        };
        let table = table_info(&self.client, oid).await.ok()??;
        if !table.is_table() {
            return read_only(ReadOnlyReason::View);
        }
        let Some(key) = &table.key else {
            return read_only(ReadOnlyReason::NoPrimaryKey);
        };
        // A column shown twice is changed through the first one only.
        let mut seen = Vec::new();
        let columns: Vec<Option<TableColumn>> = origins
            .iter()
            .map(|&(table_oid, attnum)| {
                let attnum = attnum.filter(|n| table_oid == Some(oid) && !seen.contains(n))?;
                seen.push(attnum);
                table.column(attnum).cloned()
            })
            .collect();
        let key: Option<Vec<usize>> = key
            .iter()
            .map(|attnum| {
                let name = &table.column(*attnum)?.name;
                columns
                    .iter()
                    .position(|c| c.as_ref().is_some_and(|c| &c.name == name))
            })
            .collect();
        let Some(key) = key else {
            return read_only(ReadOnlyReason::KeyNotSelected);
        };
        Some(RowSource::Table {
            schema: table.schema,
            table: table.name,
            columns,
            key,
        })
    }

    /// Makes `changes` in one go. Inside a transaction the session has open
    /// they join it, as a savepoint, and commit with it; otherwise they get a
    /// transaction of their own.
    pub async fn save_rows(&self, changes: &RowChanges) -> Result<SaveResult, Error> {
        let _working = self.work.lock().await;
        let name = qualified(&changes.schema, &changes.table);
        let table_changed = || Error::Refused {
            reason: Refusal::TableChanged,
            message: format!("{name} changed since its rows were read; run the query again."),
        };
        let oid: Option<u32> = self
            .client
            .query_one("SELECT pg_catalog.to_regclass($1)::oid", &[&name])
            .await?
            .get(0);
        let table = match oid {
            Some(oid) => table_info(&self.client, oid).await?,
            None => None,
        };
        let table = table.ok_or_else(table_changed)?;
        if !table.is_table() || !table.accepts(changes) {
            return Err(table_changed());
        }

        let nested = match self.client.batch_execute("SAVEPOINT dolmen_rows").await {
            Ok(()) => true,
            Err(err) if err.code() == Some(&SqlState::NO_ACTIVE_SQL_TRANSACTION) => {
                self.client
                    .batch_execute("BEGIN; SET LOCAL lock_timeout = '10s'")
                    .await?;
                false
            }
            // Such as a failed transaction, which only a ROLLBACK ends.
            Err(err) => return Err(err.into()),
        };
        let result = self.make_changes(&name, changes).await;
        let end = match (&result, nested) {
            (Ok(SaveResult::Saved { .. }), true) => "RELEASE SAVEPOINT dolmen_rows",
            (Ok(SaveResult::Saved { .. }), false) => "COMMIT",
            (_, true) => "ROLLBACK TO SAVEPOINT dolmen_rows; RELEASE SAVEPOINT dolmen_rows",
            (_, false) => "ROLLBACK",
        };
        let ended = self.client.batch_execute(end).await;
        let result = result?;
        // A deferred constraint can still fail the commit, which undoes it all.
        ended?;
        Ok(match result {
            SaveResult::Saved { rows, .. } => SaveResult::Saved {
                committed: !nested,
                rows,
            },
            other => other,
        })
    }

    async fn make_changes(&self, name: &str, changes: &RowChanges) -> Result<SaveResult, Error> {
        let mut rows = Vec::with_capacity(changes.changes.len());
        for (i, change) in changes.changes.iter().enumerate() {
            let statement = statement(name, change, &changes.returning);
            let params: Vec<Option<Text>> = statement
                .params
                .iter()
                .map(|value| value.as_deref().map(Text))
                .collect();
            let params: Vec<&(dyn ToSql + Sync)> =
                params.iter().map(|p| p as &(dyn ToSql + Sync)).collect();
            let found = match self.client.query(&statement.sql, &params).await {
                Ok(found) => found,
                Err(err) => {
                    return match Error::from(err) {
                        // The position points into Dolmen's statement, not the user's.
                        Error::Database {
                            message,
                            code,
                            detail,
                            hint,
                            ..
                        } => Ok(SaveResult::Failed {
                            change: i,
                            error: Error::Database {
                                message,
                                code,
                                detail,
                                hint,
                                position: None,
                            },
                        }),
                        other => Err(other),
                    };
                }
            };
            let Some(row) = found.first() else {
                return Ok(SaveResult::Stale { change: i });
            };
            rows.push(match change {
                RowChange::Delete { .. } => None,
                _ => Some(
                    (0..changes.returning.len())
                        .map(|c| row.get::<_, Option<String>>(c))
                        .collect(),
                ),
            });
        }
        Ok(SaveResult::Saved {
            committed: true,
            rows,
        })
    }
}

/// The statements `changes` make, as they read with their values written in,
/// to review them or keep them in the history.
pub fn shown(changes: &RowChanges) -> Vec<String> {
    let name = qualified(&changes.schema, &changes.table);
    changes
        .changes
        .iter()
        .map(|change| statement(&name, change, &[]).shown)
        .collect()
}

/// A statement with its parameters, and how it reads with their values in place.
#[derive(Default)]
struct Statement {
    sql: String,
    params: Vec<Option<String>>,
    /// Leaves out the checks and what the statement returns.
    shown: String,
}

impl Statement {
    fn push(&mut self, text: &str) {
        self.sql.push_str(text);
        self.shown.push_str(text);
    }

    fn value(&mut self, value: Option<&str>) {
        self.params.push(value.map(str::to_owned));
        let _ = write!(self.sql, "${}", self.params.len());
        self.shown
            .push_str(&value.map_or_else(|| "NULL".to_owned(), quote_literal));
    }

    /// Text that runs but isn't shown.
    fn hidden(&mut self, text: &str) {
        self.sql.push_str(text);
    }

    fn hidden_value(&mut self, value: Option<&str>) {
        self.params.push(value.map(str::to_owned));
        let _ = write!(self.sql, "${}", self.params.len());
    }

    fn key(&mut self, key: &[Value]) {
        self.push(" WHERE ");
        for (i, part) in key.iter().enumerate() {
            if i > 0 {
                self.push(" AND ");
            }
            self.push(&format!("{} = ", quote_ident(&part.column)));
            self.value(part.value.as_deref());
        }
    }

    /// Returns `columns` as text, or `true` when there are none, so a changed
    /// row always comes back.
    fn returning(&mut self, columns: &[String]) {
        let list: Vec<String> = columns.iter().map(|c| as_text(&quote_ident(c))).collect();
        let list = if list.is_empty() {
            "true".to_owned()
        } else {
            list.join(", ")
        };
        self.hidden(&format!(" RETURNING {list}"));
    }
}

/// The statement that makes `change` to table `name`. Values are sent as
/// text and the server reads each as the type its column has, as it would
/// a string literal there.
fn statement(name: &str, change: &RowChange, returning: &[String]) -> Statement {
    let mut statement = Statement::default();
    match change {
        RowChange::Update { key, cells } => {
            statement.push(&format!("UPDATE {name} SET "));
            for (i, cell) in cells.iter().enumerate() {
                if i > 0 {
                    statement.push(", ");
                }
                statement.push(&format!("{} = ", quote_ident(&cell.column)));
                statement.value(cell.new.as_deref());
            }
            statement.key(key);
            // The cells must still read as they did: compared as text, so
            // types without an equality operator, such as json, work too.
            for cell in cells {
                let column = as_text(&quote_ident(&cell.column));
                statement.hidden(&format!(" AND {column} IS NOT DISTINCT FROM "));
                statement.hidden_value(cell.old.as_deref());
            }
        }
        RowChange::Insert { cells } if cells.is_empty() => {
            statement.push(&format!("INSERT INTO {name} DEFAULT VALUES"));
        }
        RowChange::Insert { cells } => {
            let columns: Vec<String> = cells.iter().map(|c| quote_ident(&c.column)).collect();
            statement.push(&format!(
                "INSERT INTO {name} ({}) VALUES (",
                columns.join(", ")
            ));
            for (i, cell) in cells.iter().enumerate() {
                if i > 0 {
                    statement.push(", ");
                }
                statement.value(cell.value.as_deref());
            }
            statement.push(")");
        }
        RowChange::Delete { key } => {
            statement.push(&format!("DELETE FROM {name}"));
            statement.key(key);
        }
    }
    statement.returning(returning);
    statement.shown.push(';');
    statement
}

/// `column` as the result grid shows it: its type's text output, as a
/// simple query returns it (`format`'s `%s` uses the same function, unlike a
/// cast to text, which writes `true` for `t`), or NULL.
fn as_text(column: &str) -> String {
    format!("CASE WHEN {column} IS NULL THEN NULL ELSE pg_catalog.format('%s', {column}) END")
}

/// A parameter sent in text format, for the server to read as whatever type
/// it expects there.
#[derive(Debug)]
struct Text<'a>(&'a str);

impl ToSql for Text<'_> {
    fn to_sql(
        &self,
        _: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        out.extend_from_slice(self.0.as_bytes());
        Ok(IsNull::No)
    }

    fn accepts(_: &Type) -> bool {
        true
    }

    fn encode_format(&self, _: &Type) -> Format {
        Format::Text
    }

    to_sql_checked!();
}

/// What the catalogs say about a relation.
struct TableInfo {
    schema: String,
    name: String,
    /// `pg_class.relkind`, e.g. `r` for a table.
    kind: String,
    /// The primary key's column numbers, in key order.
    key: Option<Vec<i16>>,
    columns: Vec<(i16, TableColumn)>,
}

impl TableInfo {
    /// A table or a partitioned one, rather than a view or another relation.
    fn is_table(&self) -> bool {
        matches!(self.kind.as_str(), "r" | "p")
    }

    fn column(&self, attnum: i16) -> Option<&TableColumn> {
        self.columns
            .iter()
            .find(|(number, _)| *number == attnum)
            .map(|(_, column)| column)
    }

    fn named(&self, name: &str) -> Option<&TableColumn> {
        self.columns
            .iter()
            .map(|(_, column)| column)
            .find(|column| column.name == name)
    }

    /// Whether `changes` fit the table as it is now: rows named by its whole
    /// primary key, and values only for columns it has that take them.
    fn accepts(&self, changes: &RowChanges) -> bool {
        let Some(key) = &self.key else { return false };
        let key: Vec<&str> = key
            .iter()
            .filter_map(|attnum| self.column(*attnum).map(|c| c.name.as_str()))
            .collect();
        let is_key = |values: &[Value]| {
            values.len() == key.len()
                && key
                    .iter()
                    .all(|column| values.iter().any(|v| v.column == *column))
        };
        let settable = |column: &str| self.named(column).is_some_and(|c| !c.generated);
        changes.returning.iter().all(|c| self.named(c).is_some())
            && changes.changes.iter().all(|change| match change {
                RowChange::Update { key, cells } => {
                    is_key(key) && !cells.is_empty() && cells.iter().all(|c| settable(&c.column))
                }
                RowChange::Insert { cells } => cells.iter().all(|c| settable(&c.column)),
                RowChange::Delete { key } => is_key(key),
            })
    }
}

/// The relation with `oid` and its columns, or `None` if it's gone.
async fn table_info(client: &Client, oid: u32) -> Result<Option<TableInfo>, Error> {
    let params: [&(dyn ToSql + Sync); 1] = [&oid];
    let relation = client.query_opt(
        "SELECT n.nspname, c.relname, c.relkind::text, k.conkey
         FROM pg_catalog.pg_class c
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
         LEFT JOIN pg_catalog.pg_constraint k ON k.conrelid = c.oid AND k.contype = 'p'
         WHERE c.oid = $1",
        &params,
    );
    let columns = client.query(
        "SELECT a.attnum, a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod),
                NOT a.attnotnull, a.attgenerated <> '', a.attidentity = 'a'
         FROM pg_catalog.pg_attribute a
         WHERE a.attrelid = $1 AND a.attnum > 0 AND NOT a.attisdropped",
        &params,
    );
    let (relation, columns) = futures_util::future::try_join(relation, columns).await?;
    let Some(relation) = relation else {
        return Ok(None);
    };
    Ok(Some(TableInfo {
        schema: relation.get(0),
        name: relation.get(1),
        kind: relation.get(2),
        key: relation.get(3),
        columns: columns
            .iter()
            .map(|row| {
                (
                    row.get(0),
                    TableColumn {
                        name: row.get(1),
                        data_type: row.get(2),
                        nullable: row.get(3),
                        generated: row.get(4),
                        identity_always: row.get(5),
                    },
                )
            })
            .collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(column: &str, value: Option<&str>) -> Value {
        Value {
            column: column.into(),
            value: value.map(str::to_owned),
        }
    }

    #[test]
    fn updates_check_the_old_values_and_show_without_the_check() {
        let change = RowChange::Update {
            key: vec![value("id", Some("7"))],
            cells: vec![CellEdit {
                column: "Name".into(),
                old: None,
                new: Some("O'Neil".into()),
            }],
        };
        let statement = statement("public.people", &change, &["id".into()]);
        assert_eq!(
            statement.shown,
            r#"UPDATE public.people SET "Name" = 'O''Neil' WHERE id = '7';"#
        );
        assert_eq!(
            statement.sql,
            "UPDATE public.people SET \"Name\" = $1 WHERE id = $2 \
             AND CASE WHEN \"Name\" IS NULL THEN NULL ELSE pg_catalog.format('%s', \"Name\") END \
             IS NOT DISTINCT FROM $3 \
             RETURNING CASE WHEN id IS NULL THEN NULL ELSE pg_catalog.format('%s', id) END"
        );
        assert_eq!(
            statement.params,
            [Some("O'Neil".to_owned()), Some("7".to_owned()), None]
        );
    }

    #[test]
    fn inserts_leave_out_columns_with_defaults() {
        let empty = RowChange::Insert { cells: vec![] };
        assert_eq!(
            statement("t", &empty, &[]).sql,
            "INSERT INTO t DEFAULT VALUES RETURNING true"
        );
        let row = RowChange::Insert {
            cells: vec![value("a", Some("1")), value("b", None)],
        };
        assert_eq!(
            statement("t", &row, &[]).shown,
            "INSERT INTO t (a, b) VALUES ('1', NULL);"
        );
    }

    #[test]
    fn deletes_name_the_whole_key() {
        let change = RowChange::Delete {
            key: vec![value("a", Some("1")), value("b", Some("x"))],
        };
        let statement = statement("t", &change, &[]);
        assert_eq!(statement.shown, "DELETE FROM t WHERE a = '1' AND b = 'x';");
        assert_eq!(
            statement.sql,
            "DELETE FROM t WHERE a = $1 AND b = $2 RETURNING true"
        );
    }
}
