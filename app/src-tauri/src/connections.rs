//! Saved connection commands.
//!
//! These block on file and credential store access (which may show an OS prompt),
//! so they are async commands and stay off the main thread.

use dolmen_core::Error;
use dolmen_core::connections::{ConnectionStore, PasswordChange, SavedConnection};
use tauri::State;

#[tauri::command]
pub async fn connections_list(
    store: State<'_, ConnectionStore>,
) -> Result<Vec<SavedConnection>, Error> {
    store.list()
}

#[tauri::command]
pub async fn connections_save(
    connection: SavedConnection,
    password: PasswordChange,
    store: State<'_, ConnectionStore>,
) -> Result<SavedConnection, Error> {
    store.save(connection, password)
}

#[tauri::command]
pub async fn connections_delete(
    id: String,
    store: State<'_, ConnectionStore>,
) -> Result<(), Error> {
    store.delete(&id)
}
