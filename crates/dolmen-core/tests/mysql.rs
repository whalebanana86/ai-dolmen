//! Integration tests against a real MySQL or MariaDB server.
//!
//! Skipped unless `DOLMEN_TEST_MYSQL_HOST` is set, with `DOLMEN_TEST_MYSQL_PORT`
//! and `DOLMEN_TEST_MYSQL_PASSWORD` for the root user, e.g. for a container:
//!
//!   docker run -d --rm -e MYSQL_ROOT_PASSWORD=mysql -p 53306:3306 mysql:8.4

use std::sync::Arc;
use std::time::{Duration, Instant};

use dolmen_core::Error;
use dolmen_core::mysql::{Flavor, Session};
use dolmen_core::postgres::{ConnectConfig, SslMode, StatementResult};
use dolmen_core::schema::{ConstraintKind, FunctionKind, Identity};

fn config() -> Option<ConnectConfig> {
    let host = std::env::var("DOLMEN_TEST_MYSQL_HOST").ok()?;
    Some(ConnectConfig {
        host,
        port: std::env::var("DOLMEN_TEST_MYSQL_PORT")
            .map(|p| {
                p.parse()
                    .expect("DOLMEN_TEST_MYSQL_PORT must be a port number")
            })
            .unwrap_or(3306),
        user: "root".into(),
        password: std::env::var("DOLMEN_TEST_MYSQL_PASSWORD").ok(),
        database: String::new(),
        ssl_mode: SslMode::Prefer,
    })
}

macro_rules! config_or_skip {
    () => {
        match config() {
            Some(config) => config,
            None => {
                eprintln!("skipped: DOLMEN_TEST_MYSQL_HOST is not set");
                return;
            }
        }
    };
}

/// The tests share one database, so they take turns.
async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

async fn run(session: &Session, sql: &str) -> Vec<StatementResult> {
    session.execute(sql, 1000).await.expect(sql)
}

fn rows(result: &StatementResult) -> (&[String], &[Vec<Option<String>>], u64) {
    match result {
        StatementResult::Rows {
            columns,
            rows,
            total_rows,
            ..
        } => (columns, rows, *total_rows),
        other => panic!("expected rows, got {other:?}"),
    }
}

const SETUP: &str = "
    DROP DATABASE IF EXISTS dolmen_test;
    CREATE DATABASE dolmen_test;
    USE dolmen_test;
    CREATE TABLE customers (
      id bigint NOT NULL AUTO_INCREMENT PRIMARY KEY,
      email varchar(120) NOT NULL,
      grade varchar(10) NOT NULL DEFAULT 'basic' COMMENT 'member grade',
      joined_at datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
      email_length int GENERATED ALWAYS AS (char_length(email)) VIRTUAL,
      photo varbinary(16),
      CONSTRAINT customers_email_key UNIQUE (email),
      CONSTRAINT grade_known CHECK (grade IN ('basic', 'vip'))
    ) COMMENT = 'people who buy';
    CREATE TABLE orders (
      id int NOT NULL AUTO_INCREMENT PRIMARY KEY,
      customer_id bigint NOT NULL,
      total decimal(10,2) NOT NULL DEFAULT 0,
      note text,
      CONSTRAINT orders_customer_fk FOREIGN KEY (customer_id) REFERENCES customers (id) ON DELETE CASCADE
    );
    CREATE INDEX orders_total_note ON orders (total, note(20));
    CREATE VIEW big_orders AS SELECT id, total FROM orders WHERE total > 100;
    CREATE FUNCTION double_it(n int) RETURNS int DETERMINISTIC RETURN n * 2;
    CREATE PROCEDURE touch_orders() BEGIN UPDATE orders SET note = note; END;
    CREATE TRIGGER orders_note BEFORE INSERT ON orders FOR EACH ROW SET NEW.note = COALESCE(NEW.note, '');
";

#[tokio::test]
async fn runs_statements_and_reads_the_structure() {
    let config = config_or_skip!();
    let _turn = exclusive().await;
    let session = Session::connect(&config).await.expect("connect");
    assert!(!session.server_version().is_empty());
    let mariadb = session.server_version().to_lowercase().contains("mariadb");
    assert_eq!(session.flavor() == Flavor::MariaDb, mariadb);
    run(&session, SETUP).await;

    // Several statements, each with its own result.
    let results = run(
        &session,
        "INSERT INTO customers (email, photo) VALUES ('a@example.com', 0x00ff), ('b@example.com', NULL);
         UPDATE customers SET grade = 'vip' WHERE email = 'b@example.com';
         SELECT id, email, grade, email_length, photo FROM customers ORDER BY id;",
    )
    .await;
    assert!(matches!(
        results[0],
        StatementResult::Command { rows_affected: 2 }
    ));
    assert!(matches!(
        results[1],
        StatementResult::Command { rows_affected: 1 }
    ));
    let (columns, values, total) = rows(&results[2]);
    assert_eq!(columns, ["id", "email", "grade", "email_length", "photo"]);
    assert_eq!(total, 2);
    assert_eq!(
        values[0],
        [
            Some("1".to_owned()),
            Some("a@example.com".to_owned()),
            Some("basic".to_owned()),
            Some("13".to_owned()),
            Some("\\x00ff".to_owned())
        ]
    );
    assert_eq!(values[1][4], None);

    // Rows past the limit are counted, not kept.
    let limited = session
        .execute("SELECT * FROM information_schema.COLLATIONS", 3)
        .await
        .unwrap();
    let (_, kept, total) = rows(&limited[0]);
    assert_eq!(kept.len(), 3);
    assert!(total > 3);

    // A server error keeps its number and SQLSTATE.
    match session.execute("SELEC 1", 10).await {
        Err(Error::Database { code, .. }) => assert_eq!(code, "1064 (42000)"),
        other => panic!("expected a syntax error, got {other:?}"),
    }
    // And the session still works after it.
    run(&session, "SELECT 1").await;

    assert_eq!(
        session.current_schema().await.unwrap().as_deref(),
        Some("dolmen_test")
    );
    assert_eq!(
        session
            .set_schema("information_schema")
            .await
            .unwrap()
            .as_deref(),
        Some("information_schema")
    );
    session.set_schema("dolmen_test").await.unwrap();

    let database = session.introspect().await.unwrap();
    let schema = database
        .schemas
        .iter()
        .find(|s| s.name == "dolmen_test")
        .unwrap();
    assert!(
        database
            .schemas
            .iter()
            .all(|s| s.name != "mysql" && s.name != "sys")
    );
    assert_eq!(
        schema
            .tables
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["customers", "orders"]
    );
    let customers = &schema.tables[0];
    assert_eq!(customers.comment.as_deref(), Some("people who buy"));
    let column = |name: &str| customers.columns.iter().find(|c| c.name == name).unwrap();
    assert_eq!(column("id").identity, Some(Identity::ByDefault));
    assert_eq!(column("email").data_type, "varchar(120)");
    assert!(!column("email").nullable);
    assert_eq!(column("grade").default.as_deref(), Some("'basic'"));
    assert_eq!(column("grade").comment.as_deref(), Some("member grade"));
    assert!(
        column("joined_at")
            .default
            .as_deref()
            .is_some_and(|d| d.to_lowercase().contains("current_timestamp"))
    );
    let generated = column("email_length").generated.as_ref().unwrap();
    assert!(!generated.stored);
    assert!(generated.expression.contains("email"));
    let kinds: Vec<(&str, ConstraintKind)> = customers
        .constraints
        .iter()
        .map(|c| (c.name.as_str(), c.kind))
        .collect();
    assert!(kinds.contains(&("PRIMARY", ConstraintKind::PrimaryKey)));
    assert!(kinds.contains(&("customers_email_key", ConstraintKind::Unique)));
    assert!(kinds.contains(&("grade_known", ConstraintKind::Check)));

    let orders = &schema.tables[1];
    let foreign = orders
        .constraints
        .iter()
        .find(|c| c.kind == ConstraintKind::ForeignKey)
        .unwrap();
    assert_eq!(
        foreign.definition,
        "FOREIGN KEY (`customer_id`) REFERENCES `dolmen_test`.`customers`(`id`) ON DELETE CASCADE"
    );
    let index = orders
        .indexes
        .iter()
        .find(|i| i.name == "orders_total_note")
        .unwrap();
    assert_eq!(
        index.definition,
        "CREATE INDEX `orders_total_note` ON `dolmen_test`.`orders` (`total`, `note`(20))"
    );
    assert_eq!(orders.triggers.len(), 1);
    assert!(orders.triggers[0].definition.starts_with(
        "CREATE TRIGGER `orders_note` BEFORE INSERT ON `dolmen_test`.`orders` FOR EACH ROW"
    ));
    assert_eq!(schema.views.len(), 1);
    assert_eq!(schema.views[0].columns.len(), 2);
    assert!(schema.views[0].definition.contains("100"));
    // MariaDB writes the display width: `n` int(11).
    let routines: Vec<(&str, FunctionKind, String)> = schema
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f.kind, f.arguments.replace("(11)", "")))
        .collect();
    assert_eq!(
        routines,
        [
            ("double_it", FunctionKind::Function, "`n` int".to_owned()),
            ("touch_orders", FunctionKind::Procedure, String::new())
        ]
    );
    assert!(schema.functions[0].definition.contains("CREATE"));

    let keys = session.foreign_keys().await.unwrap();
    let key = keys.iter().find(|k| k.schema == "dolmen_test").unwrap();
    assert_eq!(key.columns, ["customer_id"]);
    assert_eq!(key.referenced_table, "customers");
    assert_eq!(key.on_delete, "CASCADE");

    let create = session
        .create_statement("dolmen_test", "orders", false)
        .await
        .unwrap();
    assert!(create.starts_with("CREATE TABLE `orders`"), "{create}");

    run(&session, "DROP DATABASE dolmen_test").await;
}

#[tokio::test]
async fn refuses_writes_while_read_only() {
    let config = config_or_skip!();
    let _turn = exclusive().await;
    let session = Session::connect(&config).await.expect("connect");
    run(
        &session,
        "DROP DATABASE IF EXISTS dolmen_ro; CREATE DATABASE dolmen_ro;
         CREATE TABLE dolmen_ro.t (n int)",
    )
    .await;
    session.set_read_only(true).await.unwrap();
    run(&session, "SELECT * FROM dolmen_ro.t").await;
    let refused = session
        .execute("INSERT INTO dolmen_ro.t VALUES (1)", 10)
        .await;
    assert!(
        matches!(refused, Err(Error::Database { .. })),
        "{refused:?}"
    );
    session.set_read_only(false).await.unwrap();
    run(&session, "INSERT INTO dolmen_ro.t VALUES (1)").await;
    run(&session, "DROP DATABASE dolmen_ro").await;
}

#[tokio::test]
async fn cancels_a_running_statement() {
    let config = config_or_skip!();
    let session = Arc::new(Session::connect(&config).await.expect("connect"));
    let started = Instant::now();
    let running = {
        let session = Arc::clone(&session);
        tokio::spawn(async move { session.execute("SELECT SLEEP(20)", 10).await })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    session.cancel().await.unwrap();
    let _ = running.await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    // The session goes on.
    run(&session, "SELECT 1").await;
}

#[tokio::test]
async fn reports_a_wrong_password_cleanly() {
    let mut config = config_or_skip!();
    config.password = Some("wrong".into());
    match Session::connect(&config).await {
        Err(Error::Database { code, .. }) => assert!(code.starts_with("1045"), "{code}"),
        Err(other) => panic!("expected an access error, got {other:?}"),
        Ok(_) => panic!("connected with a wrong password"),
    }
}
