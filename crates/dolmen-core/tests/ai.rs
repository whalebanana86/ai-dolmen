//! Asking Claude about SQL, against a local server that answers like the
//! Messages API.

use std::collections::HashMap;
use std::time::Duration;

use dolmen_core::ai::{self, Client, Setting, Task};
use dolmen_core::schema::{Column, Database, Schema, Table, TableKind};
use dolmen_core::{AiFailure, Error};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

fn database() -> Database {
    let column = |name: &str, data_type: &str, comment: Option<&str>| Column {
        name: name.into(),
        data_type: data_type.into(),
        nullable: false,
        default: None,
        identity: None,
        generated: None,
        comment: comment.map(Into::into),
    };
    Database {
        schemas: vec![Schema {
            name: "public".into(),
            tables: vec![Table {
                name: "users".into(),
                kind: TableKind::Table,
                columns: vec![
                    column("id", "bigint", None),
                    column("joined_on", "date", Some("the day the user signed up")),
                ],
                constraints: vec![],
                indexes: vec![],
                partition_key: None,
                partition_of: None,
                grants: vec![],
                triggers: vec![],
                row_security: false,
                force_row_security: false,
                policies: vec![],
                comment: Some("people with an account".into()),
            }],
            views: vec![],
            functions: vec![],
            sequences: vec![],
            enums: vec![],
        }],
        extensions: vec![],
    }
}

fn body(model: &str, task: &Task) -> Value {
    let database = database();
    let setting = Setting {
        server_version: "18.0",
        default_schema: Some("public"),
        database: &database,
    };
    ai::request_body(model, &setting, task, "Korean")
}

fn generate() -> Task {
    Task::Generate {
        request: "how many users joined last month".into(),
        context: Some("SELECT version();".into()),
    }
}

/// A request the server received.
struct Received {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

/// Serves one canned response per connection, in order, then stops. A
/// response of `None` writes `partial` and then holds the connection open.
fn serve(responses: Vec<String>) -> (String, JoinHandle<Vec<Received>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let listener = TcpListener::from_std(listener).unwrap();
    let server = tokio::spawn(async move {
        let mut received = Vec::new();
        for response in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            let (head_end, length) = loop {
                let n = socket.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .map_or(0, |v| v.trim().parse().unwrap());
                    break (end + 4, length);
                }
            };
            while request.len() < head_end + length {
                let n = socket.read(&mut buf).await.unwrap();
                request.extend_from_slice(&buf[..n]);
            }
            let head = String::from_utf8_lossy(&request[..head_end]).into_owned();
            let mut lines = head.lines();
            let path = lines.next().unwrap().split(' ').nth(1).unwrap().to_owned();
            let headers = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_owned()))
                .collect();
            let body = serde_json::from_slice(&request[head_end..]).unwrap();
            received.push(Received {
                path,
                headers,
                body,
            });
            socket.write_all(response.as_bytes()).await.unwrap();
            if response.ends_with("\n\n") && response.contains("event: hold") {
                // Keep the connection open, like an answer still being written.
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            socket.shutdown().await.ok();
        }
        received
    });
    (url, server)
}

fn event(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn stream_response(events: &[String]) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{}",
        events.concat()
    )
}

fn json_response(status: &str, error_type: &str) -> String {
    let body =
        json!({"type": "error", "error": {"type": error_type, "message": "nope"}}).to_string();
    format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn text_delta(index: u32, text: &str) -> String {
    event(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}),
    )
}

/// An answer as the API streams it: a thinking block, then text in pieces.
fn answer(model: &str, texts: &[&str], stop_reason: &str) -> String {
    let mut events = vec![
        event(
            "message_start",
            json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant", "model": model, "content": []}}),
        ),
        event(
            "content_block_start",
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        ),
        event(
            "content_block_stop",
            json!({"type": "content_block_stop", "index": 0}),
        ),
        event(
            "content_block_start",
            json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        ),
        event("ping", json!({"type": "ping"})),
    ];
    events.extend(texts.iter().map(|text| text_delta(1, text)));
    events.push(event(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": 1}),
    ));
    events.push(event(
        "message_delta",
        json!({"type": "message_delta", "delta": {"stop_reason": stop_reason}, "usage": {"output_tokens": 9}}),
    ));
    events.push(event("message_stop", json!({"type": "message_stop"})));
    stream_response(&events)
}

async fn ask(url: &str, body: &Value) -> (Result<ai::Answer, Error>, String) {
    let mut text = String::new();
    let answer = Client::with_base_url("test-key".into(), url)
        .stream(body, |t| text.push_str(t), std::future::pending())
        .await;
    (answer, text)
}

fn reason(result: Result<ai::Answer, Error>) -> AiFailure {
    match result {
        Err(Error::Ai { reason, .. }) => reason,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn streams_the_answer_and_sends_the_documented_request() {
    let (url, server) = serve(vec![answer(
        "claude-opus-5-5",
        &[
            "```sql\nSELECT count(*)\n",
            "FROM users;\n```\n지난달 가입자 수입니다.",
        ],
        "end_turn",
    )]);
    let (answer, text) = ask(&url, &body("claude-opus-5-5", &generate())).await;
    let answer = answer.unwrap();
    assert_eq!(answer.model, "claude-opus-5-5");
    assert_eq!(answer.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(
        text,
        "```sql\nSELECT count(*)\nFROM users;\n```\n지난달 가입자 수입니다."
    );

    let request = server.await.unwrap().remove(0);
    assert_eq!(request.path, "/v1/messages");
    assert_eq!(request.headers["x-api-key"], "test-key");
    assert_eq!(request.headers["anthropic-version"], "2026-10-21");
    assert_eq!(
        request.headers["anthropic-beta"],
        "server-side-fallback-2026-07-01"
    );
    let body = request.body;
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["fallbacks"], "default");
    assert_eq!(body["output_config"]["effort"], "medium");
    for removed in ["temperature", "top_p", "top_k", "thinking"] {
        assert!(body.get(removed).is_none(), "{removed}");
    }
    // The schema is the cached prefix, with the comments that give it meaning.
    let database = &body["system"][1];
    assert_eq!(database["cache_control"]["type"], "ephemeral");
    let schema = database["text"].as_str().unwrap();
    assert!(schema.contains("Server: PostgreSQL 18.0"), "{schema}");
    assert!(schema.contains("CREATE TABLE public.users"), "{schema}");
    assert!(schema.contains("the day the user signed up"), "{schema}");
    let prompt = body["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains("how many users joined last month"));
    assert!(prompt.contains("SELECT version();"));
    assert!(prompt.contains("Korean"));
}

#[test]
fn requests_differ_only_after_the_cached_prefix() {
    let explain = Task::Explain {
        sql: "SELECT 1".into(),
    };
    let a = body("claude-opus-5-5", &generate());
    let b = body("claude-opus-5-5", &explain);
    assert_eq!(a["system"], b["system"]);
    assert_ne!(a["messages"], b["messages"]);

    // Haiku has no server-side fallback, so the request doesn't ask for one.
    let haiku = body("claude-haiku-5-5", &explain);
    assert!(haiku.get("fallbacks").is_none());
    assert_eq!(body("claude-sonnet-5-5", &explain)["fallbacks"], "default");
}

#[tokio::test]
async fn retries_an_overloaded_api_before_the_answer_starts() {
    let (url, server) = serve(vec![
        json_response("529 Overloaded", "overloaded_error"),
        answer("claude-opus-5-5", &["ok"], "end_turn"),
    ]);
    let (answer, text) = ask(&url, &body("claude-opus-5-5", &generate())).await;
    assert!(answer.is_ok(), "{answer:?}");
    assert_eq!(text, "ok");
    assert_eq!(server.await.unwrap().len(), 2);
}

#[tokio::test]
async fn reports_a_rejected_key_without_retrying() {
    let (url, server) = serve(vec![json_response(
        "401 Unauthorized",
        "authentication_error",
    )]);
    let (answer, _) = ask(&url, &body("claude-opus-5-5", &generate())).await;
    assert_eq!(reason(answer), AiFailure::BadApiKey);
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn reports_declined_requests() {
    let (url, _server) = serve(vec![answer("claude-opus-5-5", &[], "refusal")]);
    let (answer, _) = ask(&url, &body("claude-opus-5-5", &generate())).await;
    assert_eq!(reason(answer), AiFailure::Declined);
}

#[tokio::test]
async fn reports_errors_in_the_middle_of_an_answer() {
    let (url, _server) = serve(vec![stream_response(&[
        text_delta(0, "SELECT"),
        event(
            "error",
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ),
    ])]);
    let (answer, text) = ask(&url, &body("claude-opus-5-5", &generate())).await;
    assert_eq!(text, "SELECT");
    assert_eq!(reason(answer), AiFailure::Unavailable);
}

#[tokio::test]
async fn reports_answers_cut_off_by_the_connection() {
    let (url, _server) = serve(vec![stream_response(&[text_delta(0, "SELECT")])]);
    let (answer, _) = ask(&url, &body("claude-opus-5-5", &generate())).await;
    assert_eq!(reason(answer), AiFailure::Network);
}

#[tokio::test]
async fn stops_when_cancelled() {
    let (url, server) = serve(vec![stream_response(&[
        text_delta(0, "SELECT"),
        event("hold", json!({})),
    ])]);
    let mut text = String::new();
    let started = std::time::Instant::now();
    let answer = Client::with_base_url("test-key".into(), &url)
        .stream(
            &body("claude-opus-5-5", &generate()),
            |t| text.push_str(t),
            tokio::time::sleep(Duration::from_millis(300)),
        )
        .await;
    assert_eq!(reason(answer), AiFailure::Cancelled);
    assert_eq!(text, "SELECT");
    assert!(started.elapsed() < Duration::from_secs(10));
    server.abort();
}
