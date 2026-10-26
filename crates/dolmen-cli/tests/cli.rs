//! `dolmen schema dump` and `dolmen schema check` against a real PostgreSQL server.
//!
//! Skipped unless `DOLMEN_TEST_PG_HOST` is set; see the README.

use std::process::{Command, Output};

use dolmen_core::postgres::{ConnectConfig, Session};
use tempfile::TempDir;

fn test_url() -> Option<String> {
    let host = std::env::var("DOLMEN_TEST_PG_HOST").ok()?;
    let port = std::env::var("DOLMEN_TEST_PG_PORT").unwrap_or_else(|_| "5432".into());
    Some(format!(
        "postgres://postgres:postgres@{host}:{port}/postgres"
    ))
}

fn dolmen(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dolmen"))
        .args(args)
        .output()
        .expect("run dolmen")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[tokio::test]
async fn dumps_and_checks_snapshots() {
    let Some(url) = test_url() else {
        eprintln!("skipped: DOLMEN_TEST_PG_HOST is not set");
        return;
    };
    let session = Session::connect(&ConnectConfig::parse(&url).unwrap())
        .await
        .unwrap();
    session
        .execute(
            "DROP SCHEMA IF EXISTS cli_test CASCADE;
             CREATE SCHEMA cli_test;
             CREATE TABLE cli_test.items (id int PRIMARY KEY);",
            0,
        )
        .await
        .unwrap();

    let dir = TempDir::new().unwrap();
    let folder = dir.path().join("schema");
    let folder = folder.to_str().unwrap();

    // Checking a folder that holds no snapshot is an error, not "everything changed".
    let missing = dolmen(&["schema", "check", "--url", &url, "--dir", folder]);
    assert_eq!(missing.status.code(), Some(2));

    let dump = dolmen(&["schema", "dump", "--url", &url, "--dir", folder]);
    assert!(
        dump.status.success(),
        "{}",
        String::from_utf8_lossy(&dump.stderr)
    );
    assert!(
        stdout(&dump).contains("A cli_test/tables/items.sql"),
        "{}",
        stdout(&dump)
    );

    let clean = dolmen(&["schema", "check", "--url", &url, "--dir", folder]);
    assert_eq!(clean.status.code(), Some(0), "{}", stdout(&clean));

    session
        .execute("ALTER TABLE cli_test.items ADD COLUMN note text", 0)
        .await
        .unwrap();
    let drift = dolmen(&["schema", "check", "--url", &url, "--dir", folder]);
    session
        .execute("DROP SCHEMA cli_test CASCADE", 0)
        .await
        .unwrap();

    assert_eq!(drift.status.code(), Some(1), "{}", stdout(&drift));
    assert!(
        stdout(&drift).contains("M cli_test/tables/items.sql"),
        "{}",
        stdout(&drift)
    );
    assert!(
        stdout(&drift).contains("+    note text"),
        "{}",
        stdout(&drift)
    );
}
