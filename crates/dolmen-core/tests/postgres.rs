//! Integration tests against a real PostgreSQL server.
//!
//! Skipped unless `DOLMEN_TEST_PG_HOST` is set. `docker compose up -d` at the
//! repository root starts a matching server; see the README.

use std::sync::Arc;
use std::time::Duration;

use dolmen_core::Error;
use dolmen_core::postgres::{ConnectConfig, RowEstimate, Session, SslMode, StatementResult};
use dolmen_core::schema::{Column, ConstraintKind, FunctionKind, Generated, Identity, TableKind};
use dolmen_core::sql;

#[macro_use]
mod common;
use common::{exclusive, test_config};

async fn connect(config: &ConnectConfig) -> Session {
    Session::connect(config).await.expect("connect")
}

async fn execute(session: &Session, sql: &str) -> Vec<StatementResult> {
    session.execute(sql, 1000).await.expect("execute")
}

fn rows(result: &StatementResult) -> (&[String], &[Vec<Option<String>>], u64) {
    match result {
        StatementResult::Rows {
            columns,
            rows,
            total_rows,
        } => (columns, rows, *total_rows),
        other => panic!("expected rows, got {other:?}"),
    }
}

fn text(value: &str) -> Option<String> {
    Some(value.to_owned())
}

#[tokio::test]
async fn reports_server_version() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    assert!(
        session
            .server_version()
            .starts_with(|c: char| c.is_ascii_digit()),
        "unexpected version {:?}",
        session.server_version()
    );
}

#[tokio::test]
async fn returns_values_as_exact_text() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let results = execute(
        &session,
        "SELECT 9223372036854775807::bigint AS big, 0.1::numeric(20, 19) AS num, \
         NULL::text AS nothing, 'héllo' AS greeting",
    )
    .await;

    let (columns, rows, total_rows) = rows(&results[0]);
    assert_eq!(columns, ["big", "num", "nothing", "greeting"]);
    assert_eq!(
        rows,
        [vec![
            text("9223372036854775807"),
            text("0.1000000000000000000"),
            None,
            text("héllo"),
        ]]
    );
    assert_eq!(total_rows, 1);
}

#[tokio::test]
async fn returns_one_result_per_statement() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let results = execute(
        &session,
        "CREATE TEMP TABLE t (id int); INSERT INTO t VALUES (1), (2); SELECT id FROM t ORDER BY id",
    )
    .await;

    assert_eq!(results.len(), 3);
    assert!(matches!(
        results[0],
        StatementResult::Command { rows_affected: 0 }
    ));
    assert!(matches!(
        results[1],
        StatementResult::Command { rows_affected: 2 }
    ));
    assert_eq!(rows(&results[2]).1, [vec![text("1")], vec![text("2")]]);
}

#[tokio::test]
async fn keeps_session_state_between_executions() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    execute(&session, "SET application_name = 'dolmen-test'").await;
    let results = execute(&session, "SHOW application_name").await;
    assert_eq!(rows(&results[0]).1, [vec![text("dolmen-test")]]);
}

#[tokio::test]
async fn keeps_columns_of_empty_results() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let results = execute(&session, "SELECT 1 AS a WHERE false").await;
    let (columns, rows, total_rows) = rows(&results[0]);
    assert_eq!(columns, ["a"]);
    assert!(rows.is_empty());
    assert_eq!(total_rows, 0);
}

#[tokio::test]
async fn caps_kept_rows_but_counts_all() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let results = session
        .execute("SELECT generate_series(1, 2500); SELECT 'after'", 1000)
        .await
        .expect("execute");

    let (_, kept, total_rows) = rows(&results[0]);
    assert_eq!(kept.len(), 1000);
    assert_eq!(total_rows, 2500);
    assert_eq!(rows(&results[1]).1, [vec![text("after")]]);
}

#[tokio::test]
async fn reports_syntax_errors_with_position() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let err = session.execute("SELECT * FORM t", 1000).await.unwrap_err();
    match err {
        Error::Database { code, position, .. } => {
            assert_eq!(code, "42601");
            assert_eq!(position, Some(10));
        }
        other => panic!("expected a database error, got {other:?}"),
    }
}

#[tokio::test]
async fn cancels_a_running_statement() {
    let config = config_or_skip!();
    let session = Arc::new(connect(&config).await);
    let running = tokio::spawn({
        let session = session.clone();
        async move { session.execute("SELECT pg_sleep(30)", 1000).await }
    });

    // Cancel only once the server runs the statement; a cancel that arrives
    // first is ignored, and the statement would then run its full 30 seconds.
    let observer = connect(&config).await;
    let running_now = "SELECT count(*) FROM pg_stat_activity \
                       WHERE query = 'SELECT pg_sleep(30)' AND state = 'active'";
    for _ in 0..100 {
        let count = rows(&execute(&observer, running_now).await[0]).1[0][0].clone();
        if count.as_deref() != Some("0") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    session.cancel().await.expect("cancel");

    let result = tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .expect("statement still running after cancel")
        .expect("task panicked");
    assert!(
        matches!(&result, Err(Error::Database { code, .. }) if code == "57014"),
        "expected query_canceled, got {result:?}"
    );
}

#[tokio::test]
async fn rejects_a_wrong_password() {
    let mut config = config_or_skip!();
    config.password = Some("wrong".into());
    let err = Session::connect(&config)
        .await
        .err()
        .expect("connect should fail");
    assert!(
        matches!(&err, Error::Database { code, .. } if code == "28P01"),
        "expected invalid_password, got {err:?}"
    );
}

#[tokio::test]
async fn introspects_the_database_structure() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = connect(&config).await;
    execute(
        &session,
        "DROP SCHEMA IF EXISTS introspect_test CASCADE;
         CREATE SCHEMA introspect_test;
         SET search_path = introspect_test;
         CREATE EXTENSION pgcrypto SCHEMA introspect_test;
         CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy');
         CREATE TABLE authors (
           id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
           email varchar(255) NOT NULL UNIQUE,
           dropped int
         );
         ALTER TABLE authors DROP COLUMN dropped;
         COMMENT ON TABLE authors IS 'people who write';
         CREATE TABLE posts (
           id serial PRIMARY KEY,
           author_id bigint NOT NULL REFERENCES authors (id),
           title text NOT NULL CHECK (title <> ''),
           mood mood DEFAULT 'ok',
           title_length int GENERATED ALWAYS AS (length(title)) STORED
         );
         COMMENT ON COLUMN posts.title IS 'shown in lists';
         CREATE INDEX posts_author_idx ON posts (author_id);
         CREATE VIEW recent_posts AS SELECT id, title FROM posts;
         CREATE MATERIALIZED VIEW post_counts AS SELECT author_id, count(*) AS n FROM posts GROUP BY author_id;
         CREATE FUNCTION add(a int, b int) RETURNS int LANGUAGE sql AS 'SELECT a + b';
         CREATE PROCEDURE touch() LANGUAGE sql AS 'SELECT 1';
         CREATE SEQUENCE invoice_numbers;",
    )
    .await;

    // The SET search_path above stays in the user's session; introspection
    // still names every schema, so snapshots don't depend on session settings.
    let database = session.introspect().await.expect("introspect");
    execute(&session, "DROP SCHEMA introspect_test CASCADE").await;

    let schema = database
        .schemas
        .iter()
        .find(|s| s.name == "introspect_test")
        .expect("schema listed");
    assert!(
        database
            .schemas
            .iter()
            .all(|s| s.name != "pg_catalog" && s.name != "information_schema")
    );
    assert!(
        database
            .extensions
            .iter()
            .any(|e| e.name == "pgcrypto" && e.schema == "introspect_test")
    );

    let names = |items: Vec<&str>| items.into_iter().map(str::to_owned).collect::<Vec<_>>();
    assert_eq!(
        names(schema.tables.iter().map(|t| t.name.as_str()).collect()),
        ["authors", "posts"]
    );

    let authors = &schema.tables[0];
    assert_eq!(authors.kind, TableKind::Table);
    assert_eq!(authors.comment.as_deref(), Some("people who write"));
    // Dropped columns are gone and the rest keep their order.
    assert_eq!(
        authors.columns,
        [
            Column {
                name: "id".into(),
                data_type: "bigint".into(),
                nullable: false,
                default: None,
                identity: Some(Identity::Always),
                generated: None,
                comment: None,
            },
            Column {
                name: "email".into(),
                data_type: "character varying(255)".into(),
                nullable: false,
                default: None,
                identity: None,
                generated: None,
                comment: None,
            },
        ]
    );
    // NOT NULL is a column property, not a listed constraint (PostgreSQL 18 also
    // stores it in pg_constraint), and constraint-backed indexes are not listed.
    assert_eq!(
        authors
            .constraints
            .iter()
            .map(|c| (c.kind, c.definition.as_str()))
            .collect::<Vec<_>>(),
        [
            (ConstraintKind::Unique, "UNIQUE (email)"),
            (ConstraintKind::PrimaryKey, "PRIMARY KEY (id)")
        ]
    );
    assert!(authors.indexes.is_empty());

    let posts = &schema.tables[1];
    let column = |name: &str| posts.columns.iter().find(|c| c.name == name).unwrap();
    assert_eq!(
        column("id").default.as_deref(),
        Some("nextval('introspect_test.posts_id_seq'::regclass)")
    );
    assert_eq!(column("mood").data_type, "introspect_test.mood");
    assert_eq!(
        column("mood").default.as_deref(),
        Some("'ok'::introspect_test.mood")
    );
    assert_eq!(column("title").comment.as_deref(), Some("shown in lists"));
    assert_eq!(
        column("title_length").generated,
        Some(Generated {
            expression: "length(title)".into(),
            stored: true
        })
    );
    assert_eq!(column("title_length").default, None);
    assert!(
        posts
            .constraints
            .iter()
            .any(|c| c.kind == ConstraintKind::ForeignKey
                && c.definition
                    == "FOREIGN KEY (author_id) REFERENCES introspect_test.authors(id)")
    );
    assert!(
        posts
            .constraints
            .iter()
            .any(|c| c.kind == ConstraintKind::Check)
    );
    assert_eq!(posts.indexes.len(), 1);
    assert_eq!(posts.indexes[0].name, "posts_author_idx");
    assert!(
        posts.indexes[0]
            .definition
            .starts_with("CREATE INDEX posts_author_idx ON introspect_test.posts")
    );

    assert_eq!(
        schema
            .views
            .iter()
            .map(|v| (v.name.as_str(), v.materialized))
            .collect::<Vec<_>>(),
        [("post_counts", true), ("recent_posts", false)]
    );
    assert_eq!(
        schema.views[1]
            .columns
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "title"]
    );

    // pgcrypto's functions belong to the extension and are left out.
    assert_eq!(
        schema
            .functions
            .iter()
            .map(|f| (f.name.as_str(), f.arguments.as_str(), f.kind))
            .collect::<Vec<_>>(),
        [
            ("add", "a integer, b integer", FunctionKind::Function),
            ("touch", "", FunctionKind::Procedure)
        ]
    );
    assert!(
        schema.functions[0]
            .definition
            .starts_with("CREATE OR REPLACE FUNCTION introspect_test.add")
    );

    // posts_id_seq belongs to the serial column, so only the standalone sequence is listed.
    assert_eq!(
        names(schema.sequences.iter().map(|s| s.name.as_str()).collect()),
        ["invoice_numbers"]
    );
    assert_eq!(schema.enums.len(), 1);
    assert_eq!(schema.enums[0].labels, ["sad", "ok", "happy"]);
}

#[tokio::test]
async fn reports_the_current_schema() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    assert_eq!(
        session.current_schema().await.unwrap().as_deref(),
        Some("public")
    );
}

#[tokio::test]
async fn read_only_sessions_refuse_writes_but_explain() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = connect(&config).await;
    // Not a temporary table: read-only transactions may still write those.
    execute(
        &session,
        "DROP TABLE IF EXISTS guarded; CREATE TABLE guarded (id int); \
         INSERT INTO guarded VALUES (1), (2)",
    )
    .await;
    session.set_read_only(true).await.unwrap();
    match session.execute("DELETE FROM guarded", 10).await {
        Err(Error::Database { code, .. }) => assert_eq!(code, "25006"),
        other => panic!("expected a read-only error, got {other:?}"),
    }
    // Estimates still work: EXPLAIN runs nothing.
    let risk = sql::risk("DELETE FROM guarded").unwrap();
    let estimate = session.estimate("DELETE FROM guarded", &risk).await;
    assert!(estimate.is_some(), "no estimate in a read-only session");
    session.set_read_only(false).await.unwrap();
    let results = execute(&session, "DELETE FROM guarded").await;
    assert!(matches!(
        results[..],
        [StatementResult::Command { rows_affected: 2 }]
    ));
    execute(&session, "DROP TABLE guarded").await;
}

#[tokio::test]
async fn estimates_the_rows_a_risky_statement_touches() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    execute(
        &session,
        "CREATE TEMP TABLE counted AS SELECT g AS id FROM generate_series(1, 500) g; \
         CREATE TEMP TABLE unanalyzed (id int); \
         INSERT INTO unanalyzed SELECT generate_series(1, 7); ANALYZE counted",
    )
    .await;
    let estimate = |statement: &'static str| {
        let session = &session;
        async move {
            let risk = sql::risk(statement).expect("risky");
            session.estimate(statement, &risk).await
        }
    };
    assert_eq!(
        estimate("DELETE FROM counted").await,
        Some(RowEstimate {
            rows: 500,
            at_least: false
        })
    );
    assert_eq!(
        estimate("UPDATE counted SET id = id + 1;")
            .await
            .map(|e| e.rows),
        Some(500)
    );
    assert_eq!(
        estimate("TRUNCATE counted").await.map(|e| e.rows),
        Some(500)
    );
    // Never analyzed: counted instead.
    assert_eq!(
        estimate("DROP TABLE unanalyzed").await,
        Some(RowEstimate {
            rows: 7,
            at_least: false
        })
    );
    assert_eq!(estimate("TRUNCATE no_such_table").await, None);
    assert_eq!(estimate("DROP FUNCTION nothing()").await, None);
    // A second statement the splitter missed is refused, not run.
    assert_eq!(
        estimate("DELETE FROM counted; DROP TABLE unanalyzed").await,
        None
    );
    assert_eq!(
        estimate("TRUNCATE unanalyzed").await.map(|e| e.rows),
        Some(7)
    );
}

#[tokio::test]
async fn explains_a_statement_as_json() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let plan = session
        .explain(
            "SELECT * FROM pg_catalog.pg_class WHERE relname = 'x';",
            false,
        )
        .await
        .unwrap();
    let top = &plan[0]["Plan"];
    assert!(top["Node Type"].is_string(), "{plan}");
    assert!(top["Total Cost"].is_number());
    assert!(top.get("Actual Total Time").is_none());

    let analyzed = session
        .explain("SELECT count(*) FROM generate_series(1, 1000)", true)
        .await
        .unwrap();
    assert!(analyzed[0]["Execution Time"].is_number(), "{analyzed}");
    assert_eq!(analyzed[0]["Plan"]["Actual Rows"].as_f64(), Some(1.0));
}

#[tokio::test]
async fn analyzing_a_change_rolls_it_back() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let session = connect(&config).await;
    execute(
        &session,
        "DROP TABLE IF EXISTS explained; CREATE TABLE explained (id int); \
         INSERT INTO explained VALUES (1), (2), (3)",
    )
    .await;
    let plan = session
        .explain("DELETE FROM explained", true)
        .await
        .unwrap();
    assert_eq!(plan[0]["Plan"]["Node Type"], "ModifyTable");
    let counted = execute(&session, "SELECT count(*) FROM explained").await;
    assert_eq!(rows(&counted[0]).1, [vec![text("3")]]);
    // One statement only: a second can't slip past the rollback.
    assert!(
        session
            .explain("DELETE FROM explained; DROP TABLE explained", true)
            .await
            .is_err()
    );
    let counted = execute(&session, "SELECT count(*) FROM explained").await;
    assert_eq!(rows(&counted[0]).1, [vec![text("3")]]);
    execute(&session, "DROP TABLE explained").await;
}

#[tokio::test]
async fn explain_errors_point_into_the_statement() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    match session.explain("  SELECT * FORM nowhere", false).await {
        Err(Error::Database { code, position, .. }) => {
            assert_eq!(code, "42601");
            // "FORM" starts at the 12th character of the statement as given.
            assert_eq!(position, Some(12));
        }
        other => panic!("expected a syntax error, got {other:?}"),
    }
}

#[tokio::test]
async fn copies_the_search_path() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    execute(&session, "SET search_path = pg_catalog, public").await;
    let path = session.search_path().await.unwrap();
    assert_eq!(path, "pg_catalog, public");
    let other = session.another().await.unwrap();
    other.set_search_path(&path).await.unwrap();
    assert_eq!(other.search_path().await.unwrap(), path);
}

// Needs no server: nothing listens on port 1.
#[tokio::test]
async fn explains_why_a_connection_failed() {
    let config = ConnectConfig {
        host: "localhost".into(),
        port: 1,
        user: "postgres".into(),
        password: None,
        database: "postgres".into(),
        ssl_mode: SslMode::Disable,
    };
    let err = Session::connect(&config)
        .await
        .err()
        .expect("connect should fail");
    match err {
        Error::Connection { message } => {
            assert!(
                message.starts_with("error connecting to server: "),
                "cause missing from {message:?}"
            );
        }
        other => panic!("expected a connection error, got {other:?}"),
    }
}

#[tokio::test]
async fn require_fails_when_the_server_has_no_tls() {
    let mut config = config_or_skip!();
    config.ssl_mode = SslMode::Require;
    let err = Session::connect(&config)
        .await
        .err()
        .expect("connect should fail");
    assert!(matches!(err, Error::Connection { .. }), "got {err:?}");
}

#[tokio::test]
async fn another_session_keeps_its_own_search_path() {
    let config = config_or_skip!();
    let _exclusive = exclusive().await;
    let first = Session::connect(&config).await.expect("connect");
    first
        .execute("CREATE SCHEMA IF NOT EXISTS tab_test", 0)
        .await
        .unwrap();
    first
        .execute(
            "DROP SCHEMA IF EXISTS tab_other CASCADE; CREATE SCHEMA tab_other; \
             CREATE TABLE tab_other.elsewhere (id int)",
            0,
        )
        .await
        .unwrap();
    let second = first.another().await.expect("another session");
    assert_eq!(
        second.set_schema("tab_test").await.unwrap().as_deref(),
        Some("tab_test")
    );
    assert_eq!(
        first.current_schema().await.unwrap().as_deref(),
        Some("public")
    );

    // Names resolve in the chosen schema alone; built-ins still work.
    let err = second
        .execute("SELECT * FROM elsewhere", 0)
        .await
        .unwrap_err();
    assert!(
        matches!(err, Error::Database { ref code, .. } if code == "42P01"),
        "{err:?}"
    );
    assert!(
        second
            .execute("SELECT now(), count(*) FROM tab_other.elsewhere", 0)
            .await
            .is_ok()
    );
    first
        .execute("DROP SCHEMA tab_test; DROP SCHEMA tab_other CASCADE", 0)
        .await
        .unwrap();
}
