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
#[serde(rename_all = "camelCase")]
pub struct Table {
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<Column>,
    pub constraints: Vec<Constraint>,
    /// Indexes that don't back a primary key, unique or exclusion constraint.
    pub indexes: Vec<Index>,
    /// The `PARTITION BY` clause of a partitioned table, e.g. `RANGE (created_at)`.
    pub partition_key: Option<String>,
    pub comment: Option<String>,
    /// For a partition, the table it belongs to; its columns come from there.
    /// Left out of the JSON when unset, so models of other tables don't change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition_of: Option<PartitionOf>,
    /// Privileges held by roles other than the owner.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<Grant>,
}

/// Privileges a role holds on a table or view, besides its owner's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// A role, or `PUBLIC`.
    pub grantee: String,
    /// `SELECT`, `INSERT` and the like, in alphabetical order.
    pub privileges: Vec<String>,
    /// Whether the grantee may grant them to others.
    pub grantable: bool,
}

/// The privileges a table or view can be granted.
pub const TABLE_PRIVILEGES: &[&str] = &[
    "DELETE",
    "INSERT",
    "MAINTAIN",
    "REFERENCES",
    "SELECT",
    "TRIGGER",
    "TRUNCATE",
    "UPDATE",
];

/// Where a partition belongs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartitionOf {
    pub schema: String,
    pub table: String,
    /// `FOR VALUES ...` or `DEFAULT`, as PostgreSQL writes it.
    pub bound: String,
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
    pub generated: Option<Generated>,
    pub comment: Option<String>,
}

/// How a generated column computes its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generated {
    pub expression: String,
    /// Stored columns keep the computed value; virtual ones compute it on read.
    pub stored: bool,
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

/// A foreign key with its columns spelled out, for showing relationships.
/// It is read alongside the model rather than kept in it: the model's
/// [`Constraint`] already holds the definition snapshots and plans use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForeignKey {
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub referenced_schema: String,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    /// `NO ACTION`, `RESTRICT`, `CASCADE`, `SET NULL` or `SET DEFAULT`.
    pub on_update: String,
    pub on_delete: String,
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
    /// A materialized view's indexes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indexes: Vec<Index>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<Grant>,
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
