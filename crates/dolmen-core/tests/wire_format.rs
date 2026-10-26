//! The JSON shapes the desktop app relies on (see `app/src/api.ts`).

use dolmen_core::Error;
use dolmen_core::connections::{PasswordChange, SavedConnection};
use dolmen_core::postgres::{ConnectConfig, SslMode, StatementResult};
use dolmen_core::schema::{Column, ConstraintKind, Identity};
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
fn schema_columns_and_kinds_use_camel_case() {
    let column = Column {
        name: "id".into(),
        data_type: "bigint".into(),
        nullable: false,
        default: None,
        identity: Some(Identity::ByDefault),
        generated: None,
        comment: None,
    };
    assert_eq!(
        serde_json::to_value(column).unwrap(),
        json!({
            "name": "id",
            "dataType": "bigint",
            "nullable": false,
            "default": null,
            "identity": "byDefault",
            "generated": null,
            "comment": null,
        })
    );
    assert_eq!(
        serde_json::to_value(ConstraintKind::PrimaryKey).unwrap(),
        json!("primaryKey")
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

#[test]
fn saved_connections_use_camel_case() {
    let connection = SavedConnection {
        id: "abc".into(),
        name: "local".into(),
        host: "localhost".into(),
        port: 5432,
        user: "postgres".into(),
        database: "postgres".into(),
        ssl_mode: SslMode::VerifyFull,
        has_password: true,
    };
    let value = serde_json::to_value(&connection).unwrap();
    assert_eq!(
        value,
        json!({
            "id": "abc",
            "name": "local",
            "host": "localhost",
            "port": 5432,
            "user": "postgres",
            "database": "postgres",
            "sslMode": "verify-full",
            "hasPassword": true,
        })
    );
    assert_eq!(
        serde_json::from_value::<SavedConnection>(value).unwrap(),
        connection
    );
}

#[test]
fn password_changes_are_tagged_by_kind() {
    let change: PasswordChange =
        serde_json::from_value(json!({ "kind": "set", "password": "pw" })).unwrap();
    assert!(matches!(change, PasswordChange::Set { password } if password == "pw"));
    assert!(matches!(
        serde_json::from_value(json!({ "kind": "keep" })).unwrap(),
        PasswordChange::Keep
    ));
    assert!(matches!(
        serde_json::from_value(json!({ "kind": "forget" })).unwrap(),
        PasswordChange::Forget
    ));
    assert_eq!(
        serde_json::to_value(Error::Storage {
            message: "disk full".into()
        })
        .unwrap(),
        json!({ "kind": "storage", "message": "disk full" })
    );
}
