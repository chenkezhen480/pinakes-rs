//! The test chat (design, "Streaming test chat"): a page at `/dev` and `POST /dev/chat`, mounted
//! only with `PINAKES_DEV=1`. The librarian's model answers a question with the MCP read tools
//! (`search`, `read`, `graph`, `path`), choosing its own calls from the same descriptions an MCP
//! agent sees, so developers can watch what an agent gets from the service. Every step streams
//! back as a Server-Sent Event: `step` (the call the model chose and why), `result` (what the
//! tool returned: a search's slots, a page, a walk), then `answer`, or `error`.
//!
//! It is for trying the service out, not part of the API; the read path itself never calls a
//! model, only this route does.

use std::convert::Infallible;
use std::sync::LazyLock;

use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use axum::http::HeaderMap;
use axum::response::Html;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::ReceiverStream;

use crate::AppState;
use crate::error::ApiError;
use crate::identity::Scope;
use crate::llm::{Llm, Prompt};

/// The tools the chat may call: the read ones. It never records.
const TOOLS: [&str; 4] = ["search", "read", "graph", "path"];
/// Tool calls before the model must answer.
const MAX_STEPS: usize = 8;
const MAX_QUESTION: usize = 2000;

/// Whether the environment turns the test chat on (`PINAKES_DEV=1`).
pub fn enabled() -> bool {
    std::env::var("PINAKES_DEV").is_ok_and(|v| v.trim() == "1")
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/dev", get(page))
        .route("/dev/chat", post(chat))
        .with_state(state)
}

async fn page() -> Html<&'static str> {
    Html(include_str!("dev.html"))
}

/// The chat's system prompt: how to step, then the service's instructions and the tools as an
/// MCP client lists them.
pub fn system() -> &'static str {
    static SYSTEM: LazyLock<String> = LazyLock::new(|| {
        let mut s = format!(
            "{}\n# The service's instructions\n\n{}\n# Tools\n",
            include_str!("../prompts/chat.md"),
            crate::mcp::INSTRUCTIONS
        );
        for (name, description) in crate::mcp::TOOLS {
            if TOOLS.contains(&name) {
                s.push_str(&format!(
                    "\n## {name}\n\n{description}\nParameters (JSON schema): {}\n",
                    crate::mcp::schema(name)
                ));
            }
        }
        s
    });
    &SYSTEM
}

/// One step, as the model must reply it.
fn step_schema() -> Value {
    let nullable = json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] });
    let mut tools: Vec<&str> = TOOLS.to_vec();
    tools.push("answer");
    json!({
        "type": "object",
        "properties": {
            "thought": { "type": "string" },
            "tool": { "type": "string", "enum": tools },
            "arguments": nullable,
            "answer": nullable,
        },
        "required": ["thought", "tool", "arguments", "answer"],
        "additionalProperties": false,
    })
}

#[derive(Deserialize)]
struct Step {
    thought: String,
    tool: String,
    arguments: Option<String>,
    answer: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatIn {
    question: String,
}

#[tracing::instrument(name = "dev_chat", skip_all)]
async fn chat(
    State(app): State<AppState>,
    scope: Scope,
    headers: HeaderMap,
    body: Result<Json<ChatIn>, JsonRejection>,
) -> Result<Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let Json(req) = body?;
    let question = req.question.trim().to_owned();
    if question.is_empty() || question.chars().count() > MAX_QUESTION {
        return Err(ApiError::BadRequest(format!(
            "question must be 1 to {MAX_QUESTION} characters"
        )));
    }
    let llm = app.librarian()?.llm();
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    tokio::spawn(converse(app, llm, scope, headers, question, tx));
    Ok(Sse::new(ReceiverStream::new(rx).map(Ok)).keep_alive(KeepAlive::default()))
}

/// Steps until the model answers, sending each as it happens; stops when the page goes away.
async fn converse(
    app: AppState,
    llm: Llm,
    scope: Scope,
    headers: HeaderMap,
    question: String,
    tx: tokio::sync::mpsc::Sender<Event>,
) {
    let send = |name: &'static str, data: Value| {
        let tx = tx.clone();
        async move {
            tx.send(Event::default().event(name).data(data.to_string()))
                .await
                .is_ok()
        }
    };
    let mut steps = String::new();
    for n in 1..=MAX_STEPS + 1 {
        let last = n > MAX_STEPS;
        let mut user = format!("Question: {question}\n\n");
        if steps.is_empty() {
            user.push_str("No steps yet.\n");
        } else {
            user.push_str(&format!("Steps so far:\n\n{steps}"));
        }
        if last {
            user.push_str("\nNo tool calls are left: answer now with what you have.\n");
        }
        let prompt = Prompt {
            system: system().to_owned(),
            user,
            max_tokens: app.config.librarian.max_tokens,
            schema: step_schema(),
        };
        // A stopping service doesn't wait for the model.
        let reply = tokio::select! {
            r = llm.complete(&prompt) => r,
            _ = app.shutdown.cancelled() => return,
        };
        let step = match reply.and_then(|r| {
            serde_json::from_str::<Step>(&r).map_err(|e| anyhow::anyhow!("a malformed step: {e}"))
        }) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "test chat: the model failed");
                send("error", json!({ "message": format!("{e:#}") })).await;
                return;
            }
        };
        let Some(&tool) = TOOLS.iter().find(|t| **t == step.tool).filter(|_| !last) else {
            let text = step
                .answer
                .filter(|a| !a.trim().is_empty())
                .unwrap_or_else(|| match last {
                    true => format!("No answer within {MAX_STEPS} tool calls."),
                    false => "The model gave up without an answer.".to_owned(),
                });
            send("answer", json!({ "thought": step.thought, "text": text })).await;
            return;
        };
        let text = step.arguments.as_deref().unwrap_or("{}");
        let args = serde_json::from_str::<Value>(text).unwrap_or_else(|_| json!(text));
        let shown = json!({ "n": n, "thought": step.thought, "tool": tool, "arguments": args });
        if !send("step", shown).await {
            return;
        }
        let result = match &args {
            Value::Object(a) => {
                match crate::mcp::run(&app, tool, scope.clone(), &headers, a.clone()).await {
                    Ok(v) => v,
                    Err(e) => e.body(),
                }
            }
            _ => ApiError::BadRequest("arguments must be a JSON object".into()).body(),
        };
        if !send("result", json!({ "n": n, "tool": tool, "result": result })).await {
            return;
        }
        steps.push_str(&format!(
            "{n}. {tool} {args}\n   why: {}\n   result: {result}\n\n",
            step.thought
        ));
    }
}
