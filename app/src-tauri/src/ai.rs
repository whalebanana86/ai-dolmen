//! AI commands: the API key, and asking Claude about SQL.

use std::collections::HashMap;
use std::sync::Mutex;

use dolmen_core::ai::{self, Answer, ApiKeys, Client, KeyStatus, Setting, Task};
use dolmen_core::{AiFailure, Error};
use serde::Deserialize;
use tauri::State;
use tauri::ipc::Channel;
use tokio::sync::oneshot;

use crate::pg::Sessions;

pub struct Ai {
    keys: ApiKeys,
    /// Answers being written, by the UI's request id, to stop them.
    running: Mutex<HashMap<u64, oneshot::Sender<()>>>,
}

impl Ai {
    pub fn new(keys: ApiKeys) -> Self {
        Self {
            keys,
            running: Mutex::default(),
        }
    }
}

#[tauri::command]
pub fn ai_key_status(ai: State<'_, Ai>) -> Result<KeyStatus, Error> {
    ai.keys.status()
}

/// Keeps the API key in the OS credential store; it never comes back to the UI.
#[tauri::command]
pub fn ai_key_set(key: String, ai: State<'_, Ai>) -> Result<(), Error> {
    ai.keys.set(&key)
}

#[tauri::command]
pub fn ai_key_forget(ai: State<'_, Ai>) -> Result<(), Error> {
    ai.keys.forget()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Question {
    /// The UI's id for it, to stop the answer with [`ai_cancel`].
    id: u64,
    session_id: u64,
    model: String,
    /// The language to answer in, e.g. `Korean`.
    language: String,
    task: Task,
}

/// Asks about the question's task with its session's database structure as
/// context, sending the answer's text to `on_text` as it arrives.
#[tauri::command]
pub async fn ai_ask(
    question: Question,
    on_text: Channel<String>,
    sessions: State<'_, Sessions>,
    ai: State<'_, Ai>,
) -> Result<Answer, Error> {
    let Question {
        id,
        session_id,
        model,
        language,
        task,
    } = question;
    if !ai::MODELS.contains(&model.as_str()) {
        return Err(Error::Ai {
            reason: AiFailure::Other,
            message: format!("unknown model {model}"),
        });
    }
    let key = ai.keys.get()?.ok_or_else(|| Error::Ai {
        reason: AiFailure::NoApiKey,
        message: "No Anthropic API key is saved; add one in the settings.".into(),
    })?;
    let session = sessions.get(session_id)?;
    let database = session.introspect().await?;
    let default_schema = session.current_schema().await?;
    let body = ai::request_body(
        &model,
        &Setting {
            server_version: session.server_version(),
            default_schema: default_schema.as_deref(),
            database: &database,
        },
        &task,
        &language,
    );

    let (stop, stopped) = oneshot::channel();
    ai.running.lock().unwrap().insert(id, stop);
    let answer = Client::new(key)
        .stream(
            &body,
            |text| {
                // The UI may be gone, e.g. after a reload; the answer is then just dropped.
                let _ = on_text.send(text.to_owned());
            },
            async {
                let _ = stopped.await;
            },
        )
        .await;
    ai.running.lock().unwrap().remove(&id);
    answer
}

/// Stops the answer to question `id`.
#[tauri::command]
pub fn ai_cancel(id: u64, ai: State<'_, Ai>) {
    if let Some(stop) = ai.running.lock().unwrap().remove(&id) {
        let _ = stop.send(());
    }
}
