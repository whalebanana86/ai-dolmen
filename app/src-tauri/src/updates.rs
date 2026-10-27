//! Updating the app from the project's GitHub releases.
//!
//! Nothing is fetched until the user checks. Only a release newer than this
//! one and not marked as a pre-release is offered, and it is installed only if
//! its signature matches the public key in tauri.conf.json.

use std::sync::Mutex;
use std::time::Duration;

use dolmen_core::{Error, Refusal};
use serde::Serialize;
use tauri::ipc::Channel;
use tauri::{AppHandle, State};
use tauri_plugin_updater::{Update, UpdaterExt};

/// The update the last check found, for `update_install`.
#[derive(Default)]
pub struct Updates(Mutex<Option<Update>>);

/// A release newer than the running app.
#[derive(Serialize)]
pub struct Available {
    version: String,
    notes: Option<String>,
}

/// How much of the update has been downloaded.
#[derive(Clone, Serialize)]
pub struct Downloaded {
    bytes: u64,
    total: Option<u64>,
}

/// The newest release, or none when this version is the latest.
#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    updates: State<'_, Updates>,
) -> Result<Option<Available>, Error> {
    let update = app
        .updater_builder()
        // Never a version such as 0.2.0-beta.1, even when a release says it's the latest.
        .version_comparator(|current, release| {
            release.version.pre.is_empty() && release.version > current
        })
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(failed)?
        .check()
        .await
        .map_err(failed)?
        .map(|mut update| {
            // The check's timeout would cut a slow download short.
            update.timeout = Some(Duration::from_secs(600));
            update
        });
    let available = update.as_ref().map(|update| Available {
        version: update.version.clone(),
        notes: update.body.clone().filter(|notes| !notes.trim().is_empty()),
    });
    *updates.0.lock().unwrap() = update;
    Ok(available)
}

/// Downloads and installs the update the last check found, then restarts the
/// app. On Windows the installer closes the app and opens the new version.
#[tauri::command]
pub async fn update_install(
    app: AppHandle,
    updates: State<'_, Updates>,
    progress: Channel<Downloaded>,
) -> Result<(), Error> {
    // A development build isn't an installed app: the update would replace
    // the build folder.
    if cfg!(debug_assertions) {
        return Err(Error::Refused {
            reason: Refusal::DevelopmentBuild,
            message: "development builds don't install updates".into(),
        });
    }
    let update = updates
        .0
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| Error::Connection {
            message: "check for updates again".into(),
        })?;
    let mut bytes = 0;
    update
        .download_and_install(
            |chunk, total| {
                bytes += chunk as u64;
                let _ = progress.send(Downloaded { bytes, total });
            },
            || {},
        )
        .await
        .map_err(failed)?;
    app.restart()
}

fn failed(err: tauri_plugin_updater::Error) -> Error {
    Error::Connection {
        message: err.to_string(),
    }
}
