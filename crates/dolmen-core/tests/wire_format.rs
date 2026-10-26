//! The JSON shapes the desktop app relies on (see `app/src/api.ts`).

use dolmen_core::Error;
use dolmen_core::postgres::{ConnectConfig, SchemaOutline, SslMode, StatementResult, TableOutline};
use serde_json::json;

#[test]
fn connect_config_reads_camel_case_and_libpq_ssl_modes() {
    let config: ConnectConfig = serde_json::from_value(json!({
        "host": "db.example.com",
        "port": 6543,
        "user": "app",
        "password": null,
        "database": "main",
        "sslMode": "verify-full",
    }))
    .unwrap();
    assert_eq!(config.port, 6543);
    assert_eq!(config.password, None);
    assert_eq!(config.ssl_mode, SslMode::VerifyFull);
}

#[test]
fn connect_config_debug_hides_the_password() {
    let config: ConnectConfig = serde_json::from_value(json!({
        "host": "localhost",
        "port": 5432,
        "user": "postgres",
        "password": "hunter2",
        "database": "postgres",
        "sslMode": "prefer",
    }))
    .unwrap();
    assert!(!format!("{config:?}").contains("hunter2"));
}

#[test]
fn statement_results_are_tagged_by_kind() {
    let rows = StatementResult::Rows {
        columns: vec!["id".into()],
        rows: vec![vec![Some("1".into())], vec![None]],
        total_rows: 2,
    };
    assert_eq!(
        serde_json::to_value(rows).unwrap(),
        json!({ "kind": "rows", "columns": ["id"], "rows": [["1"], [null]], "totalRows": 2 })
    );
    assert_eq!(
        serde_json::to_value(StatementResult::Command { rows_affected: 3 }).unwrap(),
        json!({ "kind": "command", "rowsAffected": 3 })
    );
}

#[test]
fn schema_outline_uses_camel_case() {
    let outline = SchemaOutline {
        default_schema: "public".into(),
        tables: vec![TableOutline {
            schema: "public".into(),
            name: "users".into(),
            columns: vec!["id".into(), "email".into()],
        }],
    };
    assert_eq!(
        serde_json::to_value(outline).unwrap(),
        json!({
            "defaultSchema": "public",
            "tables": [{ "schema": "public", "name": "users", "columns": ["id", "email"] }],
        })
    );
}

#[test]
fn errors_are_tagged_by_kind() {
    let database = Error::Database {
        message: "syntax error at or near \"FORM\"".into(),
        code: "42601".into(),
        detail: None,
        hint: None,
        position: Some(10),
    };
    assert_eq!(
        serde_json::to_value(database).unwrap(),
        json!({
            "kind": "database",
            "message": "syntax error at or near \"FORM\"",
            "code": "42601",
            "detail": null,
            "hint": null,
            "position": 10,
        })
    );
    assert_eq!(
        serde_json::to_value(Error::Closed).unwrap(),
        json!({ "kind": "closed" })
    );
}
