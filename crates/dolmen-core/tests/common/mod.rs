//! Helpers shared by the integration tests.

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

/// Returns the test server's config, or skips the test when none is set.
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

/// Held by tests that change shared schemas or read the whole database's
/// structure, so they don't run at the same time: reading definitions while
/// another test drops objects fails with "could not open relation".
pub async fn exclusive() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}
