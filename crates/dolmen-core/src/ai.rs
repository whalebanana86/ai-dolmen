//! Help with SQL from Claude, through the Anthropic API with the user's own
//! API key: writing SQL for a request, explaining a query, and fixing one that
//! failed.
//!
//! A request carries the database's structure (the snapshot's DDL with its
//! comments, never rows), the SQL in question and any error message. Answers
//! stream back as text; the SQL in them goes into the editor, and Dolmen never
//! runs it on its own.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::future::{Either, select};
use keyring_core::CredentialStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::schema::Database;
use crate::snapshot::Snapshot;
use crate::{AiFailure, Error};

/// The models to choose from, the default first.
pub const MODELS: &[&str] = &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];
pub const DEFAULT_MODEL: &str = MODELS[0];

const API_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2026-10-21";
/// Lets the API retry a declined request on a model it picks, `fallbacks: "default"`.
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Answers are short; this leaves room for thinking.
const MAX_TOKENS: u32 = 16_000;
/// Retries of a request that failed before any of the answer arrived.
const MAX_RETRIES: u32 = 2;

const INSTRUCTIONS: &str = "\
You help people work with their PostgreSQL database inside Dolmen, a desktop database client.

The database's structure follows, as the CREATE statements of Dolmen's schema snapshot. \
Comments in it say what tables and columns mean. Treat it as the truth about what exists: \
use only the tables, columns, types and functions in it, and when a request needs something \
it doesn't have, say so instead of inventing names.

When you write SQL:
- Put it in a single ```sql code block. It goes into the user's editor; Dolmen never runs it \
on its own, and the user reviews it first.
- Write PostgreSQL for the server version given, and qualify names outside the default schema.
- Write what was asked and nothing more. When a request needs data or structure changed, \
write that, and say in one sentence what it changes. An UPDATE or DELETE always has a WHERE \
clause, unless the request is explicitly about every row.
- Put each clause on its own line when the statement doesn't fit on one.

Keep the prose short: lead with the answer, then only what the reader needs to act on it. \
Write the prose in the language the request names.";

/// What to ask about.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Task {
    /// Write SQL for a request in plain language.
    Generate {
        request: String,
        /// The statement at the cursor or the selected text, which the request may be about.
        context: Option<String>,
    },
    /// Explain what a statement does.
    Explain { sql: String },
    /// Fix a statement that failed with `error`.
    Fix { sql: String, error: String },
}

impl Task {
    /// The user message: the volatile part, after the cached database context.
    fn prompt(&self, language: &str) -> String {
        match self {
            Task::Generate { request, context } => {
                let mut prompt = format!("<request>\n{}\n</request>\n", request.trim());
                if let Some(context) = context.as_deref().filter(|c| !c.trim().is_empty()) {
                    prompt.push_str(&format!("<editor>\n{}\n</editor>\n", context.trim()));
                }
                prompt.push_str(&format!(
                    "Write the SQL for this request. The editor's statement is context only: \
                     change it if the request is about it, and otherwise ignore it. After the SQL, \
                     add at most two short sentences, and only if something needs saying, such as \
                     an assumption you made or what the statement changes. Write in {language}."
                ));
                prompt
            }
            Task::Explain { sql } => format!(
                "<sql>\n{}\n</sql>\n\
                 Explain what this SQL does, in {language}. Start with one sentence on what it \
                 returns or changes, then how, in a few short points tied to this database's \
                 tables. Point out anything likely to be slow or surprising, such as a missing join \
                 condition or NULL handling. Don't repeat the SQL.",
                sql.trim()
            ),
            Task::Fix { sql, error } => format!(
                "<sql>\n{}\n</sql>\n<error>\n{}\n</error>\n\
                 This statement failed with the error above. Say in one or two sentences, in \
                 {language}, what caused it, then give the corrected statement in a ```sql block. \
                 Change only what the fix needs.",
                sql.trim(),
                error.trim()
            ),
        }
    }
}

/// The database a request is about.
pub struct Setting<'a> {
    /// E.g. `18.0`.
    pub server_version: &'a str,
    /// Where unqualified names resolve, usually `public`.
    pub default_schema: Option<&'a str>,
    pub database: &'a Database,
}

/// The database's structure as the model sees it: the snapshot's `CREATE`
/// statements, comments included. The same schema always gives the same text,
/// so requests share a cached prefix until the schema changes.
pub fn schema_context(database: &Database) -> String {
    let mut context = String::new();
    for (path, ddl) in Snapshot::of(database).files() {
        if path.ends_with(".sql") {
            context.push_str(ddl.trim_end());
            context.push_str("\n\n");
        }
    }
    context
}

/// The Messages API request for `task`, answered in `language` (e.g. `Korean`).
pub fn request_body(model: &str, setting: &Setting, task: &Task, language: &str) -> Value {
    let database = format!(
        "Server: PostgreSQL {}\nDefault schema: {}\n\n<database>\n{}</database>",
        setting.server_version,
        setting.default_schema.unwrap_or("(none)"),
        schema_context(setting.database),
    );
    let mut body = json!({
        "model": model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "output_config": { "effort": "medium" },
        "system": [
            { "type": "text", "text": INSTRUCTIONS },
            { "type": "text", "text": database, "cache_control": { "type": "ephemeral" } },
        ],
        "messages": [{ "role": "user", "content": task.prompt(language) }],
    });
    if has_fallbacks(model) {
        body["fallbacks"] = json!("default");
    }
    body
}

/// Whether the API can retry the model's declined requests on another model.
fn has_fallbacks(model: &str) -> bool {
    matches!(model, "claude-opus-5-5" | "claude-sonnet-5-5")
}

/// How an answer ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    /// The model that wrote it, which is a fallback model after a decline.
    pub model: String,
    /// `end_turn`, or `max_tokens` when the answer was cut off.
    pub stop_reason: Option<String>,
}

pub struct Client {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl Client {
    pub fn new(api_key: String) -> Self {
        Self::with_base_url(api_key, API_URL)
    }

    /// A client for another server that speaks the same API, such as a test double.
    pub fn with_base_url(api_key: String, base_url: &str) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            // The API sends pings while it thinks, so a long silence means a broken connection.
            .read_timeout(Duration::from_secs(300))
            // The API key travels in a header that redirects would carry along.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("the HTTP client's settings are valid");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
        }
    }

    /// Sends `body` and passes the answer's text to `on_text` as it arrives,
    /// until the answer ends or `cancel` completes.
    pub async fn stream(
        &self,
        body: &Value,
        on_text: impl FnMut(&str),
        cancel: impl Future<Output = ()>,
    ) -> Result<Answer, Error> {
        match select(pin!(self.answer(body, on_text)), pin!(cancel)).await {
            Either::Left((answer, _)) => answer,
            Either::Right(_) => Err(ai_error(AiFailure::Cancelled, "stopped")),
        }
    }

    async fn answer(&self, body: &Value, mut on_text: impl FnMut(&str)) -> Result<Answer, Error> {
        let response = self.send(body).await?;
        let mut answer = Answer {
            model: body["model"].as_str().unwrap_or_default().to_owned(),
            stop_reason: None,
        };
        let mut events = SseParser::default();
        let mut bytes = response.bytes_stream();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|err| ai_error(AiFailure::Network, error_chain(&err)))?;
            for event in events.push(&chunk) {
                let data: Value = serde_json::from_str(&event.data).unwrap_or(Value::Null);
                match event.name.as_str() {
                    "message_start" => {
                        if let Some(model) = data["message"]["model"].as_str() {
                            answer.model = model.to_owned();
                        }
                    }
                    // Other blocks (thinking, a fallback marker) aren't shown. After a
                    // mid-answer fallback the new model continues the text, so it joins up.
                    "content_block_delta" if data["delta"]["type"] == "text_delta" => {
                        on_text(data["delta"]["text"].as_str().unwrap_or_default());
                    }
                    "message_delta" => {
                        if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                            answer.stop_reason = Some(reason.to_owned());
                        }
                    }
                    "message_stop" => {
                        return if answer.stop_reason.as_deref() == Some("refusal") {
                            Err(ai_error(
                                AiFailure::Declined,
                                "the request was declined by the API's safety checks",
                            ))
                        } else {
                            Ok(answer)
                        };
                    }
                    "error" => return Err(api_error(None, &data)),
                    _ => {}
                }
            }
        }
        Err(ai_error(
            AiFailure::Network,
            "the connection closed before the answer was complete",
        ))
    }

    /// Posts the request, retrying failures that may pass on a second try.
    async fn send(&self, body: &Value) -> Result<reqwest::Response, Error> {
        let mut request = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .json(body);
        if body.get("fallbacks").is_some() {
            request = request.header("anthropic-beta", FALLBACK_BETA);
        }
        let mut attempt = 0;
        loop {
            let retry = attempt < MAX_RETRIES;
            let delay = Duration::from_millis(500 << attempt);
            attempt += 1;
            let request = request
                .try_clone()
                .expect("a JSON request body can be sent again");
            match request.send().await {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status().as_u16();
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok()?.parse().ok())
                        .map(Duration::from_secs);
                    let text = response.text().await.unwrap_or_default();
                    let error = api_error(
                        Some(status),
                        &serde_json::from_str(&text).unwrap_or_default(),
                    );
                    if retry && matches!(status, 408 | 409 | 429 | 500..) {
                        // Wait as long as the API asks, within reason, or back off.
                        tokio::time::sleep(
                            retry_after.unwrap_or(delay).min(Duration::from_secs(20)),
                        )
                        .await;
                        continue;
                    }
                    return Err(error);
                }
                Err(err) if retry && (err.is_connect() || err.is_timeout()) => {
                    tokio::time::sleep(delay).await;
                }
                Err(err) => return Err(ai_error(AiFailure::Network, error_chain(&err))),
            }
        }
    }
}

/// Words an API error by its HTTP status or, mid-stream, its error type.
fn api_error(status: Option<u16>, body: &Value) -> Error {
    let kind = body["error"]["type"].as_str().unwrap_or_default();
    let message = body["error"]["message"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| match status {
            Some(status) => format!("the API answered with HTTP {status}"),
            None => "the API reported an error".to_owned(),
        });
    let reason = match (status, kind) {
        (Some(401 | 403), _) | (_, "authentication_error" | "permission_error") => {
            AiFailure::BadApiKey
        }
        (Some(429), _) | (_, "rate_limit_error") => AiFailure::RateLimited,
        (Some(500..), _) | (_, "overloaded_error" | "api_error") => AiFailure::Unavailable,
        _ => AiFailure::Other,
    };
    ai_error(reason, message)
}

fn ai_error(reason: AiFailure, message: impl Into<String>) -> Error {
    Error::Ai {
        reason,
        message: message.into(),
    }
}

fn error_chain(err: &dyn std::error::Error) -> String {
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(err) = source {
        message.push_str(": ");
        message.push_str(&err.to_string());
        source = err.source();
    }
    message
}

/// One server-sent event.
#[derive(Debug, PartialEq, Eq)]
struct SseEvent {
    name: String,
    data: String,
}

/// Splits a byte stream into server-sent events, whichever way the bytes are
/// chunked, even in the middle of a character.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((end, separator)) = find_blank_line(&self.buffer) {
            let raw: Vec<u8> = self.buffer.drain(..end + separator).collect();
            let text = String::from_utf8_lossy(&raw[..end]);
            let mut event = SseEvent {
                name: "message".into(),
                data: String::new(),
            };
            for line in text.lines() {
                let (field, value) = line.split_once(':').unwrap_or((line, ""));
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "event" => event.name = value.to_owned(),
                    "data" => {
                        if !event.data.is_empty() {
                            event.data.push('\n');
                        }
                        event.data.push_str(value);
                    }
                    _ => {}
                }
            }
            if !event.data.is_empty() {
                events.push(event);
            }
        }
        events
    }
}

/// Where the first event ends, and the length of the blank line ending it.
fn find_blank_line(bytes: &[u8]) -> Option<(usize, usize)> {
    (0..bytes.len()).find_map(|i| {
        if bytes[i..].starts_with(b"\r\n\r\n") {
            Some((i, 4))
        } else if bytes[i..].starts_with(b"\n\n") {
            Some((i, 2))
        } else {
            None
        }
    })
}

/// Credential store service and account for the API key.
const KEY_SERVICE: &str = "Dolmen";
const KEY_ACCOUNT: &str = "anthropic-api-key";

/// The Anthropic API key: kept in the OS credential store, else taken from
/// `ANTHROPIC_API_KEY`. It never goes to the UI.
pub struct ApiKeys {
    credentials: Option<Arc<CredentialStore>>,
}

/// Where the key comes from, for the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyStatus {
    /// A key is kept in the credential store.
    pub saved: bool,
    /// `ANTHROPIC_API_KEY` is set; it's used when no key is saved.
    pub from_environment: bool,
}

impl ApiKeys {
    pub fn new(credentials: Option<Arc<CredentialStore>>) -> Self {
        Self { credentials }
    }

    pub fn get(&self) -> Result<Option<String>, Error> {
        if let Some(saved) = self.saved()? {
            return Ok(Some(saved));
        }
        Ok(environment_key())
    }

    pub fn status(&self) -> Result<KeyStatus, Error> {
        Ok(KeyStatus {
            saved: self.saved()?.is_some(),
            from_environment: environment_key().is_some(),
        })
    }

    pub fn set(&self, key: &str) -> Result<(), Error> {
        let key = key.trim();
        if key.is_empty() {
            return Err(Error::Storage {
                message: "the API key is empty".into(),
            });
        }
        let credentials = self.credentials.as_ref().ok_or_else(|| Error::Storage {
            message: "no credential store is available to keep the API key in".into(),
        })?;
        Ok(credentials
            .build(KEY_SERVICE, KEY_ACCOUNT, None)?
            .set_password(key)?)
    }

    pub fn forget(&self) -> Result<(), Error> {
        let Some(credentials) = &self.credentials else {
            return Ok(());
        };
        match credentials
            .build(KEY_SERVICE, KEY_ACCOUNT, None)?
            .delete_credential()
        {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    fn saved(&self) -> Result<Option<String>, Error> {
        let Some(credentials) = &self.credentials else {
            return Ok(None);
        };
        match credentials
            .build(KEY_SERVICE, KEY_ACCOUNT, None)?
            .get_password()
        {
            Ok(key) => Ok(Some(key)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

fn environment_key() -> Option<String> {
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_events_however_the_bytes_arrive() {
        let stream = "event: message_start\ndata: {\"a\":1}\n\n: comment\n\nevent: content_block_delta\r\ndata: {\"text\":\"한글\"}\r\n\r\nevent: ping\ndata: {}\n\n";
        let expected = vec![
            SseEvent {
                name: "message_start".into(),
                data: "{\"a\":1}".into(),
            },
            SseEvent {
                name: "content_block_delta".into(),
                data: "{\"text\":\"한글\"}".into(),
            },
            SseEvent {
                name: "ping".into(),
                data: "{}".into(),
            },
        ];
        let bytes = stream.as_bytes();
        // Every split point, including inside the multi-byte characters.
        for split in 0..bytes.len() {
            let mut parser = SseParser::default();
            let mut events = parser.push(&bytes[..split]);
            events.extend(parser.push(&bytes[split..]));
            assert_eq!(events, expected, "split at {split}");
        }
        // And one byte at a time.
        let mut parser = SseParser::default();
        let events: Vec<_> = bytes.iter().flat_map(|b| parser.push(&[*b])).collect();
        assert_eq!(events, expected);
    }

    #[test]
    fn classifies_api_errors() {
        let body = |kind: &str| json!({"type": "error", "error": {"type": kind, "message": "m"}});
        let reason = |error: Error| match error {
            Error::Ai { reason, .. } => reason,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            reason(api_error(Some(401), &body("authentication_error"))),
            AiFailure::BadApiKey
        );
        assert_eq!(
            reason(api_error(Some(429), &body("rate_limit_error"))),
            AiFailure::RateLimited
        );
        assert_eq!(
            reason(api_error(Some(529), &body("overloaded_error"))),
            AiFailure::Unavailable
        );
        assert_eq!(
            reason(api_error(None, &body("overloaded_error"))),
            AiFailure::Unavailable
        );
        assert_eq!(
            reason(api_error(Some(400), &body("invalid_request_error"))),
            AiFailure::Other
        );
        assert!(matches!(
            api_error(Some(502), &Value::Null),
            Error::Ai { reason: AiFailure::Unavailable, message } if message.contains("502")
        ));
    }
}
