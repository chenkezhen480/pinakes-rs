//! The test chat: a scripted model steps through the MCP read tools and answers; every step
//! streams back as a Server-Sent Event. Mounted only with `PINAKES_DEV=1`, so these tests merge
//! its routes themselves.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::app::TestApp;
use common::testset;
use http_body_util::BodyExt;
use pinakes::llm::{LlmClient, Prompt};
use serde_json::{Value, json};
use tower::ServiceExt;

/// One scripted step: the reply to the message the model is sent.
type Reply = Box<dyn Fn(&str) -> Value + Send + Sync>;

/// Replies with one scripted step per call, and keeps the messages it was sent.
struct Script {
    steps: Vec<Reply>,
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl LlmClient for Script {
    async fn complete(&self, prompt: &Prompt) -> anyhow::Result<String> {
        assert_eq!(prompt.system, pinakes::dev::system());
        let mut seen = self.seen.lock().unwrap();
        let step = &self.steps[seen.len().min(self.steps.len() - 1)];
        seen.push(prompt.user.clone());
        let reply = step(&prompt.user);
        common::llm::fits(&prompt.schema, &reply, "$").unwrap();
        Ok(reply.to_string())
    }
}

fn step(tool: &str, arguments: Value) -> Value {
    json!({ "thought": format!("call {tool}"), "tool": tool, "arguments": arguments.to_string(), "answer": null })
}

/// The events of a chat, as (name, data).
async fn chat(
    app: &TestApp,
    key: Option<&str>,
    question: &str,
) -> (StatusCode, Vec<(String, Value)>) {
    let router = app
        .router
        .clone()
        .merge(pinakes::dev::router(app.state.clone()));
    let mut req = Request::post("/dev/chat").header("Content-Type", "application/json");
    if let Some(k) = key {
        req = req
            .header("Authorization", format!("Bearer {k}"))
            .header("X-User-Id", "wang");
    }
    let resp = router
        .oneshot(
            req.body(Body::from(json!({ "question": question }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&body);
    let mut events = Vec::new();
    for block in text.split("\n\n") {
        let name = block
            .lines()
            .find_map(|l| l.strip_prefix("event:"))
            .map(str::trim);
        let data = block
            .lines()
            .find_map(|l| l.strip_prefix("data:"))
            .map(str::trim);
        if let (Some(n), Some(d)) = (name, data) {
            events.push((n.to_owned(), serde_json::from_str(d).unwrap()));
        }
    }
    (status, events)
}

#[tokio::test]
async fn the_model_searches_reads_and_answers_step_by_step() {
    let script = Arc::new(Script {
        steps: vec![
            Box::new(|_| {
                step(
                    "search",
                    json!({ "query": "七天无理由退货", "keywords": ["七天无理由"] }),
                )
            }),
            Box::new(|user| {
                // The search's result, with its session, is in the steps so far.
                let at = user
                    .find("\"session\":\"")
                    .expect("the search result is shown")
                    + 11;
                let session = &user[at..at + user[at..].find('"').unwrap()];
                step(
                    "read",
                    json!({ "ids": ["expertise/refund-regulations"], "session": session }),
                )
            }),
            Box::new(|_| {
                json!({ "thought": "enough", "tool": "answer", "arguments": null,
                        "answer": "签收后 7 天内可无理由退货（expertise/refund-regulations）。" })
            }),
        ],
        seen: Mutex::new(Vec::new()),
    });
    let mut app = TestApp::with_llm(script.clone());
    let key = app.create_tenant("shopline").await;
    testset::install_wiki("shopline", &app.wiki_dir("shopline"));
    app.restart();

    let (status, events) = chat(&app, Some(&key), "七天无理由退货有什么规定？").await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = events.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        ["step", "result", "step", "result", "answer"],
        "{events:?}"
    );
    assert_eq!(events[0].1["tool"], "search");
    assert_eq!(
        events[1].1["result"]["knowledge"]["hits"][0]["page"],
        "expertise/refund-regulations"
    );
    assert!(
        events[3].1["result"]["pages"][0]["text"].is_string(),
        "{:?}",
        events[3]
    );
    assert!(events[4].1["text"].as_str().unwrap().contains("7 天"));
    let seen = script.seen.lock().unwrap();
    assert!(seen[0].contains("No steps yet."));
    assert!(seen[2].contains("2. read"), "{}", seen[2]);
}

#[tokio::test]
async fn the_chat_needs_credentials_and_the_librarian() {
    let mut app = TestApp::without_llm();
    let key = app.create_tenant("shopline").await;
    app.restart();
    assert_eq!(chat(&app, None, "hi").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(
        chat(&app, Some(&key), "hi").await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    // Not mounted unless asked for.
    let (status, _) = app.get(common::app::Caller::new(&key), "/dev").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_model_that_never_answers_is_stopped() {
    let script = Arc::new(Script {
        steps: vec![Box::new(|_| step("search", json!({ "query": "退货" })))],
        seen: Mutex::new(Vec::new()),
    });
    let mut app = TestApp::with_llm(script.clone());
    let key = app.create_tenant("shopline").await;
    app.restart();
    let (_, events) = chat(&app, Some(&key), "退货？").await;
    let (last, data) = events.last().unwrap();
    assert_eq!(last, "answer");
    assert!(
        data["text"]
            .as_str()
            .unwrap()
            .starts_with("No answer within")
    );
    assert_eq!(events.iter().filter(|(n, _)| n == "step").count(), 8);
    assert!(
        script
            .seen
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .contains("No tool calls are left")
    );
}
