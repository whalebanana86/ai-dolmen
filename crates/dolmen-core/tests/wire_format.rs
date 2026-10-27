//! The JSON shapes the desktop app relies on (see `app/src/api.ts`).

use dolmen_core::Error;
use dolmen_core::connections::{PasswordChange, SavedConnection, SnapshotRemote};
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
        snapshot_dir: None,
        snapshot_remote: Some(SnapshotRemote {
            url: "git@github.com:you/schema.git".into(),
            path: "db/schema".into(),
        }),
        record_history: false,
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
            "snapshotDir": null,
            "snapshotRemote": { "url": "git@github.com:you/schema.git", "path": "db/schema" },
            "recordHistory": false,
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

#[test]
fn plans_and_renames_use_camel_case_and_kind_tags() {
    use dolmen_core::postgres::migrate::{Plan, Rename, Step};

    let rename = Rename::Column {
        schema: "app".into(),
        table: "users".into(),
        from: "email".into(),
        to: "mail".into(),
    };
    let plan = Plan {
        steps: vec![Step {
            sql: "ALTER TABLE app.users DROP COLUMN email;".into(),
            destructive: true,
        }],
        blockers: vec![],
        rename_candidates: vec![rename.clone()],
    };
    assert_eq!(
        serde_json::to_value(&plan).unwrap(),
        json!({
            "steps": [{ "sql": "ALTER TABLE app.users DROP COLUMN email;", "destructive": true }],
            "blockers": [],
            "renameCandidates": [
                { "kind": "column", "schema": "app", "table": "users", "from": "email", "to": "mail" }
            ],
        })
    );
    let back: Rename = serde_json::from_value(json!({
        "kind": "column", "schema": "app", "table": "users", "from": "email", "to": "mail"
    }))
    .unwrap();
    assert_eq!(back, rename);
    assert_eq!(
        serde_json::to_value(Error::Refused {
            reason: dolmen_core::Refusal::NotLocal,
            message: "no".into()
        })
        .unwrap(),
        json!({ "kind": "refused", "reason": "notLocal", "message": "no" })
    );
    assert_eq!(
        serde_json::to_value(dolmen_core::postgres::migrate::Blocker::EnumValuesRemoved {
            name: "app.mood".into()
        })
        .unwrap(),
        json!({ "kind": "enumValuesRemoved", "name": "app.mood" })
    );
}

#[test]
fn ai_tasks_answers_and_errors_match_the_app() {
    use dolmen_core::AiFailure;
    use dolmen_core::ai::{Answer, KeyStatus, Task};

    let tasks: Vec<Task> = serde_json::from_value(json!([
        { "kind": "generate", "request": "sign-ups per day", "context": null },
        { "kind": "explain", "sql": "SELECT 1" },
        { "kind": "fix", "sql": "SELEC 1", "error": "syntax error" },
    ]))
    .unwrap();
    assert_eq!(
        tasks,
        [
            Task::Generate {
                request: "sign-ups per day".into(),
                context: None
            },
            Task::Explain {
                sql: "SELECT 1".into()
            },
            Task::Fix {
                sql: "SELEC 1".into(),
                error: "syntax error".into()
            },
        ]
    );
    assert_eq!(
        serde_json::to_value(Answer {
            model: "claude-opus-5-5".into(),
            stop_reason: Some("end_turn".into()),
        })
        .unwrap(),
        json!({ "model": "claude-opus-5-5", "stopReason": "end_turn" })
    );
    assert_eq!(
        serde_json::to_value(KeyStatus {
            saved: true,
            from_environment: false,
        })
        .unwrap(),
        json!({ "saved": true, "fromEnvironment": false })
    );
    for (reason, name) in [
        (AiFailure::NoApiKey, "noApiKey"),
        (AiFailure::BadApiKey, "badApiKey"),
        (AiFailure::RateLimited, "rateLimited"),
        (AiFailure::Unavailable, "unavailable"),
        (AiFailure::Declined, "declined"),
        (AiFailure::Network, "network"),
        (AiFailure::Cancelled, "cancelled"),
        (AiFailure::Other, "other"),
    ] {
        assert_eq!(
            serde_json::to_value(Error::Ai {
                reason,
                message: "m".into()
            })
            .unwrap(),
            json!({ "kind": "ai", "reason": name, "message": "m" })
        );
    }
}

#[test]
fn foreign_keys_and_edits_match_the_app() {
    use dolmen_core::postgres::edit::Edit;
    use dolmen_core::schema::ForeignKey;

    assert_eq!(
        serde_json::to_value(ForeignKey {
            name: "order_user_id_fkey".into(),
            schema: "public".into(),
            table: "order".into(),
            columns: vec!["user_id".into()],
            referenced_schema: "public".into(),
            referenced_table: "users".into(),
            referenced_columns: vec!["id".into()],
            on_update: "NO ACTION".into(),
            on_delete: "CASCADE".into(),
        })
        .unwrap(),
        json!({
            "name": "order_user_id_fkey", "schema": "public", "table": "order",
            "columns": ["user_id"], "referencedSchema": "public", "referencedTable": "users",
            "referencedColumns": ["id"], "onUpdate": "NO ACTION", "onDelete": "CASCADE",
        })
    );
    let edit: Edit = serde_json::from_value(json!({
        "kind": "table", "schema": "public", "name": "users",
        "table": {
            "name": "people", "kind": "table", "columns": [], "constraints": [], "indexes": [],
            "partitionKey": null, "comment": null,
        },
        "renamedColumns": [["name", "full_name"]],
    }))
    .unwrap();
    assert!(
        matches!(edit, Edit::Table { renamed_columns, .. } if renamed_columns == [("name".to_owned(), "full_name".to_owned())])
    );
    let edit: Edit = serde_json::from_value(json!({
        "kind": "function", "schema": "public", "name": "add", "arguments": "a integer",
        "definition": "CREATE OR REPLACE FUNCTION public.add(a integer) …",
    }))
    .unwrap();
    assert!(matches!(edit, Edit::Function { .. }));
}
