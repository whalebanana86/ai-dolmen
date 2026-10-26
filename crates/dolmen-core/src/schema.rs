//! The structure of a database: schemas, tables, views, functions and types.
//!
//! The same model feeds the schema tree, completion and schema snapshots.
//! Every list is sorted by name (byte order), so a database always produces
//! the same model and snapshots of it diff cleanly.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Database {
    pub schemas: Vec<Schema>,
    pub extensions: Vec<Extension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extension {
    pub name: String,
    pub schema: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schema {
    pub name: String,
    pub tables: Vec<Table>,
    pub views: Vec<View>,
    pub functions: Vec<Function>,
    pub sequences: Vec<Sequence>,
    pub enums: Vec<EnumType>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<Column>,
    pub constraints: Vec<Constraint>,
    /// Indexes that don't back a primary key, unique or exclusion constraint.
    pub indexes: Vec<Index>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TableKind {
    Table,
    Partitioned,
    Foreign,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Column {
    pub name: String,
    /// The type as the database writes it, e.g. `character varying(255)`.
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub identity: Option<Identity>,
    /// The expression of a generated column.
    pub generated: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Identity {
    Always,
    ByDefault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Constraint {
    pub name: String,
    pub kind: ConstraintKind,
    /// The constraint as the database writes it, e.g. `PRIMARY KEY (id)`.
    pub definition: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConstraintKind {
    PrimaryKey,
    Unique,
    Check,
    ForeignKey,
    Exclusion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    /// The full `CREATE INDEX` statement.
    pub definition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub name: String,
    pub materialized: bool,
    pub columns: Vec<Column>,
    /// The view's query.
    pub definition: String,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Function {
    pub name: String,
    /// Argument list identifying the function among overloads, e.g. `a integer, b text`.
    pub arguments: String,
    pub kind: FunctionKind,
    /// The full `CREATE OR REPLACE` statement.
    pub definition: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FunctionKind {
    Function,
    Procedure,
}

/// A sequence not owned by a serial or identity column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sequence {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumType {
    pub name: String,
    pub labels: Vec<String>,
}
