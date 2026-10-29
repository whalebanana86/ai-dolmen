//! A MongoDB server's structure in the schema model: databases become
//! schemas, collections tables, and views views.
//!
//! Collections declare no columns, so a collection's columns are the fields
//! of a sample of its documents, each with the BSON types seen in it.

use std::collections::{BTreeSet, HashMap};

use futures_util::{StreamExt, TryStreamExt};
use mongodb::bson::{Bson, Document, doc};

use super::Session;
use super::shell::{quote, to_shell};
use crate::Error;
use crate::schema::{Column, Database, Index, Schema, Table, TableKind, View};

/// Databases MongoDB keeps for itself, left out of the model.
const SYSTEM_DATABASES: &[&str] = &["admin", "local", "config"];

/// How many collections of a database are sampled at once.
const CONCURRENT_SAMPLES: usize = 8;

/// Reads every database the user can list, or only the current one when
/// listing databases is refused. Collections named `system.*` are MongoDB's
/// own, such as `system.views`, and are left out.
pub(super) async fn introspect(session: &Session, sample: usize) -> Result<Database, Error> {
    let names = match database_names(session).await {
        Ok(names) => names,
        Err(Error::Database { .. }) => vec![session.database()],
        Err(err) => return Err(err),
    };
    let mut schemas = Vec::with_capacity(names.len());
    for name in names {
        schemas.push(read_database(&session.client.database(&name), sample).await?);
    }
    schemas.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Database {
        schemas,
        extensions: Vec::new(),
    })
}

async fn database_names(session: &Session) -> Result<Vec<String>, Error> {
    let reply = session
        .client
        .database("admin")
        .run_command(doc! { "listDatabases": 1, "nameOnly": true })
        .await?;
    Ok(super::documents_in(&reply, "databases")
        .iter()
        .filter_map(|database| database.get_str("name").ok())
        .filter(|name| !SYSTEM_DATABASES.contains(name))
        .map(str::to_owned)
        .collect())
}

/// A table or a view, as read from `listCollections`.
enum Object {
    Table(Table),
    View(View),
}

async fn read_database(database: &mongodb::Database, sample: usize) -> Result<Schema, Error> {
    let listed: Vec<Document> = allowed(
        async {
            Ok(database
                .run_cursor_command(doc! { "listCollections": 1 })
                .await?
                .try_collect()
                .await?)
        }
        .await,
    )?;
    let objects: Vec<Object> = futures_util::stream::iter(listed.into_iter().filter(|listed| {
        !listed
            .get_str("name")
            .unwrap_or("system.")
            .starts_with("system.")
    }))
    .map(|listed| read_object(database, listed, sample))
    .buffered(CONCURRENT_SAMPLES)
    .try_collect()
    .await?;

    let mut tables = Vec::new();
    let mut views = Vec::new();
    for object in objects {
        match object {
            Object::Table(table) => tables.push(table),
            Object::View(view) => views.push(view),
        }
    }
    tables.sort_by(|a, b| a.name.cmp(&b.name));
    views.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(schema(database.name().to_owned(), tables, views))
}

async fn read_object(
    database: &mongodb::Database,
    listed: Document,
    sample: usize,
) -> Result<Object, Error> {
    let name = listed.get_str("name").unwrap_or_default().to_owned();
    let columns = sample_columns(database, &name, sample).await?;
    if listed.get_str("type") == Ok("view") {
        let options = listed.get_document("options").cloned().unwrap_or_default();
        let pipeline = options.get_array("pipeline").cloned().unwrap_or_default();
        let mut definition = format!(
            "db.createView({}, {}, {}",
            quote(&name),
            quote(options.get_str("viewOn").unwrap_or_default()),
            to_shell(&Bson::Array(pipeline))
        );
        if let Ok(collation) = options.get_document("collation") {
            definition.push_str(", ");
            definition.push_str(&to_shell(&Bson::Document(
                doc! { "collation": collation.clone() },
            )));
        }
        definition.push(')');
        return Ok(Object::View(view(name, columns, definition)));
    }
    let indexes = read_indexes(database, &name).await?;
    Ok(Object::Table(table(name, columns, indexes)))
}

/// The fields of up to `sample` documents, in the order they first appear
/// with `_id` first. A field's type lists every BSON type seen in it, such as
/// `int | string`; it is nullable when null or missing in some document.
async fn sample_columns(
    database: &mongodb::Database,
    collection: &str,
    sample: usize,
) -> Result<Vec<Column>, Error> {
    // A limit of 0 means no limit to MongoDB.
    if sample == 0 {
        return Ok(Vec::new());
    }
    let documents: Vec<Document> = allowed(
        async {
            Ok(database
                .collection::<Document>(collection)
                .find(doc! {})
                .limit(i64::try_from(sample).unwrap_or(i64::MAX))
                .await?
                .try_collect()
                .await?)
        }
        .await,
    )?;

    struct Field {
        types: BTreeSet<&'static str>,
        present: usize,
        null: bool,
    }
    let mut order: Vec<String> = Vec::new();
    let mut fields: HashMap<String, Field> = HashMap::new();
    for document in &documents {
        for (key, value) in document {
            let field = fields.entry(key.clone()).or_insert_with(|| {
                order.push(key.clone());
                Field {
                    types: BTreeSet::new(),
                    present: 0,
                    null: false,
                }
            });
            field.present += 1;
            match value {
                Bson::Null | Bson::Undefined => field.null = true,
                value => {
                    field.types.insert(type_name(value));
                }
            }
        }
    }
    if let Some(at) = order.iter().position(|name| name == "_id") {
        let id = order.remove(at);
        order.insert(0, id);
    }
    Ok(order
        .into_iter()
        .map(|name| {
            let field = &fields[&name];
            let data_type = if field.types.is_empty() {
                "null".to_owned()
            } else {
                field.types.iter().copied().collect::<Vec<_>>().join(" | ")
            };
            column(
                name,
                data_type,
                field.null || field.present < documents.len(),
            )
        })
        .collect())
}

/// The collection's indexes but `_id_`, which every collection has, each as
/// the `createIndex` call that makes it.
async fn read_indexes(database: &mongodb::Database, collection: &str) -> Result<Vec<Index>, Error> {
    let listed: Vec<Document> = allowed(
        async {
            Ok(database
                .run_cursor_command(doc! { "listIndexes": collection })
                .await?
                .try_collect()
                .await?)
        }
        .await,
    )?;
    let mut indexes: Vec<Index> = listed
        .into_iter()
        .filter_map(|index| {
            let name = index.get_str("name").ok()?.to_owned();
            if name == "_id_" {
                return None;
            }
            let keys = index.get_document("key").cloned().unwrap_or_default();
            let mut options = doc! { "name": name.clone() };
            options.extend(
                index
                    .into_iter()
                    .filter(|(key, _)| !matches!(key.as_str(), "v" | "key" | "name" | "ns")),
            );
            let definition = format!(
                "db.getCollection({}).createIndex({}, {})",
                quote(collection),
                to_shell(&Bson::Document(keys)),
                to_shell(&Bson::Document(options))
            );
            Some(Index { name, definition })
        })
        .collect();
    indexes.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(indexes)
}

/// What a read returned, or nothing when the user may not read it or the
/// collection was dropped since it was listed, so one such collection
/// doesn't fail the whole model.
fn allowed<T: Default>(result: Result<T, Error>) -> Result<T, Error> {
    match result {
        Err(Error::Database { code, .. })
            if code == "Unauthorized" || code == "NamespaceNotFound" =>
        {
            Ok(T::default())
        }
        result => result,
    }
}

/// A value's type as `$type` names it.
fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::Undefined => "undefined",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::DbPointer(_) => "dbPointer",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::Symbol(_) => "symbol",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
    }
}

// The model's structs are built only below, so fields they gain are filled in one place.

fn schema(name: String, tables: Vec<Table>, views: Vec<View>) -> Schema {
    Schema {
        name,
        tables,
        views,
        functions: Vec::new(),
        sequences: Vec::new(),
        enums: Vec::new(),
    }
}

fn table(name: String, columns: Vec<Column>, indexes: Vec<Index>) -> Table {
    Table {
        name,
        kind: TableKind::Table,
        columns,
        constraints: Vec::new(),
        indexes,
        partition_key: None,
        comment: None,
        partition_of: None,
        grants: Vec::new(),
        triggers: Vec::new(),
        row_security: false,
        force_row_security: false,
        policies: Vec::new(),
    }
}

fn view(name: String, columns: Vec<Column>, definition: String) -> View {
    View {
        name,
        materialized: false,
        columns,
        definition,
        comment: None,
        indexes: Vec::new(),
        grants: Vec::new(),
        triggers: Vec::new(),
    }
}

fn column(name: String, data_type: String, nullable: bool) -> Column {
    Column {
        name,
        data_type,
        nullable,
        default: None,
        identity: None,
        generated: None,
        comment: None,
    }
}
