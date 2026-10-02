//! M4 "done when": new content updates the related pages instead of duplicating them, every
//! memory is anchored, and unsupported edits are held for review.
//!
//! The librarian's LLM is scripted here ([`common::llm::FakeLlm`]), so these tests check
//! everything around the model: which related pages it is shown, which anchors it may use,
//! the code checks and merge rules its edits must pass, the retry with reasons, proposals and
//! their approval, raw sources, the write queue, `index.md` and `log.md`. A real model's
//! judgement is the subject of the live test in `tests/librarian_live.rs`.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::llm::{
    FakeLlm, NO_EDITS, OK, Step, UNREACHABLE, all_ok, all_rejected, create,
    describe_from_definition, folder, related_pages, update,
};
use common::testset;
use pinakes::tenants::{group_dir_name, user_dir_name};
use serde_json::{Value, json};

/// The `shopline` scenario with its wiki and graph, behind a librarian that calls `llm`.
async fn shopline(llm: &Arc<FakeLlm>) -> (TestApp, String) {
    let mut app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("shopline").await;
    testset::install_wiki("shopline", &app.wiki_dir("shopline"));
    app.restart();
    let t = testset::tenants()
        .into_iter()
        .find(|t| t.id == "shopline")
        .unwrap();
    for call in testset::graph_calls(&t) {
        let caller = Caller::new(&key)
            .user(call.user.as_deref())
            .groups(&call.groups);
        let (status, body) = app.post(caller, call.uri, call.body).await;
        assert!(status.is_success(), "{}: {status} {body}", call.uri);
    }
    (app, key)
}

/// A scripted model that keeps a description as code wrote it and accepts what it is asked to
/// verify, with `plan` deciding the edits.
fn model(plan: impl Fn(&str, usize) -> Value + Send + Sync + 'static) -> Arc<FakeLlm> {
    FakeLlm::new(move |step, msg, n| match step {
        Step::Plan => plan(msg, n).to_string(),
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => common::llm::NOT_THE_SAME.into(),
        Step::Read => common::llm::skip_to_end(msg),
    })
}

async fn submit(app: &TestApp, caller: Caller<'_>, uri: &str, body: Value) -> Value {
    let (status, resp) = app.post(caller, uri, body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{uri}: {resp}");
    assert_eq!(resp["status"], "queued", "{resp}");
    let job = app.wait_job(caller, resp["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    job
}

fn page_count(pages: &Value, prefix: &str) -> usize {
    pages["pages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["page"].as_str().unwrap().starts_with(prefix))
        .count()
}

/// The librarian is shown the existing playbook, updates it, and no second page appears. The
/// raw record is kept and readable through the page that cites it; the index and log record
/// the job; search finds the new text.
#[tokio::test]
async fn new_content_updates_the_related_playbook() {
    let llm = model(|msg, _| {
        let related = related_pages(msg);
        match related.iter().find(|p| p.starts_with("experience/")) {
            Some(page) => {
                let mut e = update(
                    page,
                    Some("Tracking numbers"),
                    Some(
                        "- Finance also needs the shipment's tracking number on the ledger entry. Seen 1 time.",
                    ),
                );
                e["add_keywords"] = json!(["tracking number", "运单号"]);
                e["add_about"] = json!(["mainland:order/1042"]);
                json!({ "edits": [e] })
            }
            None => json!({ "edits": [] }),
        }
    });
    let (app, key) = shopline(&llm).await;
    let wang = Caller::new(&key).user(Some("wang"));
    let (_, before) = app.get(wang, "/pages?kind=experience").await;

    let job = submit(
        &app,
        wang,
        "/ingest/experience",
        json!({
            "task": "partial refund after shipment",
            "outcome": "The buyer asked for a 部分退款 on order 1042 after it shipped. We added the manual ledger entry, and finance asked for the tracking number too.",
            "entities": ["mainland:order/1042"],
            "date": "2026-10-01"
        }),
    )
    .await;

    // The playbook was among the pages the librarian saw, first of its kind.
    let plan = &llm.calls(Step::Plan)[0];
    let related = related_pages(plan);
    assert_eq!(
        related.first().map(String::as_str),
        Some("experience/partial-refund-after-shipment"),
        "{related:?}"
    );
    assert!(
        plan.contains("mainland:order/1042"),
        "the named node is an allowed anchor"
    );
    assert_eq!(folder(plan), "experience");

    let result = &job["result"];
    assert_eq!(
        result["pages_updated"],
        json!(["experience/partial-refund-after-shipment"])
    );
    assert_eq!(result["pages_created"], json!([]));
    assert_eq!(result["rejected"], json!([]));
    let source = result["source"].as_str().unwrap();
    assert!(
        source.starts_with("sources/") && source.contains("-experience-"),
        "{source}"
    );

    let (_, after) = app.get(wang, "/pages?kind=experience").await;
    assert_eq!(
        page_count(&after, ""),
        page_count(&before, ""),
        "no duplicate"
    );
    let (_, page) = app
        .get(wang, "/pages/experience/partial-refund-after-shipment")
        .await;
    let f = &page["front_matter"];
    assert!(
        page["body"]
            .as_str()
            .unwrap()
            .contains("## Tracking numbers")
    );
    assert!(
        page["body"].as_str().unwrap().contains("## Pitfalls"),
        "other sections stay"
    );
    let name = source.strip_prefix("sources/").unwrap();
    assert!(f["sources"].as_array().unwrap().iter().any(|s| s == name));
    assert_eq!(
        f["sources"][0], "2026-09-02-task-4402.json",
        "earlier sources stay"
    );
    assert!(
        f["about"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "mainland:order/1042")
    );
    assert_eq!(f["updated"].as_str().unwrap().len(), 10);

    // Searchable at once, through the re-indexed wiki.
    let (_, resp) = app
        .post(
            wang,
            "/search",
            json!({ "query": "tracking number for a partial refund ledger entry", "keywords": ["运单号"] }),
        )
        .await;
    assert_eq!(
        resp["memories"]["hits"][0]["page"], "experience/partial-refund-after-shipment",
        "{resp}"
    );
    // Hits list sources as `/read` takes them.
    let cited = resp["memories"]["hits"][0]["sources"].as_array().unwrap();
    assert!(cited.iter().any(|s| s == source), "{resp}");

    // The raw record, unchanged, for anyone who can see a page citing it.
    let lily = Caller::new(&key).user(Some("lily"));
    let (_, read) = app.post(lily, "/read", json!({ "ids": [source] })).await;
    let text = read["pages"][0]["text"].as_str().unwrap();
    let rec: Value = serde_json::from_str(text).unwrap();
    assert_eq!(rec["entities"], json!(["mainland:order/1042"]));

    let (_, index) = app.get(wang, "/pages/index").await;
    assert!(
        index["body"]
            .as_str()
            .unwrap()
            .contains("[[experience/partial-refund-after-shipment]]")
    );
    let (_, log) = app.get(wang, "/pages/log").await;
    let log = log["body"].as_str().unwrap();
    assert!(
        log.contains(
            "ingest experience: created []; updated [experience/partial-refund-after-shipment]"
        ),
        "{log}"
    );
}

/// A memory page without an anchor, or with one the page may not use, is refused by code and
/// sent back once with the reasons; the second plan is filed.
#[tokio::test]
async fn every_memory_is_anchored() {
    let llm = model(|msg, n| {
        let page = format!("{}/coupon-refund-on-cancel", folder(msg));
        let about: &[&str] = match n {
            0 => &[],
            _ => {
                assert!(msg.contains("Why the last plan was rejected"), "{msg}");
                assert!(msg.contains("needs at least one anchor"), "{msg}");
                assert!(msg.contains("no such node"), "{msg}");
                &["commerce:order"]
            }
        };
        let mut new = create(
            &page,
            "Refunding coupons when an order is cancelled",
            "Coupons come back only when the whole order is cancelled.",
            about,
            "# Refunding coupons\n\n## Steps that work\n\n1. Cancel the whole order.",
        );
        new["aliases"] = json!(["coupon refund", "退券"]);
        let mut anchor = update("experience/coupon-stacking-errors", None, None);
        anchor["add_about"] = json!(["mainland:order/999999"]);
        json!({ "edits": [new, anchor] })
    });
    let (app, key) = shopline(&llm).await;
    let c = Caller::new(&key);
    let job = submit(
        &app,
        c,
        "/ingest/experience",
        json!({ "task": "coupon refund on cancel", "outcome": "Coupons returned only on full cancellation." }),
    )
    .await;
    assert_eq!(llm.calls(Step::Plan).len(), 2);
    let r = &job["result"];
    assert_eq!(
        r["pages_created"],
        json!(["experience/coupon-refund-on-cancel"])
    );
    // The bad anchor on the other page was refused twice, so that edit is dropped.
    assert_eq!(
        r["rejected"][0]["page"],
        "experience/coupon-stacking-errors"
    );
    assert!(
        r["rejected"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("no such node")
    );
    let (_, page) = app
        .get(c, "/pages/experience/coupon-refund-on-cancel")
        .await;
    assert_eq!(page["front_matter"]["about"], json!(["commerce:order"]));
    assert_eq!(page["front_matter"]["kind"], "experience");
}

/// Edits the verifier rejects twice are held as a proposal: nothing is written, the caller
/// can see what is held and why, and the admin key applies it.
#[tokio::test]
async fn unsupported_edits_are_held_for_review() {
    let llm = FakeLlm::new(|step, msg, _| match step {
        Step::Plan => json!({ "edits": [create(
            &format!("{}/free-shipping-threshold", folder(msg)),
            "Free shipping threshold",
            "Free shipping from 99 yuan.",
            &["commerce:order"],
            "# Free shipping threshold\n\n## 2026-10-01: free shipping from 99 yuan\n\nDecided.",
        )]})
        .to_string(),
        Step::Verify => all_rejected(msg, "the record does not say the threshold is 99 yuan"),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => common::llm::NOT_THE_SAME.into(),
        Step::Read => common::llm::skip_to_end(msg),
    });
    let (app, key) = shopline(&llm).await;
    let lily = Caller::new(&key).user(Some("lily"));
    let wang = Caller::new(&key).user(Some("wang"));
    let job = submit(
        &app,
        lily,
        "/ingest/decision",
        json!({ "topic": "free shipping threshold", "decision": "Offer free shipping on big orders",
                "entities": ["commerce:order"] }),
    )
    .await;
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 2, "sent back once");
    assert!(plans[1].contains("the record does not say the threshold is 99 yuan"));
    let r = &job["result"];
    assert_eq!(r["pages_created"], json!([]));
    let proposal = r["proposal"].as_str().unwrap().to_owned();
    let page = format!(
        "users/{}/decisions/free-shipping-threshold",
        user_dir_name("lily")
    );
    let (status, _) = app.get(lily, &format!("/pages/{page}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nothing written");

    // Only lily sees her held decision.
    let (_, list) = app.get(lily, "/proposals").await;
    let held = &list["proposals"][0];
    assert_eq!(held["proposal"], proposal.as_str());
    assert_eq!(held["status"], "held");
    assert_eq!(held["kind"], "decision");
    assert_eq!(held["edits"][0]["edit"]["page"], page.as_str());
    assert!(
        held["edits"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("99 yuan")
    );
    assert!(
        held.get("user").is_none(),
        "the writer's folders stay inside"
    );
    for other in [wang, Caller::new(&key)] {
        let (_, list) = app.get(other, "/proposals").await;
        assert_eq!(list["proposals"], json!([]));
    }
    // Nor can anyone else read the source: no page they can see cites it.
    let source = r["source"].as_str().unwrap();
    let (_, read) = app.post(lily, "/read", json!({ "ids": [source] })).await;
    assert_eq!(read["pages"][0]["error"], "not_found", "not cited yet");

    // Approving needs the admin key.
    let uri = format!("/proposals/{proposal}/approve");
    let (status, _) = app.post(lily, &uri, json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = app
        .post_admin(
            "/proposals/p_0011223344556677_0011223344556677/approve",
            json!({}),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Two approvals at once: one applies it, the other is told it already was.
    let (a, b) = tokio::join!(
        app.post_admin(&uri, json!({})),
        app.post_admin(&uri, json!({}))
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "{a:?} {b:?}"
    );
    let body = if a.0 == StatusCode::OK { a.1 } else { b.1 };
    assert_eq!(body["status"], "approved");
    assert_eq!(
        body["job"]["result"]["pages_created"],
        json!([page.as_str()])
    );
    let (status, body) = app.post_admin(&uri, json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (status, got) = app.get(lily, &format!("/pages/{page}")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got["front_matter"]["kind"], "decision");
    let (_, list) = app.get(lily, "/proposals").await;
    assert_eq!(list["proposals"], json!([]));
    let (_, read) = app.post(lily, "/read", json!({ "ids": [source] })).await;
    assert!(
        read["pages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("free shipping")
    );
    let (_, read) = app.post(wang, "/read", json!({ "ids": [source] })).await;
    assert_eq!(
        read["pages"][0]["error"], "not_found",
        "lily's source stays hers"
    );
    // The tenant-wide log doesn't name her page.
    let (_, log) = app.get(wang, "/pages/log").await;
    assert!(!log["body"].as_str().unwrap().contains("free-shipping"));
}

/// The audience comes from the credentials: a group the caller is in, or the caller's own
/// user; never a group it isn't in. Jobs are visible only to their caller.
#[tokio::test]
async fn audiences_follow_credentials() {
    let llm = model(|msg, _| {
        json!({ "edits": [create(
            &format!("{}/bin-attacks", folder(msg)),
            "Card testing from one BIN",
            "Block the BIN, then review.",
            &["payments:merchant"],
            "# Card testing\n\n## Steps that work\n\n1. Block the BIN.",
        )]})
    });
    let mut app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("paybridge").await;
    testset::install_wiki("paybridge", &app.wiki_dir("paybridge"));
    app.restart();
    let t = testset::tenants()
        .into_iter()
        .find(|t| t.id == "paybridge")
        .unwrap();
    for call in testset::graph_calls(&t) {
        let caller = Caller::new(&key)
            .user(call.user.as_deref())
            .groups(&call.groups);
        let (status, body) = app.post(caller, call.uri, call.body).await;
        assert!(status.is_success(), "{}: {status} {body}", call.uri);
    }
    let risk = vec!["risk-team".to_owned()];
    let marco = Caller::new(&key).user(Some("marco")).groups(&risk);
    let outsider = Caller::new(&key).user(Some("dana"));
    // The audience is chosen in a header, like the caller's identity, never in the body.
    let for_risk = marco.audience("group:risk-team");

    let body = json!({ "task": "card testing", "outcome": "Blocked the BIN." });
    for (caller, uri, body) in [
        (
            outsider.audience("group:risk-team"),
            "/ingest/experience",
            body.clone(),
        ),
        (
            marco,
            "/ingest/experience",
            json!({ "task": "x", "outcome": "y", "group": "risk-team" }),
        ),
        (
            marco.audience("risk-team"),
            "/ingest/experience",
            body.clone(),
        ),
        (marco.audience("user"), "/ingest/experience", body.clone()),
        (
            Caller::new(&key),
            "/ingest/decision",
            json!({ "topic": "x", "decision": "y" }),
        ),
        (for_risk, "/ingest/preference", json!({ "statement": "x" })),
        (
            marco,
            "/ingest/experience",
            json!({ "task": "", "outcome": "y" }),
        ),
        (
            marco,
            "/ingest/experience",
            json!({ "task": "x", "outcome": "y", "date": "yesterday" }),
        ),
    ] {
        let (status, resp) = app.post(caller, uri, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {resp}");
    }

    let (status, queued) = app.post(for_risk, "/ingest/experience", body).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = queued["job"].as_str().unwrap();
    let job = app.wait_job(marco, id).await;
    let page = format!(
        "groups/{}/experience/bin-attacks",
        group_dir_name("risk-team")
    );
    assert_eq!(
        job["result"]["pages_created"],
        json!([page.as_str()]),
        "{job}"
    );
    assert!(llm.calls(Step::Plan)[0].contains(&format!(
        "Pages of this job are in `groups/{}/experience/`",
        group_dir_name("risk-team")
    )));

    let (status, _) = app.get(outsider, &format!("/jobs/{id}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another user's job");
    let (status, _) = app.get(outsider, &format!("/pages/{page}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = app.get(marco, &format!("/pages/{page}")).await;
    assert_eq!(status, StatusCode::OK);
    let (_, log) = app.get(outsider, "/pages/log").await;
    let log = log["body"].as_str().unwrap();
    assert!(log.contains("1 created, 0 updated (group pages)"), "{log}");
    assert!(!log.contains("bin-attacks"));
    let (_, index) = app.get(outsider, "/pages/index").await;
    assert!(!index["body"].as_str().unwrap().contains("bin-attacks"));
}

/// Without a librarian every write that needs one answers 503, graph schemas included: a
/// schema can't exist without its description.
#[tokio::test]
async fn no_librarian_means_503() {
    let app = TestApp::without_llm();
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key).user(Some("ann"));
    for (uri, body) in [
        ("/ingest/experience", json!({ "task": "x", "outcome": "y" })),
        ("/ingest/decision", json!({ "topic": "x", "decision": "y" })),
        ("/ingest/expertise", json!({ "title": "x", "content": "y" })),
        ("/ingest/preference", json!({ "statement": "x" })),
        (
            "/ingest/graph/schema",
            json!({ "schema": "work", "nodes": { "ticket": { "key": "no" } } }),
        ),
    ] {
        let (status, resp) = app.post(c, uri, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}: {resp}");
        assert_eq!(resp["error"]["code"], "unavailable");
    }
    let (status, resp) = app
        .put(
            c,
            "/pages/expertise/x",
            json!({ "front_matter": { "title": "X", "summary": "S." }, "body": "# X\n" }),
        )
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{resp}");
    // The key can be given after startup.
    app.state.set_llm(common::llm::describer().llm());
    let (status, resp) = app
        .post(
            c,
            "/ingest/graph/schema",
            json!({ "schema": "work", "nodes": { "ticket": { "key": "no" } } }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
}

/// The librarian writes what a schema's types mean; code keeps the structure from the stored
/// definition. There is no fallback: when the model can't be reached or the verifier rejects
/// the description twice, the schema is not created, and a change leaves it as it was.
#[tokio::test]
async fn schema_descriptions_come_from_the_librarian() {
    let llm = FakeLlm::new(|step, msg, _| match step {
        Step::Describe if msg.contains("\"schema\": \"down\"") => UNREACHABLE.into(),
        Step::Describe if msg.contains("\"coupon\"") => UNREACHABLE.into(),
        Step::Describe => {
            let names: Vec<String> = common::llm::definition(msg)["nodes"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            let nodes: serde_json::Map<String, Value> = names
                .iter()
                .map(|n| {
                    (
                        n.clone(),
                        json!({ "en": "A support ticket", "zh": "工单", "aliases": ["tickets"] }),
                    )
                })
                .collect();
            json!({
                "title": "Work graph", "summary": "Tickets and who works on them.",
                "aliases": ["work", "工单"],
                "nodes": nodes,
                "relations": { "blocks": { "en": "One ticket blocks another", "zh": "阻塞", "aliases": [] } },
                "body": "# Work graph\n\n## Node types\n\n- `ticket`: a support ticket."
            })
            .to_string()
        }
        Step::VerifyDescribe if msg.contains("\"schema\": \"held\"") => {
            r#"{"ok": false, "reason": "zh is wrong"}"#.into()
        }
        Step::VerifyDescribe => OK.into(),
        Step::Match => common::llm::NOT_THE_SAME.into(),
        Step::Read => common::llm::skip_to_end(msg),
        Step::Plan => NO_EDITS.into(),
        Step::Verify => all_ok(msg),
    });
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let schema = |name: &str| {
        json!({ "schema": name, "nodes": { "ticket": { "key": "no" } },
                "relations": { "blocks": { "from": "ticket", "to": "ticket" } } })
    };
    let (status, resp) = app.post(c, "/ingest/graph/schema", schema("work")).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(
        resp,
        json!({ "schema": "work", "page": "graph/work", "created": true })
    );
    let (_, page) = app.get(c, "/pages/graph/work").await;
    let f = &page["front_matter"];
    assert_eq!(f["title"], "Work graph");
    assert_eq!(
        f["nodes"]["ticket"]["key"], "no",
        "structure from the definition"
    );
    assert_eq!(f["nodes"]["ticket"]["zh"], "工单");
    assert_eq!(f["relations"]["blocks"]["to"], "ticket");
    assert_eq!(f["relations"]["blocks"]["zh"], "阻塞");
    assert!(page["body"].as_str().unwrap().contains("a support ticket"));
    // Searchable at once, by the librarian's aliases.
    let (_, resp) = app.post(c, "/search", json!({ "query": "工单" })).await;
    assert_eq!(resp["knowledge"]["hits"][0]["page"], "graph/work", "{resp}");
    let (_, index) = app.get(c, "/pages/index").await;
    assert!(index["body"].as_str().unwrap().contains("[[graph/work]]"));
    // Sent again unchanged, as a host does at every start: no model call.
    let describes = llm.calls(Step::Describe).len();
    let (status, resp) = app.post(c, "/ingest/graph/schema", schema("work")).await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["created"], false);
    assert_eq!(llm.calls(Step::Describe).len(), describes);

    // Rejected twice, or the model unreachable: nothing is stored.
    for (name, want) in [
        ("held", StatusCode::UNPROCESSABLE_ENTITY),
        ("down", StatusCode::BAD_GATEWAY),
    ] {
        let (status, resp) = app.post(c, "/ingest/graph/schema", schema(name)).await;
        assert_eq!(status, want, "{name}: {resp}");
        let (status, _) = app.get(c, &format!("/pages/graph/{name}")).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{name}: no page");
        let (status, resp) = app
            .post(
                c,
                "/ingest/graph/space",
                json!({ "space": format!("{name}_s"), "schema": name }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name}: no schema: {resp}");
    }
    assert!(
        llm.calls(Step::Describe)
            .iter()
            .filter(|m| m.contains("\"schema\": \"held\""))
            .count()
            == 2,
        "sent back once"
    );
    let (_, list) = app.get(c, "/proposals").await;
    assert_eq!(list["proposals"], json!([]), "a description is never held");

    // A change the model can't describe leaves the schema as it was.
    let mut changed = schema("work");
    changed["nodes"]["coupon"] = json!({ "key": "code" });
    let (status, _) = app.post(c, "/ingest/graph/schema", changed).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let (_, page) = app.get(c, "/pages/graph/work").await;
    assert!(page["front_matter"]["nodes"].get("coupon").is_none());
    app.post(
        c,
        "/ingest/graph/space",
        json!({ "space": "w1", "schema": "work" }),
    )
    .await;
    let (status, resp) = app
        .post(
            c,
            "/ingest/graph/data",
            json!({ "space": "w1", "nodes": [{ "type": "coupon", "key": "X" }] }),
        )
        .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "coupon is not a type: {resp}"
    );
}

/// A hand edit is checked by code at once, then verified: saved when the verifier agrees,
/// held when it objects. Sources stay what the service recorded.
#[tokio::test]
async fn hand_edits_are_verified() {
    let llm = FakeLlm::new(|step, msg, _| match step {
        Step::Verify if msg.contains("99 yuan") => all_rejected(msg, "no page says 99 yuan"),
        Step::Verify => all_ok(msg),
        Step::Plan => NO_EDITS.into(),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => common::llm::NOT_THE_SAME.into(),
        Step::Read => common::llm::skip_to_end(msg),
    });
    let (app, key) = shopline(&llm).await;
    let wang = Caller::new(&key).user(Some("wang"));
    let lily = Caller::new(&key).user(Some("lily"));
    let uri = "/pages/expertise/shipping-sla";
    let (_, page) = app.get(wang, uri).await;
    let mut front = page["front_matter"].clone();
    front["summary"] = json!("Sellers ship within 48 hours; late shipments are penalized.");
    front["sources"] = json!(["forged.json"]);
    let body = format!(
        "{}\n## Note\n\nChecked by hand.\n",
        page["body"].as_str().unwrap()
    );

    let (status, resp) = app
        .put(
            wang,
            uri,
            // GET's answer sent back as it is, edited.
            {
                let mut sent = page.clone();
                sent["front_matter"] = front.clone();
                sent["body"] = json!(body);
                sent
            },
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resp}");
    let job = app.wait_job(wang, resp["job"].as_str().unwrap()).await;
    assert_eq!(
        job["result"]["pages_updated"],
        json!(["expertise/shipping-sla"]),
        "{job}"
    );
    let (_, saved) = app.get(wang, uri).await;
    assert!(saved["body"].as_str().unwrap().contains("Checked by hand."));
    assert_eq!(
        saved["front_matter"]["sources"], page["front_matter"]["sources"],
        "a hand edit can't change sources"
    );
    let verify = &llm.calls(Step::Verify)[0];
    assert!(
        verify.contains("### expertise/shipping-sla"),
        "the page as it stands is shown"
    );

    // An edit of the page as it was before is refused: it would undo the one just saved.
    let (status, resp) = app
        .put(
            lily,
            uri,
            json!({ "version": page["version"], "front_matter": front, "body": "# x\n" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("changed since it was read"),
        "{resp}"
    );

    // Code refuses at once; the verifier never sees it.
    let mut bad = front.clone();
    bad["summary"] = json!("");
    let (status, resp) = app
        .put(wang, uri, json!({ "front_matter": bad, "body": "# x\n" }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{resp}");
    let lily_prefs = format!("/pages/users/{}/preferences", user_dir_name("lily"));
    let (status, _) = app
        .put(
            wang,
            &lily_prefs,
            json!({ "front_matter": front, "body": "# x\n" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another user's page");
    for path in ["/pages/index", "/pages/graph/commerce"] {
        let (status, _) = app
            .put(
                wang,
                path,
                json!({ "front_matter": front, "body": "# x\n" }),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
    }

    // The verifier objects: held, unchanged.
    let mut claim = front.clone();
    claim["summary"] = json!("Free shipping from 99 yuan.");
    let (_, resp) = app
        .put(
            lily,
            uri,
            json!({ "version": saved["version"], "front_matter": claim,
                    "body": "# Shipping\n\nFree from 99 yuan.\n" }),
        )
        .await;
    let job = app.wait_job(lily, resp["job"].as_str().unwrap()).await;
    assert!(job["result"]["proposal"].is_string(), "{job}");
    let (_, now) = app.get(wang, uri).await;
    assert!(now["body"].as_str().unwrap().contains("Checked by hand."));
    let (_, list) = app.get(wang, "/proposals").await;
    assert_eq!(list["proposals"][0]["edits"][0]["edit"]["op"], "replace");
    let proposal = list["proposals"][0]["proposal"]
        .as_str()
        .unwrap()
        .to_owned();

    // The page changes before the held edit is approved: approving it would undo that change,
    // so it is refused and the proposal stays held.
    let body = format!(
        "{}\n## Later\n\nAdded later.\n",
        now["body"].as_str().unwrap()
    );
    let (_, resp) = app
        .put(
            wang,
            uri,
            json!({ "version": now["version"], "front_matter": now["front_matter"], "body": body }),
        )
        .await;
    app.wait_job(wang, resp["job"].as_str().unwrap()).await;
    let (status, resp) = app
        .post_admin(&format!("/proposals/{proposal}/approve"), json!({}))
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{resp}");
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("changed since it was read"),
        "{resp}"
    );
    let (_, list) = app.get(wang, "/proposals").await;
    assert_eq!(
        list["proposals"][0]["proposal"],
        proposal.as_str(),
        "still held"
    );
    let (_, now) = app.get(wang, uri).await;
    assert!(now["body"].as_str().unwrap().contains("Added later."));
}

/// Related pages, without an LLM: for every memory page of every scenario, a record about the
/// same thing (its title as the task or topic, its summary as the content) shows the
/// librarian that page, so it can update it instead of writing a duplicate. Each record is
/// sent by the page's own audience.
#[tokio::test]
async fn related_pages_include_the_page_a_record_is_about() {
    let llm = model(|_, _| json!({ "edits": [] }));
    let mut app = TestApp::with_llm(llm.llm());
    let tenants = testset::tenants();
    let mut keys = std::collections::HashMap::new();
    for t in &tenants {
        keys.insert(t.id.clone(), app.create_tenant(&t.id).await);
        testset::install_wiki(&t.id, &app.wiki_dir(&t.id));
    }
    app.restart();

    let mut misses = Vec::new();
    let mut total = 0;
    for t in &tenants {
        let key = &keys[&t.id];
        let wiki = testset::testset_dir()
            .join("tenants")
            .join(&t.id)
            .join("wiki");
        let mut files = Vec::new();
        collect(&wiki, &wiki, &mut files);
        for rel in files {
            let text = std::fs::read_to_string(wiki.join(format!("{rel}.md"))).unwrap();
            let front: Value = serde_norway::from_str(text.split("---").nth(1).unwrap()).unwrap();
            let (title, summary) = (
                front["title"].as_str().unwrap().to_owned(),
                front["summary"].as_str().unwrap().to_owned(),
            );
            let segs: Vec<&str> = rel.split('/').collect();
            let (user, groups, uri, body, audience) = match segs.as_slice() {
                ["experience", _] => (
                    None,
                    vec![],
                    "/ingest/experience",
                    json!({ "task": title, "outcome": summary }),
                    None,
                ),
                ["groups", g, "experience", _] => (
                    None,
                    vec![(*g).to_owned()],
                    "/ingest/experience",
                    json!({ "task": title, "outcome": summary }),
                    Some(format!("group:{g}")),
                ),
                ["groups", g, "decisions", _] => (
                    None,
                    vec![(*g).to_owned()],
                    "/ingest/decision",
                    json!({ "topic": title, "decision": summary }),
                    Some(format!("group:{g}")),
                ),
                ["users", u, "decisions", _] => (
                    Some((*u).to_owned()),
                    t.groups_of(u),
                    "/ingest/decision",
                    json!({ "topic": title, "decision": summary }),
                    None,
                ),
                ["users", u, "preferences"] => (
                    Some((*u).to_owned()),
                    t.groups_of(u),
                    "/ingest/preference",
                    json!({ "statement": summary }),
                    None,
                ),
                _ => continue,
            };
            total += 1;
            llm.clear();
            let mut caller = Caller::new(key).user(user.as_deref()).groups(&groups);
            if let Some(a) = &audience {
                caller = caller.audience(a);
            }
            submit(&app, caller, uri, body).await;
            let related = related_pages(&llm.calls(Step::Plan)[0]);
            let want = testset::page_path(&rel);
            if related.first() != Some(&want) {
                misses.push(format!("{}: {want} not first in {related:?}", t.id));
            }
        }
    }
    assert!(total >= 40, "only {total} memory pages");
    assert!(
        misses.is_empty(),
        "{} of {total} missed:\n{}",
        misses.len(),
        misses.join("\n")
    );
}

fn collect(base: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect(base, &p, out);
        } else if p.extension().is_some_and(|e| e == "md") {
            let rel = p.strip_prefix(base).unwrap().with_extension("");
            out.push(
                rel.components()
                    .map(|c| c.as_os_str().to_str().unwrap())
                    .collect::<Vec<_>>()
                    .join("/"),
            );
        }
    }
}
