//! Schema snapshot commands: compare the database with its snapshot folder,
//! write the snapshot, and commit it with Git.

use std::path::{Path, PathBuf};

use dolmen_core::Error;
use dolmen_core::git::{self, Commit, Repository};
use dolmen_core::postgres::ddl::{self, SchemaObject};
use dolmen_core::postgres::migrate::{self, Plan, Rename};
use dolmen_core::schema::Database;
use dolmen_core::snapshot::{ChangeKind, FileChange, MARKER, MODEL_FILE, Snapshot};
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
    pushed: bool,
}

#[tauri::command]
pub async fn snapshot_status(
    session_id: u64,
    dir: String,
    sessions: State<'_, Sessions>,
) -> Result<SnapshotStatus, Error> {
    let dir = PathBuf::from(dir);
    let current = Snapshot::of(&sessions.get(session_id)?.introspect().await?);
    let saved = Snapshot::read(&dir)?;
    let mut changes = saved.changes(&current);
    // The DDL can match while the model file is missing (snapshots from before
    // models) or stale; list it so writing it is possible, without a diff.
    if saved.model() != current.model() {
        changes.push(FileChange {
            path: MODEL_FILE.into(),
            kind: if saved.model().is_some() {
                ChangeKind::Modified
            } else {
                ChangeKind::Added
            },
            diff: String::new(),
        });
    }
    Ok(SnapshotStatus {
        repository: git::repository(&dir)?,
        changes,
        initialized: dir.join(MARKER).exists(),
    })
}

/// Writes the database's current snapshot into `dir`, commits the folder when
/// `commit_message` is given, and pushes the commit when `push` is set.
#[tauri::command]
pub async fn snapshot_save(
    session_id: u64,
    dir: String,
    commit_message: Option<String>,
    push: bool,
    sessions: State<'_, Sessions>,
) -> Result<SavedSnapshot, Error> {
    let dir = PathBuf::from(dir);
    Snapshot::of(&sessions.get(session_id)?.introspect().await?).write(&dir)?;
    let commit = match commit_message {
        Some(message) => git::commit(&dir, &message)?,
        None => None,
    };
    let pushed = commit.is_some() && push;
    if pushed {
        git::push(&dir)?;
    }
    Ok(SavedSnapshot { commit, pushed })
}

#[tauri::command]
pub async fn snapshot_push(dir: String) -> Result<(), Error> {
    git::push(&PathBuf::from(dir))
}

/// Recent commits of the repository holding `dir`, for the history graph.
#[tauri::command]
pub async fn snapshot_history(dir: String) -> Result<Vec<Commit>, Error> {
    let dir = PathBuf::from(dir);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    git::history(&dir, 300)
}

/// What would change in the database to make it match the snapshot at `rev`.
#[tauri::command]
pub async fn snapshot_compare(
    session_id: u64,
    dir: String,
    rev: String,
    sessions: State<'_, Sessions>,
) -> Result<Vec<FileChange>, Error> {
    let dir = PathBuf::from(dir);
    let current = Snapshot::of(&sessions.get(session_id)?.introspect().await?);
    Ok(current.changes(&git::snapshot_at(&dir, &rev)?))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPreview {
    plan: Plan,
    /// Only local databases take snapshot plans.
    local: bool,
}

/// The plan that makes the database match the snapshot at `rev`, or the files
/// in `dir` when `rev` is `None` (say, after a `git checkout`).
#[tauri::command]
pub async fn snapshot_plan(
    session_id: u64,
    dir: String,
    rev: Option<String>,
    renames: Vec<Rename>,
    sessions: State<'_, Sessions>,
) -> Result<ApplyPreview, Error> {
    let session = sessions.get(session_id)?;
    let target = target_model(Path::new(&dir), rev.as_deref())?;
    Ok(ApplyPreview {
        plan: migrate::plan(&session.introspect().await?, &target, &renames),
        local: session.is_local(),
    })
}

/// Applies the plan the user reviewed, given as its steps' SQL. The plan is
/// made again here, and if it differs from what they reviewed, because the
/// database or the snapshot changed since, nothing is applied.
#[tauri::command]
pub async fn snapshot_apply(
    session_id: u64,
    dir: String,
    rev: Option<String>,
    renames: Vec<Rename>,
    reviewed: Vec<String>,
    allow_destructive: bool,
    sessions: State<'_, Sessions>,
) -> Result<(), Error> {
    let session = sessions.get(session_id)?;
    if !session.is_local() {
        return Err(refused(
            "Only local databases (localhost, a loopback address or a Unix socket) can be changed from a snapshot.",
        ));
    }
    let target = target_model(Path::new(&dir), rev.as_deref())?;
    let plan = migrate::plan(&session.introspect().await?, &target, &renames);
    if !plan.steps.iter().map(|step| &step.sql).eq(reviewed.iter()) {
        return Err(refused(
            "The database or the snapshot changed since you reviewed the plan; review it again.",
        ));
    }
    if !plan.blockers.is_empty() {
        return Err(refused(
            "The plan has changes Dolmen can't make; see the plan.",
        ));
    }
    if plan.is_destructive() && !allow_destructive {
        return Err(refused(
            "The plan drops data or changes column types; allow that explicitly to apply it.",
        ));
    }
    session.apply(&plan).await
}

fn target_model(dir: &Path, rev: Option<&str>) -> Result<Database, Error> {
    let snapshot = match rev {
        Some(rev) => git::snapshot_at(dir, rev)?,
        None => Snapshot::read(dir)?,
    };
    snapshot.model().cloned().ok_or_else(|| Error::Storage {
        message: "This snapshot has no dolmen-model.json, so it can't be applied: it was written \
                  before Dolmen saved models. Write and commit a new snapshot to apply it later."
            .into(),
    })
}

fn refused(message: &str) -> Error {
    Error::Refused {
        message: message.into(),
    }
}

/// The `CREATE` statement for one object, as it appears in snapshots.
#[tauri::command]
pub fn ddl_create_statement(schema: String, object: SchemaObject) -> String {
    ddl::create_statement(&schema, &object)
}
