//! Integration tests against a real PostgreSQL server.
//!
//! Skipped unless `DOLMEN_TEST_PG_HOST` is set. `docker compose up -d` at the
//! repository root starts a matching server; see the README.

use std::sync::Arc;
use std::time::Duration;

use dolmen_core::Error;
use dolmen_core::postgres::{ConnectConfig, Session, SslMode, StatementResult};

fn test_config() -> Option<ConnectConfig> {
    let host = std::env::var("DOLMEN_TEST_PG_HOST").ok()?;
    let port = std::env::var("DOLMEN_TEST_PG_PORT")
        .map(|port| {
            port.parse()
                .expect("DOLMEN_TEST_PG_PORT must be a port number")
        })
        .unwrap_or(5432);
    Some(ConnectConfig {
        host,
        port,
        user: "postgres".into(),
        password: Some("postgres".into()),
        database: "postgres".into(),
        ssl_mode: SslMode::Prefer,
    })
}

macro_rules! config_or_skip {
    () => {
        match test_config() {
            Some(config) => config,
            None => {
                eprintln!("skipped: DOLMEN_TEST_PG_HOST is not set");
                return;
            }
        }
    };
}

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

    tokio::time::sleep(Duration::from_millis(300)).await;
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
