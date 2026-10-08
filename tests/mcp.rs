//! `/mcp` driven with raw JSON-RPC, as MCP clients send it: a client of protocol 2026-07-28
//! (no sessions; the session comes back in `search` results and is passed on) and one of an
//! earlier version (its `Mcp-Session-Id` stands in for the session). The tools answer as the
//! HTTP endpoints do, with the caller's scope from the same credentials.

mod common;

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use common::app::{Caller, TestApp};
use common::llm::{FakeLlm, Step, all_ok, create, describe_from_definition};
use common::testset;
use serde_json::{Value, json};

const NEW: &str = "2026-07-28";
const OLD: &str = "2025-06-18";

/// One MCP client: its credentials and, before 2026-07-28, its session.
struct Client<'a> {
    app: &'a TestApp,
    key: String,
    user: Option<&'a str>,
    version: &'static str,
    session: Option<String>,
    next_id: u64,
}

impl<'a> Client<'a> {
    async fn connect(
        app: &'a TestApp,
        key: &str,
        user: Option<&'a str>,
        version: &'static str,
    ) -> Self {
        let mut c = Client {
            app,
            key: key.to_owned(),
            user,
            version,
            session: None,
            next_id: 1,
        };
        if version != NEW {
            let (status, headers, body) = c
                .post(json!({
                    "jsonrpc": "2.0", "id": 0, "method": "initialize",
                    "params": {
                        "protocolVersion": version, "capabilities": {},
                        "clientInfo": { "name": "test", "version": "1" },
                    },
                }))
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            c.session = Some(headers["mcp-session-id"].to_str().unwrap().to_owned());
            let (status, _, body) = c
                .post(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
                .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        }
        c
    }

    async fn post(&self, mut message: Value) -> (StatusCode, axum::http::HeaderMap, String) {
        let mut req = Request::post("/mcp")
            .header("Host", "127.0.0.1")
            .header("Authorization", format!("Bearer {}", self.key))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", self.version);
        if let Some(u) = self.user {
            req = req.header("X-User-Id", u);
        }
        if let Some(s) = &self.session {
            req = req.header("Mcp-Session-Id", s);
        }
        if self.version == NEW && message.get("id").is_some() {
            let method = message["method"].as_str().unwrap().to_owned();
            req = req.header("Mcp-Method", &method);
            if let Some(name) = message["params"]["name"].as_str() {
                req = req.header("Mcp-Name", name);
            }
            message["params"]["_meta"] = json!({
                "io.modelcontextprotocol/protocolVersion": NEW,
                "io.modelcontextprotocol/clientInfo": { "name": "test", "version": "1" },
                "io.modelcontextprotocol/clientCapabilities": {},
            });
        }
        self.app
            .send_raw(req.body(Body::from(message.to_string())).unwrap())
            .await
    }

    /// A request's result, read from a JSON body or from the event stream it came in.
    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let (status, _, body) = self
            .post(
                json!({ "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params }),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let message: Value = match serde_json::from_str(&body) {
            Ok(v) => v,
            Err(_) => body
                .lines()
                .filter_map(|l| l.strip_prefix("data:"))
                .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
                .find(|m| m.get("id").is_some())
                .unwrap_or_else(|| panic!("no response in {body}")),
        };
        assert!(message.get("error").is_none(), "{message}");
        message["result"].clone()
    }

    /// Calls a tool: its JSON answer, and whether it is an error.
    async fn call(&mut self, tool: &str, args: Value) -> (Value, bool) {
        let result = self
            .request("tools/call", json!({ "name": tool, "arguments": args }))
            .await;
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            serde_json::from_str(text).unwrap(),
            result["isError"] == true,
        )
    }

    async fn ok(&mut self, tool: &str, args: Value) -> Value {
        let (v, error) = self.call(tool, args).await;
        assert!(!error, "{tool}: {v}");
        v
    }
}

/// The `shopline` scenario, with a scripted librarian that files every experience as a new
/// playbook anchored to the knowledge pages it is told the task read (and refused without).
async fn shopline(llm: &Arc<FakeLlm>) -> (TestApp, String) {
    let mut app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("shopline").await;
    testset::install_wiki("shopline", &app.wiki_dir("shopline"));
    app.restart();
    (app, key)
}

fn librarian() -> Arc<FakeLlm> {
    FakeLlm::new(|step, msg, _| match step {
        Step::Plan => {
            let read = msg
                .lines()
                .find_map(|l| l.strip_prefix("Knowledge pages the caller read for this task: "))
                .map(|l| l.split(", ").map(str::to_owned).collect::<Vec<_>>())
                .unwrap_or_default();
            let about: Vec<&str> = read.iter().map(String::as_str).collect();
            json!({ "edits": [create(
                "experience/mcp-refund-lesson",
                "Refund lesson",
                "Check the shipment before a partial refund.",
                &about,
                "# Refund lesson\n\n## Steps\n\n- Check the shipment first.\n",
            )] })
            .to_string()
        }
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => common::llm::OK.into(),
        Step::Match => common::llm::NOT_THE_SAME.into(),
        Step::Read => common::llm::skip_to_end(msg),
    })
}

#[tokio::test]
async fn tools_are_listed_with_their_strategy() {
    let (app, key) = shopline(&librarian()).await;
    for version in [NEW, OLD] {
        let mut c = Client::connect(&app, &key, None, version).await;
        let tools = c.request("tools/list", json!({})).await;
        let tools = tools["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            [
                "search",
                "read",
                "graph",
                "path",
                "record_experience",
                "record_decision",
                "record_preference"
            ]
        );
        let search = &tools[0];
        let text = search["description"].as_str().unwrap();
        assert!(
            text.contains("回复语言"),
            "first-person questions are taught"
        );
        assert!(text.contains("unmatched_keywords"));
        let schema = &search["inputSchema"];
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["required"], json!(["query"]));
        let params: Vec<&String> = schema["properties"].as_object().unwrap().keys().collect();
        assert_eq!(
            params.len(),
            4,
            "query, keywords, mode, session: {params:?}"
        );
        assert_eq!(search["annotations"]["readOnlyHint"], true);
        assert_eq!(tools[4]["annotations"]["readOnlyHint"], false);
        assert!(tools[6]["inputSchema"]["properties"]["session"].is_null());
    }
}

#[tokio::test]
async fn mcp_needs_credentials() {
    let (app, _) = shopline(&librarian()).await;
    for key in [None, Some("pnx_not-a-key")] {
        let mut req = Request::post("/mcp")
            .header("Host", "127.0.0.1")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");
        if let Some(k) = key {
            req = req.header("Authorization", format!("Bearer {k}"));
        }
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} });
        let (status, _, body) = app
            .send_raw(req.body(Body::from(body.to_string())).unwrap())
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    }
}

/// A 2026-07-28 client: search hands out a session, the agent passes it on, and reads, later
/// searches and a recorded experience all use it; another user can't use it.
#[tokio::test]
async fn an_agent_searches_reads_and_records_with_the_session_search_gave() {
    let llm = librarian();
    let (app, key) = shopline(&llm).await;
    let mut wang = Client::connect(&app, &key, Some("wang"), NEW).await;

    let found = wang
        .ok(
            "search",
            json!({ "query": "七天无理由退货的规则是什么？", "keywords": ["七天无理由", "退货"] }),
        )
        .await;
    let session = found["session"].as_str().unwrap().to_owned();
    assert!(session.starts_with("s_"), "{found}");
    let hit = &found["knowledge"]["hits"][0];
    assert_eq!(hit["page"], "expertise/refund-regulations", "{found}");
    assert_eq!(hit["read"], false);
    let page = hit["route"]["read"].as_str().unwrap().to_owned();

    let read = wang
        .ok("read", json!({ "ids": [page], "session": session }))
        .await;
    assert!(
        read["pages"][0]["text"].as_str().unwrap().contains("退款"),
        "{read}"
    );
    let again = wang
        .ok("read", json!({ "ids": [page], "session": session }))
        .await;
    assert_eq!(again["pages"][0]["already_read"], true, "{again}");
    let found = wang
        .ok(
            "search",
            json!({ "query": "七天无理由退货", "keywords": ["七天无理由"], "session": session }),
        )
        .await;
    assert_eq!(found["session"], session.as_str());
    assert_eq!(found["knowledge"]["hits"][0]["read"], true);

    // Another user with the same session id starts fresh.
    let mut lily = Client::connect(&app, &key, Some("lily"), NEW).await;
    let theirs = lily
        .ok("read", json!({ "ids": [page], "session": session }))
        .await;
    assert!(theirs["pages"][0]["already_read"].is_null(), "{theirs}");

    // The experience is filed with the knowledge the session read as an anchor.
    let job = wang
        .ok(
            "record_experience",
            json!({
                "task": "partial refund after shipment",
                "outcome": "Checked the shipment first; the refund went through.",
                "session": session,
            }),
        )
        .await;
    assert_eq!(job["status"], "queued", "{job}");
    let done = app
        .wait_job(
            Caller::new(&key).user(Some("wang")),
            job["job"].as_str().unwrap(),
        )
        .await;
    assert_eq!(done["status"], "done", "{done}");
    let plan = llm.calls(Step::Plan).pop().unwrap();
    assert!(
        plan.contains(
            "Knowledge pages the caller read for this task: expertise/refund-regulations"
        ),
        "{plan}"
    );
    let (status, page) = app
        .get(Caller::new(&key), "/pages/experience/mcp-refund-lesson")
        .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert!(
        page.to_string().contains("expertise/refund-regulations"),
        "{page}"
    );
}

/// A client before 2026-07-28: its `Mcp-Session-Id` is the session when the agent passes none.
#[tokio::test]
async fn an_older_client_s_session_header_is_the_session() {
    let (app, key) = shopline(&librarian()).await;
    let mut c = Client::connect(&app, &key, Some("wang"), OLD).await;
    let header = c.session.clone().unwrap();
    let found = c
        .ok(
            "search",
            json!({ "query": "七天无理由退货", "keywords": ["七天无理由"] }),
        )
        .await;
    assert_eq!(found["session"], header.as_str());
    let page = "expertise/refund-regulations";
    c.ok("read", json!({ "ids": [page] })).await;
    let again = c.ok("read", json!({ "ids": [page] })).await;
    assert_eq!(again["pages"][0]["already_read"], true, "{again}");
}

/// Failures come back as tool errors the agent can read: parameters the tool doesn't take, ids
/// it can't see, a node that isn't there.
#[tokio::test]
async fn tool_errors_are_results() {
    let (app, key) = shopline(&librarian()).await;
    let mut c = Client::connect(&app, &key, Some("wang"), NEW).await;
    let (err, is_error) = c
        .call(
            "search",
            json!({ "query": "退货", "slots": { "memories": 9 } }),
        )
        .await;
    assert!(is_error);
    assert_eq!(err["error"]["code"], "bad_request", "{err}");
    assert!(err["error"]["message"].as_str().unwrap().contains("slots"));

    let (err, is_error) = c.call("search", json!({ "query": " " })).await;
    assert!(is_error, "{err}");

    let (err, is_error) = c
        .call("graph", json!({ "node": "commerce:order/0000" }))
        .await;
    assert!(is_error);
    assert_eq!(err["error"]["code"], "not_found", "{err}");

    let other = pinakes::tenants::user_dir_name("lily");
    let read = c
        .ok(
            "read",
            json!({ "ids": [format!("users/{other}/preferences")] }),
        )
        .await;
    assert_eq!(read["pages"][0]["error"], "not_found", "{read}");
}
