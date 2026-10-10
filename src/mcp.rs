//! `/mcp`: the MCP server, over Streamable HTTP (rmcp). Its tools call the same handlers as the
//! HTTP API, with the caller's `Scope` from the same credentials, so they answer, check and trace
//! exactly as those endpoints do (design, "MCP server").
//!
//! The tools expose only what an agent has to judge; the rest keeps its defaults. The read
//! tracker's `session` is the one parameter that isn't the agent's judgement: MCP 2026-07-28
//! removed protocol sessions, so `search` hands out a session id in its result and the other
//! tools take it back. A client of an earlier protocol version has a `Mcp-Session-Id`, which
//! stands in when the agent passes none.

use axum::Json;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::AppState;
use crate::api::{draft, graph, insert, read, search};
use crate::error::ApiError;
use crate::identity::Scope;

pub(crate) const INSTRUCTIONS: &str = include_str!("../prompts/tools/instructions.md");

/// The header an MCP client before protocol 2026-07-28 sends its session in.
const SESSION_HEADER: &str = "mcp-session-id";

/// The tools, in the order clients list them: the entry, the follow-ups, then the records.
pub(crate) const TOOLS: [(&str, &str); 8] = [
    ("search", include_str!("../prompts/tools/search.md")),
    ("read", include_str!("../prompts/tools/read.md")),
    ("graph", include_str!("../prompts/tools/graph.md")),
    ("path", include_str!("../prompts/tools/path.md")),
    (
        "record_experience",
        include_str!("../prompts/tools/record_experience.md"),
    ),
    (
        "record_decision",
        include_str!("../prompts/tools/record_decision.md"),
    ),
    (
        "record_preference",
        include_str!("../prompts/tools/record_preference.md"),
    ),
    (
        "propose_graph_change",
        include_str!("../prompts/tools/propose_graph_change.md"),
    ),
];

/// A tool's parameters as a closed JSON schema object; `session` is added to every tool that
/// uses the read tracker.
pub(crate) fn schema(tool: &str) -> Value {
    let text = |d: &str| json!({ "type": "string", "description": d });
    let list =
        |d: &str| json!({ "type": "array", "items": { "type": "string" }, "description": d });
    let entities = list("Ids of graph nodes or node types it concerns, as search returned them.");
    let relations = list("Relation types to follow, such as placed_by; all when left out.");
    let (mut properties, required) = match tool {
        "search" => (
            json!({
                "query": text("The request as one sentence with meaning."),
                "keywords": list("2 to 6 short terms of 1 to 3 words: names, identifiers, \
                    canonical terms, in both English and Chinese."),
                "mode": {
                    "type": "string",
                    "enum": ["keyword", "semantic", "both", "auto"],
                    "description": "How pages are ranked. Leave it out at first; use semantic \
                        (by meaning) when a slot came back weak.",
                },
            }),
            vec!["query"],
        ),
        "read" => (
            json!({
                "ids": {
                    "type": "array", "items": { "type": "string" },
                    "minItems": 1, "maxItems": read::MAX_IDS,
                    "description": "Page or source ids from search results or routes.",
                },
                "heading": text("Only this heading of the page, as a route names it."),
            }),
            vec!["ids"],
        ),
        "graph" => (
            json!({
                "node": text("A node id from search, such as commerce:order/1042."),
                "depth": {
                    "type": "integer", "minimum": 1,
                    "description": "How many edges away to walk; 1 when left out, 3 at most \
                        unless the service allows more.",
                },
                "relations": relations,
            }),
            vec!["node"],
        ),
        "path" => (
            json!({
                "from": text("A node id from search or graph."),
                "to": text("Another node id from search or graph."),
                "relations": relations,
            }),
            vec!["from", "to"],
        ),
        "record_experience" => (
            json!({
                "task": text("The kind of task, short and general."),
                "outcome": text("What happened, what worked or didn't, with exact numbers and \
                    conditions."),
                "details": text("Steps, pitfalls and the reasons behind them."),
                "entities": entities,
            }),
            vec!["task", "outcome"],
        ),
        "record_decision" => (
            json!({
                "topic": text("What the decision is about, short."),
                "decision": text("What the user decided, with exact numbers and conditions."),
                "context": text("The situation that led to it."),
                "outcome": text("What came of it, if known."),
                "entities": entities,
            }),
            vec!["topic", "decision"],
        ),
        "record_preference" => (
            json!({
                "statement": text("The preference, in one or two sentences."),
                "entities": entities,
            }),
            vec!["statement"],
        ),
        "propose_graph_change" => (
            json!({
                "space": text("The graph space, as a node id names it (the part before the colon)."),
                "changes": {
                    "type": "array",
                    "maxItems": crate::graph::draft::MAX_CHANGES,
                    "description": "Changes to add to the space's pending draft, in order; leave \
                        out to only see the draft.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "op": { "type": "string",
                                    "enum": ["put_node", "remove_node", "add_edge", "remove_edge"] },
                            "type": text("The node type (put_node, remove_node) or relation \
                                type (add_edge, remove_edge), from the space's schema."),
                            "key": text("The node's key (put_node, remove_node)."),
                            "name": text("The node's name (put_node); left out, it stays (a new \
                                node's is its key)."),
                            "aliases": list("Other names of the node (put_node), the whole list; \
                                left out, they stay."),
                            "properties": {
                                "type": "object",
                                "description": "Values of properties the node's type declares \
                                    in the schema (put_node), merged in by name; null takes one out.",
                            },
                            "from": text("The edge's start as type/key (add_edge, remove_edge)."),
                            "to": text("The edge's end as type/key (add_edge, remove_edge)."),
                        },
                        "required": ["op", "type"],
                        "additionalProperties": false,
                    },
                },
                "reason": text("Why, for the person who reviews it (required with changes)."),
            }),
            vec!["space"],
        ),
        _ => unreachable!("every tool has a schema"),
    };
    if !matches!(tool, "record_preference" | "propose_graph_change") {
        properties["session"] = text("The session from this task's first search result.");
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn tool(name: &'static str, description: &'static str) -> Tool {
    let Value::Object(schema) = schema(name) else {
        unreachable!("a schema is an object")
    };
    let read_only = !name.starts_with("record_") && name != "propose_graph_change";
    Tool::new(name, description, schema).annotate(
        ToolAnnotations::new()
            .read_only(read_only)
            .destructive(false)
            .open_world(false),
    )
}

/// The MCP server for one client: the shared state, nothing of its own.
#[derive(Clone)]
pub struct Mcp {
    app: AppState,
}

/// The `/mcp` service. With legacy session mode on, clients before protocol 2026-07-28 get a
/// `Mcp-Session-Id`; later ones are served without sessions. It stops its streams when
/// `app.shutdown` is cancelled.
pub fn service(app: AppState) -> StreamableHttpService<Mcp, LocalSessionManager> {
    let config = StreamableHttpServerConfig::default()
        .with_json_response(true)
        .with_cancellation_token(app.shutdown.child_token())
        // The host check guards local servers without credentials against DNS rebinding;
        // every call here needs a key, and in a container the host is the service's name.
        .disable_allowed_hosts();
    StreamableHttpService::new(
        move || Ok(Mcp { app: app.clone() }),
        Default::default(),
        config,
    )
}

/// Resolves the caller once per HTTP request, as every route does, and hands the `Scope` to the
/// tools in the request's extensions; without valid credentials, 401 before any MCP message.
pub async fn authenticate(State(app): State<AppState>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    match Scope::from_request_parts(&mut parts, &app).await {
        Ok(scope) => {
            parts.extensions.insert(scope);
            next.run(Request::from_parts(parts, body)).await
        }
        Err(e) => e.into_response(),
    }
}

impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("pinakes", env!("CARGO_PKG_VERSION")).with_title("Pinakes"),
            )
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(
            TOOLS.iter().map(|(n, d)| tool(n, d)).collect(),
        ))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        TOOLS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(n, d)| tool(n, d))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let Some(&(name, _)) = TOOLS.iter().find(|(n, _)| *n == request.name) else {
            return Err(ErrorData::invalid_params(
                format!("no tool named {:?}", request.name),
                None,
            ));
        };
        let parts = context
            .extensions
            .get::<Parts>()
            .ok_or_else(|| ErrorData::internal_error("not an HTTP request", None))?;
        let scope = parts
            .extensions
            .get::<Scope>()
            .cloned()
            .ok_or_else(|| ErrorData::internal_error("the caller was not resolved", None))?;
        let args = request.arguments.unwrap_or_default();
        // A tool's failure is its result, so the agent reads why and can call again.
        let result = match run(&self.app, name, scope, &parts.headers, args).await {
            Ok(v) => CallToolResult::success(vec![ContentBlock::text(v.to_string())]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e.body().to_string())]),
        };
        Ok(result.into())
    }
}

/// Runs one tool for a caller, through the HTTP handler it stands for. `headers` are the HTTP
/// request's: the audience header for records, and an earlier protocol's session header.
#[tracing::instrument(name = "mcp_tool", skip_all, fields(tool = name))]
pub(crate) async fn run(
    app: &AppState,
    name: &'static str,
    scope: Scope,
    headers: &HeaderMap,
    mut args: JsonObject,
) -> Result<Value, ApiError> {
    let schema = schema(name);
    if let Some(k) = args.keys().find(|k| schema["properties"].get(k).is_none()) {
        return Err(ApiError::BadRequest(format!(
            "{name} takes no parameter {k:?}"
        )));
    }
    let session = match args.remove("session") {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s),
        None | Some(Value::Null) | Some(Value::String(_)) => headers
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        Some(_) => return Err(ApiError::BadRequest("session must be a string".into())),
    };
    let app = app.clone();
    match name {
        "search" => {
            let session = session.unwrap_or_else(new_session);
            args.insert("session".into(), json!(session));
            let Json(found) = search::search(State(app), scope, Ok(Json(request(args)?))).await?;
            let mut out = to_value(found)?;
            out["session"] = json!(session);
            Ok(out)
        }
        _ => {
            if let Some(s) =
                session.filter(|_| !matches!(name, "record_preference" | "propose_graph_change"))
            {
                args.insert("session".into(), json!(s));
            }
            match name {
                "read" => to_value(
                    read::read(State(app), scope, Ok(Json(request(args)?)))
                        .await?
                        .0,
                ),
                "graph" => to_value(
                    graph::graph(State(app), scope, Ok(Json(request(args)?)))
                        .await?
                        .0,
                ),
                "path" => to_value(
                    graph::path(State(app), scope, Ok(Json(request(args)?)))
                        .await?
                        .0,
                ),
                "propose_graph_change" => to_value(
                    draft::propose(State(app), scope, Ok(Json(request(args)?)))
                        .await?
                        .0,
                ),
                _ => {
                    let record = Ok(Json(Value::Object(args)));
                    let headers = headers.clone();
                    let (_, Json(job)) = match name {
                        "record_experience" => {
                            insert::experience(State(app), scope, headers, record).await?
                        }
                        "record_decision" => {
                            insert::decision(State(app), scope, headers, record).await?
                        }
                        _ => insert::preference(State(app), scope, headers, record).await?,
                    };
                    to_value(job)
                }
            }
        }
    }
}

/// A read-tracker session for a search that came without one.
fn new_session() -> String {
    format!("s_{}", hex::encode(rand::random::<[u8; 8]>()))
}

fn request<T: DeserializeOwned>(args: JsonObject) -> Result<T, ApiError> {
    serde_json::from_value(Value::Object(args)).map_err(|e| ApiError::BadRequest(e.to_string()))
}

fn to_value(v: impl serde::Serialize) -> Result<Value, ApiError> {
    serde_json::to_value(v).map_err(|e| ApiError::Internal(e.into()))
}
