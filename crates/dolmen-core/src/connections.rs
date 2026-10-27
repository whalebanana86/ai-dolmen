//! Saved connections.
//!
//! Connection details are kept in a JSON file. Passwords are kept in a credential
//! store, normally the OS one, and never written to the file.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use keyring_core::{CredentialStore, Entry};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::postgres::SslMode;

/// Credential store service name; entries are keyed by connection id.
const SERVICE: &str = "Dolmen";
const FILE_VERSION: u32 = 1;

/// A connection the user saved, without its password.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConnection {
    /// Empty for a connection that has not been saved yet.
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub ssl_mode: SslMode,
    /// Whether a password is kept in the credential store.
    #[serde(default)]
    pub has_password: bool,
    /// Where this database's schema snapshot is written, usually inside a Git repository.
    #[serde(default)]
    pub snapshot_dir: Option<String>,
    /// A Git repository Dolmen clones and pushes for the snapshot; when set,
    /// it is used instead of `snapshot_dir`.
    #[serde(default)]
    pub snapshot_remote: Option<SnapshotRemote>,
    /// Whether statements run on this connection go into the query history.
    /// Turned off for databases whose SQL shouldn't be kept, such as production.
    #[serde(default = "record_by_default")]
    pub record_history: bool,
    /// Whether snapshot plans may change this database. Unset, only a server
    /// on this machine allows it, which a tunnel or proxy to production also
    /// looks like, so the user can say so explicitly either way.
    #[serde(default)]
    pub allow_snapshot_apply: Option<bool>,
}

fn record_by_default() -> bool {
    true
}

/// A snapshot kept in a repository Dolmen manages its own clone of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRemote {
    pub url: String,
    /// The snapshot folder inside the repository, e.g. `db/schema`.
    pub path: String,
}

/// What to do with the stored password when saving a connection.
// No Debug: it would print the password.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum PasswordChange {
    /// Leave the stored password, if any, as it is.
    Keep,
    Set {
        password: String,
    },
    Forget,
}

#[derive(Serialize, Deserialize)]
struct ConnectionsFile {
    version: u32,
    connections: Vec<SavedConnection>,
}

pub struct ConnectionStore {
    path: PathBuf,
    /// `None` when no credential store is available; passwords then can't be saved.
    credentials: Option<Arc<CredentialStore>>,
}

impl ConnectionStore {
    pub fn new(path: impl Into<PathBuf>, credentials: Option<Arc<CredentialStore>>) -> Self {
        Self {
            path: path.into(),
            credentials,
        }
    }

    pub fn list(&self) -> Result<Vec<SavedConnection>, Error> {
        Ok(self.read()?.connections)
    }

    /// Adds `connection`, or replaces the saved connection with the same id.
    /// Assigns an id to new connections and returns the connection as saved.
    pub fn save(
        &self,
        mut connection: SavedConnection,
        password: PasswordChange,
    ) -> Result<SavedConnection, Error> {
        let mut file = self.read()?;
        let existing = file.connections.iter().position(|c| c.id == connection.id);
        // An id the store didn't hand out is a new connection, so it can't
        // name another credential store entry.
        if existing.is_none() {
            connection.id = uuid::Uuid::new_v4().to_string();
        }

        connection.has_password = match password {
            PasswordChange::Keep => existing.is_some_and(|i| file.connections[i].has_password),
            PasswordChange::Set { password } => {
                self.entry(&connection.id)?.set_password(&password)?;
                true
            }
            PasswordChange::Forget => {
                self.forget_password(&connection.id)?;
                false
            }
        };

        match existing {
            Some(i) => file.connections[i] = connection.clone(),
            None => file.connections.push(connection.clone()),
        }
        self.write(&file)?;
        Ok(connection)
    }

    pub fn delete(&self, id: &str) -> Result<(), Error> {
        let mut file = self.read()?;
        if !file.connections.iter().any(|c| c.id == id) {
            return Ok(());
        }
        file.connections.retain(|c| c.id != id);
        self.write(&file)?;
        self.forget_password(id)
    }

    /// The stored password of a saved connection, or `None` if it has none.
    pub fn password(&self, id: &str) -> Result<Option<String>, Error> {
        if !self.read()?.connections.iter().any(|c| c.id == id) {
            return Ok(None);
        }
        match self.entry(id)?.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn forget_password(&self, id: &str) -> Result<(), Error> {
        if self.credentials.is_none() {
            return Ok(());
        }
        match self.entry(id)?.delete_credential() {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    fn entry(&self, id: &str) -> Result<Entry, Error> {
        let credentials = self.credentials.as_ref().ok_or_else(|| Error::Storage {
            message: "no credential store is available to keep passwords in".into(),
        })?;
        Ok(credentials.build(SERVICE, id, None)?)
    }

    fn read(&self) -> Result<ConnectionsFile, Error> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => {
                return Ok(ConnectionsFile {
                    version: FILE_VERSION,
                    connections: Vec::new(),
                });
            }
            Err(err) => return Err(self.file_error(err)),
        };
        serde_json::from_slice(&bytes).map_err(|err| self.file_error(err))
    }

    /// Writes through a temporary file, so a crash never leaves a half-written file.
    fn write(&self, file: &ConnectionsFile) -> Result<(), Error> {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir)?;
            }
            let temp = self.path.with_extension("json.tmp");
            fs::write(&temp, serde_json::to_vec_pretty(file)?)?;
            fs::rename(&temp, &self.path)
        };
        write().map_err(|err| self.file_error(err))
    }

    fn file_error(&self, err: impl std::fmt::Display) -> Error {
        Error::Storage {
            message: format!("{}: {err}", self.path.display()),
        }
    }
}

/// The OS credential store: Keychain on macOS, Credential Manager on Windows
/// and the Secret Service on Linux and other Unix systems.
pub fn native_credential_store() -> Result<Arc<CredentialStore>, Error> {
    #[cfg(target_os = "macos")]
    let store: Arc<CredentialStore> = apple_native_keyring_store::keychain::Store::new()?;
    #[cfg(target_os = "windows")]
    let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let store: Arc<CredentialStore> = zbus_secret_service_keyring_store::Store::new()?;
    Ok(store)
}
