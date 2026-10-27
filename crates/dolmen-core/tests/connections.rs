//! Saved connections, with an in-memory credential store instead of the OS one.

use std::sync::Arc;

use dolmen_core::Error;
use dolmen_core::connections::{ConnectionStore, PasswordChange, SavedConnection};
use dolmen_core::postgres::SslMode;
use keyring_core::CredentialStore;
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    credentials: Arc<CredentialStore>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
            credentials: keyring_core::mock::Store::new().unwrap(),
        }
    }

    fn path(&self) -> std::path::PathBuf {
        self.dir.path().join("nested").join("connections.json")
    }

    fn store(&self) -> ConnectionStore {
        ConnectionStore::new(self.path(), Some(self.credentials.clone()))
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.path()).unwrap()
    }
}

fn local() -> SavedConnection {
    SavedConnection {
        id: String::new(),
        name: "local".into(),
        host: "localhost".into(),
        port: 5432,
        user: "postgres".into(),
        database: "postgres".into(),
        ssl_mode: SslMode::Prefer,
        has_password: false,
        snapshot_dir: None,
        snapshot_remote: None,
        record_history: true,
        allow_snapshot_apply: None,
        environment: None,
        read_only: None,
    }
}

fn set(password: &str) -> PasswordChange {
    PasswordChange::Set {
        password: password.into(),
    }
}

#[test]
fn starts_empty_without_a_file() {
    let fixture = Fixture::new();
    assert!(fixture.store().list().unwrap().is_empty());
}

#[test]
fn saves_new_connections_with_an_id() {
    let fixture = Fixture::new();
    let saved = fixture.store().save(local(), PasswordChange::Keep).unwrap();
    assert!(!saved.id.is_empty());
    assert!(!saved.has_password);
    // A fresh store reads what the first one wrote.
    assert_eq!(fixture.store().list().unwrap(), [saved]);
}

#[test]
fn keeps_passwords_out_of_the_file() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let saved = store.save(local(), set("hunter2")).unwrap();

    assert!(saved.has_password);
    assert_eq!(
        store.password(&saved.id).unwrap().as_deref(),
        Some("hunter2")
    );
    assert!(!fixture.file().contains("hunter2"));
}

#[test]
fn updates_by_id_and_keeps_or_forgets_the_password() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let saved = store.save(local(), set("hunter2")).unwrap();

    let renamed = SavedConnection {
        name: "renamed".into(),
        ..saved.clone()
    };
    let kept = store.save(renamed, PasswordChange::Keep).unwrap();
    assert!(kept.has_password);
    assert_eq!(store.list().unwrap(), std::slice::from_ref(&kept));
    assert_eq!(
        store.password(&saved.id).unwrap().as_deref(),
        Some("hunter2")
    );

    let forgotten = store.save(kept, PasswordChange::Forget).unwrap();
    assert!(!forgotten.has_password);
    assert_eq!(store.password(&saved.id).unwrap(), None);
}

#[test]
fn deletes_the_connection_and_its_password() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let first = store.save(local(), set("one")).unwrap();
    let second = store.save(local(), PasswordChange::Keep).unwrap();

    store.delete(&first.id).unwrap();
    assert_eq!(store.list().unwrap(), [second]);
    assert_eq!(store.password(&first.id).unwrap(), None);
}

#[test]
fn reports_a_corrupt_file() {
    let fixture = Fixture::new();
    std::fs::create_dir_all(fixture.path().parent().unwrap()).unwrap();
    std::fs::write(fixture.path(), "not json").unwrap();
    assert!(matches!(fixture.store().list(), Err(Error::Storage { .. })));
}

#[test]
fn works_without_a_credential_store_until_a_password_is_set() {
    let fixture = Fixture::new();
    let store = ConnectionStore::new(fixture.path(), None);
    let saved = store.save(local(), PasswordChange::Forget).unwrap();
    store.delete(&saved.id).unwrap();
    assert!(matches!(
        store.save(local(), set("x")),
        Err(Error::Storage { .. })
    ));
}

// Touches the real OS credential store, so it only runs on request:
// cargo test -p dolmen-core --test connections -- --ignored
#[test]
#[ignore]
fn round_trips_through_the_native_credential_store() {
    let dir = TempDir::new().unwrap();
    let credentials = dolmen_core::connections::native_credential_store().unwrap();
    let store = ConnectionStore::new(dir.path().join("connections.json"), Some(credentials));

    let saved = store.save(local(), set("dolmen-test-password")).unwrap();
    let password = store.password(&saved.id);
    store.delete(&saved.id).unwrap();

    assert_eq!(password.unwrap().as_deref(), Some("dolmen-test-password"));
    assert_eq!(store.password(&saved.id).unwrap(), None);
}

#[test]
fn ids_the_store_did_not_hand_out_name_no_credentials() {
    let fixture = Fixture::new();
    let store = fixture.store();
    // Another entry in the same credential store, such as the AI's API key.
    let other = fixture
        .credentials
        .build("Dolmen", "anthropic-api-key", None)
        .unwrap();
    other.set_password("sk-secret").unwrap();

    assert_eq!(store.password("anthropic-api-key").unwrap(), None);
    let saved = store
        .save(
            SavedConnection {
                id: "anthropic-api-key".into(),
                ..local()
            },
            set("pw"),
        )
        .unwrap();
    assert_ne!(saved.id, "anthropic-api-key");
    store.delete("anthropic-api-key").unwrap();
    assert_eq!(other.get_password().unwrap(), "sk-secret");
}
