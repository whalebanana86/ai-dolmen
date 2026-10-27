//! Applying migration plans to a real PostgreSQL server.
//!
//! Skipped unless `DOLMEN_TEST_PG_HOST` is set; see the README.

use dolmen_core::Error;
use dolmen_core::postgres::migrate::{Plan, plan};
use dolmen_core::postgres::{ConnectConfig, Session};
use dolmen_core::schema::Database;

#[macro_use]
mod common;
use common::{exclusive, test_config};

const SCHEMA: &str = "migrate_test";

/// Version A, with data in it.
const VERSION_A: &str = "
    CREATE SCHEMA migrate_test;
    CREATE TYPE migrate_test.mood AS ENUM ('sad', 'ok');
    CREATE TABLE migrate_test.authors (
      id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
      email varchar(255) NOT NULL UNIQUE,
      mood migrate_test.mood DEFAULT 'ok'
    );
    CREATE TABLE migrate_test.posts (
      id serial PRIMARY KEY,
      author_id bigint NOT NULL REFERENCES migrate_test.authors (id),
      title text NOT NULL,
      legacy text
    );
    CREATE TABLE migrate_test.tags (name text PRIMARY KEY);
    CREATE INDEX posts_title_idx ON migrate_test.posts (title);
    CREATE VIEW migrate_test.titles AS SELECT id, title FROM migrate_test.posts;
    CREATE FUNCTION migrate_test.shout(t text) RETURNS text LANGUAGE sql AS 'SELECT upper(t)';
    COMMENT ON TABLE migrate_test.posts IS 'what authors write';
    INSERT INTO migrate_test.authors (email) VALUES ('a@example.com'), ('b@example.com');
    INSERT INTO migrate_test.posts (author_id, title, legacy) VALUES (1, 'hello', 'x'), (2, 'world', NULL);
";

/// Version B: version A after a round of schema changes.
const TO_VERSION_B: &str = "
    ALTER TABLE migrate_test.authors ADD COLUMN name text NOT NULL DEFAULT '';
    ALTER TABLE migrate_test.authors ALTER COLUMN mood DROP DEFAULT;
    DROP VIEW migrate_test.titles;
    ALTER TABLE migrate_test.posts DROP COLUMN legacy;
    ALTER TABLE migrate_test.posts ALTER COLUMN title TYPE varchar(200);
    ALTER TABLE migrate_test.posts ALTER COLUMN title DROP NOT NULL;
    DROP TABLE migrate_test.tags;
    CREATE TABLE migrate_test.comments (
      id serial PRIMARY KEY,
      post_id integer NOT NULL REFERENCES migrate_test.posts (id),
      body text NOT NULL
    );
    DROP INDEX migrate_test.posts_title_idx;
    CREATE INDEX posts_author_idx ON migrate_test.posts (author_id);
    CREATE VIEW migrate_test.titles AS SELECT id, upper(title) AS title FROM migrate_test.posts;
    CREATE OR REPLACE FUNCTION migrate_test.shout(t text) RETURNS text LANGUAGE sql AS 'SELECT upper(t) || ''!''';
    CREATE FUNCTION migrate_test.whisper(t text) RETURNS text LANGUAGE sql AS 'SELECT lower(t)';
    COMMENT ON TABLE migrate_test.posts IS 'posts by authors';
";

async fn connect(config: &ConnectConfig) -> Session {
    Session::connect(config).await.expect("connect")
}

/// Just the test schema, so other schemas in the database don't take part.
async fn model(session: &Session) -> Database {
    let mut database = session.introspect().await.expect("introspect");
    database.schemas.retain(|schema| schema.name == SCHEMA);
    database.extensions.clear();
    database
}

async fn run(session: &Session, sql: &str) {
    session.execute(sql, 0).await.expect(sql);
}

async fn apply(session: &Session, plan: &Plan) {
    assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
    session.apply(plan).await.unwrap_or_else(|err| {
        let steps: Vec<&str> = plan.steps.iter().map(|s| s.sql.as_str()).collect();
        panic!("{err}\n{steps:#?}")
    });
}

#[tokio::test]
async fn applies_a_past_schema_and_comes_back() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let _exclusive = exclusive().await;
    run(&session, &format!("DROP SCHEMA IF EXISTS {SCHEMA} CASCADE")).await;
    run(&session, VERSION_A).await;
    let version_a = model(&session).await;
    run(&session, TO_VERSION_B).await;
    let version_b = model(&session).await;

    // Back to A: dropping B's additions and changing title's type lose data.
    let back = plan(&version_b, &version_a, &[]);
    assert!(back.is_destructive());
    apply(&session, &back).await;
    assert_eq!(model(&session).await, version_a);
    // Rows survive in the tables both versions have.
    let rows = session
        .execute(&format!("SELECT count(*) FROM {SCHEMA}.posts"), 10)
        .await
        .unwrap();
    assert!(
        matches!(&rows[0], dolmen_core::postgres::StatementResult::Rows { rows, .. } if rows[0][0].as_deref() == Some("2"))
    );

    // And forward to B again.
    apply(&session, &plan(&version_a, &version_b, &[])).await;
    assert_eq!(model(&session).await, version_b);

    // Nothing left to do once the database matches.
    assert!(
        plan(&model(&session).await, &version_b, &[])
            .steps
            .is_empty()
    );
    run(&session, &format!("DROP SCHEMA {SCHEMA} CASCADE")).await;
}

#[tokio::test]
async fn changes_nothing_when_a_step_fails() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let _exclusive = exclusive().await;
    run(&session, "DROP SCHEMA IF EXISTS migrate_fail_test CASCADE").await;
    run(
        &session,
        "CREATE SCHEMA migrate_fail_test;
         CREATE TABLE migrate_fail_test.items (id int);
         INSERT INTO migrate_fail_test.items VALUES (1);",
    )
    .await;
    let mut current = session.introspect().await.unwrap();
    current.schemas.retain(|s| s.name == "migrate_fail_test");
    current.extensions.clear();

    // A new table, then a NOT NULL column without a default on a table with rows.
    let mut target = current.clone();
    let schema = &mut target.schemas[0];
    let mut new_table = schema.tables[0].clone();
    new_table.name = "created_first".into();
    schema.tables.push(new_table);
    let mut column = schema.tables[0].columns[0].clone();
    column.name = "required".into();
    column.nullable = false;
    schema.tables[0].columns.push(column);

    let plan = plan(&current, &target, &[]);
    let err = session.apply(&plan).await.unwrap_err();
    assert!(
        matches!(&err, Error::Database { message, .. } if message.contains("nothing was changed")),
        "{err:?}"
    );
    let mut after = session.introspect().await.unwrap();
    after.schemas.retain(|s| s.name == "migrate_fail_test");
    after.extensions.clear();
    assert_eq!(
        after, current,
        "the table created before the failure is rolled back"
    );
    run(&session, "DROP SCHEMA migrate_fail_test CASCADE").await;
}

#[test]
fn only_local_servers_count_as_local() {
    let config = |host: &str| ConnectConfig {
        host: host.into(),
        port: 5432,
        user: "u".into(),
        password: None,
        database: "d".into(),
        ssl_mode: Default::default(),
    };
    for host in [
        "localhost",
        "LOCALHOST",
        "127.0.0.1",
        "127.1.2.3",
        "::1",
        "[::1]",
        "/tmp",
    ] {
        assert!(config(host).is_local(), "{host}");
    }
    for host in [
        "db.example.com",
        "10.0.0.5",
        "192.168.1.2",
        "host.docker.internal",
    ] {
        assert!(!config(host).is_local(), "{host}");
    }
}

#[tokio::test]
async fn reads_foreign_keys_with_their_columns_in_order() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let _exclusive = exclusive().await;
    run(
        &session,
        "DROP SCHEMA IF EXISTS fk_test CASCADE; DROP SCHEMA IF EXISTS fk_other CASCADE;
         CREATE SCHEMA fk_test; CREATE SCHEMA fk_other;
         CREATE TABLE fk_other.regions (code text PRIMARY KEY);
         CREATE TABLE fk_test.customers (id int, region text, PRIMARY KEY (region, id));
         CREATE TABLE fk_test.orders (
           \"Order Id\" int PRIMARY KEY,
           customer int, region text REFERENCES fk_other.regions ON UPDATE CASCADE,
           CONSTRAINT orders_customer_fkey FOREIGN KEY (region, customer)
             REFERENCES fk_test.customers (region, id) ON DELETE SET NULL
         );",
    )
    .await;
    let keys: Vec<_> = session
        .foreign_keys()
        .await
        .unwrap()
        .into_iter()
        .filter(|k| k.schema == "fk_test")
        .collect();
    let summary: Vec<_> = keys
        .iter()
        .map(|k| {
            (
                k.name.as_str(),
                k.columns.join(","),
                format!("{}.{}", k.referenced_schema, k.referenced_table),
                k.referenced_columns.join(","),
                k.on_update.as_str(),
                k.on_delete.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (
                "orders_customer_fkey",
                "region,customer".to_owned(),
                "fk_test.customers".to_owned(),
                "region,id".to_owned(),
                "NO ACTION",
                "SET NULL"
            ),
            (
                "orders_region_fkey",
                "region".to_owned(),
                "fk_other.regions".to_owned(),
                "code".to_owned(),
                "CASCADE",
                "NO ACTION"
            ),
        ]
    );
    run(
        &session,
        "DROP SCHEMA fk_test CASCADE; DROP SCHEMA fk_other CASCADE",
    )
    .await;
}

#[tokio::test]
async fn applies_edits_from_the_detail_view() {
    use dolmen_core::postgres::edit::{self, Edit};

    let config = config_or_skip!();
    let session = connect(&config).await;
    let _exclusive = exclusive().await;
    run(
        &session,
        "DROP SCHEMA IF EXISTS edit_test CASCADE;
         CREATE SCHEMA edit_test;
         CREATE TABLE edit_test.users (id int PRIMARY KEY, name text);
         INSERT INTO edit_test.users VALUES (1, 'kim');
         CREATE VIEW edit_test.names AS SELECT name FROM edit_test.users;
         CREATE FUNCTION edit_test.twice(n int) RETURNS int LANGUAGE sql AS 'SELECT n * 2';",
    )
    .await;
    let model = |database: Database| {
        let mut database = database;
        database.schemas.retain(|s| s.name == "edit_test");
        database
    };
    let current = session.introspect().await.unwrap();
    let schema = &model(current.clone()).schemas[0];

    // Rename the table and a column, add a column: the row and the view survive.
    let mut table = schema.tables[0].clone();
    table.name = "people".into();
    table.columns[1].name = "full_name".into();
    let mut email = table.columns[1].clone();
    email.name = "email".into();
    email.comment = Some("where receipts go".into());
    table.columns.push(email);
    let plan = edit::plan(
        &current,
        &Edit::Table {
            schema: "edit_test".into(),
            name: "users".into(),
            table,
            renamed_columns: vec![("name".into(), "full_name".into())],
        },
    )
    .unwrap();
    apply(&session, &plan).await;
    let after = model(session.introspect().await.unwrap());
    let people = &after.schemas[0].tables[0];
    assert_eq!(people.name, "people");
    let columns: Vec<_> = people.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(columns, ["id", "full_name", "email"]);
    assert_eq!(
        people.columns[2].comment.as_deref(),
        Some("where receipts go")
    );
    assert!(
        after.schemas[0].views[0]
            .definition
            .contains("edit_test.people"),
        "{}",
        after.schemas[0].views[0].definition
    );

    // A new view definition and a new function body.
    let current = session.introspect().await.unwrap();
    let mut view = model(current.clone()).schemas[0].views[0].clone();
    view.definition = "SELECT upper(full_name) AS name FROM edit_test.people".into();
    apply(
        &session,
        &edit::plan(
            &current,
            &Edit::View {
                schema: "edit_test".into(),
                name: "names".into(),
                view,
            },
        )
        .unwrap(),
    )
    .await;
    let current = session.introspect().await.unwrap();
    let function = model(current.clone()).schemas[0].functions[0].clone();
    apply(
        &session,
        &edit::plan(
            &current,
            &Edit::Function {
                schema: "edit_test".into(),
                name: function.name.clone(),
                arguments: function.arguments.clone(),
                definition: function.definition.replace("n * 2", "n * 3"),
            },
        )
        .unwrap(),
    )
    .await;
    let rows = session
        .execute(
            "SELECT name, edit_test.twice(2)::text FROM edit_test.names",
            10,
        )
        .await
        .unwrap();
    assert!(
        matches!(&rows[0], dolmen_core::postgres::StatementResult::Rows { rows, .. }
            if rows[0][0].as_deref() == Some("KIM") && rows[0][1].as_deref() == Some("6")),
        "{rows:?}"
    );
    run(&session, "DROP SCHEMA edit_test CASCADE").await;
}
