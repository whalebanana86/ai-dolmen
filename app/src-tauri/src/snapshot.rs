//! Schema snapshot commands: compare the database with its snapshot folder,
//! write the snapshot, and commit it with Git.

use std::path::PathBuf;

use dolmen_core::Error;
use dolmen_core::git::{self, Repository};
use dolmen_core::postgres::ddl::{self, SchemaObject};
use dolmen_core::snapshot::{FileChange, MARKER, Snapshot};
use serde::Serialize;
use tauri::State;

use crate::pg::Sessions;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotStatus {
    /// The Git repository holding the folder, if any.
    repository: Option<Repository>,
    /// What writing the snapshot now would add, change or remove.
    changes: Vec<FileChange>,
    /// Whether the folder already holds a snapshot.
    initialized: bool,
}

#[derive(Serialize)]
pub struct SavedSnapshot {
    /// The new commit's hash, when a commit was asked for and something changed.
    commit: Option<String>,
}

#[tauri::command]
pub async fn snapshot_status(
    session_id: u64,
    dir: String,
    sessions: State<'_, Sessions>,
) -> Result<SnapshotStatus, Error> {
    let dir = PathBuf::from(dir);
    let current = Snapshot::of(&sessions.get(session_id)?.introspect().await?);
    Ok(SnapshotStatus {
        repository: git::repository(&dir)?,
        changes: Snapshot::read(&dir)?.changes(&current),
        initialized: dir.join(MARKER).exists(),
    })
}

/// Writes the database's current snapshot into `dir`, then commits the folder
/// when `commit_message` is given.
#[tauri::command]
pub async fn snapshot_save(
    session_id: u64,
    dir: String,
    commit_message: Option<String>,
    sessions: State<'_, Sessions>,
) -> Result<SavedSnapshot, Error> {
    let dir = PathBuf::from(dir);
    Snapshot::of(&sessions.get(session_id)?.introspect().await?).write(&dir)?;
    let commit = match commit_message {
        Some(message) => git::commit(&dir, &message)?,
        None => None,
    };
    Ok(SavedSnapshot { commit })
}

/// The `CREATE` statement for one object, as it appears in snapshots.
#[tauri::command]
pub fn ddl_create_statement(schema: String, object: SchemaObject) -> String {
    ddl::create_statement(&schema, &object)
}
