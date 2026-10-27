//! Changes to one table, view or function, and new ones, as made in the app's
//! detail view.
//!
//! An edit replaces the object in a copy of the database's model, and the
//! migration planner works out the statements, so edits get the same ordering,
//! data-loss flags and blockers as applying a snapshot.

use serde::Deserialize;

use crate::Error;
use crate::postgres::ddl::{self, qualified};
use crate::postgres::migrate::{self, Plan, Rename, Step};
use crate::schema::{Database, Schema, Table, View};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Edit {
    /// Table `name` becomes `table`, which may have a new name. Columns in
    /// `renamed_columns` (old name, new name) are renamed, keeping their data.
    Table {
        schema: String,
        name: String,
        table: Table,
        #[serde(default)]
        renamed_columns: Vec<(String, String)>,
    },
    /// View `name` becomes `view`: a new definition or comment. A plain view is
    /// replaced in place, keeping what depends on it; a materialized one is
    /// dropped and created again, which fills it anew.
    View {
        schema: String,
        name: String,
        view: View,
    },
    /// The function `name(arguments)` gets a new `CREATE OR REPLACE` definition.
    Function {
        schema: String,
        name: String,
        arguments: String,
        definition: String,
    },
    /// A new table in `schema`.
    CreateTable { schema: String, table: Table },
    /// A new view in `schema`.
    CreateView { schema: String, view: View },
    /// A new function or procedure, from its `CREATE FUNCTION` or `CREATE PROCEDURE` statement.
    CreateFunction { schema: String, definition: String },
}

impl Edit {
    /// The schema the edited or new object is in.
    pub fn schema(&self) -> &str {
        match self {
            Edit::Table { schema, .. }
            | Edit::View { schema, .. }
            | Edit::Function { schema, .. }
            | Edit::CreateTable { schema, .. }
            | Edit::CreateView { schema, .. }
            | Edit::CreateFunction { schema, .. } => schema,
        }
    }
}

/// The steps that make the object in `current` match the edit.
pub fn plan(current: &Database, edit: &Edit) -> Result<Plan, Error> {
    let mut target = current.clone();
    let mut renames = Vec::new();
    match edit {
        Edit::Table {
            schema,
            name,
            table,
            renamed_columns,
        } => {
            let slot = schema_mut(&mut target, schema)?
                .tables
                .iter_mut()
                .find(|t| &t.name == name)
                .ok_or_else(|| missing("table", schema, name))?;
            *slot = table.clone();
            // The table is renamed first, so the column renames name it by its new name.
            if &table.name != name {
                renames.push(Rename::Table {
                    schema: schema.clone(),
                    from: name.clone(),
                    to: table.name.clone(),
                });
            }
            for (from, to) in renamed_columns.iter().filter(|(from, to)| from != to) {
                renames.push(Rename::Column {
                    schema: schema.clone(),
                    table: table.name.clone(),
                    from: from.clone(),
                    to: to.clone(),
                });
            }
        }
        Edit::View { schema, name, view } => {
            let current_view = current
                .schemas
                .iter()
                .find(|s| &s.name == schema)
                .and_then(|s| s.views.iter().find(|v| &v.name == name))
                .ok_or_else(|| missing("view", schema, name))?;
            if view.name != *name || view.materialized != current_view.materialized {
                return Err(unsupported(
                    "renaming a view or changing whether it is materialized",
                ));
            }
            check_query(view)?;
            return Ok(view_plan(schema, current_view, view));
        }
        Edit::Function {
            schema,
            name,
            arguments,
            definition,
        } => {
            let slot = schema_mut(&mut target, schema)?
                .functions
                .iter_mut()
                .find(|f| &f.name == name && &f.arguments == arguments)
                .ok_or_else(|| missing("function", schema, &format!("{name}({arguments})")))?;
            slot.definition = definition
                .trim()
                .trim_end_matches(';')
                .trim_end()
                .to_owned();
        }
        Edit::CreateTable { schema, table } => {
            let tables = schema_mut(&mut target, schema)?;
            check_new_relation(tables, &table.name)?;
            tables.tables.push(table.clone());
        }
        Edit::CreateView { schema, view } => {
            check_new_relation(schema_mut(&mut target, schema)?, &view.name)?;
            check_query(view)?;
            return Ok(steps_plan([ddl::create_view(schema, view)]));
        }
        Edit::CreateFunction { schema, definition } => {
            schema_mut(&mut target, schema)?;
            let statement = definition.trim().trim_end_matches(';').trim_end();
            let words: Vec<String> = statement
                .split_whitespace()
                .take(4)
                .map(str::to_uppercase)
                .collect();
            let creates = match words.as_slice() {
                [create, kind, ..] if create == "CREATE" => {
                    kind == "FUNCTION"
                        || kind == "PROCEDURE"
                        || (kind == "OR"
                            && words.get(2).is_some_and(|w| w == "REPLACE")
                            && words
                                .get(3)
                                .is_some_and(|w| w == "FUNCTION" || w == "PROCEDURE"))
                }
                _ => false,
            };
            if !creates || crate::sql::split_statements(statement).len() != 1 {
                return Err(invalid(
                    "write one CREATE FUNCTION or CREATE PROCEDURE statement",
                ));
            }
            return Ok(steps_plan([format!("{statement};")]));
        }
    }
    Ok(migrate::plan_keeping_views(current, &target, &renames))
}

fn view_plan(schema: &str, current: &View, target: &View) -> Plan {
    let name = qualified(schema, &target.name);
    let object = if target.materialized {
        "MATERIALIZED VIEW"
    } else {
        "VIEW"
    };
    let same_query = |a: &View, b: &View| {
        a.definition.trim().trim_end_matches(';').trim_end()
            == b.definition.trim().trim_end_matches(';').trim_end()
    };
    let mut steps = Vec::new();
    if !same_query(current, target) {
        if target.materialized {
            steps.push(format!("DROP MATERIALIZED VIEW {name};"));
            steps.push(ddl::create_view(schema, target));
        } else {
            // create_view writes the comments too, which keeps them as they are.
            steps.push(ddl::create_view(schema, target).replacen(
                "CREATE VIEW ",
                "CREATE OR REPLACE VIEW ",
                1,
            ));
        }
    } else if current.comment != target.comment {
        steps.push(ddl::comment_on(object, &name, target.comment.as_deref()));
    }
    steps_plan(steps)
}

/// A plan of statements that lose no data, run in order, one statement a step.
fn steps_plan(steps: impl IntoIterator<Item = String>) -> Plan {
    Plan {
        steps: steps
            .into_iter()
            .flat_map(|sql| migrate::statements(&sql))
            .map(|sql| Step {
                sql,
                destructive: false,
            })
            .collect(),
        blockers: Vec::new(),
        rename_candidates: Vec::new(),
    }
}

/// A view's query must be one statement, so its step can't run another.
fn check_query(view: &View) -> Result<(), Error> {
    if view.definition.trim().is_empty() {
        return Err(invalid("the view needs a query"));
    }
    if !migrate::single(&view.definition, true) {
        return Err(invalid(
            "the view's query must be a single SELECT statement",
        ));
    }
    Ok(())
}

/// Tables, views and sequences share their schema's names, so a new one needs a free name.
fn check_new_relation(schema: &Schema, name: &str) -> Result<(), Error> {
    if name.trim().is_empty() {
        return Err(invalid("a name is needed"));
    }
    let taken = schema.tables.iter().any(|t| t.name == name)
        || schema.views.iter().any(|v| v.name == name)
        || schema.sequences.iter().any(|s| s.name == name);
    if taken {
        return Err(invalid(&format!("{}.{name} already exists", schema.name)));
    }
    Ok(())
}

fn invalid(message: &str) -> Error {
    Error::Storage {
        message: message.to_owned(),
    }
}

fn schema_mut<'a>(database: &'a mut Database, name: &str) -> Result<&'a mut Schema, Error> {
    database
        .schemas
        .iter_mut()
        .find(|s| s.name == name)
        .ok_or_else(|| Error::Storage {
            message: format!("schema {name} no longer exists; reload the schema"),
        })
}

fn missing(kind: &str, schema: &str, name: &str) -> Error {
    Error::Storage {
        message: format!("{kind} {schema}.{name} no longer exists; reload the schema"),
    }
}

fn unsupported(what: &str) -> Error {
    Error::Storage {
        message: format!("{what} isn't supported here; write the SQL in the editor"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Column, Constraint, ConstraintKind, TableKind};

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

    fn database() -> Database {
        Database {
            schemas: vec![Schema {
                name: "app".into(),
                tables: vec![Table {
                    name: "users".into(),
                    kind: TableKind::Table,
                    columns: vec![column("id", "bigint"), column("name", "text")],
                    constraints: vec![Constraint {
                        name: "users_pkey".into(),
                        kind: ConstraintKind::PrimaryKey,
                        definition: "PRIMARY KEY (id)".into(),
                    }],
                    indexes: vec![],
                    partition_key: None,
                    partition_of: None,
                    grants: vec![],
                    comment: None,
                }],
                views: vec![],
                functions: vec![],
                sequences: vec![],
                enums: vec![],
            }],
            extensions: vec![],
        }
    }

    fn sql(plan: &Plan) -> Vec<&str> {
        plan.steps.iter().map(|s| s.sql.as_str()).collect()
    }

    #[test]
    fn renames_tables_and_columns_and_changes_the_rest() {
        let mut table = database().schemas[0].tables[0].clone();
        table.name = "people".into();
        table.columns[1].name = "full_name".into();
        table.columns[1].nullable = false;
        table.columns.push(column("email", "text"));
        table.comment = Some("who signs in".into());
        let plan = plan(
            &database(),
            &Edit::Table {
                schema: "app".into(),
                name: "users".into(),
                table,
                renamed_columns: vec![("name".into(), "full_name".into())],
            },
        )
        .unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        assert_eq!(
            sql(&plan),
            [
                "ALTER TABLE app.users RENAME TO people;",
                "ALTER TABLE app.people RENAME COLUMN name TO full_name;",
                "ALTER TABLE app.people ADD COLUMN email text;",
                "ALTER TABLE app.people ALTER COLUMN full_name SET NOT NULL;",
                "COMMENT ON TABLE app.people IS 'who signs in';",
            ]
        );
        assert!(!plan.is_destructive());
    }

    #[test]
    fn marks_dropped_columns_as_losing_data() {
        let mut table = database().schemas[0].tables[0].clone();
        table.columns.pop();
        let plan = plan(
            &database(),
            &Edit::Table {
                schema: "app".into(),
                name: "users".into(),
                table,
                renamed_columns: vec![],
            },
        )
        .unwrap();
        assert_eq!(sql(&plan), ["ALTER TABLE app.users DROP COLUMN name;"]);
        assert!(plan.is_destructive());
    }

    #[test]
    fn creates_tables_with_their_keys_after_them() {
        let mut table = database().schemas[0].tables[0].clone();
        table.name = "posts".into();
        table.constraints = vec![
            Constraint {
                name: "posts_pkey".into(),
                kind: ConstraintKind::PrimaryKey,
                definition: "PRIMARY KEY (id)".into(),
            },
            Constraint {
                name: "posts_id_fkey".into(),
                kind: ConstraintKind::ForeignKey,
                definition: "FOREIGN KEY (id) REFERENCES app.users(id)".into(),
            },
        ];
        let plan = plan(
            &database(),
            &Edit::CreateTable {
                schema: "app".into(),
                table: table.clone(),
            },
        )
        .unwrap();
        let steps = sql(&plan);
        assert!(steps[0].starts_with("CREATE TABLE app.posts"), "{steps:?}");
        assert_eq!(
            steps.last(),
            Some(
                &"ALTER TABLE app.posts ADD CONSTRAINT posts_id_fkey FOREIGN KEY (id) REFERENCES app.users(id);"
            )
        );
        assert!(!plan.is_destructive());

        table.name = "users".into();
        let taken = Edit::CreateTable {
            schema: "app".into(),
            table,
        };
        assert!(plan_err(&taken).contains("already exists"));
    }

    #[test]
    fn creates_views_and_functions_from_their_statements() {
        let view = View {
            name: "names".into(),
            materialized: false,
            columns: vec![],
            definition: "SELECT name FROM app.users".into(),
            comment: Some("just names".into()),
            indexes: vec![],
            grants: vec![],
        };
        let edit = Edit::CreateView {
            schema: "app".into(),
            view,
        };
        assert_eq!(
            sql(&plan(&database(), &edit).unwrap()),
            [
                "CREATE VIEW app.names AS\nSELECT name FROM app.users;",
                "COMMENT ON VIEW app.names IS 'just names';"
            ]
        );

        let function = |definition: &str| Edit::CreateFunction {
            schema: "app".into(),
            definition: definition.into(),
        };
        assert_eq!(
            sql(&plan(
                &database(),
                &function(
                    "create or replace function app.one() returns int language sql as 'select 1';"
                )
            )
            .unwrap()),
            ["create or replace function app.one() returns int language sql as 'select 1';"]
        );
        assert!(plan_err(&function("DROP TABLE app.users")).contains("CREATE FUNCTION"));
    }

    fn plan_err(edit: &Edit) -> String {
        plan(&database(), edit).unwrap_err().to_string()
    }

    #[test]
    fn reports_objects_that_are_gone() {
        let edit = Edit::Table {
            schema: "app".into(),
            name: "missing".into(),
            table: database().schemas[0].tables[0].clone(),
            renamed_columns: vec![],
        };
        assert!(plan(&database(), &edit).is_err());
    }
}
