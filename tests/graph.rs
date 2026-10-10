//! The graph endpoints over HTTP: `/ingest/graph/schema`, `/space` and `/data`, the relations
//! slot of `/search`, and the `/graph` and `/graph/path` follow-ups, with the visibility rules
//! of graph spaces (design, "Who can see a graph space").

use crate::common::app::{Caller, TestApp};
use axum::http::StatusCode;
use serde_json::{Value, json};

fn schema() -> Value {
    json!({
        "schema": "commerce",
        "summary": "Orders, their buyers and products.",
        "aliases": ["commerce", "订单关系"],
        "nodes": {
            "order": { "key": "order_id", "en": "A customer order", "aliases": ["orders", "订单"] },
            "customer": { "key": "customer_id", "aliases": ["customers", "buyer", "客户"] },
            "product": { "key": "sku", "aliases": ["products", "商品"] }
        },
        "relations": {
            "placed_by": { "from": "order", "to": "customer", "via": "customer_id",
                           "aliases": ["placed", "ordered by", "下单"] },
            "contains": { "from": "order", "to": "product", "aliases": ["bought", "买了"] }
        }
    })
}

fn node(ty: &str, key: &str, name: &str) -> Value {
    json!({ "type": ty, "key": key, "name": name })
}

fn edge(from: &str, ty: &str, to: &str) -> Value {
    json!({ "from": from, "type": ty, "to": to })
}

struct World {
    app: TestApp,
    key: String,
}

const RISK: &[String] = &[];

impl World {
    fn caller(&self) -> Caller<'_> {
        Caller::new(&self.key)
    }

    async fn post(&self, caller: Caller<'_>, uri: &str, body: Value) -> (StatusCode, Value) {
        self.app.post(caller, uri, body).await
    }

    async fn ok(&self, caller: Caller<'_>, uri: &str, body: Value) -> Value {
        let (status, resp) = self.post(caller, uri, body).await;
        assert!(status.is_success(), "{uri}: {status} {resp}");
        resp
    }
}

/// One tenant with the commerce schema and four spaces: `mainland` and `crossborder` (shared),
/// `secret` (private to jin) and `vault` (restricted to marco and the risk group).
async fn world() -> World {
    let app = TestApp::new();
    let key = app.create_tenant("acme").await;
    let w = World { app, key };
    w.ok(w.caller(), "/ingest/graph/schema", schema()).await;
    for space in ["mainland", "crossborder"] {
        w.ok(
            w.caller(),
            "/ingest/graph/space",
            json!({ "space": space, "schema": "commerce", "name": format!("{space} store") }),
        )
        .await;
    }
    let jin = w.caller().user(Some("jin"));
    w.ok(
        jin,
        "/ingest/graph/space",
        json!({ "space": "secret", "schema": "commerce", "name": "Jin's leads", "visibility": "private" }),
    )
    .await;
    let marco = w.caller().user(Some("marco"));
    w.ok(
        marco,
        "/ingest/graph/space",
        json!({ "space": "vault", "schema": "commerce", "visibility": "restricted",
                "members": { "users": ["marco"], "groups": ["risk"] } }),
    )
    .await;

    w.ok(
        w.caller(),
        "/ingest/graph/data",
        json!({ "space": "mainland", "schema": "commerce",
                "nodes": [node("order", "1042", "Order 1042"), node("customer", "c1", "林晓"),
                          node("product", "p1", "AirBeat Pro")],
                "edges": [edge("order/1042", "placed_by", "customer/c1"),
                          edge("order/1042", "contains", "product/p1")] }),
    )
    .await;
    w.ok(
        w.caller(),
        "/ingest/graph/data",
        json!({ "space": "crossborder",
                "nodes": [node("order", "1042", "Cross-border order 1042"), node("customer", "c9", "Olivia Tan")],
                "edges": [edge("order/1042", "placed_by", "customer/c9")] }),
    )
    .await;
    w.ok(
        jin,
        "/ingest/graph/data",
        json!({ "space": "secret",
                "nodes": [node("order", "7", "Order 7"), node("customer", "h1", "Hidden Buyer")],
                "edges": [edge("order/7", "placed_by", "customer/h1")] }),
    )
    .await;
    w.ok(
        marco,
        "/ingest/graph/data",
        json!({ "space": "vault",
                "nodes": [node("order", "9", "Order 9"), node("customer", "v1", "Vault Buyer")],
                "edges": [edge("order/9", "placed_by", "customer/v1")] }),
    )
    .await;
    w
}

async fn ask(w: &World, who: Caller<'_>, query: &str) -> Value {
    w.ok(who, "/search", json!({ "query": query })).await
}

fn edges(resp: &Value) -> Vec<String> {
    resp["relations"]["edges"]
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
        .collect()
}

#[tokio::test]
async fn schema_ingest_writes_its_description_and_only_adds() {
    let w = world().await;
    // The description is a tenant page, with structure from the definition.
    let (status, page) = w.app.get(w.caller(), "/pages/graph/commerce").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["front_matter"]["kind"], "graph");
    assert_eq!(page["front_matter"]["nodes"]["order"]["key"], "order_id");
    assert_eq!(
        page["front_matter"]["relations"]["placed_by"]["via"],
        "customer_id"
    );
    // It is searchable right away, in the knowledge slot.
    let resp = w
        .ok(w.caller(), "/search", json!({ "query": "订单关系" }))
        .await;
    assert_eq!(
        resp["knowledge"]["hits"][0]["page"], "graph/commerce",
        "{resp}"
    );

    let mut changed = schema();
    changed["nodes"]["order"]["key"] = json!("id");
    let (status, body) = w.post(w.caller(), "/ingest/graph/schema", changed).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let mut removed = schema();
    removed["relations"]
        .as_object_mut()
        .unwrap()
        .remove("contains");
    let (status, body) = w.post(w.caller(), "/ingest/graph/schema", removed).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a relation with edges can't go: {body}"
    );

    let mut added = schema();
    added["nodes"]["coupon"] = json!({ "key": "code", "zh": "优惠券" });
    added["relations"]["used_coupon"] = json!({ "from": "order", "to": "coupon" });
    let resp = w.ok(w.caller(), "/ingest/graph/schema", added).await;
    assert_eq!(resp["created"], false);
    let (_, page) = w.app.get(w.caller(), "/pages/graph/commerce").await;
    assert_eq!(page["front_matter"]["nodes"]["coupon"]["zh"], "优惠券");
    assert_eq!(
        page["front_matter"]["nodes"]["order"]["en"],
        "A customer order"
    );

    for (body, want) in [
        (
            json!({ "schema": "Bad-Name", "nodes": { "a": { "key": "k" } } }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "schema": "x", "nodes": {} }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "schema": "mainland", "nodes": { "a": { "key": "k" } } }),
            StatusCode::CONFLICT,
        ),
        (
            json!({ "schema": "x", "nodes": { "a": { "key": "k" } }, "extra": 1 }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, resp) = w
            .post(w.caller(), "/ingest/graph/schema", body.clone())
            .await;
        assert_eq!(status, want, "{body}: {resp}");
    }
}

#[tokio::test]
async fn spaces_and_data_are_checked() {
    let w = world().await;
    let space = |extra: Value| {
        let mut body = json!({ "space": "s2", "schema": "commerce" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    for (caller, body, want) in [
        (
            w.caller(),
            space(json!({ "visibility": "private" })),
            StatusCode::BAD_REQUEST,
        ),
        (
            w.caller(),
            space(json!({ "visibility": "restricted" })),
            StatusCode::BAD_REQUEST,
        ),
        (
            w.caller(),
            space(json!({ "members": { "users": ["a"] } })),
            StatusCode::BAD_REQUEST,
        ),
        (
            w.caller(),
            space(json!({ "schema": "nope" })),
            StatusCode::BAD_REQUEST,
        ),
        (
            w.caller(),
            space(json!({ "space": "commerce" })),
            StatusCode::CONFLICT,
        ),
        (
            w.caller(),
            space(json!({ "space": "mainland" })),
            StatusCode::CONFLICT,
        ),
        // Names are unique in the tenant, hidden spaces included.
        (
            w.caller(),
            space(json!({ "space": "secret" })),
            StatusCode::CONFLICT,
        ),
        (
            w.caller(),
            space(json!({ "owner": "jin" })),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, resp) = w.post(caller, "/ingest/graph/space", body.clone()).await;
        assert_eq!(status, want, "{body}: {resp}");
    }

    let data = |space: &str, nodes: Value, edges: Value| json!({ "space": space, "nodes": nodes, "edges": edges });
    let ana = w.caller().user(Some("ana"));
    for (caller, body, want) in [
        (
            w.caller(),
            data("nowhere", json!([]), json!([])),
            StatusCode::NOT_FOUND,
        ),
        // Another user's private space is reported as missing, not forbidden.
        (
            ana,
            data("secret", json!([node("order", "8", "O")]), json!([])),
            StatusCode::NOT_FOUND,
        ),
        (
            w.caller(),
            data("vault", json!([]), json!([])),
            StatusCode::NOT_FOUND,
        ),
        (
            w.caller(),
            data("mainland", json!([node("coupon", "c", "C")]), json!([])),
            StatusCode::BAD_REQUEST,
        ),
        (
            w.caller(),
            data(
                "mainland",
                json!([]),
                json!([edge("order/1042", "placed_by", "customer/zz")]),
            ),
            StatusCode::BAD_REQUEST,
        ),
        (
            w.caller(),
            data(
                "mainland",
                json!([]),
                json!([edge("customer/c1", "placed_by", "order/1042")]),
            ),
            StatusCode::BAD_REQUEST,
        ),
        (
            w.caller(),
            json!({ "space": "mainland", "schema": "other" }),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, resp) = w.post(caller, "/ingest/graph/data", body.clone()).await;
        assert_eq!(status, want, "{body}: {resp}");
    }
    // A node already stored can be an edge end; writing a node again renames it.
    w.ok(
        w.caller(),
        "/ingest/graph/data",
        data(
            "mainland",
            json!([
                node("customer", "c1", "Lin Xiao"),
                node("order", "2077", "Order 2077")
            ]),
            json!([edge("order/2077", "placed_by", "customer/c1")]),
        ),
    )
    .await;
    let resp = w
        .ok(
            w.caller(),
            "/graph",
            json!({ "node": "mainland:customer/c1" }),
        )
        .await;
    assert_eq!(resp["node"]["name"], "Lin Xiao");
    assert_eq!(resp["edges"].as_array().unwrap().len(), 2, "{resp}");
}

#[tokio::test]
async fn relationship_questions_get_real_edges() {
    let w = world().await;
    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "Which customer placed order 1042?", "keywords": ["order 1042", "placed by"] }),
        )
        .await;
    let got = edges(&resp);
    // No space named: both spaces' order 1042, each edge saying its space.
    assert!(
        got.contains(&"mainland:order/1042 placed_by mainland:customer/c1".to_string()),
        "{resp}"
    );
    assert!(
        got.contains(&"crossborder:order/1042 placed_by crossborder:customer/c9".to_string()),
        "{resp}"
    );
    assert_eq!(resp["relations"]["coverage"], "strong");
    assert_eq!(
        resp["relations"]["relation_types"],
        json!(["commerce:placed_by"])
    );
    let first = &resp["relations"]["edges"][0];
    assert_eq!(first["space"], "mainland");
    assert_eq!(first["via"], "customer_id");
    assert_eq!(first["to_name"], "林晓");
    assert_eq!(resp["relations"]["route"]["graph"], "mainland:order/1042");
    assert!(
        resp["entities"]
            .as_array()
            .unwrap()
            .contains(&json!("mainland:order/1042"))
    );
    assert!(
        resp["unmatched_keywords"].as_array().unwrap().is_empty(),
        "{resp}"
    );

    // A space's name narrows it to that space.
    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "crossborder store 订单1042 是谁下的" }),
        )
        .await;
    let got = edges(&resp);
    assert_eq!(
        got[0], "crossborder:order/1042 placed_by crossborder:customer/c9",
        "{resp}"
    );
    assert!(!got.iter().any(|e| e.starts_with("mainland:")), "{resp}");

    // Nothing named, nothing returned: the slot stays empty rather than padded.
    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "what is the weather in Hangzhou" }),
        )
        .await;
    assert!(edges(&resp).is_empty());
    assert_eq!(resp["relations"]["coverage"], "weak");
}

#[tokio::test]
async fn hidden_spaces_never_appear() {
    let w = world().await;
    // A tenant page anchored (wrongly) into a private space still hides the anchor.
    let wiki = w.app.wiki_dir("acme");
    std::fs::write(
        wiki.join("expertise").join("leads.md"),
        "---\nkind: expertise\ntitle: Lead handling\naliases: [lead handling]\nsummary: How leads are worked.\nabout: [secret:order/7, mainland:order/1042, commerce:order, vault:order/9, ghost:order/1, secret:order]\n---\n\n# Lead handling\n\nWork every lead within a day.\n",
    )
    .unwrap();
    let mut app = w.app;
    app.restart();
    let w = World { app, key: w.key };

    let ana = w.caller().user(Some("ana"));
    for who in [w.caller(), ana] {
        let resp = ask(
            &w,
            who,
            "Which orders did Hidden Buyer place? Vault Buyer too?",
        )
        .await;
        let text = resp.to_string();
        assert!(
            !text.contains("secret:") && !text.contains("vault:"),
            "{resp}"
        );
        assert!(edges(&resp).is_empty(), "{resp}");
        let resp = ask(&w, who, "lead handling").await;
        assert_eq!(
            resp["knowledge"]["hits"][0]["about"],
            json!(["mainland:order/1042", "commerce:order"]),
            "{resp}"
        );
    }
    // The owner sees the private space; members see the restricted one, by user or group.
    let jin = w.caller().user(Some("jin"));
    let resp = ask(&w, jin, "Which orders did Hidden Buyer place?").await;
    assert_eq!(
        edges(&resp),
        vec!["secret:order/7 placed_by secret:customer/h1"],
        "{resp}"
    );
    let resp = ask(&w, jin, "lead handling").await;
    assert!(
        resp["knowledge"]["hits"][0]["about"]
            .as_array()
            .unwrap()
            .contains(&json!("secret:order/7"))
    );
    let risk = vec!["risk".to_string()];
    for who in [
        w.caller().user(Some("marco")),
        w.caller().user(Some("zoe")).groups(&risk),
        w.caller().groups(&risk),
    ] {
        let resp = ask(&w, who, "Which orders did Vault Buyer place?").await;
        assert_eq!(
            edges(&resp),
            vec!["vault:order/9 placed_by vault:customer/v1"],
            "{resp}"
        );
    }

    // Follow-ups: hidden and unknown nodes are both 404; malformed ids are 400.
    for (who, node, want) in [
        (ana, "secret:order/7", StatusCode::NOT_FOUND),
        (w.caller(), "secret:order/7", StatusCode::NOT_FOUND),
        (
            w.caller().groups(RISK),
            "vault:order/9",
            StatusCode::NOT_FOUND,
        ),
        (w.caller(), "mainland:order/9999", StatusCode::NOT_FOUND),
        (w.caller(), "nowhere:order/1", StatusCode::NOT_FOUND),
        (w.caller(), "commerce:order", StatusCode::BAD_REQUEST),
        (w.caller(), "expertise/leads", StatusCode::BAD_REQUEST),
        (jin, "secret:order/7", StatusCode::OK),
        (w.caller().groups(&risk), "vault:order/9", StatusCode::OK),
    ] {
        let (status, resp) = w.post(who, "/graph", json!({ "node": node })).await;
        assert_eq!(status, want, "{node}: {resp}");
    }
    let (status, _) = w
        .post(
            ana,
            "/graph/path",
            json!({ "from": "mainland:order/1042", "to": "secret:customer/h1" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn walks_and_paths() {
    let w = world().await;
    let wiki = w.app.wiki_dir("acme");
    std::fs::write(
        wiki.join("experience").join("vip-orders.md"),
        "---\nkind: experience\ntitle: VIP orders\nsummary: Handle order 1042 with care.\nabout: [mainland:order/1042]\n---\n\n# VIP orders\n",
    )
    .unwrap();
    let mut app = w.app;
    app.restart();
    let w = World { app, key: w.key };

    let resp = w
        .ok(
            w.caller(),
            "/graph",
            json!({ "node": "mainland:order/1042" }),
        )
        .await;
    let mut names: Vec<&str> = resp["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap())
        .collect();
    // The start first, then its neighbours.
    names[1..].sort();
    assert_eq!(names, vec!["Order 1042", "AirBeat Pro", "林晓"], "{resp}");
    assert_eq!(resp["edges"].as_array().unwrap().len(), 2);
    assert_eq!(resp["truncated"], false);
    // Pages anchored to the node, and to its type (the schema description isn't anchored).
    assert_eq!(resp["pages"][0]["page"], "experience/vip-orders", "{resp}");
    assert_eq!(resp["pages"][0]["anchored_to"], "mainland:order/1042");

    let resp = w
        .ok(
            w.caller(),
            "/graph",
            json!({ "node": "mainland:order/1042", "relations": ["commerce:placed_by"] }),
        )
        .await;
    assert_eq!(resp["nodes"].as_array().unwrap().len(), 2);
    let resp = w
        .ok(
            w.caller(),
            "/graph",
            json!({ "node": "mainland:customer/c1", "direction": "out" }),
        )
        .await;
    assert_eq!(
        resp["nodes"].as_array().unwrap().len(),
        1,
        "no outgoing edges"
    );
    let resp = w
        .ok(
            w.caller(),
            "/graph",
            json!({ "node": "mainland:customer/c1", "depth": 2, "max_nodes": 2 }),
        )
        .await;
    assert_eq!(resp["truncated"], true);
    for body in [
        json!({ "node": "mainland:order/1042", "depth": 4 }),
        json!({ "node": "mainland:order/1042", "depth": 0 }),
        json!({ "node": "mainland:order/1042", "max_nodes": 1000 }),
        json!({ "node": "mainland:order/1042", "relations": ["ships"] }),
        json!({ "node": "mainland:order/1042", "direction": "sideways" }),
    ] {
        let (status, resp) = w.post(w.caller(), "/graph", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {resp}");
    }

    let resp = w
        .ok(
            w.caller(),
            "/graph/path",
            json!({ "from": "mainland:customer/c1", "to": "mainland:product/p1" }),
        )
        .await;
    assert_eq!(resp["found"], true);
    let hops: Vec<&str> = resp["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        hops,
        vec![
            "mainland:customer/c1",
            "mainland:order/1042",
            "mainland:product/p1"
        ]
    );
    assert_eq!(resp["edges"][0]["type"], "placed_by");
    // Edges never cross spaces.
    let resp = w
        .ok(
            w.caller(),
            "/graph/path",
            json!({ "from": "mainland:order/1042", "to": "crossborder:order/1042" }),
        )
        .await;
    assert_eq!(resp["found"], false);
    let resp = w
        .ok(
            w.caller(),
            "/graph/path",
            json!({ "from": "mainland:customer/c1", "to": "mainland:product/p1", "relations": ["placed_by"] }),
        )
        .await;
    assert_eq!(resp["found"], false, "contains is filtered out");
}

#[tokio::test]
async fn other_tenants_graphs_are_invisible_and_graphs_survive_restarts() {
    let w = world().await;
    let other = w.app.create_tenant("globex").await;
    let them = Caller::new(&other);
    let (status, _) = w
        .post(them, "/graph", json!({ "node": "mainland:order/1042" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let resp = w
        .ok(
            them,
            "/search",
            json!({ "query": "Which customer placed order 1042? 林晓" }),
        )
        .await;
    assert!(edges(&resp).is_empty(), "{resp}");
    assert!(resp["entities"].as_array().unwrap().is_empty(), "{resp}");
    let (status, _) = w
        .post(them, "/ingest/graph/data", json!({ "space": "mainland" }))
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The graph is read back from the store after a restart.
    let mut app = w.app;
    app.restart();
    let w = World { app, key: w.key };
    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "林晓下过哪些订单？" }),
        )
        .await;
    assert_eq!(
        edges(&resp)[0],
        "mainland:order/1042 placed_by mainland:customer/c1",
        "{resp}"
    );
}

/// A keyword that is only part of a node's name still names the node, and the trace line says
/// so, for lint to ask the host for the missing alias. The response itself doesn't carry it.
#[tokio::test]
async fn partial_names_are_traced() {
    let w = world().await;
    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "who bought the AirBeat", "keywords": ["AirBeat", "bought"] }),
        )
        .await;
    assert_eq!(
        edges(&resp),
        vec!["mainland:order/1042 contains mainland:product/p1"],
        "{resp}"
    );
    assert!(resp.get("partial_names").is_none());
    w.app.state.traces.flush();
    let files =
        pinakes::trace::replay::trace_files(&[w.app.config().tenants_dir()], Some(7)).unwrap();
    let last = pinakes::trace::replay::TraceReader::new(files)
        .last()
        .unwrap();
    assert_eq!(
        last.result["partial_names"],
        json!([{ "keyword": "airbeat", "nodes": ["mainland:product/p1"] }])
    );

    // The same keyword as the whole query is still matched as part of a name.
    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "AirBeat", "keywords": ["AirBeat"] }),
        )
        .await;
    assert_eq!(resp["entities"], json!(["mainland:product/p1"]), "{resp}");
}

/// An id the agent passes is looked up exactly: it never also names the same key in another
/// space, and a space named in the query never drops it.
#[tokio::test]
async fn entity_ids_are_exact() {
    let w = world().await;
    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "who placed it", "entities": ["mainland:order/1042"] }),
        )
        .await;
    assert_eq!(resp["entities"][0], "mainland:order/1042", "{resp}");
    assert!(!resp.to_string().contains("crossborder:"), "{resp}");
    assert!(
        resp["unmatched_keywords"].as_array().unwrap().is_empty(),
        "{resp}"
    );

    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "crossborder store order 1042 and mainland:customer/c1" }),
        )
        .await;
    let entities = resp["entities"].to_string();
    assert!(entities.contains("crossborder:order/1042"), "{resp}");
    assert!(
        entities.contains("mainland:customer/c1"),
        "the id survives: {resp}"
    );
    assert!(
        !entities.contains("mainland:order/1042"),
        "narrowed away: {resp}"
    );

    // Punctuation around an id doesn't hide it; a word shaped like `schema:type` that names no
    // type stays text.
    for query in [
        "who placed mainland:order/1042.",
        "谁下的（mainland:order/1042）？",
        "mainland:order/1042的下单客户是谁",
    ] {
        let resp = w.ok(w.caller(), "/search", json!({ "query": query })).await;
        assert_eq!(
            resp["entities"][0], "mainland:order/1042",
            "{query}: {resp}"
        );
    }
    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "x", "keywords": ["commerce:order", "commerce:nonexistent"] }),
        )
        .await;
    assert_eq!(
        resp["unmatched_keywords"],
        json!(["commerce:nonexistent"]),
        "{resp}"
    );
    assert!(resp["entities"].to_string().contains("commerce:order"));

    // A hidden id behaves like an unknown one.
    let hidden = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "x", "entities": ["secret:order/7"] }),
        )
        .await;
    let unknown = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "x", "entities": ["nowhere:order/7"] }),
        )
        .await;
    assert_eq!(hidden["entities"], unknown["entities"]);
    assert_eq!(hidden["relations"], unknown["relations"]);
    assert_eq!(
        hidden["unmatched_keywords"],
        json!(["secret:order/7"]),
        "{hidden}"
    );
}

/// Whoever creates a restricted space sees it, member or not.
#[tokio::test]
async fn restricted_space_creators_see_it() {
    let w = world().await;
    let kim = w.caller().user(Some("kim"));
    w.ok(
        kim,
        "/ingest/graph/space",
        json!({ "space": "ops", "schema": "commerce", "visibility": "restricted",
                "members": { "groups": ["ops-team"] } }),
    )
    .await;
    w.ok(
        kim,
        "/ingest/graph/data",
        json!({ "space": "ops", "nodes": [node("order", "5", "Ops order")] }),
    )
    .await;
    let (status, _) = w
        .post(kim, "/graph", json!({ "node": "ops:order/5" }))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = w
        .post(
            w.caller().user(Some("ana")),
            "/graph",
            json!({ "node": "ops:order/5" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Browsing a page hides its anchors into spaces the caller can't see, like search does.
#[tokio::test]
async fn page_anchors_are_filtered_when_browsing() {
    let w = world().await;
    std::fs::write(
        w.app.wiki_dir("acme").join("expertise").join("leads.md"),
        "---\nkind: expertise\ntitle: Leads\nsummary: s\nabout: [secret:order/7, mainland:order/1042, commerce:order, commerce:nonexistent, legacy:ticket]\n---\n\nbody\n",
    )
    .unwrap();
    let (status, page) = w
        .app
        .get(w.caller().user(Some("ana")), "/pages/expertise/leads")
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        page["front_matter"]["about"],
        json!(["mainland:order/1042", "commerce:order"])
    );
    let (_, page) = w
        .app
        .get(w.caller().user(Some("jin")), "/pages/expertise/leads")
        .await;
    assert_eq!(page["front_matter"]["about"].as_array().unwrap().len(), 3);
}

/// Without a reachable graph store, searches still answer from the wiki, new tenants are not
/// created (old graph data under their id can't be cleared), and startup fails at once when
/// the Neo4j password is missing.
#[tokio::test]
async fn an_unreachable_graph_store() {
    let mut w = world().await;
    // A tenant whose graph data is in a Neo4j that can't be reached (built on SQLite here, so
    // its record says where its data would be).
    rusqlite::Connection::open(w.app.tenant_dir("acme").join("pinakes.sqlite"))
        .unwrap()
        .execute(
            "UPDATE meta SET value = 'neo4j' WHERE key = 'graph_backend'",
            [],
        )
        .unwrap();
    let broken = "[graph]\nbackend = \"neo4j\"\n[neo4j]\nuri = \"bolt://127.0.0.1:1\"\npassword_env = \"PINAKES_TEST_NO_SUCH_PASSWORD\"\n";
    w.app.restart_with_config(broken);
    let started = std::time::Instant::now();
    assert!(w.app.state.prepare().await.is_err());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "no retries"
    );

    let resp = w
        .ok(w.caller(), "/search", json!({ "query": "订单关系" }))
        .await;
    assert_eq!(
        resp["knowledge"]["hits"][0]["page"], "graph/commerce",
        "{resp}"
    );
    assert!(edges(&resp).is_empty());

    let (status, _) = w
        .app
        .post_admin("/admin/tenants", json!({ "tenant_id": "globex" }))
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let dirs = std::fs::read_dir(w.app.config().tenants_dir())
        .unwrap()
        .count();
    assert_eq!(dirs, 1, "only acme's folder");
}

/// The response conventions (design, "Response formats"): pages are `page`, lists are always
/// present, absent scalars are left out, and counts don't reuse the names of lists.
#[tokio::test]
async fn response_formats_follow_the_conventions() {
    let w = world().await;
    let (_, created) = w
        .post(
            w.caller(),
            "/ingest/graph/space",
            json!({ "space": "outlet", "schema": "commerce", "name": "Outlet store" }),
        )
        .await;
    assert_eq!(
        created,
        json!({ "space": "outlet", "schema": "commerce", "name": "Outlet store", "visibility": "shared" })
    );
    let written = w
        .ok(
            w.caller(),
            "/ingest/graph/data",
            json!({ "space": "outlet", "nodes": [{ "type": "order", "key": "1" }] }),
        )
        .await;
    assert_eq!(
        written,
        json!({ "space": "outlet", "nodes_written": 1, "edges_written": 0 })
    );

    // A node without aliases still has the list.
    let walk = w
        .ok(w.caller(), "/graph", json!({ "node": "outlet:order/1" }))
        .await;
    assert_eq!(walk["node"]["aliases"], json!([]));
    assert_eq!(walk["node"]["name"], "1", "the name defaults to the key");

    // Edges found on the way to a named type say `node_type`.
    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "林晓 products", "keywords": ["林晓", "products"] }),
        )
        .await;
    let matches: Vec<&str> = resp["relations"]["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["match"].as_str().unwrap())
        .collect();
    assert!(matches.contains(&"node_type"), "{resp}");

    // Browsing lists pages as `page`, like search hits.
    let (_, list) = w.app.get(w.caller(), "/pages?kind=graph").await;
    assert_eq!(list["pages"][0]["page"], "graph/commerce");
    let (_, page) = w.app.get(w.caller(), "/pages/graph/commerce").await;
    assert_eq!(page["page"], "graph/commerce");

    // Traces keep edges as objects and leave out an absent intent.
    w.app.state.traces.flush();
    let files =
        pinakes::trace::replay::trace_files(&[w.app.config().tenants_dir()], Some(7)).unwrap();
    let last = pinakes::trace::replay::TraceReader::new(files)
        .last()
        .unwrap();
    assert!(
        last.result["relations"]["edges"][0].is_object(),
        "{}",
        last.result
    );
    assert!(last.result.get("intent").is_none());
}

/// A node sent without a name is named by its key only through the key rules: a bare number
/// needs its type next to it, and a plain word is never a name. Ids in the query are looked up
/// whole: their pieces are neither keywords nor unmatched, even with Chinese right after them.
#[tokio::test]
async fn keys_and_ids_in_text() {
    let w = world().await;
    w.ok(
        w.caller(),
        "/ingest/graph/data",
        json!({ "space": "mainland", "nodes": [
            { "type": "order", "key": "5150" },
            { "type": "customer", "key": "support" }
        ] }),
    )
    .await;
    for (query, named) in [
        ("what happened in 5150", false),
        ("support hours please", false),
        ("status of order 5150", true),
    ] {
        let resp = w.ok(w.caller(), "/search", json!({ "query": query })).await;
        let hit = resp["entities"].to_string().contains("mainland:order/5150")
            || resp["entities"].to_string().contains("customer/support");
        assert_eq!(hit, named, "{query}: {resp}");
    }

    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "who placed mainland:order/1042" }),
        )
        .await;
    let unmatched = resp["unmatched_keywords"].to_string();
    assert!(
        !unmatched.contains("mainland") && !unmatched.contains("1042"),
        "{resp}"
    );

    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "x", "keywords": ["mainland:order/1042的客户"] }),
        )
        .await;
    assert_eq!(resp["entities"][0], "mainland:order/1042", "{resp}");
    assert_eq!(resp["unmatched_keywords"], json!([]), "{resp}");
}

/// A full batch is accepted even though it is over axum's 2 MB default body limit; keys with
/// whitespace at either end are refused (ids are looked up trimmed); a keyword holding a found
/// id among other words matched something.
#[tokio::test]
async fn batches_keys_and_mixed_keywords() {
    let w = world().await;
    let nodes: Vec<_> = (0..9_000)
        .map(|i| json!({ "type": "product", "key": format!("bulk-{i}"), "name": format!("{i} {}", "x".repeat(150)),
                    "aliases": [format!("{i} {}", "y".repeat(100)), format!("{i} {}", "z".repeat(100))] }))
        .collect();
    let body = json!({ "space": "mainland", "nodes": nodes });
    assert!(body.to_string().len() > 2 * 1024 * 1024);
    let resp = w.ok(w.caller(), "/ingest/graph/data", body).await;
    assert_eq!(resp["nodes_written"], 9_000);

    let (status, resp) = w
        .post(
            w.caller(),
            "/ingest/graph/data",
            json!({ "space": "mainland", "nodes": [{ "type": "order", "key": " 77" }] }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");

    let resp = w
        .ok(
            w.caller(),
            "/search",
            json!({ "query": "x", "keywords": ["who placed mainland:order/1042"] }),
        )
        .await;
    assert_eq!(resp["entities"][0], "mainland:order/1042", "{resp}");
    assert_eq!(resp["unmatched_keywords"], json!([]), "{resp}");
}
