//! Integration tests against a real MongoDB server, or a DocumentDB one.
//!
//! Skipped unless `DOLMEN_TEST_MONGO_HOST` is set; see `common::mongo_config`
//! for the other settings. For a throwaway server:
//!
//! ```sh
//! docker run -d --rm --name dolmen-test-mongo -p 57017:27017 \
//!   -e MONGO_INITDB_ROOT_USERNAME=root -e MONGO_INITDB_ROOT_PASSWORD=example mongo:8
//! DOLMEN_TEST_MONGO_HOST=localhost DOLMEN_TEST_MONGO_PORT=57017 cargo test -p dolmen-core --test mongo
//! ```
//!
//! Microsoft's DocumentDB Local runs the same tests with
//! `DOLMEN_TEST_MONGO_FLAVOR=documentdb DOLMEN_TEST_MONGO_SSLMODE=require`.
//!
//! Each test works in a database of its own, named after it, so tests can run
//! at the same time.

use std::time::{Duration, Instant};

use dolmen_core::mongo::{Flavor, Session, statement_ranges};
use dolmen_core::postgres::{ConnectConfig, StatementResult};
use dolmen_core::{Error, Refusal};

#[macro_use]
mod common;
use common::mongo_config;

macro_rules! mongo_or_skip {
    () => {
        config_or_skip!(mongo_config(), "DOLMEN_TEST_MONGO_HOST")
    };
}

fn flavor() -> Flavor {
    match std::env::var("DOLMEN_TEST_MONGO_FLAVOR").as_deref() {
        Ok("documentdb") => Flavor::DocumentDb,
        _ => Flavor::MongoDb,
    }
}

/// A session in a database named after the test, emptied first.
async fn fresh(config: &ConnectConfig, test: &str) -> Session {
    let session = Session::connect(config, flavor()).await.expect("connect");
    session.set_database(&format!("dolmen_test_{test}"));
    execute(&session, "db.dropDatabase()").await;
    session
}

async fn execute(session: &Session, text: &str) -> Vec<StatementResult> {
    session
        .execute(text, 1000, false)
        .await
        .unwrap_or_else(|err| panic!("{text}: {err:?}"))
}

/// The result of the one statement in `text`.
async fn one(session: &Session, text: &str) -> StatementResult {
    let mut results = execute(session, text).await;
    assert_eq!(results.len(), 1, "{text}");
    results.remove(0)
}

/// Columns, rows and total of a result with rows.
fn rows(result: StatementResult) -> (Vec<String>, Vec<Vec<Option<String>>>, u64) {
    match result {
        StatementResult::Rows {
            columns,
            rows,
            total_rows,
            ..
        } => (columns, rows, total_rows),
        other => panic!("expected rows, got {other:?}"),
    }
}

fn affected(result: StatementResult) -> u64 {
    match result {
        StatementResult::Command { rows_affected } => rows_affected,
        other => panic!("expected a command, got {other:?}"),
    }
}

/// The value of `column` in each row.
fn column(result: StatementResult, name: &str) -> Vec<Option<String>> {
    let (columns, rows, _) = rows(result);
    let at = columns
        .iter()
        .position(|c| c == name)
        .unwrap_or_else(|| panic!("no column {name} in {columns:?}"));
    rows.into_iter().map(|mut row| row.remove(at)).collect()
}

fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

async fn count(session: &Session, collection: &str) -> String {
    let result = one(session, &format!("db.{collection}.countDocuments()")).await;
    column(result, "count")[0].clone().unwrap()
}

#[tokio::test]
async fn connects_and_reports_the_server() {
    let config = mongo_or_skip!();
    let session = Session::connect(&config, flavor()).await.expect("connect");
    assert!(
        session
            .server_version()
            .starts_with(|c: char| c.is_ascii_digit()),
        "unexpected version {:?}",
        session.server_version()
    );
    assert_eq!(session.flavor(), flavor());
    assert_eq!(session.config().host, config.host);
    assert_eq!(session.database(), "test");

    let mut nameless = config.clone();
    nameless.database = String::new();
    let session = Session::connect(&nameless, flavor()).await.unwrap();
    assert_eq!(session.database(), "test");
}

#[tokio::test]
async fn rejects_a_wrong_password() {
    let mut config = mongo_or_skip!();
    config.password = Some("wrong".into());
    let err = Session::connect(&config, flavor())
        .await
        .err()
        .expect("connect should fail");
    assert!(
        matches!(&err, Error::Database { code, message, .. }
            if code == "AuthenticationFailed" && !message.is_empty()),
        "expected an authentication error, got {err:?}"
    );
}

#[tokio::test]
async fn fails_promptly_without_a_server() {
    let mut config = mongo_or_skip!();
    // Nothing listens on port 1.
    config.port = 1;
    let started = Instant::now();
    let err = Session::connect(&config, flavor())
        .await
        .err()
        .expect("connect should fail");
    // The cause, without the driver's description of the servers it knows.
    assert!(
        matches!(&err, Error::Connection { message }
            if message.starts_with("Couldn't reach the server: ") && !message.contains("Topology")),
        "expected a connection error, got {err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test]
async fn inserts_finds_updates_and_deletes() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "crud").await;

    let results = execute(
        &session,
        "db.people.insertOne({ _id: 1, name: 'Ann', age: 30 })
         db.people.insertMany([
           { _id: 2, name: 'Bo', age: 25, tags: ['a', 'b'] },
           { _id: 3, name: 'Cy', age: 35, address: { city: 'Seoul' } },
         ]);
         db.people.find({}, { _id: 1, name: 1, age: 1 }).sort({ age: -1 })",
    )
    .await;
    assert_eq!(results.len(), 3);
    let mut results = results.into_iter();
    assert_eq!(affected(results.next().unwrap()), 1);
    assert_eq!(affected(results.next().unwrap()), 2);
    let (columns, found, total_rows) = rows(results.next().unwrap());
    assert_eq!(columns, ["_id", "name", "age"]);
    assert_eq!(
        found,
        [
            vec![text("3"), text("Cy"), text("35")],
            vec![text("1"), text("Ann"), text("30")],
            vec![text("2"), text("Bo"), text("25")],
        ]
    );
    assert_eq!(total_rows, 3);

    // Every field becomes a column, `_id` first; missing fields are None.
    let (columns, found, _) = rows(one(&session, "db.people.find().sort({ _id: 1 })").await);
    assert_eq!(columns, ["_id", "name", "age", "tags", "address"]);
    assert_eq!(found[0][3], None);
    assert_eq!(found[1][3], text(r#"["a","b"]"#));
    assert_eq!(found[2][4], text(r#"{"city":"Seoul"}"#));

    // Updates count documents changed, and one an upsert inserted.
    let updated = |text: &'static str| {
        let session = &session;
        async move { affected(one(session, text).await) }
    };
    assert_eq!(
        updated("db.people.updateOne({ _id: 1 }, { $set: { age: 31 } })").await,
        1
    );
    assert_eq!(
        updated("db.people.updateOne({ _id: 1 }, { $set: { age: 31 } })").await,
        0
    );
    assert_eq!(
        updated("db.people.updateMany({}, { $inc: { age: 1 } })").await,
        3
    );
    assert_eq!(
        updated("db.people.updateMany({ age: { $gt: 30 } }, [{ $set: { senior: true } }])").await,
        2
    );
    assert_eq!(
        updated("db.people.updateOne({ _id: 4 }, { $set: { name: 'Di' } }, { upsert: true })")
            .await,
        1
    );
    assert_eq!(
        updated("db.people.replaceOne({ _id: 4 }, { name: 'Dee' })").await,
        1
    );
    let (_, found, _) = rows(one(&session, "db.people.findOne({ _id: 4 })").await);
    assert_eq!(found, [vec![text("4"), text("Dee")]]);
    assert_eq!(count(&session, "people").await, "4");

    assert_eq!(updated("db.people.deleteOne({ age: { $gt: 0 } })").await, 1);
    assert_eq!(updated("db.people.deleteMany({})").await, 3);
    assert_eq!(count(&session, "people").await, "0");
    execute(&session, "db.dropDatabase()").await;
}

#[tokio::test]
async fn chains_cursor_methods_and_caps_rows() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "cursor").await;
    let documents: Vec<String> = (1..=30)
        .map(|n| format!("{{ _id: {n}, n: {n} }}"))
        .collect();
    execute(
        &session,
        &format!("db.numbers.insertMany([{}])", documents.join(", ")),
    )
    .await;

    let found = one(
        &session,
        "db.numbers.find({ n: { $gt: 10 } })\n  .sort({ n: -1 })\n  .skip(2)\n  .limit(3)\n  .projection({ _id: 0 })\n  .toArray()",
    )
    .await;
    assert_eq!(column(found, "n"), [text("28"), text("27"), text("26")]);

    let counted = one(
        &session,
        "db.numbers.find({ n: { $lte: 10 } }).limit(2).count()",
    )
    .await;
    assert_eq!(
        rows(counted),
        (vec!["count".into()], vec![vec![text("10")]], 1)
    );

    // Reading stops one document past max_rows.
    let mut results = session
        .execute("db.numbers.find()", 10, false)
        .await
        .unwrap();
    let (_, kept, total_rows) = rows(results.remove(0));
    assert_eq!((kept.len(), total_rows), (10, 11));
    let mut results = session
        .execute("db.numbers.find().limit(5)", 10, false)
        .await
        .unwrap();
    let (_, kept, total_rows) = rows(results.remove(0));
    assert_eq!((kept.len(), total_rows), (5, 5));
    let mut results = session
        .execute("db.numbers.aggregate([{ $match: {} }])", 7, false)
        .await
        .unwrap();
    let (_, kept, total_rows) = rows(results.remove(0));
    assert_eq!((kept.len(), total_rows), (7, 8));
    execute(&session, "db.dropDatabase()").await;
}

#[tokio::test]
async fn shows_values_as_text() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "values").await;
    execute(
        &session,
        "db.values.insertOne({
           _id: ObjectId('507f1f77bcf86cd799439011'),
           when: ISODate('2024-05-01T10:00:00.123Z'),
           int: NumberInt(1), long: NumberLong('9007199254740993'), double: 2.5,
           yes: true, nothing: null, price: NumberDecimal('9.99'),
           pattern: /^a/i, id: UUID('01234567-89ab-cdef-0123-456789abcdef'),
         })",
    )
    .await;
    let (columns, found, _) = rows(one(&session, "db.values.find()").await);
    let cells: Vec<(String, Option<String>)> =
        columns.into_iter().zip(found[0].iter().cloned()).collect();
    let cell = |name: &str| {
        cells
            .iter()
            .find(|(column, _)| column == name)
            .unwrap_or_else(|| panic!("no column {name}"))
            .1
            .clone()
    };
    assert_eq!(cell("_id"), text("507f1f77bcf86cd799439011"));
    assert_eq!(cell("when"), text("2024-05-01T10:00:00.123Z"));
    assert_eq!(cell("int"), text("1"));
    assert_eq!(cell("long"), text("9007199254740993"));
    assert_eq!(cell("double"), text("2.5"));
    assert_eq!(cell("yes"), text("true"));
    assert_eq!(cell("nothing"), None);
    assert_eq!(cell("price"), text(r#"{"$numberDecimal":"9.99"}"#));
    assert_eq!(
        cell("pattern"),
        text(r#"{"$regularExpression":{"pattern":"^a","options":"i"}}"#)
    );
    assert_eq!(
        cell("id"),
        text(r#"{"$binary":{"base64":"ASNFZ4mrze8BI0VniavN7w==","subType":"04"}}"#)
    );
    execute(&session, "db.dropDatabase()").await;
}

#[tokio::test]
async fn counts_distinct_values_and_aggregates() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "aggregate").await;
    execute(
        &session,
        "db.orders.insertMany([
           { item: 'a', qty: 2 }, { item: 'b', qty: 5 }, { item: 'a', qty: 1 },
         ])",
    )
    .await;

    let estimated = one(&session, "db.orders.estimatedDocumentCount()").await;
    assert_eq!(column(estimated, "count"), [text("3")]);
    let counted = one(&session, "db.orders.countDocuments({ item: 'a' })").await;
    assert_eq!(column(counted, "count"), [text("2")]);

    let mut values = column(one(&session, "db.orders.distinct('item')").await, "value");
    values.sort();
    assert_eq!(values, [text("a"), text("b")]);
    let values = column(
        one(&session, "db.orders.distinct('qty', { item: 'b' })").await,
        "value",
    );
    assert_eq!(values, [text("5")]);

    let totals = one(
        &session,
        "db.orders.aggregate([
           { $group: { _id: '$item', total: { $sum: '$qty' } } },
           { $sort: { _id: 1 } },
         ])",
    )
    .await;
    assert_eq!(
        rows(totals),
        (
            vec!["_id".into(), "total".into()],
            vec![vec![text("a"), text("3")], vec![text("b"), text("5")]],
            2
        )
    );
    // Stages one by one, as mongosh also takes them.
    let counted = one(
        &session,
        "db.orders.aggregate({ $match: { qty: { $gt: 1 } } }, { $count: 'n' })",
    )
    .await;
    assert_eq!(column(counted, "n"), [text("2")]);

    // $out writes when the connection may write.
    execute(
        &session,
        "db.orders.aggregate([{ $match: { item: 'a' } }, { $out: 'copies' }])",
    )
    .await;
    assert_eq!(count(&session, "copies").await, "2");
    execute(&session, "db.dropDatabase()").await;
}

#[tokio::test]
async fn creates_lists_and_drops_indexes() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "indexes").await;
    execute(
        &session,
        "db.users.createIndex({ email: 1 }, { unique: true })
         db.users.createIndex({ last: 1, first: -1 }, { name: 'by_name' })",
    )
    .await;

    let (columns, indexes, _) = rows(one(&session, "db.users.getIndexes()").await);
    assert_eq!(&columns[..3], ["name", "key", "unique"]);
    let mut summary: Vec<_> = indexes
        .into_iter()
        .map(|row| {
            (
                row[0].clone().unwrap(),
                row[1].clone().unwrap(),
                row[2].clone().unwrap(),
            )
        })
        .collect();
    summary.sort();
    assert_eq!(
        summary,
        [
            ("_id_".into(), r#"{"_id":1}"#.into(), "false".into()),
            (
                "by_name".into(),
                r#"{"last":1,"first":-1}"#.into(),
                "false".into()
            ),
            ("email_1".into(), r#"{"email":1}"#.into(), "true".into()),
        ]
    );

    execute(&session, "db.users.insertOne({ email: 'a@example.com' })").await;
    let err = session
        .execute(
            "db.users.insertOne({ email: 'a@example.com' })",
            1000,
            false,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, Error::Database { code, .. } if code == "DuplicateKey"),
        "expected a duplicate key error, got {err:?}"
    );

    execute(
        &session,
        "db.users.dropIndex('email_1'); db.users.dropIndex({ last: 1, first: -1 })",
    )
    .await;
    let names = column(one(&session, "db.users.getIndexes()").await, "name");
    assert_eq!(names, [text("_id_")]);
    execute(&session, "db.dropDatabase()").await;
}

#[tokio::test]
async fn runs_database_methods_and_commands() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "database").await;
    let database = session.database();

    let pong = one(&session, "db.runCommand({ ping: 1 })").await;
    assert_eq!(column(pong, "ok"), [text("1")]);
    let pong = one(&session, "db.runCommand('ping')").await;
    assert_eq!(column(pong, "ok"), [text("1")]);
    let listed = one(
        &session,
        "db.adminCommand({ listDatabases: 1, nameOnly: true })",
    )
    .await;
    assert!(column(listed, "databases")[0].is_some());
    assert_eq!(
        column(one(&session, "db.getName()").await, "name"),
        [Some(database.clone())]
    );

    // DocumentDB has no capped collections.
    let logs = match flavor() {
        Flavor::MongoDb => "db.createCollection('logs', { capped: true, size: 65536 })",
        Flavor::DocumentDb => "db.createCollection('logs', {})",
    };
    execute(
        &session,
        &format!(
            "{logs}
             db.items.insertMany([{{ n: 1 }}, {{ n: 2 }}])
             db.createView('big_items', 'items', [{{ $match: {{ n: {{ $gt: 1 }} }} }}])"
        ),
    )
    .await;
    // MongoDB's own collections, such as system.views, are listed too.
    let names = |result| {
        column(result, "name")
            .into_iter()
            .flatten()
            .filter(|name| !name.starts_with("system."))
            .collect::<Vec<_>>()
    };
    let listed = names(one(&session, "db.getCollectionNames()").await);
    assert_eq!(listed, ["big_items", "items", "logs"]);
    let listed = names(one(&session, "show collections").await);
    assert_eq!(listed, ["big_items", "items", "logs"]);
    let viewed = one(&session, "db.big_items.find({}, { _id: 0 })").await;
    assert_eq!(column(viewed, "n"), [text("2")]);

    let stats = one(&session, "db.stats()").await;
    assert_eq!(column(stats, "db"), [Some(database.clone())]);

    let (columns, databases, _) = rows(one(&session, "show dbs").await);
    assert_eq!(columns, ["name", "sizeOnDisk", "empty"]);
    assert!(databases.iter().any(|row| row[0] == Some(database.clone())));

    execute(&session, "db.items.renameCollection('things')").await;
    execute(&session, "db.logs.drop()").await;
    let listed = names(one(&session, "db.getCollectionNames()").await);
    assert_eq!(listed, ["big_items", "things"]);

    // Server errors keep their code name, which DocumentDB words its own way.
    let err = session
        .execute("db.runCommand({ noSuchCommand: 1 })", 1000, false)
        .await
        .unwrap_err();
    let expected = match flavor() {
        Flavor::MongoDb => "CommandNotFound",
        Flavor::DocumentDb => "CommandNotSupported",
    };
    assert!(
        matches!(&err, Error::Database { code, .. } if code == expected),
        "got {err:?}"
    );
    // So do mistakes the driver catches before sending.
    let err = session
        .execute("db.things.updateOne({}, { n: 1 })", 1000, false)
        .await
        .unwrap_err();
    assert!(matches!(&err, Error::Database { .. }), "got {err:?}");

    assert_eq!(affected(one(&session, "db.dropDatabase()").await), 0);
    let (_, databases, _) = rows(one(&session, "show dbs").await);
    assert!(!databases.iter().any(|row| row[0] == Some(database.clone())));
}

#[tokio::test]
async fn use_changes_the_current_database() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "use_a").await;
    let other = fresh(&config, "use_b").await;
    drop(other);

    let results = execute(
        &session,
        "db.marks.insertOne({ in: 'a' })
         use dolmen_test_use_b
         db.marks.insertOne({ in: 'b' })
         db.getName()",
    )
    .await;
    assert_eq!(affected(results.into_iter().nth(1).unwrap()), 0);
    assert_eq!(session.database(), "dolmen_test_use_b");
    let found = one(&session, "db.marks.find({}, { _id: 0 })").await;
    assert_eq!(column(found, "in"), [text("b")]);

    // Another session starts in this one's database, then goes its own way.
    let another = session.another().await.unwrap();
    assert_eq!(another.database(), "dolmen_test_use_b");
    another.set_database("dolmen_test_use_a");
    assert_eq!(session.database(), "dolmen_test_use_b");
    let found = one(&another, "db.marks.find({}, { _id: 0 })").await;
    assert_eq!(column(found, "in"), [text("a")]);

    // getSiblingDB names a database for one statement only.
    let found = one(
        &session,
        "db.getSiblingDB('dolmen_test_use_a').marks.find({}, { _id: 0 })",
    )
    .await;
    assert_eq!(column(found, "in"), [text("a")]);
    let counted = one(
        &session,
        "db.getSiblingDB(\"dolmen_test_use_a\").marks.countDocuments({})",
    )
    .await;
    assert_eq!(column(counted, "count"), [text("1")]);
    let pong = one(
        &session,
        "db.getSiblingDB('dolmen_test_use_a').runCommand({ ping: 1 })",
    )
    .await;
    assert_eq!(column(pong, "ok"), [text("1")]);
    let shown = one(
        &session,
        &Session::show_statement_in("dolmen_test_use_a", "marks"),
    )
    .await;
    assert_eq!(column(shown, "in"), [text("a")]);
    assert_eq!(session.database(), "dolmen_test_use_b");

    let shown = one(&session, &Session::show_statement("marks")).await;
    assert_eq!(column(shown, "in"), [text("b")]);

    execute(
        &session,
        "db.getSiblingDB('dolmen_test_use_a').dropDatabase()",
    )
    .await;
    execute(&session, "db.dropDatabase()").await;
}

#[tokio::test]
async fn refuses_writes_when_read_only() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "read_only").await;
    execute(&session, "db.items.insertOne({ _id: 1 })").await;

    for text in [
        "db.items.insertOne({})",
        "db.items.insertMany([{}])",
        "db.items.updateOne({}, { $set: { a: 1 } })",
        "db.items.updateMany({}, { $set: { a: 1 } })",
        "db.items.replaceOne({}, {})",
        "db.items.deleteOne({})",
        "db.items.deleteMany({})",
        "db.items.createIndex({ a: 1 })",
        "db.items.dropIndex('a_1')",
        "db.items.drop()",
        "db.items.renameCollection('other')",
        "db.createCollection('more')",
        "db.createView('v', 'items', [])",
        "db.dropDatabase()",
        "db.getSiblingDB('dolmen_test_read_only').dropDatabase()",
        "db.items.aggregate([{ $out: 'copy' }])",
        "db.items.aggregate([{ $merge: { into: 'copy' } }])",
        "db.runCommand({ drop: 'items' })",
        "db.adminCommand({ renameCollection: 'a.b', to: 'a.c' })",
        // Nothing runs, not even the statements before the write.
        "db.items.find(); db.items.deleteMany({})",
    ] {
        let err = session.execute(text, 1000, true).await.unwrap_err();
        assert!(
            matches!(
                &err,
                Error::Refused {
                    reason: Refusal::ReadOnly,
                    ..
                }
            ),
            "{text}: expected a read-only refusal, got {err:?}"
        );
    }
    assert_eq!(count(&session, "items").await, "1");

    // Reads still run.
    for text in [
        "db.items.find()",
        "db.items.aggregate([{ $match: {} }])",
        "db.runCommand({ ping: 1 })",
        "db.runCommand({ count: 'items' })",
        "db.adminCommand({ listDatabases: 1 })",
        "show dbs",
        "use dolmen_test_read_only",
    ] {
        session
            .execute(text, 1000, true)
            .await
            .unwrap_or_else(|err| panic!("{text}: {err:?}"));
    }
    execute(&session, "db.dropDatabase()").await;
}

#[tokio::test]
async fn reports_syntax_errors_before_running_anything() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "syntax").await;
    let err = session
        .execute(
            "db.items.insertOne({ a: 1 })\ndb.items.find({ a: })",
            1000,
            false,
        )
        .await
        .unwrap_err();
    match err {
        Error::Database { code, position, .. } => {
            assert_eq!(code, "SyntaxError");
            assert_eq!(position, Some(49));
        }
        other => panic!("expected a syntax error, got {other:?}"),
    }
    assert_eq!(count(&session, "items").await, "0");
    execute(&session, "db.dropDatabase()").await;
}

#[tokio::test]
async fn runs_each_statement_range_alone() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "ranges").await;
    let script = "db.items.insertOne({ a: 1 })\n\n// the second\ndb.items.insertOne({\n  a: 2,\n});\ndb.items.find({}, { _id: 0 })\n  .sort({ a: -1 })";
    let ranges = statement_ranges(script);
    assert_eq!(ranges.len(), 3);
    for range in &ranges[..2] {
        assert_eq!(execute(&session, &script[range.clone()]).await.len(), 1);
    }
    let found = one(&session, &script[ranges[2].clone()]).await;
    assert_eq!(column(found, "a"), [text("2"), text("1")]);
    execute(&session, "db.dropDatabase()").await;
}

#[tokio::test]
async fn introspects_collections_views_and_indexes() {
    let config = mongo_or_skip!();
    let session = fresh(&config, "introspect").await;
    execute(
        &session,
        "db.people.insertMany([
           { _id: 1, name: 'Ann', age: 30, joined: ISODate('2024-01-01') },
           { _id: 2, name: 'Bo', age: '25', nickname: null },
           { _id: 3, name: 'Cy', age: 40 },
         ])
         db.people.createIndex({ name: 1 }, { unique: true })
         db.people.createIndex({ 'address.city': 1, age: -1 }, { name: 'by_city', sparse: true })
         db.createView('adults', 'people', [{ $match: { age: { $gte: 18 } } }])
         db.empty.insertOne({ _id: 'x' })
         db.empty.deleteMany({})",
    )
    .await;

    let model = session.introspect(100).await.expect("introspect");
    for system in ["admin", "local", "config"] {
        assert!(model.schemas.iter().all(|schema| schema.name != system));
    }
    let schema = model
        .schemas
        .iter()
        .find(|schema| schema.name == session.database())
        .expect("the test database");
    let names: Vec<_> = schema.tables.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["empty", "people"]);

    let people = &schema.tables[1];
    let columns: Vec<_> = people
        .columns
        .iter()
        .map(|c| (c.name.as_str(), c.data_type.as_str(), c.nullable))
        .collect();
    assert_eq!(
        columns,
        [
            ("_id", "int", false),
            ("name", "string", false),
            ("age", "int | string", false),
            ("joined", "date", true),
            ("nickname", "null", true),
        ]
    );
    assert!(schema.tables[0].columns.is_empty());

    let indexes: Vec<_> = people
        .indexes
        .iter()
        .map(|i| (i.name.as_str(), i.definition.as_str()))
        .collect();
    assert_eq!(
        indexes,
        [
            (
                "by_city",
                r#"db.getCollection("people").createIndex({ "address.city": 1, age: -1 }, { name: "by_city", sparse: true })"#
            ),
            (
                "name_1",
                r#"db.getCollection("people").createIndex({ name: 1 }, { name: "name_1", unique: true })"#
            ),
        ]
    );

    assert_eq!(schema.views.len(), 1);
    let adults = &schema.views[0];
    assert_eq!(adults.name, "adults");
    assert_eq!(
        adults.definition,
        r#"db.createView("adults", "people", [ { $match: { age: { $gte: 18 } } } ])"#
    );
    // Sampled through the view: only documents whose age is a number pass.
    assert_eq!(adults.columns.len(), 4);

    // A sample of nothing leaves the columns out.
    let model = session.introspect(0).await.unwrap();
    let schema = model
        .schemas
        .iter()
        .find(|schema| schema.name == session.database())
        .unwrap();
    assert!(schema.tables.iter().all(|t| t.columns.is_empty()));
    execute(&session, "db.dropDatabase()").await;
}
