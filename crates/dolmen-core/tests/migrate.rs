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
