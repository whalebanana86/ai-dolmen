//! Table data export and import commands.

use std::path::PathBuf;

use dolmen_core::Error;
use dolmen_core::history::Source as HistorySource;
use dolmen_core::postgres::ddl::quote_ident;
use dolmen_core::postgres::transfer::{
    self, CsvOptions, Delimiter, Encoding, Format, Preview, Source, Transferred,
};
use tauri::State;
use tauri::ipc::Channel;

use crate::history::{HistoryStore, Started};
use crate::pg::Sessions;

/// Rows shown to choose how to import a file.
const PREVIEW_ROWS: usize = 21;

/// Writes rows to a file at `path`, sending the rows written so far to
/// `progress`. It runs on a session of its own, read-only, with the search
/// path of `session_id`, so a query's names resolve as in the editor.
#[tauri::command]
pub async fn data_export(
    session_id: u64,
    job_id: u64,
    source: Source,
    format: Format,
    path: PathBuf,
    progress: Channel<u64>,
    sessions: State<'_, Sessions>,
) -> Result<Transferred, Error> {
    let search_path = sessions.get(session_id)?.search_path().await?;
    let worker = sessions.start_job(session_id, job_id).await?;
    let result = async {
        worker.set_search_path(&search_path).await?;
        worker
            .export(&source, &format, &path, |rows| {
                let _ = progress.send(rows);
            })
            .await
    }
    .await;
    sessions.end_job(job_id);
    result
}

/// The first rows of a CSV file; without a delimiter, the likeliest one is used.
#[tauri::command]
pub async fn data_import_preview(
    path: PathBuf,
    delimiter: Option<Delimiter>,
    encoding: Encoding,
) -> Result<Preview, Error> {
    tauri::async_runtime::spawn_blocking(move || {
        transfer::preview_csv(&path, delimiter, encoding, PREVIEW_ROWS)
    })
    .await
    .map_err(|err| Error::Storage {
        message: err.to_string(),
    })?
}

/// Appends a CSV file's rows to a table in one transaction, sending the bytes
/// read so far to `progress`. Refused while the connection is read-only.
// Tauri hands a command its arguments and app state as separate parameters.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn data_import(
    session_id: u64,
    job_id: u64,
    schema: String,
    table: String,
    path: PathBuf,
    options: CsvOptions,
    columns: Vec<Option<String>>,
    progress: Channel<u64>,
    sessions: State<'_, Sessions>,
    history: State<'_, HistoryStore>,
) -> Result<Transferred, Error> {
    sessions.check_writable(session_id)?;
    let worker = sessions.start_job(session_id, job_id).await?;
    let started = Started::now();
    let result = worker
        .import_csv(&schema, &table, &path, options, &columns, |bytes| {
            let _ = progress.send(bytes);
        })
        .await;
    sessions.end_job(job_id);
    if let Some(connection) = sessions.connection(session_id) {
        let targets: Vec<String> = columns.iter().flatten().map(|c| quote_ident(c)).collect();
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        // The file's name only: its folder says nothing about the database.
        let sql = format!(
            "COPY {}.{} ({}) FROM '{}'",
            quote_ident(&schema),
            quote_ident(&table),
            targets.join(", "),
            file.replace('\'', "''"),
        );
        history.record(
            &connection,
            &sql,
            started,
            &result,
            |done| i64::try_from(done.rows).ok(),
            HistorySource::Import,
        );
    }
    result
}

/// Stops an export or import.
#[tauri::command]
pub async fn data_transfer_cancel(job_id: u64, sessions: State<'_, Sessions>) -> Result<(), Error> {
    match sessions.job(job_id) {
        Some(worker) => worker.cancel().await,
        None => Ok(()),
    }
}
