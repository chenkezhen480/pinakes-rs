//! Agents propose changes to graph data for a person's review (design, "Agents propose graph
//! changes"): a space has one pending draft; a later change is checked against the graph as
//! the draft would leave it and merged in, so it builds on what is pending; nothing changes
//! until the admin approves, and then the whole draft is applied at once. Drafts follow their
//! space's visibility and leave with the user who wrote into them.

use crate::common::app::{ADMIN, Caller, TestApp};
use axum::http::StatusCode;
use serde_json::{Value, json};

fn schema() -> Value {
    json!({
        "schema": "commerce",
        "summary": "Orders, their buyers and products.",
        "nodes": {
            "order": { "key": "order_id", "aliases": ["订单"] },
            "customer": { "key": "customer_id", "aliases": ["客户"],
                          "properties": { "tier": { "type": "text", "zh": "会员等级" },
                                          "since": { "type": "date" } } },
            "product": { "key": "sku", "aliases": ["商品"] }
        },
        "relations": {
            "placed_by": { "from": "order", "to": "customer" },
            "contains": { "from": "order", "to": "product" }
        }
    })
}

fn node(ty: &str, key: &str, name: &str) -> Value {
    json!({ "type": ty, "key": key, "name": name })
}

fn edge(from: &str, ty: &str, to: &str) -> Value {
    json!({ "from": from, "type": ty, "to": to })
}

async fn ok(app: &TestApp, caller: Caller<'_>, uri: &str, body: Value) -> Value {
    let (status, resp) = app.post(caller, uri, body).await;
    assert!(status.is_success(), "{uri}: {status} {resp}");
    resp
}

/// One tenant: `mainland` (shared) with an order, its customer and its product; `secret`
/// (private to jin); `vault` (restricted to marco).
async fn world() -> (TestApp, String) {
    world_with("", "acme").await
}

/// [`world`] with extra config, under a tenant id of the caller's.
async fn world_with(config: &str, tenant: &str) -> (TestApp, String) {
    let app = TestApp::with_config(config);
    let key = app.create_tenant(tenant).await;
    let c = Caller::new(&key);
    ok(&app, c, "/ingest/graph/schema", schema()).await;
    ok(
        &app,
        c,
        "/ingest/graph/space",
        json!({ "space": "mainland", "schema": "commerce" }),
    )
    .await;
    let jin = c.user(Some("jin"));
    ok(
        &app,
        jin,
        "/ingest/graph/space",
        json!({ "space": "secret", "schema": "commerce", "visibility": "private" }),
    )
    .await;
    let marco = c.user(Some("marco"));
    ok(
        &app,
        marco,
        "/ingest/graph/space",
        json!({ "space": "vault", "schema": "commerce", "visibility": "restricted",
                "members": { "users": ["marco"] } }),
    )
    .await;
    ok(
        &app,
        c,
        "/ingest/graph/data",
        json!({ "space": "mainland",
                "nodes": [node("order", "1042", "Order 1042"),
                          json!({ "type": "customer", "key": "c1", "name": "林晓",
                                  "properties": { "tier": "gold", "since": "2024-01-05" } }),
                          node("product", "p1", "AirBeat Pro")],
                "edges": [edge("order/1042", "placed_by", "customer/c1"),
                          edge("order/1042", "contains", "product/p1")] }),
    )
    .await;
    (app, key)
}

/// The edges a walk from a node shows, as `from type to`.
async fn edges_of(app: &TestApp, caller: Caller<'_>, node: &str) -> Vec<String> {
    let resp = ok(app, caller, "/graph", json!({ "node": node })).await;
    let mut out: Vec<String> = resp["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            format!(
                "{} {} {}",
                e["from"].as_str().unwrap(),
                e["type"].as_str().unwrap(),
                e["to"].as_str().unwrap()
            )
        })
        .collect();
    out.sort();
    out
}

/// With `[librarian] auto_approve`, a draft is applied the moment it is proposed: the answer
/// says what it applied, nothing waits for review, and the audit names `auto-approve`.
#[tokio::test]
async fn auto_approve_applies_a_draft_at_once() {
    let (app, key) = world_with("[librarian]\nauto_approve = true\n", "acme").await;
    let c = Caller::new(&key);
    let resp = ok(
        &app,
        c,
        "/graph/draft",
        json!({ "space": "mainland", "reason": "the order was handed to another buyer",
                "changes": [
                    { "op": "put_node", "type": "customer", "key": "c2", "name": "王芳" },
                    { "op": "add_edge", "from": "order/1042", "type": "placed_by", "to": "customer/c2" },
                    { "op": "remove_edge", "from": "order/1042", "type": "placed_by", "to": "customer/c1" }
                ] }),
    )
    .await;
    assert!(resp.get("proposal").is_none(), "{resp}");
    assert_eq!(resp["applied"]["nodes_put"], 1, "{resp}");
    assert_eq!(resp["applied"]["edges_added"], 1, "{resp}");
    assert_eq!(
        edges_of(&app, c, "mainland:order/1042").await,
        vec![
            "mainland:order/1042 contains mainland:product/p1",
            "mainland:order/1042 placed_by mainland:customer/c2"
        ]
    );
    let (_, list) = app.get(c, "/proposals").await;
    assert_eq!(list["proposals"], json!([]));
    // A change that no longer fits the graph is still refused when proposed.
    let (status, resp) = app
        .post(
            c,
            "/graph/draft",
            json!({ "space": "mainland", "reason": "x",
                    "changes": [{ "op": "remove_edge", "from": "order/1042", "type": "placed_by",
                                  "to": "customer/c1" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
}

#[tokio::test]
async fn later_changes_build_on_the_pending_draft_and_approval_applies_it_whole() {
    let (app, key) = world().await;
    let c = Caller::new(&key);
    let before = edges_of(&app, c, "mainland:order/1042").await;
    assert_eq!(
        before,
        vec![
            "mainland:order/1042 contains mainland:product/p1",
            "mainland:order/1042 placed_by mainland:customer/c1"
        ]
    );

    // Properties are the schema author's: listed on the schema's page, shown on walks, and
    // only those declared, of their kind, are taken.
    let (_, page) = app.get(c, "/pages/graph/commerce").await;
    assert_eq!(
        page["front_matter"]["nodes"]["customer"]["properties"]["tier"]["type"], "text",
        "{page}"
    );
    let walk = ok(&app, c, "/graph", json!({ "node": "mainland:customer/c1" })).await;
    assert_eq!(
        walk["node"]["properties"],
        json!({ "tier": "gold", "since": "2024-01-05" })
    );
    for bad in [
        json!({ "tier": 5 }),
        json!({ "height": 170 }),
        json!({ "since": "last year" }),
    ] {
        let (status, resp) = app
            .post(
                c,
                "/ingest/graph/data",
                json!({ "space": "mainland",
                        "nodes": [{ "type": "customer", "key": "c8", "properties": bad }] }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    }

    // A first call: a new customer took the order over.
    let first = ok(
        &app,
        c,
        "/graph/draft",
        json!({ "space": "mainland", "reason": "the order was handed to another buyer",
                "changes": [
                    { "op": "put_node", "type": "customer", "key": "c2", "name": "王芳" },
                    { "op": "add_edge", "from": "order/1042", "type": "placed_by", "to": "customer/c2" },
                    { "op": "remove_edge", "from": "order/1042", "type": "placed_by", "to": "customer/c1" }
                ] }),
    )
    .await;
    let id = first["proposal"].as_str().unwrap().to_owned();
    assert_eq!(first["put_nodes"].as_array().unwrap().len(), 1, "{first}");
    // Nothing changed in the graph.
    assert_eq!(edges_of(&app, c, "mainland:order/1042").await, before);
    let (status, _) = app
        .post(c, "/graph", json!({ "node": "mainland:customer/c2" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Refused changes leave the draft as it was: an edge to a node nobody adds, a relation
    // between the wrong types, a call without a reason, a space the caller can't see.
    for (body, want) in [
        (
            json!({ "space": "mainland", "reason": "r", "changes": [
                { "op": "add_edge", "from": "order/2001", "type": "placed_by", "to": "customer/c2" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "space": "mainland", "reason": "r", "changes": [
                { "op": "add_edge", "from": "customer/c2", "type": "placed_by", "to": "order/1042" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "space": "mainland", "changes": [
                { "op": "put_node", "type": "customer", "key": "c3" }] }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "space": "secret", "reason": "r", "changes": [
                { "op": "put_node", "type": "customer", "key": "c3" }] }),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, resp) = app.post(c, "/graph/draft", body).await;
        assert_eq!(status, want, "{resp}");
    }

    // A later call (another agent's) builds on the pending draft: it links the customer the
    // draft adds to a new order, renames her, and removes a stored product with its edge.
    let second = ok(
        &app,
        c.user(Some("lily")),
        "/graph/draft",
        json!({ "space": "mainland", "reason": "a second order and the right name",
                "changes": [
                    { "op": "put_node", "type": "order", "key": "2001", "name": "Order 2001" },
                    { "op": "add_edge", "from": "order/2001", "type": "placed_by", "to": "customer/c2" },
                    { "op": "put_node", "type": "customer", "key": "c2", "name": "王芳芳", "aliases": ["Fang"],
                      "properties": { "tier": "silver" } },
                    { "op": "remove_node", "type": "product", "key": "p1" }
                ] }),
    )
    .await;
    assert_eq!(
        second["proposal"],
        id.as_str(),
        "one draft per space: {second}"
    );
    assert_eq!(second["put_nodes"].as_array().unwrap().len(), 2, "{second}");
    assert_eq!(second["add_edges"].as_array().unwrap().len(), 2, "{second}");
    assert_eq!(second["remove_nodes"][0]["key"], "p1");
    assert_eq!(second["notes"].as_array().unwrap().len(), 2);
    let (_, shown) = app.get(c, "/graph/draft?space=mainland").await;
    assert_eq!(shown, second);

    // It waits in the review queue; only the admin approves.
    let (_, list) = app.get(c, "/proposals").await;
    assert_eq!(list["proposals"][0]["proposal"], id.as_str(), "{list}");
    assert_eq!(list["proposals"][0]["kind"], "graph");
    let (status, _) = app
        .post(c, &format!("/proposals/{id}/approve"), json!({}))
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, approved) = app
        .post(
            Caller::new(ADMIN),
            &format!("/proposals/{id}/approve"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    assert_eq!(
        approved["job"]["result"]["graph"],
        json!({ "space": "mainland", "space_deleted": false, "nodes_put": 2, "nodes_removed": 1,
                "edges_added": 2, "edges_removed": 1 })
    );
    let (again, _) = app
        .post(
            Caller::new(ADMIN),
            &format!("/proposals/{id}/approve"),
            json!({}),
        )
        .await;
    assert_eq!(again, StatusCode::CONFLICT);

    // The whole draft is in the graph, and searchable at once.
    assert_eq!(
        edges_of(&app, c, "mainland:customer/c2").await,
        vec![
            "mainland:order/1042 placed_by mainland:customer/c2",
            "mainland:order/2001 placed_by mainland:customer/c2"
        ]
    );
    let (status, _) = app
        .post(c, "/graph", json!({ "node": "mainland:product/p1" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "removed with its edge");
    let fang = ok(&app, c, "/graph", json!({ "node": "mainland:customer/c2" })).await;
    assert_eq!(fang["node"]["properties"], json!({ "tier": "silver" }));
    let (status, resp) = app
        .post(
            c,
            "/graph/draft",
            json!({ "space": "mainland", "reason": "r", "changes": [
                { "op": "put_node", "type": "customer", "key": "c2", "properties": { "tier": 3 } }] }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a property of the wrong kind: {resp}"
    );
    let found = ok(
        &app,
        c,
        "/search",
        json!({ "query": "王芳芳的订单", "keywords": ["王芳芳"] }),
    )
    .await;
    assert!(
        found.to_string().contains("mainland:customer/c2"),
        "{found}"
    );
    let (_, after) = app.get(c, "/graph/draft?space=mainland").await;
    assert!(after.get("proposal").is_none(), "{after}");

    // A new change starts a new draft; taking it back leaves nothing to review.
    let third = ok(
        &app,
        c,
        "/graph/draft",
        json!({ "space": "mainland", "reason": "r",
                "changes": [{ "op": "put_node", "type": "customer", "key": "c3" }] }),
    )
    .await;
    assert_ne!(third["proposal"], id.as_str());
    let gone = ok(
        &app,
        c,
        "/graph/draft",
        json!({ "space": "mainland", "reason": "a mistake",
                "changes": [{ "op": "remove_node", "type": "customer", "key": "c3" }] }),
    )
    .await;
    assert!(gone.get("proposal").is_none(), "{gone}");
    let (_, list) = app.get(c, "/proposals").await;
    assert_eq!(list["proposals"], json!([]));
}

/// `mainland` is no longer there: not listed, its nodes not found.
async fn space_gone(app: &TestApp, c: Caller<'_>) {
    let (status, _) = app
        .post(c, "/graph", json!({ "node": "mainland:order/1042" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, spaces) = app.get(c, "/graph/spaces").await;
    assert!(!spaces.to_string().contains("mainland"), "{spaces}");
}

/// An agent deletes a space only by proposing it: the draft then holds only that, shown for
/// review, and approving deletes the space with its nodes and edges, in memory and in store. The
/// console's admin deletes one directly; nobody else does.
#[tokio::test]
async fn a_space_is_deleted_by_review_or_by_the_admin() {
    let (mut app, key) = world().await;
    let c = Caller::new(&key);
    // A change pending first, then the deletion: the draft keeps only the deletion.
    ok(
        &app,
        c,
        "/graph/draft",
        json!({ "space": "mainland", "reason": "A new customer.",
                "changes": [{ "op": "put_node", "type": "customer", "key": "c2", "name": "王芳" }] }),
    )
    .await;
    let draft = ok(
        &app,
        c,
        "/graph/draft",
        json!({ "space": "mainland", "reason": "The store closed.",
                "changes": [{ "op": "delete_space", "type": null, "key": null }] }),
    )
    .await;
    assert_eq!(draft["delete_space"], true, "{draft}");
    assert_eq!(draft["put_nodes"], json!([]));
    let id = draft["proposal"].as_str().unwrap().to_owned();
    let (status, r) = app
        .post(
            c,
            "/graph/draft",
            json!({ "space": "mainland", "reason": "More.",
                    "changes": [{ "op": "remove_node", "type": "order", "key": "1042" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(r.to_string().contains("deletes the space"), "{r}");
    // Nothing changes before approval.
    assert_eq!(edges_of(&app, c, "mainland:order/1042").await.len(), 2);

    let (status, approved) = app
        .post(
            Caller::new(ADMIN),
            &format!("/proposals/{id}/approve"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    assert_eq!(approved["job"]["result"]["graph"]["space_deleted"], true);
    space_gone(&app, c).await;
    app.restart();
    space_gone(&app, c).await;
    // The name is free again.
    ok(
        &app,
        c,
        "/ingest/graph/space",
        json!({ "space": "mainland", "schema": "commerce" }),
    )
    .await;
    let (_, spaces) = app.get(c, "/graph/spaces").await;
    let mainland = spaces["spaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["space"] == "mainland")
        .unwrap()
        .clone();
    assert_eq!(
        mainland["nodes"],
        json!({}),
        "the old nodes stay gone: {mainland}"
    );

    // Directly: refused to a tenant's caller (forbidden when it sees the space, not found when
    // it doesn't); the admin key isn't a console session, so it has no tenant to look at.
    let (status, _) = app.delete(c, "/graph/spaces/mainland").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = app.delete(c, "/graph/spaces/secret").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let jin = c.user(Some("jin"));
    let (status, _) = app.delete(jin, "/graph/spaces/secret").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn drafts_follow_their_space_and_leave_with_their_author() {
    let (app, key) = world().await;
    let c = Caller::new(&key);
    let (jin, marco, lily) = (
        c.user(Some("jin")),
        c.user(Some("marco")),
        c.user(Some("lily")),
    );
    ok(
        &app,
        jin,
        "/graph/draft",
        json!({ "space": "secret", "reason": "a lead",
                "changes": [{ "op": "put_node", "type": "customer", "key": "h1", "name": "Hidden" }] }),
    )
    .await;
    ok(
        &app,
        marco,
        "/graph/draft",
        json!({ "space": "vault", "reason": "a risk case",
                "changes": [{ "op": "put_node", "type": "customer", "key": "r1", "name": "Risky" }] }),
    )
    .await;
    // Each draft is seen by who may see its space, and no one else.
    let count = |v: &Value| v["proposals"].as_array().unwrap().len();
    let (_, mine) = app.get(jin, "/proposals").await;
    let (_, his) = app.get(marco, "/proposals").await;
    let (_, hers) = app.get(lily, "/proposals").await;
    assert_eq!(
        (count(&mine), count(&his), count(&hers)),
        (1, 1, 0),
        "{mine} {his} {hers}"
    );
    let (status, _) = app.get(lily, "/graph/draft?space=vault").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Jin writes into the shared space's draft too; deleting him takes every draft he wrote
    // into, his words with it, and his private space's.
    ok(
        &app,
        jin,
        "/graph/draft",
        json!({ "space": "mainland", "reason": "JINS-PRIVATE-NOTE",
                "changes": [{ "op": "put_node", "type": "customer", "key": "j9" }] }),
    )
    .await;
    let (status, body) = app.delete(c, "/users/jin").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, left) = app.get(marco, "/proposals").await;
    assert_eq!(count(&left), 1, "only marco's: {left}");
    let (_, shared) = app.get(c, "/graph/draft?space=mainland").await;
    assert!(shared.get("proposal").is_none(), "{shared}");
    let db = std::fs::read(app.tenant_dir("acme").join("pinakes.sqlite")).unwrap();
    assert!(
        !db.windows(b"JINS-PRIVATE-NOTE".len())
            .any(|w| w == b"JINS-PRIVATE-NOTE"),
        "the deleted user's words stay in the store file"
    );
}

/// The same approval against Neo4j: the draft is applied in one Neo4j transaction, and a
/// restarted service reads the changed graph back from it. Skipped without
/// `PINAKES_TEST_NEO4J_URI`.
#[tokio::test]
async fn an_approved_draft_reaches_neo4j() {
    let Ok(uri) = std::env::var("PINAKES_TEST_NEO4J_URI") else {
        println!("PINAKES_TEST_NEO4J_URI is not set; skipping the Neo4j run");
        return;
    };
    let config = format!(
        "[graph]
backend = \"neo4j\"
[neo4j]
uri = {uri:?}
password_env = \"PINAKES_TEST_NEO4J_PASSWORD\"
"
    );
    let tenant = format!("draft-{}", hex::encode(rand::random::<[u8; 4]>()));
    let (mut app, key) = world_with(&config, &tenant).await;
    let c = Caller::new(&key);
    let draft = ok(
        &app,
        c,
        "/graph/draft",
        json!({ "space": "mainland", "reason": "handed over",
                "changes": [
                    { "op": "put_node", "type": "customer", "key": "c2", "name": "王芳",
                      "properties": { "tier": "silver", "since": "2026-10-10" } },
                    { "op": "add_edge", "from": "order/1042", "type": "placed_by", "to": "customer/c2" },
                    { "op": "remove_edge", "from": "order/1042", "type": "placed_by", "to": "customer/c1" },
                    { "op": "remove_node", "type": "product", "key": "p1" }
                ] }),
    )
    .await;
    let id = draft["proposal"].as_str().unwrap();
    let (status, body) = app
        .post(
            Caller::new(ADMIN),
            &format!("/proposals/{id}/approve"),
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    app.restart();
    let c = Caller::new(&key);
    assert_eq!(
        edges_of(&app, c, "mainland:order/1042").await,
        vec!["mainland:order/1042 placed_by mainland:customer/c2"]
    );
    let (status, _) = app
        .post(c, "/graph", json!({ "node": "mainland:product/p1" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Properties come back from Neo4j: the host's and the draft's.
    for (node, want) in [
        (
            "mainland:customer/c1",
            json!({ "tier": "gold", "since": "2024-01-05" }),
        ),
        (
            "mainland:customer/c2",
            json!({ "tier": "silver", "since": "2026-10-10" }),
        ),
    ] {
        let walk = ok(&app, c, "/graph", json!({ "node": node })).await;
        assert_eq!(walk["node"]["properties"], want, "{node}");
    }
    let (status, _) = app
        .delete(Caller::new(ADMIN), &format!("/admin/tenants/{tenant}"))
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}
