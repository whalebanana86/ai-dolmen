//! Integration tests against a real Redis server.
//!
//! Skipped unless `DOLMEN_TEST_REDIS_HOST` is set (and `DOLMEN_TEST_REDIS_PORT`,
//! 6379 by default), e.g. with `docker run -d --rm -p 56379:6379 redis:8`.
//! Tests use keys under their own prefix and leave other keys alone.
//!
//! Optional servers for logging in:
//! - `DOLMEN_TEST_REDIS_AUTH_PORT`: one started with `--requirepass`, whose
//!   password is `DOLMEN_TEST_REDIS_AUTH_PASSWORD` (`dolmen` by default).
//! - `DOLMEN_TEST_REDIS_TLS_PORT`: one that only takes TLS, with a
//!   self-signed certificate.

use dolmen_core::postgres::{ConnectConfig, SslMode, StatementResult};
use dolmen_core::redis::{Key, Session, show_command};
use dolmen_core::{Error, Refusal};

/// The test server from `DOLMEN_TEST_REDIS_HOST` (and `DOLMEN_TEST_REDIS_PORT`), if set.
fn test_config() -> Option<ConnectConfig> {
    let host = std::env::var("DOLMEN_TEST_REDIS_HOST").ok()?;
    Some(ConnectConfig {
        host,
        port: port("DOLMEN_TEST_REDIS_PORT").unwrap_or(6379),
        user: String::new(),
        password: None,
        database: String::new(),
        ssl_mode: SslMode::Disable,
    })
}

fn port(variable: &str) -> Option<u16> {
    std::env::var(variable).ok().map(|port| {
        port.parse()
            .unwrap_or_else(|_| panic!("{variable} must be a port number"))
    })
}

/// Returns the test server's config, or skips the test when none is set.
macro_rules! config_or_skip {
    () => {
        match test_config() {
            Some(config) => config,
            None => {
                eprintln!("skipped: DOLMEN_TEST_REDIS_HOST is not set");
                return;
            }
        }
    };
}

async fn connect(config: &ConnectConfig) -> Session {
    Session::connect(config).await.expect("connect")
}

async fn run(session: &Session, text: &str) -> Vec<StatementResult> {
    session.execute(text, 1000, false).await.expect("execute")
}

fn rows(result: &StatementResult) -> (&[String], &[Vec<Option<String>>], u64) {
    match result {
        StatementResult::Rows {
            columns,
            rows,
            total_rows,
            source: None,
        } => (columns, rows, *total_rows),
        other => panic!("expected rows, got {other:?}"),
    }
}

/// The single value of a result, as for GET.
fn value(result: &StatementResult) -> Option<String> {
    let (columns, rows, _) = rows(result);
    assert_eq!(columns, ["value"]);
    assert_eq!(rows.len(), 1);
    rows[0][0].clone()
}

fn row(values: &[&str]) -> Vec<Option<String>> {
    values
        .iter()
        .map(|value| Some((*value).to_owned()))
        .collect()
}

/// A key prefix of the test's own, so tests running at once don't meet.
fn prefix(test: &str) -> String {
    format!("dolmen-test:{test}:{}", std::process::id())
}

/// Deletes the test's keys in the session's database.
async fn clean(session: &Session, prefix: &str) {
    let keys = session
        .keys(&format!("{prefix}*"), usize::MAX)
        .await
        .expect("keys");
    for key in keys.keys {
        run(
            session,
            &format!("DEL {}", dolmen_core::redis::quote(&key.name)),
        )
        .await;
    }
}

fn assert_refused(result: Result<Vec<StatementResult>, Error>, expected: Refusal) -> String {
    match result {
        Err(Error::Refused { reason, message }) if reason == expected => message,
        other => panic!("expected a {expected:?} refusal, got {other:?}"),
    }
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
    assert_eq!(session.database(), 0);

    // Valkey also reports the Redis version it's compatible with; its own wins.
    let info = value(&run(&session, "INFO server").await[0]).unwrap();
    let field = |name: &str| {
        info.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
    };
    assert_eq!(
        Some(session.server_version()),
        field("valkey_version:").or(field("redis_version:"))
    );

    // RESP3 where the server has it (Redis 6 and later).
    let major: u32 = session
        .server_version()
        .split('.')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    if major >= 7 {
        let client = value(&run(&session, "CLIENT INFO").await[0]).unwrap();
        assert!(client.contains(" resp=3 "), "{client}");
    }
}

#[test]
fn sessions_can_be_shared_between_tasks() {
    fn shared<T: Send + Sync>() {}
    fn sendable<T: Send>(_: &T) {}
    shared::<Session>();
    // Type-checked, never run: the app awaits these on other threads.
    let _ = |session: &'static Session| {
        sendable(&Session::connect(session.config()));
        sendable(&session.another());
        sendable(&session.execute("GET k", 10, true));
        sendable(&session.select(1));
        sendable(&session.keys("*", 10));
        sendable(&session.databases());
    };
}

#[tokio::test]
async fn reports_unreachable_servers() {
    let config = config_or_skip!();
    // Nothing listens on port 1.
    let result = Session::connect(&ConnectConfig { port: 1, ..config }).await;
    match result {
        Err(Error::Connection { message }) => {
            assert!(message.contains("couldn't connect"), "{message}")
        }
        other => panic!("expected a connection error, got {:?}", other.err()),
    }
}

#[tokio::test]
async fn survives_unsubscribing_without_subscriptions() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    for command in ["UNSUBSCRIBE", "PUNSUBSCRIBE news", "SUNSUBSCRIBE"] {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            session.execute(command, 10, false),
        )
        .await
        .unwrap_or_else(|_| panic!("{command} hung"));
        assert!(result.is_ok(), "{command}: {result:?}");
    }
    assert_eq!(
        value(&run(&session, "PING").await[0]).as_deref(),
        Some("PONG")
    );
}

#[tokio::test]
async fn runs_commands_line_by_line() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("lines");
    let results = run(
        &session,
        &format!(
            "# A comment\n\nSET {p}:a hello\n   \n  GET {p}:a\r\nGET {p}:missing\nINCR {p}:n\n# done"
        ),
    )
    .await;
    assert_eq!(results.len(), 4);
    assert_eq!(value(&results[0]).as_deref(), Some("OK"));
    assert_eq!(value(&results[1]).as_deref(), Some("hello"));
    assert_eq!(value(&results[2]), None);
    assert_eq!(value(&results[3]).as_deref(), Some("1"));
    assert!(run(&session, "\n# nothing\n").await.is_empty());
    clean(&session, &p).await;
}

#[tokio::test]
async fn shapes_each_kind_of_reply() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("shapes");
    run(
        &session,
        &format!(
            "RPUSH {p}:list a b c d e\n\
             SADD {p}:set x\n\
             HSET {p}:hash f1 v1\n\
             ZADD {p}:zset 1 a 2.5 b 3 c\n\
             XADD {p}:stream 1-1 name ann"
        ),
    )
    .await;

    // Arrays and sets: an index and a value.
    let results = run(&session, &format!("LRANGE {p}:list 0 -1\nSMEMBERS {p}:set")).await;
    let (columns, list, _) = rows(&results[0]);
    assert_eq!(columns, ["#", "value"]);
    assert_eq!(list[0], row(&["1", "a"]));
    assert_eq!(list[4], row(&["5", "e"]));
    let (columns, set, _) = rows(&results[1]);
    assert_eq!(columns, ["#", "value"]);
    assert_eq!(set, [row(&["1", "x"])]);

    // Only max_rows rows are kept; the rest are counted.
    let results = session
        .execute(&format!("LRANGE {p}:list 0 -1"), 2, false)
        .await
        .unwrap();
    let (_, list, total) = rows(&results[0]);
    assert_eq!(list.len(), 2);
    assert_eq!(total, 5);

    // Maps and pairs: two named columns.
    let results = run(
        &session,
        &format!(
            "HGETALL {p}:hash\n\
             CONFIG GET maxmemory\n\
             ZRANGE {p}:zset 0 1 WITHSCORES\n\
             ZRANGEBYSCORE {p}:zset 2 +inf WITHSCORES\n\
             HRANDFIELD {p}:hash 1 WITHVALUES\n\
             ZPOPMIN {p}:zset"
        ),
    )
    .await;
    let (columns, hash, _) = rows(&results[0]);
    assert_eq!(columns, ["field", "value"]);
    assert_eq!(hash, [row(&["f1", "v1"])]);
    let (columns, config_rows, _) = rows(&results[1]);
    assert_eq!(columns, ["field", "value"]);
    assert_eq!(config_rows[0][0].as_deref(), Some("maxmemory"));
    let (columns, zset, _) = rows(&results[2]);
    assert_eq!(columns, ["member", "score"]);
    assert_eq!(zset, [row(&["a", "1"]), row(&["b", "2.5"])]);
    let (columns, zset, _) = rows(&results[3]);
    assert_eq!(columns, ["member", "score"]);
    assert_eq!(zset, [row(&["b", "2.5"]), row(&["c", "3"])]);
    let (columns, random, _) = rows(&results[4]);
    assert_eq!(columns, ["field", "value"]);
    assert_eq!(random, [row(&["f1", "v1"])]);
    let (columns, popped, _) = rows(&results[5]);
    assert_eq!(columns, ["member", "score"]);
    assert_eq!(popped, [row(&["a", "1"])]);

    // Nested replies: a row per item.
    let results = run(
        &session,
        &format!("XRANGE {p}:stream - +\nSCAN 0 MATCH {p}:list COUNT 1000\nZSCORE {p}:zset b"),
    )
    .await;
    let (columns, stream, _) = rows(&results[0]);
    assert_eq!(columns, ["id", "fields"]);
    assert_eq!(stream, [row(&["1-1", r#"{"name":"ann"}"#])]);
    let (columns, scan, total) = rows(&results[1]);
    assert_eq!(columns, ["#", "value"]);
    assert_eq!(total, 2);
    assert_eq!(scan[0][0].as_deref(), Some("1"));
    // The cursor, then the keys found, until the scan ends.
    assert!(scan[1][1].as_deref().unwrap().starts_with('['));
    // A double in RESP3, a string in RESP2: the same text.
    assert_eq!(value(&results[2]).as_deref(), Some("2.5"));
    clean(&session, &p).await;
}

#[tokio::test]
async fn takes_quoted_arguments_like_redis_cli() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("quotes");
    let results = run(
        &session,
        &format!(
            "SET \"{p}:a key\" \"line\\nbreak \\\"quoted\\\" \\x41\"\n\
             GET \"{p}:a key\"\n\
             SET {p}:single 'it\\'s \\n as is'\n\
             GET {p}:single\n\
             SET {p}:empty \"\"\n\
             STRLEN {p}:empty"
        ),
    )
    .await;
    assert_eq!(
        value(&results[1]).as_deref(),
        Some("line\nbreak \"quoted\" A")
    );
    assert_eq!(value(&results[3]).as_deref(), Some(r"it's \n as is"));
    assert_eq!(value(&results[5]).as_deref(), Some("0"));

    // A quote left open is an error at the quote, and nothing runs.
    let text = format!("SET {p}:ran 1\nGET \"{p}:open");
    match session.execute(&text, 1000, false).await {
        Err(Error::Database { position, .. }) => {
            assert_eq!(position, Some(text.find('"').unwrap() as u32 + 1));
        }
        other => panic!("expected a quoting error, got {other:?}"),
    }
    assert_eq!(
        value(&run(&session, &format!("EXISTS {p}:ran")).await[0]).as_deref(),
        Some("0")
    );
    clean(&session, &p).await;
}

#[tokio::test]
async fn shows_bytes_that_are_not_utf8_as_hex() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("binary");
    let results = run(
        &session,
        &format!("SET {p}:bytes \"\\xff\\x00\\x01\"\nGET {p}:bytes\nSET \"{p}:\\xfe\\xff\" named"),
    )
    .await;
    assert_eq!(value(&results[1]).as_deref(), Some(r"\xff0001"));

    // A key name that isn't UTF-8 lists as hex, and quotes back to its bytes.
    let keys = session.keys(&format!("{p}:*"), 100).await.unwrap();
    let binary = keys
        .keys
        .iter()
        .find(|key| key.name.starts_with(r"\x"))
        .expect("the binary key");
    assert_eq!(binary.kind, "string");
    let shown = run(&session, &show_command(binary)).await;
    assert_eq!(value(&shown[0]).as_deref(), Some("named"));
    run(
        &session,
        &format!("DEL {}", dolmen_core::redis::quote(&binary.name)),
    )
    .await;
    clean(&session, &p).await;
}

#[tokio::test]
async fn reports_errors_with_their_code_and_line() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("errors");
    let text = format!("SET {p}:s x\n  LPUSH {p}:s y\nSET {p}:after 1");
    match session.execute(&text, 1000, false).await {
        Err(Error::Database { code, position, .. }) => {
            assert_eq!(code, "WRONGTYPE");
            assert_eq!(position, Some(text.find("LPUSH").unwrap() as u32 + 1));
        }
        other => panic!("expected WRONGTYPE, got {other:?}"),
    }
    // It stopped at the error.
    assert_eq!(
        value(&run(&session, &format!("EXISTS {p}:after")).await[0]).as_deref(),
        Some("0")
    );

    match session.execute("NOSUCHCOMMAND x", 1000, false).await {
        Err(Error::Database { code, message, .. }) => {
            assert_eq!(code, "ERR");
            assert!(message.contains("unknown command"), "{message}");
        }
        other => panic!("expected ERR, got {other:?}"),
    }
    // The session still works afterwards.
    assert_eq!(
        value(&run(&session, "PING").await[0]).as_deref(),
        Some("PONG")
    );
    clean(&session, &p).await;
}

#[tokio::test]
async fn refuses_writes_while_read_only() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("read-only");
    run(&session, &format!("SET {p}:a 1")).await;
    let read_only = |text: String| {
        let session = &session;
        async move { session.execute(&text, 1000, true).await }
    };

    // Nothing runs when any command would write.
    let message = assert_refused(
        read_only(format!("GET {p}:a\nSET {p}:a 2")).await,
        Refusal::ReadOnly,
    );
    assert!(message.contains("SET"), "{message}");
    assert_eq!(
        value(&run(&session, &format!("GET {p}:a")).await[0]).as_deref(),
        Some("1")
    );

    // Commands no list names, learned from COMMAND INFO.
    for command in [
        format!("SETRANGE {p}:a 0 x"),
        format!("HINCRBYFLOAT {p}:h f 1"),
        format!("XGROUP CREATE {p}:s g $ MKSTREAM"),
    ] {
        assert_refused(read_only(command).await, Refusal::ReadOnly);
    }
    // Admin commands that change the server, and scripts that could write.
    for command in [
        "FLUSHALL",
        "flushdb",
        "CONFIG SET maxmemory 1",
        "SCRIPT FLUSH",
        "CLIENT KILL ID 1",
        "SHUTDOWN",
        "EVAL \"return 1\" 0",
    ] {
        assert_refused(read_only(command.into()).await, Refusal::ReadOnly);
    }

    // Reading goes through, including subcommands that only read.
    let results = read_only(format!(
        "GET {p}:a\nTTL {p}:a\nCONFIG GET maxmemory\nOBJECT ENCODING {p}:a\nCLIENT LIST\nEVAL_RO \"return 1\" 0\nINFO server"
    ))
    .await
    .expect("reads");
    assert_eq!(value(&results[0]).as_deref(), Some("1"));
    assert_eq!(results.len(), 7);
    clean(&session, &p).await;
}

#[tokio::test]
async fn refuses_commands_that_would_break_the_shared_connection() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let message = assert_refused(
        session.execute("SELECT 1", 1000, false).await,
        Refusal::Unsupported,
    );
    assert!(message.contains("database list"), "{message}");
    for command in [
        "MULTI",
        "EXEC",
        "WATCH k",
        "SUBSCRIBE news",
        "PSUBSCRIBE n*",
        "SSUBSCRIBE news",
        "MONITOR",
        "QUIT",
        "RESET",
        "HELLO 3",
        "CLIENT REPLY OFF",
    ] {
        assert_refused(
            session.execute(command, 1000, false).await,
            Refusal::Unsupported,
        );
    }
    // Nothing changed: still in database 0, and answering.
    assert_eq!(session.database(), 0);
    assert_eq!(
        value(&run(&session, "PING").await[0]).as_deref(),
        Some("PONG")
    );
}

#[tokio::test]
async fn selects_databases() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("select");
    session.select(3).await.unwrap();
    assert_eq!(session.database(), 3);
    run(&session, &format!("SET {p}:here 3")).await;

    // Another session starts in the database this one is in.
    let other = session.another().await.unwrap();
    assert_eq!(other.database(), 3);
    assert_eq!(
        value(&run(&other, &format!("GET {p}:here")).await[0]).as_deref(),
        Some("3")
    );

    session.select(0).await.unwrap();
    assert_eq!(
        value(&run(&session, &format!("GET {p}:here")).await[0]),
        None
    );
    assert!(matches!(
        session.select(100_000).await,
        Err(Error::Database { .. })
    ));
    assert_eq!(session.database(), 0);

    // A database in the settings is selected when connecting.
    let in_three = connect(&ConnectConfig {
        database: "3".into(),
        ..config.clone()
    })
    .await;
    assert_eq!(in_three.database(), 3);
    assert_eq!(
        value(&run(&in_three, &format!("GET {p}:here")).await[0]).as_deref(),
        Some("3")
    );
    clean(&in_three, &p).await;
}

#[tokio::test]
async fn rejects_databases_that_dont_exist() {
    let config = config_or_skip!();
    for database in ["shop", "-1"] {
        let result = Session::connect(&ConnectConfig {
            database: database.into(),
            ..config.clone()
        })
        .await;
        assert!(
            matches!(result, Err(Error::Connection { .. })),
            "{database}: {:?}",
            result.err()
        );
    }
    let result = Session::connect(&ConnectConfig {
        database: "100000".into(),
        ..config
    })
    .await;
    assert!(
        matches!(result, Err(Error::Database { ref code, .. }) if code == "ERR"),
        "{:?}",
        result.err()
    );
}

#[tokio::test]
async fn lists_keys_by_pattern() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("keys");
    let mut text = String::new();
    for i in 0..30 {
        text.push_str(&format!("SET {p}:string:{i:02} v\n"));
    }
    text.push_str(&format!("RPUSH {p}:list a\nHSET {p}:hash f v\nSADD {p}:set m\nZADD {p}:zset 1 m\nXADD {p}:stream * f v"));
    run(&session, &text).await;

    let all = session.keys(&format!("{p}:*"), 1000).await.unwrap();
    assert!(all.complete);
    assert_eq!(all.keys.len(), 35);
    // Sorted by name.
    assert!(all.keys.windows(2).all(|pair| pair[0].name < pair[1].name));
    let kind = |name: &str| {
        all.keys
            .iter()
            .find(|key| key.name == format!("{p}:{name}"))
            .map(|key| key.kind.as_str())
    };
    assert_eq!(kind("string:00"), Some("string"));
    assert_eq!(kind("list"), Some("list"));
    assert_eq!(kind("hash"), Some("hash"));
    assert_eq!(kind("set"), Some("set"));
    assert_eq!(kind("zset"), Some("zset"));
    assert_eq!(kind("stream"), Some("stream"));

    let strings = session.keys(&format!("{p}:string:*"), 1000).await.unwrap();
    assert_eq!(strings.keys.len(), 30);

    // The limit stops the scan.
    let some = session.keys(&format!("{p}:*"), 10).await.unwrap();
    assert_eq!(some.keys.len(), 10);
    assert!(!some.complete);

    // An empty pattern matches every key.
    let any = session.keys("", 5).await.unwrap();
    assert_eq!(any.keys.len(), 5);
    clean(&session, &p).await;
}

#[tokio::test]
async fn lists_databases_with_keys() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("databases");
    session.select(5).await.unwrap();
    run(&session, &format!("SET {p}:a 1\nSET {p}:b 2")).await;

    let databases = session.databases().await.unwrap();
    let configured = run(&session, "CONFIG GET databases").await;
    let (_, configured, _) = rows(&configured[0]);
    assert_eq!(
        databases.count.to_string(),
        configured[0][1].clone().unwrap()
    );
    let (_, keys) = databases
        .used
        .iter()
        .find(|(index, _)| *index == 5)
        .expect("database 5 has keys");
    assert!(*keys >= 2);
    assert!(databases.used.windows(2).all(|pair| pair[0].0 < pair[1].0));
    clean(&session, &p).await;
}

#[tokio::test]
async fn shows_each_kind_of_key() {
    let config = config_or_skip!();
    let session = connect(&config).await;
    let p = prefix("show");
    run(
        &session,
        &format!(
            "SET \"{p}:a string\" hello\n\
             HSET {p}:hash f1 v1 f2 v2\n\
             RPUSH {p}:list a b c\n\
             SADD {p}:set x y\n\
             ZADD {p}:zset 1 a 2.5 b\n\
             XADD {p}:stream 1-1 name ann"
        ),
    )
    .await;
    let json = session
        .execute(&format!("JSON.SET {p}:json $ '{{\"a\":1}}'"), 10, false)
        .await
        .is_ok();

    let keys = session.keys(&format!("{p}:*"), 100).await.unwrap();
    assert_eq!(keys.keys.len(), if json { 7 } else { 6 });
    for key in &keys.keys {
        let results = run(&session, &show_command(key)).await;
        let (columns, mut rows, _) = {
            let (columns, rows, total) = rows(&results[0]);
            (columns.to_vec(), rows.to_vec(), total)
        };
        rows.sort();
        match key.kind.as_str() {
            "string" => assert_eq!(rows, [row(&["hello"])]),
            "hash" => {
                assert_eq!(columns, ["field", "value"]);
                assert_eq!(rows, [row(&["f1", "v1"]), row(&["f2", "v2"])]);
            }
            "list" => {
                assert_eq!(columns, ["#", "value"]);
                assert_eq!(rows, [row(&["1", "a"]), row(&["2", "b"]), row(&["3", "c"])]);
            }
            "set" => {
                assert_eq!(columns, ["#", "value"]);
                let mut members: Vec<_> = rows.iter().map(|row| row[1].clone()).collect();
                members.sort();
                assert_eq!(members, [Some("x".into()), Some("y".into())]);
            }
            "zset" => {
                assert_eq!(columns, ["member", "score"]);
                assert_eq!(rows, [row(&["a", "1"]), row(&["b", "2.5"])]);
            }
            "stream" => {
                assert_eq!(columns, ["id", "fields"]);
                assert_eq!(rows, [row(&["1-1", r#"{"name":"ann"}"#])]);
            }
            "ReJSON-RL" => {
                assert_eq!(columns, ["value"]);
                assert_eq!(rows, [row(&[r#"{"a":1}"#])]);
            }
            other => panic!("unexpected kind {other}"),
        }
    }
    // A kind without its own command shows its type.
    let unknown = Key {
        name: format!("{p}:a string"),
        kind: "module-type".into(),
    };
    assert_eq!(
        value(&run(&session, &show_command(&unknown)).await[0]).as_deref(),
        Some("string")
    );
    clean(&session, &p).await;
}

#[tokio::test]
async fn logs_in_as_an_acl_user() {
    let config = config_or_skip!();
    let admin = connect(&config).await;
    let user = format!("dolmen-test-{}", std::process::id());
    // Everything but CONFIG, as managed services often do.
    run(
        &admin,
        &format!("ACL SETUSER {user} on >secret ~* &* +@all -config"),
    )
    .await;

    let login = ConnectConfig {
        user: user.clone(),
        password: Some("secret".into()),
        ..config.clone()
    };
    let session = connect(&login).await;
    assert_eq!(
        value(&run(&session, "ACL WHOAMI").await[0]).as_deref(),
        Some(user.as_str())
    );
    // CONFIG is refused, so the count falls back to Redis's default.
    assert!(matches!(
        session.execute("CONFIG GET databases", 10, false).await,
        Err(Error::Database { ref code, .. }) if code == "NOPERM"
    ));
    assert_eq!(session.databases().await.unwrap().count, 16);

    let wrong = Session::connect(&ConnectConfig {
        password: Some("wrong".into()),
        ..login.clone()
    })
    .await;
    assert!(
        matches!(wrong, Err(Error::Database { ref code, .. }) if code == "WRONGPASS"),
        "{:?}",
        wrong.err()
    );

    // Without COMMAND there's no telling what writes, so read-only refuses.
    run(&admin, &format!("ACL SETUSER {user} -command")).await;
    let session = connect(&login).await;
    assert_refused(session.execute("GET k", 10, true).await, Refusal::ReadOnly);
    run(&admin, &format!("ACL DELUSER {user}")).await;
}

#[tokio::test]
async fn logs_in_with_a_password_alone() {
    let (Some(config), Some(port)) = (test_config(), port("DOLMEN_TEST_REDIS_AUTH_PORT")) else {
        eprintln!("skipped: DOLMEN_TEST_REDIS_HOST or DOLMEN_TEST_REDIS_AUTH_PORT is not set");
        return;
    };
    let password = std::env::var("DOLMEN_TEST_REDIS_AUTH_PASSWORD").unwrap_or("dolmen".into());
    let config = ConnectConfig {
        port,
        password: Some(password),
        ..config
    };
    let session = connect(&config).await;
    assert_eq!(
        value(&run(&session, "PING").await[0]).as_deref(),
        Some("PONG")
    );
    session.another().await.expect("another session");

    let wrong = Session::connect(&ConnectConfig {
        password: Some("wrong".into()),
        ..config.clone()
    })
    .await;
    assert!(
        matches!(wrong, Err(Error::Database { ref code, .. }) if code == "WRONGPASS"),
        "{:?}",
        wrong.err()
    );
    let none = Session::connect(&ConnectConfig {
        password: None,
        ..config
    })
    .await;
    assert!(
        matches!(none, Err(Error::Database { ref code, .. }) if code == "NOAUTH"),
        "{:?}",
        none.err()
    );
}

#[tokio::test]
async fn connects_over_tls() {
    let (Some(config), Some(port)) = (test_config(), port("DOLMEN_TEST_REDIS_TLS_PORT")) else {
        eprintln!("skipped: DOLMEN_TEST_REDIS_HOST or DOLMEN_TEST_REDIS_TLS_PORT is not set");
        return;
    };
    let config = ConnectConfig { port, ..config };

    let session = connect(&ConnectConfig {
        ssl_mode: SslMode::Require,
        ..config.clone()
    })
    .await;
    assert_eq!(
        value(&run(&session, "PING").await[0]).as_deref(),
        Some("PONG")
    );

    // The certificate is self-signed, so verifying it fails.
    let verified = Session::connect(&ConnectConfig {
        ssl_mode: SslMode::VerifyFull,
        ..config.clone()
    })
    .await;
    assert!(
        matches!(verified, Err(Error::Connection { .. })),
        "{:?}",
        verified.err()
    );

    // Plain text to a server that only takes TLS.
    match Session::connect(&ConnectConfig {
        ssl_mode: SslMode::Prefer,
        ..config
    })
    .await
    {
        Err(Error::Connection { message }) => assert!(message.contains("TLS"), "{message}"),
        other => panic!("expected a connection error, got {:?}", other.err()),
    }
}
