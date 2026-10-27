//! Plans the DDL that changes a database from one schema model to another, so
//! a past snapshot can be applied to a (local) database.
//!
//! Steps run in phases, as pg_dump orders a restore: create schemas, types and
//! sequences; drop what depends on things about to change (views, foreign keys,
//! constraints, indexes) and dropped tables; create functions and tables; alter
//! tables; add constraints, indexes and foreign keys; create views; drop the rest.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use serde::{Deserialize, Serialize};

use super::ddl::{self, comment_on, qualified, quote_ident, quote_literal};
use crate::schema::{
    Column, Constraint, ConstraintKind, Database, EnumType, FunctionKind, Identity, Schema, Table,
    View,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub steps: Vec<Step>,
    /// Changes the plan can't make, such as removing an enum value. A plan with
    /// blockers must not be applied.
    pub blockers: Vec<Blocker>,
    /// Drops paired with adds that look like renames. Planning again with them
    /// as renames keeps the data instead of dropping it.
    pub rename_candidates: Vec<Rename>,
}

/// A change the plan can't make. Tagged by kind, so the app can word it in the
/// user's language; `Display` gives the English wording.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Blocker {
    /// Enum values can be added in place but not removed or reordered.
    EnumValuesRemoved { name: String },
    /// A table's kind (table, partitioned, foreign) or partition key changed.
    TableKindChanged { name: String },
    /// A column would become generated, or switch between stored and virtual.
    GeneratedColumnChanged { name: String },
    /// A rename asked for no longer matches the database or the snapshot.
    RenameNoLongerApplies { from: String, to: String },
    /// A type, expression or definition in the target isn't one expression or
    /// statement, or hides text with bidirectional controls: a snapshot from
    /// a repository could smuggle extra statements into a plan that way.
    InvalidDefinition { name: String },
}

impl fmt::Display for Blocker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocker::InvalidDefinition { name } => write!(
                f,
                "{name}: its definition isn't a single expression or statement, so the plan won't run it."
            ),
            Blocker::EnumValuesRemoved { name } => write!(
                f,
                "Enum {name}: removing or reordering values isn't supported; only adding values is."
            ),
            Blocker::TableKindChanged { name } => {
                write!(
                    f,
                    "Table {name}: changing its kind or partitioning isn't supported."
                )
            }
            Blocker::GeneratedColumnChanged { name } => write!(
                f,
                "Column {name}: turning it into a generated column, or changing whether it is stored, isn't supported."
            ),
            Blocker::RenameNoLongerApplies { from, to } => {
                write!(f, "The rename of {from} to {to} no longer applies.")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub sql: String,
    /// Whether the step can lose data: dropping a table, column, sequence or
    /// extension, or changing a column's type.
    pub destructive: bool,
}

impl Plan {
    pub fn is_destructive(&self) -> bool {
        self.steps.iter().any(|step| step.destructive)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Rename {
    Table {
        schema: String,
        from: String,
        to: String,
    },
    Column {
        schema: String,
        table: String,
        from: String,
        to: String,
    },
}

/// The steps that change `current` into `target`, treating `renames` as
/// renames rather than drops and adds.
pub fn plan(current: &Database, target: &Database, renames: &[Rename]) -> Plan {
    plan_with(Planner::default(), current, target, renames)
}

/// Like [`plan`], for changes made to the live database's own model, whose
/// views still read the old names: views are left to PostgreSQL, which follows
/// renames by itself and refuses to drop or retype a column a view uses,
/// instead of being recreated from definitions that name what was renamed.
pub(crate) fn plan_keeping_views(
    current: &Database,
    target: &Database,
    renames: &[Rename],
) -> Plan {
    let planner = Planner {
        keep_views: true,
        ..Planner::default()
    };
    plan_with(planner, current, target, renames)
}

fn plan_with(
    mut planner: Planner,
    current: &Database,
    target: &Database,
    renames: &[Rename],
) -> Plan {
    planner.blockers.extend(invalid_definitions(target));
    let mut current = current.clone();
    for rename in renames {
        planner.rename(&mut current, target, rename);
    }
    planner.database(&current, target);
    planner.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Rename,
    CreateSchema,
    CreateExtension,
    CreateType,
    CreateSequence,
    DropView,
    DropForeignKey,
    DropConstraint,
    DropIndex,
    DropTable,
    Function,
    CreateTable,
    /// Partitions, after the tables they belong to: depth 1 for a partition
    /// of a plain partitioned table, 2 for one of a partition, and so on.
    CreatePartition(u8),
    AlterTable,
    AddConstraint,
    CreateIndex,
    AddForeignKey,
    CreateView,
    DropFunction,
    DropSequence,
    DropType,
    DropExtension,
    DropSchema,
}

#[derive(Default)]
struct Planner {
    phases: BTreeMap<Phase, Vec<Step>>,
    blockers: Vec<Blocker>,
    candidates: Vec<Rename>,
    /// Set by changes that can break views on the table: dropped tables and
    /// columns, type changes and renames. Views are then all recreated.
    reshaped: bool,
    /// Leave views alone unless their own definition changed; see [`plan_keeping_views`].
    keep_views: bool,
    /// The target's tables, by schema and name, with how deep in partitions they are.
    target_tables: HashMap<(String, String), u8>,
}

impl Planner {
    /// Adds the statements in `sql` as steps, one statement each, which is
    /// how [`Session::apply`](crate::postgres::Session::apply) runs them.
    fn push(&mut self, phase: Phase, sql: String, destructive: bool) {
        let steps = self.phases.entry(phase).or_default();
        for step in statements(&sql) {
            steps.push(Step {
                sql: step,
                destructive,
            });
        }
    }

    fn finish(self) -> Plan {
        Plan {
            steps: self.phases.into_values().flatten().collect(),
            blockers: self.blockers,
            rename_candidates: self.candidates,
        }
    }

    /// Renames in the database and in `current`, so the rest of the plan sees
    /// the new name instead of a drop and an add.
    fn rename(&mut self, current: &mut Database, target: &Database, rename: &Rename) {
        match rename {
            Rename::Table { schema, from, to } => {
                let exists_in_target = find_table(target, schema, to).is_some();
                let Some(table) =
                    find_table_mut(current, schema, from).filter(|_| exists_in_target)
                else {
                    self.blockers.push(Blocker::RenameNoLongerApplies {
                        from: format!("{schema}.{from}"),
                        to: to.clone(),
                    });
                    return;
                };
                table.name = to.clone();
                self.push(
                    Phase::Rename,
                    format!(
                        "ALTER TABLE {} RENAME TO {};",
                        qualified(schema, from),
                        quote_ident(to)
                    ),
                    false,
                );
            }
            Rename::Column {
                schema,
                table,
                from,
                to,
            } => {
                let exists_in_target = find_table(target, schema, table)
                    .is_some_and(|t| t.columns.iter().any(|c| &c.name == to));
                let column = find_table_mut(current, schema, table)
                    .and_then(|t| t.columns.iter_mut().find(|c| &c.name == from))
                    .filter(|_| exists_in_target);
                let Some(column) = column else {
                    self.blockers.push(Blocker::RenameNoLongerApplies {
                        from: format!("{schema}.{table}.{from}"),
                        to: to.clone(),
                    });
                    return;
                };
                column.name = to.clone();
                self.push(
                    Phase::Rename,
                    format!(
                        "ALTER TABLE {} RENAME COLUMN {} TO {};",
                        qualified(schema, table),
                        quote_ident(from),
                        quote_ident(to)
                    ),
                    false,
                );
            }
        }
        self.reshaped = true;
    }

    fn database(&mut self, current: &Database, target: &Database) {
        self.target_tables = partition_depths(target);
        for extension in &target.extensions {
            if !current.extensions.iter().any(|e| e.name == extension.name) {
                self.push(
                    Phase::CreateExtension,
                    ddl::create_extension(extension),
                    false,
                );
            }
        }
        for extension in &current.extensions {
            if !target.extensions.iter().any(|e| e.name == extension.name) {
                self.push(
                    Phase::DropExtension,
                    format!("DROP EXTENSION {};", quote_ident(&extension.name)),
                    true,
                );
            }
        }

        for schema in &target.schemas {
            match current.schemas.iter().find(|s| s.name == schema.name) {
                Some(existing) => self.schema(existing, schema),
                None => {
                    if schema.name != "public" {
                        self.push(Phase::CreateSchema, ddl::create_schema(&schema.name), false);
                    }
                    self.schema(&empty_schema(&schema.name), schema);
                }
            }
        }
        for schema in &current.schemas {
            if !target.schemas.iter().any(|s| s.name == schema.name) {
                self.schema(schema, &empty_schema(&schema.name));
                if schema.name != "public" {
                    // Without CASCADE: everything in it is dropped by earlier steps.
                    self.push(
                        Phase::DropSchema,
                        format!("DROP SCHEMA {};", quote_ident(&schema.name)),
                        false,
                    );
                }
            }
        }

        // Views go last: whether they must be recreated depends on the tables.
        self.views(current, target);
    }

    fn schema(&mut self, current: &Schema, target: &Schema) {
        let name = &target.name;

        for enum_type in &target.enums {
            match current.enums.iter().find(|e| e.name == enum_type.name) {
                None => self.push(Phase::CreateType, ddl::create_enum(name, enum_type), false),
                Some(existing) if existing.labels != enum_type.labels => {
                    self.enum_labels(name, existing, enum_type);
                }
                Some(_) => {}
            }
        }
        for enum_type in &current.enums {
            if !target.enums.iter().any(|e| e.name == enum_type.name) {
                self.push(
                    Phase::DropType,
                    format!("DROP TYPE {};", qualified(name, &enum_type.name)),
                    false,
                );
            }
        }

        for sequence in &target.sequences {
            if !current.sequences.iter().any(|s| s.name == sequence.name) {
                self.push(
                    Phase::CreateSequence,
                    ddl::create_sequence(name, sequence),
                    false,
                );
            }
        }
        for sequence in &current.sequences {
            if !target.sequences.iter().any(|s| s.name == sequence.name) {
                self.push(
                    Phase::DropSequence,
                    format!("DROP SEQUENCE {};", qualified(name, &sequence.name)),
                    true,
                );
            }
        }

        let same_function = |a: &crate::schema::Function, b: &crate::schema::Function| {
            a.name == b.name && a.arguments == b.arguments
        };
        for function in &target.functions {
            let existing = current
                .functions
                .iter()
                .find(|f| same_function(f, function));
            // pg_get_functiondef ends with a newline that edits don't keep.
            if existing.is_none_or(|f| f.definition.trim_end() != function.definition.trim_end()) {
                self.push(Phase::Function, ddl::create_function(function), false);
            }
        }
        for function in &current.functions {
            if !target.functions.iter().any(|f| same_function(f, function)) {
                let kind = match function.kind {
                    FunctionKind::Function => "FUNCTION",
                    FunctionKind::Procedure => "PROCEDURE",
                };
                self.push(
                    Phase::DropFunction,
                    format!(
                        "DROP {kind} {}({});",
                        qualified(name, &function.name),
                        function.arguments
                    ),
                    false,
                );
            }
        }

        let added: Vec<&Table> = target
            .tables
            .iter()
            .filter(|t| !current.tables.iter().any(|c| c.name == t.name))
            .collect();
        let dropped: Vec<&Table> = current
            .tables
            .iter()
            .filter(|c| !target.tables.iter().any(|t| t.name == c.name))
            .collect();
        for table in &added {
            self.create_table(name, table);
        }
        for table in &dropped {
            self.drop_table(name, table);
        }
        self.suggest_table_renames(name, &dropped, &added);
        for table in &target.tables {
            if let Some(existing) = current.tables.iter().find(|c| c.name == table.name) {
                self.table(name, existing, table);
            }
        }
    }

    /// Adds labels in place when the old ones keep their order; anything else
    /// would need the type rebuilt, which the plan doesn't do.
    fn enum_labels(&mut self, schema: &str, current: &EnumType, target: &EnumType) {
        let kept: Vec<&String> = target
            .labels
            .iter()
            .filter(|l| current.labels.contains(l))
            .collect();
        if kept.len() != current.labels.len() || !kept.iter().copied().eq(current.labels.iter()) {
            self.blockers.push(Blocker::EnumValuesRemoved {
                name: qualified(schema, &target.name),
            });
            return;
        }
        let name = qualified(schema, &target.name);
        for (i, label) in target.labels.iter().enumerate() {
            if current.labels.contains(label) {
                continue;
            }
            // Labels before it already exist, either from the start or added just now.
            let position = match i {
                0 => format!(" BEFORE {}", quote_literal(&current.labels[0])),
                _ => format!(" AFTER {}", quote_literal(&target.labels[i - 1])),
            };
            self.push(
                Phase::CreateType,
                format!(
                    "ALTER TYPE {name} ADD VALUE {}{position};",
                    quote_literal(label)
                ),
                false,
            );
        }
    }

    fn create_table(&mut self, schema: &str, table: &Table) {
        let name = qualified(schema, &table.name);
        // Foreign keys come last, once every table they may point at exists.
        let mut sql = ddl::table_statement(
            schema,
            table,
            table
                .constraints
                .iter()
                .filter(|c| c.kind != ConstraintKind::ForeignKey),
        );
        if let Some(comment) = &table.comment {
            sql.push_str(&format!(
                "{}\n",
                comment_on(ddl::table_object(table), &name, Some(comment))
            ));
        }
        for column in table.columns.iter().filter(|c| c.comment.is_some()) {
            sql.push_str(&format!(
                "{}\n",
                comment_on(
                    "COLUMN",
                    &column_name(&name, column),
                    column.comment.as_deref()
                )
            ));
        }
        let phase = match self
            .target_tables
            .get(&(schema.to_owned(), table.name.clone()))
        {
            Some(&depth) if depth > 0 => Phase::CreatePartition(depth),
            _ => Phase::CreateTable,
        };
        self.push(phase, sql, false);
        for index in &table.indexes {
            self.push(Phase::CreateIndex, ddl::create_index(table, index), false);
        }
        for constraint in foreign_keys(table) {
            self.add_constraint(&name, constraint);
        }
    }

    fn drop_table(&mut self, schema: &str, table: &Table) {
        // Dropping a partitioned table drops its partitions with it.
        if let Some(parent) = &table.partition_of
            && !self
                .target_tables
                .contains_key(&(parent.schema.clone(), parent.table.clone()))
        {
            return;
        }
        let name = qualified(schema, &table.name);
        // Its foreign keys go first, in case it points at another dropped table.
        for constraint in foreign_keys(table) {
            self.drop_constraint(&name, constraint);
        }
        self.push(Phase::DropTable, format!("DROP TABLE {name};"), true);
        self.reshaped = true;
    }

    fn table(&mut self, schema: &str, current: &Table, target: &Table) {
        let name = qualified(schema, &target.name);
        // Attaching and detaching partitions isn't planned.
        if current.kind != target.kind
            || current.partition_key != target.partition_key
            || current.partition_of != target.partition_of
        {
            self.blockers.push(Blocker::TableKindChanged { name });
            return;
        }

        // A partition's columns are its parent's, which changes them for it.
        let own_columns = target.partition_of.is_none();
        let dropped: Vec<&Column> = current
            .columns
            .iter()
            .filter(|c| own_columns && !target.columns.iter().any(|t| t.name == c.name))
            .collect();
        let added: Vec<&Column> = target
            .columns
            .iter()
            .filter(|t| own_columns && !current.columns.iter().any(|c| c.name == t.name))
            .collect();
        for column in &dropped {
            self.push(
                Phase::AlterTable,
                format!(
                    "ALTER TABLE {name} DROP COLUMN {};",
                    quote_ident(&column.name)
                ),
                true,
            );
            self.reshaped = true;
        }
        for column in &added {
            self.push(
                Phase::AlterTable,
                format!(
                    "ALTER TABLE {name} ADD COLUMN {};",
                    ddl::column_definition(schema, &target.name, column)
                ),
                false,
            );
            if column.comment.is_some() {
                self.push(
                    Phase::AlterTable,
                    comment_on(
                        "COLUMN",
                        &column_name(&name, column),
                        column.comment.as_deref(),
                    ),
                    false,
                );
            }
        }
        self.suggest_column_renames(schema, &target.name, &dropped, &added);

        for column in target.columns.iter().filter(|_| own_columns) {
            if let Some(existing) = current.columns.iter().find(|c| c.name == column.name) {
                self.column(&name, existing, column);
            }
        }

        let same =
            |a: &Constraint, b: &Constraint| a.name == b.name && a.definition == b.definition;
        for constraint in &current.constraints {
            if !target.constraints.iter().any(|t| same(t, constraint)) {
                self.drop_constraint(&name, constraint);
            }
        }
        for constraint in &target.constraints {
            if !current.constraints.iter().any(|c| same(c, constraint)) {
                self.add_constraint(&name, constraint);
            }
        }

        for index in &current.indexes {
            if !target.indexes.contains(index) {
                self.push(
                    Phase::DropIndex,
                    format!("DROP INDEX {};", qualified(schema, &index.name)),
                    false,
                );
            }
        }
        for index in &target.indexes {
            if !current.indexes.contains(index) {
                self.push(Phase::CreateIndex, ddl::create_index(target, index), false);
            }
        }

        if current.comment != target.comment {
            self.push(
                Phase::AlterTable,
                comment_on(ddl::table_object(target), &name, target.comment.as_deref()),
                false,
            );
        }
    }

    /// Alters one column. The order matters: NOT NULL must hold before an
    /// identity is added, a default must go before one is, and an identity must
    /// go before NOT NULL can be dropped.
    fn column(&mut self, table: &str, current: &Column, target: &Column) {
        let alter = format!(
            "ALTER TABLE {table} ALTER COLUMN {}",
            quote_ident(&target.name)
        );
        let push = |planner: &mut Self, clause: String, destructive: bool| {
            planner.push(Phase::AlterTable, format!("{alter} {clause};"), destructive);
        };

        if current.data_type != target.data_type {
            push(self, format!("TYPE {}", target.data_type), true);
            self.reshaped = true;
        }
        if current.nullable && !target.nullable {
            push(self, "SET NOT NULL".into(), false);
        }
        if current.identity.is_some() && target.identity.is_none() {
            push(self, "DROP IDENTITY".into(), false);
        }
        if current.generated.is_none()
            && target.generated.is_none()
            && current.default != target.default
        {
            match &target.default {
                Some(default) => push(self, format!("SET DEFAULT {default}"), false),
                None => push(self, "DROP DEFAULT".into(), false),
            }
        }
        match (current.identity, target.identity) {
            (None, Some(identity)) => {
                push(self, format!("ADD {}", identity_clause(identity)), false)
            }
            (Some(a), Some(b)) if a != b => {
                push(self, format!("SET {}", generated_clause(b)), false)
            }
            _ => {}
        }
        match (&current.generated, &target.generated) {
            (Some(a), Some(b)) if a.stored == b.stored && a.expression != b.expression => {
                push(self, format!("SET EXPRESSION AS ({})", b.expression), false);
            }
            (Some(_), None) => push(self, "DROP EXPRESSION".into(), false),
            (a, b) if a != b => self.blockers.push(Blocker::GeneratedColumnChanged {
                name: column_name(table, target),
            }),
            _ => {}
        }
        if !current.nullable && target.nullable {
            push(self, "DROP NOT NULL".into(), false);
        }
        if current.comment != target.comment {
            self.push(
                Phase::AlterTable,
                comment_on(
                    "COLUMN",
                    &column_name(table, target),
                    target.comment.as_deref(),
                ),
                false,
            );
        }
    }

    fn add_constraint(&mut self, table: &str, constraint: &Constraint) {
        let phase = match constraint.kind {
            ConstraintKind::ForeignKey => Phase::AddForeignKey,
            _ => Phase::AddConstraint,
        };
        self.push(
            phase,
            format!(
                "ALTER TABLE {table} ADD CONSTRAINT {} {};",
                quote_ident(&constraint.name),
                constraint.definition
            ),
            false,
        );
    }

    fn drop_constraint(&mut self, table: &str, constraint: &Constraint) {
        let phase = match constraint.kind {
            ConstraintKind::ForeignKey => Phase::DropForeignKey,
            _ => Phase::DropConstraint,
        };
        self.push(
            phase,
            format!(
                "ALTER TABLE {table} DROP CONSTRAINT {};",
                quote_ident(&constraint.name)
            ),
            false,
        );
    }

    /// A dropped and an added table with the same column names and types.
    fn suggest_table_renames(&mut self, schema: &str, dropped: &[&Table], added: &[&Table]) {
        let shape = |t: &Table| -> Vec<(String, String)> {
            t.columns
                .iter()
                .map(|c| (c.name.clone(), c.data_type.clone()))
                .collect()
        };
        for old in dropped {
            let matches: Vec<&&Table> = added
                .iter()
                .filter(|new| shape(new) == shape(old))
                .collect();
            if let [new] = matches[..] {
                self.candidates.push(Rename::Table {
                    schema: schema.to_owned(),
                    from: old.name.clone(),
                    to: new.name.clone(),
                });
            }
        }
    }

    /// A dropped and an added column of the same type, when that type is the
    /// only one dropped and added once in the table.
    fn suggest_column_renames(
        &mut self,
        schema: &str,
        table: &str,
        dropped: &[&Column],
        added: &[&Column],
    ) {
        for old in dropped {
            let same_type = |c: &&&Column| c.data_type == old.data_type;
            if let ([new], 1) = (
                &added.iter().filter(same_type).collect::<Vec<_>>()[..],
                dropped.iter().filter(same_type).count(),
            ) {
                self.candidates.push(Rename::Column {
                    schema: schema.to_owned(),
                    table: table.to_owned(),
                    from: old.name.clone(),
                    to: new.name.clone(),
                });
            }
        }
    }

    /// Drops views that changed or went away, and recreates changed and new ones.
    /// When a table was reshaped, every view is dropped and recreated, since the
    /// model doesn't say which views read which tables.
    fn views(&mut self, current: &Database, target: &Database) {
        let current_views = all_views(current);
        let target_views = all_views(target);

        let mut drop = Vec::new();
        let mut create = Vec::new();
        for (key, view) in &current_views {
            match target_views.get(key) {
                Some(new) if (self.keep_views || !self.reshaped) && same_view(view, new) => {
                    if view.comment != new.comment {
                        let object = if new.materialized {
                            "MATERIALIZED VIEW"
                        } else {
                            "VIEW"
                        };
                        self.push(
                            Phase::CreateView,
                            comment_on(object, &qualified(&key.0, &key.1), new.comment.as_deref()),
                            false,
                        );
                    }
                }
                _ => drop.push(key.clone()),
            }
        }
        for key in target_views.keys() {
            if !current_views.contains_key(key) || drop.contains(key) {
                create.push(key.clone());
            }
        }

        // Dependents are dropped before what they read, and created after it.
        for key in dependency_order(&drop, &current_views).into_iter().rev() {
            let view = current_views[&key];
            let object = if view.materialized {
                "MATERIALIZED VIEW"
            } else {
                "VIEW"
            };
            self.push(
                Phase::DropView,
                format!("DROP {object} {};", qualified(&key.0, &key.1)),
                false,
            );
        }
        for key in dependency_order(&create, &target_views) {
            let sql = ddl::create_view(&key.0, target_views[&key]);
            self.push(Phase::CreateView, sql, false);
        }
    }
}

/// The statements in generated SQL, each with its `;`. Definitions were
/// checked by [`invalid_definitions`] first, so every `;` here is one the
/// generator wrote.
pub(crate) fn statements(sql: &str) -> Vec<String> {
    crate::sql::split_statements(sql)
        .into_iter()
        .map(|range| sql[range].trim().to_owned())
        .collect()
}

/// Definitions in `database` that aren't one expression or one statement.
///
/// Types, defaults, expressions and constraint, index, view and function
/// definitions go into the plan as they are, since they come from the
/// server's own pg_get_*def functions. A snapshot model from a repository
/// may hold anything, though, and a `;` would end the step's statement and
/// start another one the review wouldn't flag.
fn invalid_definitions(database: &Database) -> Vec<Blocker> {
    let mut blockers = Vec::new();
    let mut check = |name: String, text: &str, statement: bool| {
        if !single(text, statement) {
            blockers.push(Blocker::InvalidDefinition { name });
        }
    };
    for schema in &database.schemas {
        let name = |object: &str| format!("{}.{object}", schema.name);
        for table in &schema.tables {
            let table_name = name(&table.name);
            for column in &table.columns {
                let column_name = format!("{table_name}.{}", column.name);
                check(column_name.clone(), &column.data_type, false);
                if let Some(default) = &column.default {
                    check(column_name.clone(), default, false);
                }
                if let Some(generated) = &column.generated {
                    check(column_name, &generated.expression, false);
                }
            }
            for constraint in &table.constraints {
                check(
                    format!("{table_name}.{}", constraint.name),
                    &constraint.definition,
                    false,
                );
            }
            for index in &table.indexes {
                check(name(&index.name), &index.definition, true);
            }
            if let Some(key) = &table.partition_key {
                check(table_name.clone(), key, false);
            }
        }
        for view in &schema.views {
            check(name(&view.name), &view.definition, true);
        }
        for function in &schema.functions {
            check(name(&function.name), &function.definition, true);
        }
    }
    blockers
}

/// Whether `text` is one statement (when `statement` is set) or an expression
/// with no statement-ending `;`, free of bidirectional text controls.
pub(crate) fn single(text: &str, statement: bool) -> bool {
    let bidi = text
        .chars()
        .any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'));
    let ranges = crate::sql::split_statements(text);
    let one = match ranges.as_slice() {
        [] => !statement,
        [range] => statement || !text[range.clone()].trim_end().ends_with(';'),
        _ => false,
    };
    one && !bidi
}

/// Every table in `database`, with 0 for one that isn't a partition, 1 for a
/// partition of a table that isn't, and so on.
fn partition_depths(database: &Database) -> HashMap<(String, String), u8> {
    let parents: HashMap<(String, String), (String, String)> = database
        .schemas
        .iter()
        .flat_map(|s| {
            s.tables.iter().map(move |t| {
                let parent = t
                    .partition_of
                    .as_ref()
                    .map(|p| (p.schema.clone(), p.table.clone()));
                ((s.name.clone(), t.name.clone()), parent)
            })
        })
        .filter_map(|(table, parent)| parent.map(|p| (table, p)))
        .collect();
    let mut depths = HashMap::new();
    for schema in &database.schemas {
        for table in &schema.tables {
            let key = (schema.name.clone(), table.name.clone());
            let mut depth = 0u8;
            let mut at = &key;
            while let Some(parent) = parents.get(at) {
                depth = depth.saturating_add(1);
                at = parent;
                if depth == u8::MAX {
                    break; // A cycle, which PostgreSQL wouldn't allow.
                }
            }
            depths.insert(key, depth);
        }
    }
    depths
}

type ViewKey = (String, String);

fn all_views(database: &Database) -> BTreeMap<ViewKey, &View> {
    database
        .schemas
        .iter()
        .flat_map(|s| {
            s.views
                .iter()
                .map(|v| ((s.name.clone(), v.name.clone()), v))
        })
        .collect()
}

fn same_view(a: &View, b: &View) -> bool {
    a.materialized == b.materialized && a.definition == b.definition
}

/// Orders views so each comes after the views its definition names. Definitions
/// name every schema (introspection runs with an empty search path), so a view
/// reads another when it mentions `schema.name`.
fn dependency_order(keys: &[ViewKey], views: &BTreeMap<ViewKey, &View>) -> Vec<ViewKey> {
    let reads = |view: &View, other: &ViewKey| {
        let name = qualified(&other.0, &other.1);
        view.definition.match_indices(&name).any(|(i, _)| {
            let after = view.definition[i + name.len()..].chars().next();
            !after.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
    };
    let mut remaining: BTreeSet<ViewKey> = keys.iter().cloned().collect();
    let mut ordered = Vec::new();
    while !remaining.is_empty() {
        let ready: Vec<ViewKey> = remaining
            .iter()
            .filter(|key| {
                !remaining
                    .iter()
                    .any(|other| other != *key && reads(views[*key], other))
            })
            .cloned()
            .collect();
        // A cycle can't happen between real views; take the rest as they come.
        let next = if ready.is_empty() {
            remaining.iter().cloned().collect()
        } else {
            ready
        };
        for key in next {
            remaining.remove(&key);
            ordered.push(key);
        }
    }
    ordered
}

fn foreign_keys(table: &Table) -> impl Iterator<Item = &Constraint> {
    table
        .constraints
        .iter()
        .filter(|c| c.kind == ConstraintKind::ForeignKey)
}

fn column_name(table: &str, column: &Column) -> String {
    format!("{table}.{}", quote_ident(&column.name))
}

fn identity_clause(identity: Identity) -> String {
    format!("{} AS IDENTITY", generated_clause(identity))
}

fn generated_clause(identity: Identity) -> &'static str {
    match identity {
        Identity::Always => "GENERATED ALWAYS",
        Identity::ByDefault => "GENERATED BY DEFAULT",
    }
}

fn empty_schema(name: &str) -> Schema {
    Schema {
        name: name.to_owned(),
        tables: Vec::new(),
        views: Vec::new(),
        functions: Vec::new(),
        sequences: Vec::new(),
        enums: Vec::new(),
    }
}

fn find_table<'a>(database: &'a Database, schema: &str, table: &str) -> Option<&'a Table> {
    database
        .schemas
        .iter()
        .find(|s| s.name == schema)?
        .tables
        .iter()
        .find(|t| t.name == table)
}

fn find_table_mut<'a>(
    database: &'a mut Database,
    schema: &str,
    table: &str,
) -> Option<&'a mut Table> {
    database
        .schemas
        .iter_mut()
        .find(|s| s.name == schema)?
        .tables
        .iter_mut()
        .find(|t| t.name == table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Generated;

    fn column(name: &str, data_type: &str) -> Column {
        Column {
            name: name.into(),
            data_type: data_type.into(),
            nullable: true,
            default: None,
            identity: None,
            generated: None,
            comment: None,
        }
    }

    fn table(name: &str, columns: Vec<Column>, constraints: Vec<Constraint>) -> Table {
        Table {
            name: name.into(),
            kind: crate::schema::TableKind::Table,
            columns,
            constraints,
            indexes: vec![],
            partition_key: None,
            partition_of: None,
            comment: None,
        }
    }

    fn constraint(name: &str, kind: ConstraintKind, definition: &str) -> Constraint {
        Constraint {
            name: name.into(),
            kind,
            definition: definition.into(),
        }
    }

    fn database(schema: Schema) -> Database {
        Database {
            schemas: vec![schema],
            extensions: vec![],
        }
    }

    fn app(tables: Vec<Table>) -> Schema {
        Schema {
            tables,
            ..empty_schema("app")
        }
    }

    fn sql(plan: &Plan) -> Vec<&str> {
        plan.steps.iter().map(|s| s.sql.as_str()).collect()
    }

    #[test]
    fn refuses_definitions_that_hold_more_than_one_statement() {
        let current = database(app(vec![table("t", vec![column("id", "integer")], vec![])]));
        let mut target = current.clone();
        let smuggled = column("note", "text; DROP TABLE app.t; --");
        target.schemas[0].tables[0].columns.push(smuggled);
        let planned = plan(&current, &target, &[]);
        assert_eq!(
            planned.blockers,
            [Blocker::InvalidDefinition {
                name: "app.t.note".into()
            }]
        );

        // Defaults and checks with semicolons in their strings are fine.
        let mut target = current.clone();
        let mut note = column("note", "text");
        note.default = Some("'a;b'::text".into());
        target.schemas[0].tables[0].columns.push(note);
        target.schemas[0].tables[0].constraints.push(constraint(
            "t_check",
            ConstraintKind::Check,
            "CHECK (id <> 0)",
        ));
        let planned = plan(&current, &target, &[]);
        assert!(planned.blockers.is_empty(), "{:?}", planned.blockers);
        // Each step is one statement.
        assert!(
            planned
                .steps
                .iter()
                .all(|s| crate::sql::split_statements(&s.sql).len() == 1)
        );

        let mut target = current.clone();
        target.schemas[0].tables[0].columns[0].data_type = "integer\u{202E}".into();
        assert!(!plan(&current, &target, &[]).blockers.is_empty());
    }

    #[test]
    fn plans_nothing_for_the_same_database() {
        let db = database(app(vec![table(
            "users",
            vec![column("id", "bigint")],
            vec![],
        )]));
        let plan = plan(&db, &db, &[]);
        assert!(
            plan.steps.is_empty() && plan.blockers.is_empty() && plan.rename_candidates.is_empty()
        );
    }

    #[test]
    fn creates_tables_before_the_foreign_keys_between_them() {
        let current = Database {
            schemas: vec![],
            extensions: vec![],
        };
        let target = database(app(vec![
            table(
                "posts",
                vec![column("id", "integer"), column("author_id", "integer")],
                vec![constraint(
                    "posts_author_id_fkey",
                    ConstraintKind::ForeignKey,
                    "FOREIGN KEY (author_id) REFERENCES app.users(id)",
                )],
            ),
            table(
                "users",
                vec![column("id", "integer")],
                vec![constraint(
                    "users_pkey",
                    ConstraintKind::PrimaryKey,
                    "PRIMARY KEY (id)",
                )],
            ),
        ]));

        let plan = plan(&current, &target, &[]);
        assert_eq!(
            sql(&plan),
            [
                "CREATE SCHEMA app;",
                "CREATE TABLE app.posts (\n    id integer,\n    author_id integer\n);",
                "CREATE TABLE app.users (\n    id integer,\n    CONSTRAINT users_pkey PRIMARY KEY (id)\n);",
                "ALTER TABLE app.posts ADD CONSTRAINT posts_author_id_fkey FOREIGN KEY (author_id) REFERENCES app.users(id);",
            ]
        );
        assert!(!plan.is_destructive());
    }

    #[test]
    fn marks_steps_that_lose_data() {
        let current = database(app(vec![
            table("old", vec![column("id", "integer")], vec![]),
            table(
                "users",
                vec![column("id", "integer"), column("legacy", "text")],
                vec![],
            ),
        ]));
        let target = database(app(vec![table(
            "users",
            vec![
                column("id", "bigint"),
                column("email", "character varying(255)"),
            ],
            vec![],
        )]));

        let plan = plan(&current, &target, &[]);
        let destructive: Vec<&str> = plan
            .steps
            .iter()
            .filter(|s| s.destructive)
            .map(|s| s.sql.as_str())
            .collect();
        assert_eq!(
            destructive,
            [
                "DROP TABLE app.old;",
                "ALTER TABLE app.users DROP COLUMN legacy;",
                "ALTER TABLE app.users ALTER COLUMN id TYPE bigint;",
            ]
        );
        assert!(
            sql(&plan).contains(&"ALTER TABLE app.users ADD COLUMN email character varying(255);")
        );
    }

    #[test]
    fn adds_enum_values_in_place_and_blocks_removing_them() {
        let mood = |labels: &[&str]| {
            database(Schema {
                enums: vec![EnumType {
                    name: "mood".into(),
                    labels: labels.iter().map(|l| l.to_string()).collect(),
                }],
                ..empty_schema("app")
            })
        };

        let grow = plan(&mood(&["ok"]), &mood(&["sad", "ok", "happy"]), &[]);
        assert_eq!(
            sql(&grow),
            [
                "ALTER TYPE app.mood ADD VALUE 'sad' BEFORE 'ok';",
                "ALTER TYPE app.mood ADD VALUE 'happy' AFTER 'ok';",
            ]
        );
        let shrink = plan(&mood(&["sad", "ok"]), &mood(&["ok"]), &[]);
        assert_eq!(shrink.blockers.len(), 1);
    }

    #[test]
    fn suggests_renames_and_keeps_data_when_they_are_taken() {
        let current = database(app(vec![table(
            "users",
            vec![column("id", "integer"), column("email", "text")],
            vec![],
        )]));
        let target = database(app(vec![table(
            "users",
            vec![column("id", "integer"), column("mail", "text")],
            vec![],
        )]));

        let blind = plan(&current, &target, &[]);
        assert!(blind.is_destructive());
        let rename = Rename::Column {
            schema: "app".into(),
            table: "users".into(),
            from: "email".into(),
            to: "mail".into(),
        };
        assert_eq!(blind.rename_candidates, std::slice::from_ref(&rename));

        let renamed = plan(&current, &target, &[rename]);
        assert_eq!(
            sql(&renamed),
            ["ALTER TABLE app.users RENAME COLUMN email TO mail;"]
        );
        assert!(!renamed.is_destructive());
    }

    #[test]
    fn recreates_views_when_a_table_is_reshaped() {
        let view = View {
            name: "ids".into(),
            materialized: false,
            columns: vec![column("id", "integer")],
            definition: " SELECT id\n   FROM app.users;".into(),
            comment: None,
        };
        let with = |columns: Vec<Column>| {
            database(Schema {
                views: vec![view.clone()],
                ..app(vec![table("users", columns, vec![])])
            })
        };

        let plan = plan(
            &with(vec![column("id", "integer"), column("legacy", "text")]),
            &with(vec![column("id", "integer")]),
            &[],
        );
        assert_eq!(
            sql(&plan),
            [
                "DROP VIEW app.ids;",
                "ALTER TABLE app.users DROP COLUMN legacy;",
                "CREATE VIEW app.ids AS\n SELECT id\n   FROM app.users;",
            ]
        );
    }

    #[test]
    fn orders_column_changes_so_each_one_is_allowed() {
        let mut current = column("id", "integer");
        current.default = Some("0".into());
        let mut target = column("id", "integer");
        target.nullable = false;
        target.identity = Some(Identity::Always);
        let plan = plan(
            &database(app(vec![table("t", vec![current], vec![])])),
            &database(app(vec![table("t", vec![target], vec![])])),
            &[],
        );
        assert_eq!(
            sql(&plan),
            [
                "ALTER TABLE app.t ALTER COLUMN id SET NOT NULL;",
                "ALTER TABLE app.t ALTER COLUMN id DROP DEFAULT;",
                "ALTER TABLE app.t ALTER COLUMN id ADD GENERATED ALWAYS AS IDENTITY;",
            ]
        );
    }

    #[test]
    fn blocks_turning_a_column_into_a_generated_one() {
        let mut target = column("n", "integer");
        target.generated = Some(Generated {
            expression: "1".into(),
            stored: true,
        });
        let plan = plan(
            &database(app(vec![table("t", vec![column("n", "integer")], vec![])])),
            &database(app(vec![table("t", vec![target], vec![])])),
            &[],
        );
        assert_eq!(plan.blockers.len(), 1);
    }
}
