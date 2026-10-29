//! Helpers shared by the integration tests.

// Each test crate uses only some of them.
#![allow(dead_code)]

use dolmen_core::postgres::{ConnectConfig, SslMode};

/// The test server from `DOLMEN_TEST_PG_HOST` (and `DOLMEN_TEST_PG_PORT`), if set.
pub fn test_config() -> Option<ConnectConfig> {
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

/// The MongoDB test server from `DOLMEN_TEST_MONGO_HOST` (and
/// `DOLMEN_TEST_MONGO_PORT`), if set. It signs in as `DOLMEN_TEST_MONGO_USER`
/// with `DOLMEN_TEST_MONGO_PASSWORD`, root and example unless set, and uses
/// TLS as `DOLMEN_TEST_MONGO_SSLMODE` says (`require` for DocumentDB Local).
pub fn mongo_config() -> Option<ConnectConfig> {
    let host = std::env::var("DOLMEN_TEST_MONGO_HOST").ok()?;
    let port = std::env::var("DOLMEN_TEST_MONGO_PORT")
        .map(|port| {
            port.parse()
                .expect("DOLMEN_TEST_MONGO_PORT must be a port number")
        })
        .unwrap_or(27017);
    let setting = |name: &str, default: &str| std::env::var(name).unwrap_or(default.into());
    let ssl_mode = match setting("DOLMEN_TEST_MONGO_SSLMODE", "disable").as_str() {
        "disable" => SslMode::Disable,
        "prefer" => SslMode::Prefer,
        "require" => SslMode::Require,
        "verify-full" => SslMode::VerifyFull,
        other => panic!("unknown DOLMEN_TEST_MONGO_SSLMODE {other:?}"),
    };
    Some(ConnectConfig {
        host,
        port,
        user: setting("DOLMEN_TEST_MONGO_USER", "root"),
        password: Some(setting("DOLMEN_TEST_MONGO_PASSWORD", "example")),
        database: "test".into(),
        ssl_mode,
    })
}

/// Returns the test server's config, or skips the test when none is set.
/// Without arguments it is the PostgreSQL server's; otherwise give the
/// config and the variable that sets it, as in
/// `config_or_skip!(mongo_config(), "DOLMEN_TEST_MONGO_HOST")`.
macro_rules! config_or_skip {
    () => {
        config_or_skip!(test_config(), "DOLMEN_TEST_PG_HOST")
    };
    ($config:expr, $variable:literal) => {
        match $config {
            Some(config) => config,
            None => {
                eprintln!(concat!("skipped: ", $variable, " is not set"));
                return;
            }
        }
    };
}

/// Held by tests that change shared schemas or read the whole database's
/// structure, so they don't run at the same time: reading definitions while
/// another test drops objects fails with "could not open relation".
pub async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}
