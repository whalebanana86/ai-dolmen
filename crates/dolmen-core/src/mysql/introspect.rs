//! Reads a MySQL or MariaDB server's structure from `information_schema`,
//! into the same model as PostgreSQL's: each database is a schema. Statements
//! are written with backticks, as the server writes them.

use std::collections::HashMap;

use mysql_async::Conn;
use mysql_async::prelude::Queryable;

use super::{Flavor, qualified, quote_ident, quote_literal};
use crate::Error;
use crate::schema::{
    Column, Constraint, ConstraintKind, Database, ForeignKey, Function, FunctionKind, Generated,
    Identity, Index, Schema, Sequence, Table, TableKind, Trigger, TriggerFiring, View,
};

/// The server's own databases, left out like PostgreSQL's catalogs.
const SYSTEM: &str = "('mysql', 'information_schema', 'performance_schema', 'sys')";

pub(super) async fn foreign_keys(conn: &mut Conn) -> Result<Vec<ForeignKey>, Error> {
    type KeyRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    );
    let rows: Vec<KeyRow> = conn
        .query(format!(
            "SELECT k.CONSTRAINT_NAME, k.TABLE_SCHEMA, k.TABLE_NAME, k.COLUMN_NAME,
                    k.REFERENCED_TABLE_SCHEMA, k.REFERENCED_TABLE_NAME, k.REFERENCED_COLUMN_NAME,
                    r.UPDATE_RULE, r.DELETE_RULE
             FROM information_schema.KEY_COLUMN_USAGE k
             JOIN information_schema.REFERENTIAL_CONSTRAINTS r
               ON r.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA
              AND r.CONSTRAINT_NAME = k.CONSTRAINT_NAME
              AND r.TABLE_NAME = k.TABLE_NAME
             WHERE k.REFERENCED_TABLE_NAME IS NOT NULL AND k.TABLE_SCHEMA NOT IN {SYSTEM}
             ORDER BY k.TABLE_SCHEMA, k.TABLE_NAME, k.CONSTRAINT_NAME, k.ORDINAL_POSITION"
        ))
        .await?;
    let mut keys: Vec<ForeignKey> = Vec::new();
    for (name, schema, table, column, ref_schema, ref_table, ref_column, on_update, on_delete) in
        rows
    {
        match keys.last_mut() {
            Some(key) if key.name == name && key.schema == schema && key.table == table => {
                key.columns.push(column);
                key.referenced_columns.push(ref_column);
            }
            _ => keys.push(ForeignKey {
                name,
                schema,
                table,
                columns: vec![column],
                referenced_schema: ref_schema,
                referenced_table: ref_table,
                referenced_columns: vec![ref_column],
                on_update,
                on_delete,
            }),
        }
    }
    keys.sort_by(|a, b| (&a.schema, &a.table, &a.name).cmp(&(&b.schema, &b.table, &b.name)));
    Ok(keys)
}

enum Relation {
    Table(usize, usize),
    View(usize, usize),
}

pub(super) async fn introspect(conn: &mut Conn, flavor: Flavor) -> Result<Database, Error> {
    let names: Vec<String> = conn
        .query(format!(
            "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME NOT IN {SYSTEM}"
        ))
        .await?;
    let mut schemas: Vec<Schema> = names.into_iter().map(empty_schema).collect();
    schemas.sort_by(|a, b| a.name.cmp(&b.name));
    let schema_index: HashMap<String, usize> = schemas
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.clone(), i))
        .collect();

    // Tables and views; MariaDB's sequences are tables of their own type.
    let rows: Vec<(String, String, String, Option<String>)> = conn
        .query(format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME, TABLE_TYPE, TABLE_COMMENT
             FROM information_schema.TABLES WHERE TABLE_SCHEMA NOT IN {SYSTEM}"
        ))
        .await?;
    let views: HashMap<(String, String), String> = conn
        .query::<(String, String, Option<String>), _>(format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME, VIEW_DEFINITION
             FROM information_schema.VIEWS WHERE TABLE_SCHEMA NOT IN {SYSTEM}"
        ))
        .await?
        .into_iter()
        .map(|(schema, name, definition)| ((schema, name), definition.unwrap_or_default()))
        .collect();
    for (schema, name, kind, comment) in rows {
        let Some(&i) = schema_index.get(&schema) else {
            continue;
        };
        match kind.as_str() {
            "VIEW" | "SYSTEM VIEW" => {
                let definition = views
                    .get(&(schema, name.clone()))
                    .cloned()
                    .unwrap_or_default();
                schemas[i].views.push(view(name, definition));
            }
            "SEQUENCE" => schemas[i].sequences.push(Sequence { name }),
            _ => schemas[i]
                .tables
                .push(table(name, comment.filter(|c| !c.is_empty()))),
        }
    }
    for schema in &mut schemas {
        schema.tables.sort_by(|a, b| a.name.cmp(&b.name));
        schema.views.sort_by(|a, b| a.name.cmp(&b.name));
        schema.sequences.sort_by(|a, b| a.name.cmp(&b.name));
    }
    let mut relations: HashMap<(String, String), Relation> = HashMap::new();
    for (i, schema) in schemas.iter().enumerate() {
        for (j, table) in schema.tables.iter().enumerate() {
            relations.insert(
                (schema.name.clone(), table.name.clone()),
                Relation::Table(i, j),
            );
        }
        for (j, view) in schema.views.iter().enumerate() {
            relations.insert(
                (schema.name.clone(), view.name.clone()),
                Relation::View(i, j),
            );
        }
    }

    type ColumnRow = (
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        String,
    );
    let rows: Vec<ColumnRow> = conn
        .query(format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE,
                    COLUMN_DEFAULT, EXTRA, GENERATION_EXPRESSION, COLUMN_COMMENT
             FROM information_schema.COLUMNS WHERE TABLE_SCHEMA NOT IN {SYSTEM}
             ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION"
        ))
        .await?;
    for (schema, table, name, data_type, nullable, default, extra, expression, comment) in rows {
        let extra_lower = extra.to_ascii_lowercase();
        let generated = expression
            .filter(|e| {
                !e.is_empty()
                    && extra_lower.contains("generated")
                    && !extra_lower.contains("default_generated")
            })
            .map(|expression| Generated {
                expression,
                stored: extra_lower.contains("stored"),
            });
        let column = Column {
            default: if generated.is_some() {
                None
            } else {
                column_default(flavor, &data_type, default, &extra_lower)
            },
            identity: extra_lower
                .contains("auto_increment")
                .then_some(Identity::ByDefault),
            generated,
            nullable: nullable == "YES",
            name,
            data_type,
            comment: Some(comment).filter(|c| !c.is_empty()),
        };
        match relations.get(&(schema, table)) {
            Some(&Relation::Table(i, j)) => schemas[i].tables[j].columns.push(column),
            Some(&Relation::View(i, j)) => schemas[i].views[j].columns.push(column),
            None => {}
        }
    }

    // Key columns in order, for primary keys, unique keys and foreign keys.
    type KeyColumnRow = (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let key_rows: Vec<KeyColumnRow> = conn
        .query(format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, COLUMN_NAME,
                    REFERENCED_TABLE_SCHEMA, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME
             FROM information_schema.KEY_COLUMN_USAGE WHERE TABLE_SCHEMA NOT IN {SYSTEM}
             ORDER BY TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION"
        ))
        .await?;
    /// A constraint's quoted columns, and for a foreign key the columns and table it references.
    type KeyColumns = (Vec<String>, Vec<String>, Option<(String, String)>);
    let mut key_columns: HashMap<(String, String, String), KeyColumns> = HashMap::new();
    for (schema, table, name, column, ref_schema, ref_table, ref_column) in key_rows {
        let entry = key_columns.entry((schema, table, name)).or_default();
        entry.0.push(quote_ident(&column));
        if let (Some(ref_schema), Some(ref_table), Some(ref_column)) =
            (ref_schema, ref_table, ref_column)
        {
            entry.1.push(quote_ident(&ref_column));
            entry.2 = Some((ref_schema, ref_table));
        }
    }
    let rules: HashMap<(String, String, String), (String, String)> = conn
        .query::<(String, String, String, String, String), _>(format!(
            "SELECT CONSTRAINT_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, UPDATE_RULE, DELETE_RULE
             FROM information_schema.REFERENTIAL_CONSTRAINTS WHERE CONSTRAINT_SCHEMA NOT IN {SYSTEM}"
        ))
        .await?
        .into_iter()
        .map(|(schema, table, name, update, delete)| ((schema, table, name), (update, delete)))
        .collect();
    let constraint_rows: Vec<(String, String, String, String)> = conn
        .query(format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME, CONSTRAINT_NAME, CONSTRAINT_TYPE
             FROM information_schema.TABLE_CONSTRAINTS
             WHERE TABLE_SCHEMA NOT IN {SYSTEM} AND CONSTRAINT_TYPE <> 'CHECK'"
        ))
        .await?;
    let mut unique_names: HashMap<(String, String), Vec<String>> = HashMap::new();
    for (schema, table, name, kind) in constraint_rows {
        let key = (schema.clone(), table.clone(), name.clone());
        let Some((columns, ref_columns, target)) = key_columns.get(&key) else {
            continue;
        };
        let (kind, definition) = match kind.as_str() {
            "PRIMARY KEY" => (
                ConstraintKind::PrimaryKey,
                format!("PRIMARY KEY ({})", columns.join(", ")),
            ),
            "UNIQUE" => {
                unique_names
                    .entry((schema.clone(), table.clone()))
                    .or_default()
                    .push(name.clone());
                (
                    ConstraintKind::Unique,
                    format!("UNIQUE ({})", columns.join(", ")),
                )
            }
            "FOREIGN KEY" => {
                let Some((ref_schema, ref_table)) = target else {
                    continue;
                };
                let mut definition = format!(
                    "FOREIGN KEY ({}) REFERENCES {}({})",
                    columns.join(", "),
                    qualified(ref_schema, ref_table),
                    ref_columns.join(", ")
                );
                if let Some((update, delete)) = rules.get(&key) {
                    for (event, rule) in [("DELETE", delete), ("UPDATE", update)] {
                        if rule != "NO ACTION" && rule != "RESTRICT" {
                            definition.push_str(&format!(" ON {event} {rule}"));
                        }
                    }
                }
                (ConstraintKind::ForeignKey, definition)
            }
            _ => continue,
        };
        if let Some(&Relation::Table(i, j)) = relations.get(&(schema, table)) {
            schemas[i].tables[j].constraints.push(Constraint {
                name,
                kind,
                definition,
            });
        }
    }

    // Check constraints (MySQL 8.0.16+, MariaDB 10.2+). MariaDB names them per
    // table, so its join needs the table too.
    let table_join = match flavor {
        Flavor::MariaDb => "AND c.TABLE_NAME = t.TABLE_NAME",
        Flavor::MySql => "",
    };
    let checks: Result<Vec<(String, String, String, String)>, _> = conn
        .query(format!(
            "SELECT t.TABLE_SCHEMA, t.TABLE_NAME, t.CONSTRAINT_NAME, c.CHECK_CLAUSE
             FROM information_schema.TABLE_CONSTRAINTS t
             JOIN information_schema.CHECK_CONSTRAINTS c
               ON c.CONSTRAINT_SCHEMA = t.CONSTRAINT_SCHEMA
              AND c.CONSTRAINT_NAME = t.CONSTRAINT_NAME {table_join}
             WHERE t.CONSTRAINT_TYPE = 'CHECK' AND t.TABLE_SCHEMA NOT IN {SYSTEM}"
        ))
        .await;
    // Servers before check constraints have no such table.
    for (schema, table, name, clause) in checks.unwrap_or_default() {
        if let Some(&Relation::Table(i, j)) = relations.get(&(schema, table)) {
            schemas[i].tables[j].constraints.push(Constraint {
                name,
                kind: ConstraintKind::Check,
                definition: format!("CHECK ({clause})"),
            });
        }
    }
    for schema in &mut schemas {
        for table in &mut schema.tables {
            table.constraints.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    // Indexes besides the primary key and unique constraints' own. MySQL
    // 8.0.13+ indexes expressions too; MariaDB has no EXPRESSION column.
    let expression = match flavor {
        Flavor::MySql => "EXPRESSION",
        Flavor::MariaDb => "NULL",
    };
    type IndexRow = (
        String,
        String,
        String,
        i64,
        Option<String>,
        Option<i64>,
        String,
        Option<String>,
    );
    let index_rows: Vec<IndexRow> = conn
        .query(format!(
            "SELECT TABLE_SCHEMA, TABLE_NAME, INDEX_NAME, NON_UNIQUE, COLUMN_NAME, SUB_PART,
                    INDEX_TYPE, {expression}
             FROM information_schema.STATISTICS
             WHERE TABLE_SCHEMA NOT IN {SYSTEM} AND INDEX_NAME <> 'PRIMARY'
             ORDER BY TABLE_SCHEMA, TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX"
        ))
        .await?;
    /// (schema, table, name), unique, index type, quoted parts.
    type IndexParts = ((String, String, String), bool, String, Vec<String>);
    let mut indexes: Vec<IndexParts> = Vec::new();
    for (schema, table, name, non_unique, column, sub_part, kind, expression) in index_rows {
        let part = match (column, expression) {
            (Some(column), _) => match sub_part {
                Some(length) => format!("{}({length})", quote_ident(&column)),
                None => quote_ident(&column),
            },
            (None, Some(expression)) => format!("({expression})"),
            (None, None) => continue,
        };
        let key = (schema, table, name);
        match indexes.last_mut() {
            Some(last) if last.0 == key => last.3.push(part),
            _ => indexes.push((key, non_unique == 0, kind, vec![part])),
        }
    }
    for ((schema, table, name), unique, kind, parts) in indexes {
        let backs_constraint = unique_names
            .get(&(schema.clone(), table.clone()))
            .is_some_and(|names| names.contains(&name));
        if backs_constraint {
            continue;
        }
        let prefix = match kind.as_str() {
            "FULLTEXT" => "FULLTEXT ",
            "SPATIAL" => "SPATIAL ",
            _ if unique => "UNIQUE ",
            _ => "",
        };
        let using = if kind == "HASH" { " USING HASH" } else { "" };
        let definition = format!(
            "CREATE {prefix}INDEX {} ON {} ({}){using}",
            quote_ident(&name),
            qualified(&schema, &table),
            parts.join(", ")
        );
        if let Some(&Relation::Table(i, j)) = relations.get(&(schema, table)) {
            schemas[i].tables[j]
                .indexes
                .push(Index { name, definition });
        }
    }

    let triggers: Vec<(String, String, String, String, String, String, String)> = conn
        .query(format!(
            "SELECT EVENT_OBJECT_SCHEMA, EVENT_OBJECT_TABLE, TRIGGER_NAME, ACTION_TIMING,
                    EVENT_MANIPULATION, ACTION_ORIENTATION, ACTION_STATEMENT
             FROM information_schema.TRIGGERS WHERE EVENT_OBJECT_SCHEMA NOT IN {SYSTEM}
             ORDER BY EVENT_OBJECT_SCHEMA, EVENT_OBJECT_TABLE, TRIGGER_NAME"
        ))
        .await?;
    for (schema, table, name, timing, event, orientation, statement) in triggers {
        let definition = format!(
            "CREATE TRIGGER {} {timing} {event} ON {} FOR EACH {orientation} {statement}",
            quote_ident(&name),
            qualified(&schema, &table),
        );
        if let Some(&Relation::Table(i, j)) = relations.get(&(schema, table)) {
            schemas[i].tables[j].triggers.push(Trigger {
                name,
                definition,
                firing: TriggerFiring::Origin,
            });
        }
    }

    // Routines with their parameters; the full statement needs SHOW CREATE,
    // which asks for more privileges, so the body stands in when it's refused.
    let routines: Vec<(String, String, String, Option<String>)> = conn
        .query(format!(
            "SELECT ROUTINE_SCHEMA, ROUTINE_NAME, ROUTINE_TYPE, ROUTINE_DEFINITION
             FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA NOT IN {SYSTEM}
             ORDER BY ROUTINE_SCHEMA, ROUTINE_NAME"
        ))
        .await?;
    type ParameterRow = (String, String, String, Option<String>, Option<String>);
    let parameters: Vec<ParameterRow> = conn
        .query(format!(
            "SELECT SPECIFIC_SCHEMA, SPECIFIC_NAME, ROUTINE_TYPE, PARAMETER_NAME, DTD_IDENTIFIER
             FROM information_schema.PARAMETERS
             WHERE SPECIFIC_SCHEMA NOT IN {SYSTEM} AND ORDINAL_POSITION > 0
             ORDER BY SPECIFIC_SCHEMA, SPECIFIC_NAME, ORDINAL_POSITION"
        ))
        .await?;
    let mut arguments: HashMap<(String, String, String), Vec<String>> = HashMap::new();
    for (schema, name, kind, parameter, data_type) in parameters {
        let argument = match parameter {
            Some(parameter) => format!(
                "{} {}",
                quote_ident(&parameter),
                data_type.unwrap_or_default()
            ),
            None => data_type.unwrap_or_default(),
        };
        arguments
            .entry((schema, name, kind))
            .or_default()
            .push(argument);
    }
    for (schema, name, kind, body) in routines {
        let Some(&i) = schema_index.get(&schema) else {
            continue;
        };
        let shown: Result<Option<mysql_async::Row>, _> = conn
            .query_first(format!("SHOW CREATE {kind} {}", qualified(&schema, &name)))
            .await;
        let definition = shown
            .ok()
            .flatten()
            .and_then(|row| row.get_opt::<Option<String>, _>(2))
            .and_then(Result::ok)
            .flatten()
            .unwrap_or_else(|| body.unwrap_or_default());
        let args = arguments
            .remove(&(schema, name.clone(), kind.clone()))
            .unwrap_or_default();
        schemas[i].functions.push(Function {
            name,
            arguments: args.join(", "),
            kind: if kind == "PROCEDURE" {
                FunctionKind::Procedure
            } else {
                FunctionKind::Function
            },
            definition,
        });
    }

    Ok(Database {
        schemas,
        extensions: Vec::new(),
    })
}

/// A column's default as an expression. MySQL gives literals bare (`basic`)
/// and marks expressions as DEFAULT_GENERATED; MariaDB quotes literals itself
/// and writes no default as `NULL`.
fn column_default(
    flavor: Flavor,
    data_type: &str,
    default: Option<String>,
    extra: &str,
) -> Option<String> {
    let default = default?;
    match flavor {
        Flavor::MariaDb => Some(default).filter(|d| d != "NULL"),
        Flavor::MySql => {
            let numeric = data_type.split(['(', ' ']).next().is_some_and(|base| {
                matches!(
                    base,
                    "tinyint"
                        | "smallint"
                        | "mediumint"
                        | "int"
                        | "bigint"
                        | "decimal"
                        | "float"
                        | "double"
                        | "bit"
                        | "year"
                )
            });
            if extra.contains("default_generated") || numeric {
                Some(default)
            } else {
                Some(quote_literal(&default))
            }
        }
    }
}

fn empty_schema(name: String) -> Schema {
    Schema {
        name,
        tables: Vec::new(),
        views: Vec::new(),
        functions: Vec::new(),
        sequences: Vec::new(),
        enums: Vec::new(),
    }
}

fn table(name: String, comment: Option<String>) -> Table {
    Table {
        name,
        kind: TableKind::Table,
        columns: Vec::new(),
        constraints: Vec::new(),
        indexes: Vec::new(),
        partition_key: None,
        comment,
        partition_of: None,
        grants: Vec::new(),
        triggers: Vec::new(),
        row_security: false,
        force_row_security: false,
        policies: Vec::new(),
    }
}

fn view(name: String, definition: String) -> View {
    View {
        name,
        materialized: false,
        columns: Vec::new(),
        definition,
        comment: None,
        indexes: Vec::new(),
        grants: Vec::new(),
        triggers: Vec::new(),
    }
}
