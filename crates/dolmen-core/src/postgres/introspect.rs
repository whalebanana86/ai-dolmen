//! Reads a PostgreSQL database's structure from the system catalogs.
//!
//! Definitions come from the server's own `pg_get_*def` functions, the same
//! ones `pg_dump` uses. System schemas and objects created by extensions are
//! left out.

use std::collections::HashMap;

use tokio_postgres::GenericClient;

use crate::Error;
use crate::schema::{
    Column, Constraint, ConstraintKind, Database, EnumType, Extension, ForeignKey, Function,
    FunctionKind, Generated, Grant, Identity, Index, PartitionOf, Schema, Sequence, Table,
    TableKind, View,
};

/// Matches user schemas through the alias `n` for `pg_namespace`.
const USER_SCHEMAS: &str = "n.nspname NOT IN ('pg_catalog', 'information_schema') \
     AND n.nspname NOT LIKE 'pg\\_toast%' AND n.nspname NOT LIKE 'pg\\_temp%'";

/// Matches objects that no extension owns, given the object's oid expression.
fn not_from_extension(oid: &str) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend dep \
         WHERE dep.objid = {oid} AND dep.deptype = 'e')"
    )
}

/// Every foreign key between user tables, ordered by table and name.
pub(super) async fn foreign_keys(client: &impl GenericClient) -> Result<Vec<ForeignKey>, Error> {
    // Column names in key order; conkey and confkey list attribute numbers.
    let columns = |keys: &str, relation: &str| {
        format!(
            "ARRAY(SELECT a.attname::text FROM unnest(con.{keys}) WITH ORDINALITY k(attnum, i)
                   JOIN pg_catalog.pg_attribute a ON a.attrelid = con.{relation} AND a.attnum = k.attnum
                   ORDER BY k.i)"
        )
    };
    let rows = client
        .query(
            &format!(
                "SELECT con.conname::text, n.nspname::text, c.relname::text, {},
                        rn.nspname::text, rc.relname::text, {},
                        con.confupdtype::text, con.confdeltype::text
                 FROM pg_catalog.pg_constraint con
                 JOIN pg_catalog.pg_class c ON c.oid = con.conrelid
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                 JOIN pg_catalog.pg_class rc ON rc.oid = con.confrelid
                 JOIN pg_catalog.pg_namespace rn ON rn.oid = rc.relnamespace
                 WHERE con.contype = 'f' AND con.conparentid = 0 AND {USER_SCHEMAS}
                   AND {} AND {} AND {}
                 ORDER BY n.nspname COLLATE \"C\", c.relname COLLATE \"C\", con.conname COLLATE \"C\"",
                columns("conkey", "conrelid"),
                columns("confkey", "confrelid"),
                // Both tables must be in the model, which leaves out extensions' tables.
                USER_SCHEMAS.replace("n.nspname", "rn.nspname"),
                not_from_extension("c.oid"),
                not_from_extension("rc.oid"),
            ),
            &[],
        )
        .await?;
    let action = |code: &str| {
        match code {
            "r" => "RESTRICT",
            "c" => "CASCADE",
            "n" => "SET NULL",
            "d" => "SET DEFAULT",
            _ => "NO ACTION",
        }
        .to_owned()
    };
    Ok(rows
        .iter()
        .map(|row| ForeignKey {
            name: row.get(0),
            schema: row.get(1),
            table: row.get(2),
            columns: row.get(3),
            referenced_schema: row.get(4),
            referenced_table: row.get(5),
            referenced_columns: row.get(6),
            on_update: action(row.get(7)),
            on_delete: action(row.get(8)),
        })
        .collect())
}

enum Relation {
    Table { schema: usize, table: usize },
    View { schema: usize, view: usize },
}

pub(super) async fn introspect(client: &impl GenericClient) -> Result<Database, Error> {
    let mut schemas: Vec<Schema> = client
        .query(
            &format!(
                "SELECT n.nspname FROM pg_catalog.pg_namespace n
                 WHERE {USER_SCHEMAS} AND {}
                 ORDER BY n.nspname COLLATE \"C\"",
                not_from_extension("n.oid")
            ),
            &[],
        )
        .await?
        .into_iter()
        .map(|row| Schema {
            name: row.get(0),
            tables: Vec::new(),
            views: Vec::new(),
            functions: Vec::new(),
            sequences: Vec::new(),
            enums: Vec::new(),
        })
        .collect();
    let schema_index: HashMap<String, usize> = schemas
        .iter()
        .enumerate()
        .map(|(i, schema)| (schema.name.clone(), i))
        .collect();

    let mut relations = HashMap::new();
    for row in client
        .query(
            &format!(
                "SELECT c.oid, n.nspname, c.relname, c.relkind::text,
                        CASE WHEN c.relkind IN ('v', 'm') THEN pg_catalog.pg_get_viewdef(c.oid, true) END,
                        pg_catalog.obj_description(c.oid, 'pg_class'),
                        CASE WHEN c.relkind = 'p' THEN pg_catalog.pg_get_partkeydef(c.oid) END,
                        CASE WHEN c.relispartition THEN pg_catalog.pg_get_expr(c.relpartbound, c.oid) END,
                        parent.nspname::text, parent.relname::text
                 FROM pg_catalog.pg_class c
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                 LEFT JOIN LATERAL (
                     SELECT pn.nspname, p.relname
                     FROM pg_catalog.pg_inherits inh
                     JOIN pg_catalog.pg_class p ON p.oid = inh.inhparent
                     JOIN pg_catalog.pg_namespace pn ON pn.oid = p.relnamespace
                     WHERE inh.inhrelid = c.oid AND c.relispartition
                 ) parent ON true
                 WHERE c.relkind IN ('r', 'p', 'f', 'v', 'm') AND {USER_SCHEMAS} AND {}
                 ORDER BY n.nspname COLLATE \"C\", c.relname COLLATE \"C\"",
                not_from_extension("c.oid")
            ),
            &[],
        )
        .await?
    {
        let oid: u32 = row.get(0);
        let Some(&schema) = schema_index.get(row.get::<_, &str>(1)) else {
            continue;
        };
        let name: String = row.get(2);
        let comment: Option<String> = row.get(5);
        let kind = match row.get::<_, &str>(3) {
            "r" => Some(TableKind::Table),
            "p" => Some(TableKind::Partitioned),
            "f" => Some(TableKind::Foreign),
            _ => None,
        };
        let relation = match kind {
            Some(kind) => {
                let tables = &mut schemas[schema].tables;
                tables.push(Table {
                    name,
                    kind,
                    columns: Vec::new(),
                    constraints: Vec::new(),
                    indexes: Vec::new(),
                    partition_key: row.get(6),
                    comment,
                    partition_of: match (row.get(7), row.get(8), row.get(9)) {
                        (Some(bound), Some(schema), Some(table)) => Some(PartitionOf {
                            schema,
                            table,
                            bound,
                        }),
                        _ => None,
                    },
                    grants: Vec::new(),
                });
                Relation::Table {
                    schema,
                    table: tables.len() - 1,
                }
            }
            None => {
                let views = &mut schemas[schema].views;
                views.push(View {
                    name,
                    materialized: row.get::<_, &str>(3) == "m",
                    columns: Vec::new(),
                    definition: row.get::<_, Option<String>>(4).unwrap_or_default(),
                    comment,
                    indexes: Vec::new(),
                    grants: Vec::new(),
                });
                Relation::View {
                    schema,
                    view: views.len() - 1,
                }
            }
        };
        relations.insert(oid, relation);
    }

    for row in client
        .query(
            &format!(
                "SELECT a.attrelid, a.attname, pg_catalog.format_type(a.atttypid, a.atttypmod),
                        a.attnotnull, pg_catalog.pg_get_expr(d.adbin, d.adrelid),
                        a.attidentity::text, a.attgenerated::text,
                        pg_catalog.col_description(a.attrelid, a.attnum)
                 FROM pg_catalog.pg_attribute a
                 JOIN pg_catalog.pg_class c ON c.oid = a.attrelid
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                 LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
                 WHERE a.attnum > 0 AND NOT a.attisdropped
                   AND c.relkind IN ('r', 'p', 'f', 'v', 'm') AND {USER_SCHEMAS}
                 ORDER BY a.attrelid, a.attnum"
            ),
            &[],
        )
        .await?
    {
        let Some(relation) = relations.get(&row.get::<_, u32>(0)) else {
            continue;
        };
        // A generated column's expression is stored as its default.
        let expression: Option<String> = row.get(4);
        let generated = match row.get::<_, &str>(6) {
            "s" => Some(true),
            "v" => Some(false),
            _ => None,
        };
        let column = Column {
            name: row.get(1),
            data_type: row.get(2),
            nullable: !row.get::<_, bool>(3),
            default: if generated.is_some() {
                None
            } else {
                expression.clone()
            },
            identity: match row.get::<_, &str>(5) {
                "a" => Some(Identity::Always),
                "d" => Some(Identity::ByDefault),
                _ => None,
            },
            generated: generated
                .zip(expression)
                .map(|(stored, expression)| Generated { expression, stored }),
            comment: row.get(7),
        };
        match *relation {
            Relation::Table { schema, table } => schemas[schema].tables[table].columns.push(column),
            Relation::View { schema, view } => schemas[schema].views[view].columns.push(column),
        }
    }

    for row in client
        .query(
            &format!(
                "SELECT con.conrelid, con.conname, con.contype::text,
                        pg_catalog.pg_get_constraintdef(con.oid, true)
                 FROM pg_catalog.pg_constraint con
                 JOIN pg_catalog.pg_namespace n ON n.oid = con.connamespace
                 WHERE con.contype IN ('p', 'u', 'c', 'f', 'x') AND con.conrelid <> 0
                   -- Copies PostgreSQL keeps on partitions belong to the parent's constraint.
                   AND con.conparentid = 0
                   AND {USER_SCHEMAS}
                 ORDER BY con.conrelid, con.conname COLLATE \"C\""
            ),
            &[],
        )
        .await?
    {
        let Some(&Relation::Table { schema, table }) = relations.get(&row.get::<_, u32>(0)) else {
            continue;
        };
        let kind = match row.get::<_, &str>(2) {
            "p" => ConstraintKind::PrimaryKey,
            "u" => ConstraintKind::Unique,
            "c" => ConstraintKind::Check,
            "f" => ConstraintKind::ForeignKey,
            _ => ConstraintKind::Exclusion,
        };
        schemas[schema].tables[table].constraints.push(Constraint {
            name: row.get(1),
            kind,
            definition: row.get(3),
        });
    }

    for row in client
        .query(
            &format!(
                "SELECT i.indrelid, ic.relname, pg_catalog.pg_get_indexdef(i.indexrelid)
                 FROM pg_catalog.pg_index i
                 JOIN pg_catalog.pg_class ic ON ic.oid = i.indexrelid
                 JOIN pg_catalog.pg_namespace n ON n.oid = ic.relnamespace
                 WHERE {USER_SCHEMAS}
                   AND NOT EXISTS (
                     SELECT 1 FROM pg_catalog.pg_constraint con
                     WHERE con.conrelid = i.indrelid AND con.conindid = i.indexrelid
                       AND con.contype IN ('p', 'u', 'x'))
                   -- A partition's index attached to its parent's comes with the parent's.
                   AND NOT EXISTS (
                     SELECT 1 FROM pg_catalog.pg_inherits inh WHERE inh.inhrelid = i.indexrelid)
                 ORDER BY i.indrelid, ic.relname COLLATE \"C\""
            ),
            &[],
        )
        .await?
    {
        let index = Index {
            name: row.get(1),
            definition: row.get(2),
        };
        match relations.get(&row.get::<_, u32>(0)) {
            Some(&Relation::Table { schema, table }) => {
                schemas[schema].tables[table].indexes.push(index)
            }
            Some(&Relation::View { schema, view }) => {
                schemas[schema].views[view].indexes.push(index)
            }
            None => {}
        }
    }

    // Privileges besides the owner's, one entry per role and grant option.
    for row in client
        .query(
            &format!(
                "SELECT relation, role, grantable, privilege FROM (
                     SELECT c.oid AS relation,
                            CASE WHEN a.grantee = 0 THEN 'PUBLIC'
                                 ELSE pg_catalog.pg_get_userbyid(a.grantee)::text END AS role,
                            a.is_grantable AS grantable,
                            a.privilege_type AS privilege
                     FROM pg_catalog.pg_class c
                     JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                     CROSS JOIN LATERAL pg_catalog.aclexplode(c.relacl) a
                     WHERE c.relkind IN ('r', 'p', 'f', 'v', 'm') AND {USER_SCHEMAS}
                       AND a.grantee <> c.relowner
                 ) privileges
                 ORDER BY relation, role COLLATE \"C\", grantable, privilege COLLATE \"C\""
            ),
            &[],
        )
        .await?
    {
        let grants = match relations.get(&row.get::<_, u32>(0)) {
            Some(&Relation::Table { schema, table }) => &mut schemas[schema].tables[table].grants,
            Some(&Relation::View { schema, view }) => &mut schemas[schema].views[view].grants,
            None => continue,
        };
        let grantee: String = row.get(1);
        let grantable: bool = row.get(2);
        let privilege: String = row.get(3);
        match grants.last_mut() {
            Some(last) if last.grantee == grantee && last.grantable == grantable => {
                last.privileges.push(privilege)
            }
            _ => grants.push(Grant {
                grantee,
                privileges: vec![privilege],
                grantable,
            }),
        }
    }

    for row in client
        .query(
            &format!(
                "SELECT n.nspname, p.proname, pg_catalog.pg_get_function_identity_arguments(p.oid),
                        p.prokind::text, pg_catalog.pg_get_functiondef(p.oid)
                 FROM pg_catalog.pg_proc p
                 JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace
                 WHERE p.prokind IN ('f', 'p') AND {USER_SCHEMAS} AND {}
                 ORDER BY n.nspname COLLATE \"C\", p.proname COLLATE \"C\",
                          pg_catalog.pg_get_function_identity_arguments(p.oid) COLLATE \"C\"",
                not_from_extension("p.oid")
            ),
            &[],
        )
        .await?
    {
        let Some(&schema) = schema_index.get(row.get::<_, &str>(0)) else {
            continue;
        };
        schemas[schema].functions.push(Function {
            name: row.get(1),
            arguments: row.get(2),
            kind: if row.get::<_, &str>(3) == "p" {
                FunctionKind::Procedure
            } else {
                FunctionKind::Function
            },
            definition: row.get(4),
        });
    }

    for row in client
        .query(
            &format!(
                "SELECT n.nspname, c.relname
                 FROM pg_catalog.pg_class c
                 JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
                 WHERE c.relkind = 'S' AND {USER_SCHEMAS} AND {}
                   AND NOT EXISTS (
                     SELECT 1 FROM pg_catalog.pg_depend dep
                     WHERE dep.objid = c.oid AND dep.deptype IN ('a', 'i'))
                 ORDER BY n.nspname COLLATE \"C\", c.relname COLLATE \"C\"",
                not_from_extension("c.oid")
            ),
            &[],
        )
        .await?
    {
        if let Some(&schema) = schema_index.get(row.get::<_, &str>(0)) {
            schemas[schema]
                .sequences
                .push(Sequence { name: row.get(1) });
        }
    }

    for row in client
        .query(
            &format!(
                "SELECT n.nspname, t.typname,
                        array_agg(e.enumlabel::text ORDER BY e.enumsortorder)
                 FROM pg_catalog.pg_type t
                 JOIN pg_catalog.pg_namespace n ON n.oid = t.typnamespace
                 JOIN pg_catalog.pg_enum e ON e.enumtypid = t.oid
                 WHERE {USER_SCHEMAS} AND {}
                 GROUP BY n.nspname, t.typname
                 ORDER BY n.nspname COLLATE \"C\", t.typname COLLATE \"C\"",
                not_from_extension("t.oid")
            ),
            &[],
        )
        .await?
    {
        if let Some(&schema) = schema_index.get(row.get::<_, &str>(0)) {
            schemas[schema].enums.push(EnumType {
                name: row.get(1),
                labels: row.get(2),
            });
        }
    }

    // plpgsql is installed in every database, so it is left out.
    let extensions = client
        .query(
            "SELECT e.extname, n.nspname
             FROM pg_catalog.pg_extension e
             JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace
             WHERE e.extname <> 'plpgsql'
             ORDER BY e.extname COLLATE \"C\"",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| Extension {
            name: row.get(0),
            schema: row.get(1),
        })
        .collect();

    Ok(Database {
        schemas,
        extensions,
    })
}
