//! Documents filed as expertise: ids issued by the service, new versions finding every page
//! the earlier ones were filed into (however the librarian spread them over concept pages),
//! only what changed being filed again, uploads without an id recognized from the wiki, and
//! documents read whole and filed one concept at a time, as the outline divides them.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::llm::{
    FakeLlm, NOT_THE_SAME, OK, Step, all_ok, candidates, concept, create, describe_from_definition,
    focus_title, folder, record_of, related_pages, same_as, update,
};
use serde_json::{Value, json};

fn filler(topic: &str, n: usize) -> String {
    (0..n)
        .map(|i| format!("{topic}的第{i}条细则：办理时核对订单状态、支付渠道和售后记录，并在工单里写明处理人与时间。"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A manual of three sections; the last one, on cross-border returns, is the one that changes.
fn manual(cross: &str) -> String {
    format!(
        "# 售后规则手册\n\n## 七天无理由退货\n\n签收后七天内可无理由退货，商品需完好。\n{}\n\n## 运费险\n\n运费险赔付寄回运费，上限 25 元。\n{}\n\n## 跨境商品退货\n\n{cross}\n",
        filler("七天无理由退货", 25),
        filler("运费险", 25)
    )
}

const V1: &str = "跨境商品退货需在 3 天内申报，退回保税仓，关税不退。";
const V2: &str = "自 2026-11-01 起，跨境商品退货申报期延长到 7 天，关税按比例退还。";

/// The document an outline message shows.
fn document_of(msg: &str) -> Value {
    let block = msg
        .split("## The document\n\n```json\n")
        .nth(1)
        .and_then(|s| s.split("\n```").next())
        .expect("a document");
    serde_json::from_str(block).unwrap()
}

/// The page each concept of the manual is filed into.
fn page_of(title: &str) -> &'static str {
    match title {
        "七天无理由退货" => "return-window",
        "运费险" => "shipping-insurance",
        "跨境商品退货" => "crossborder-returns",
        _ => "service-scripts",
    }
}

/// A librarian that outlines the manual's first version as three concepts and files each as
/// a page; for a later version it outlines only the cross-border concept (in the page the
/// outline is shown for it) and updates that page when it is shown it, writing a duplicate
/// when it isn't, which the tests would catch. `answer` says which candidate a match is.
fn librarian(answer: impl Fn(&str) -> String + Send + Sync + 'static) -> Arc<FakeLlm> {
    FakeLlm::new(move |step, msg, _| match step {
        Step::Outline => {
            let doc = document_of(msg);
            // The folder the outline is told new pages go in.
            let dir = msg
                .split("New pages of this document are in `")
                .nth(1)
                .and_then(|s| s.split("/`").next())
                .unwrap()
                .to_owned();
            let at = |name: &str| format!("{dir}/{name}");
            if !doc["title"].as_str().unwrap().starts_with("售后规则手册") {
                return json!({ "concepts": [
                    concept("客服话术", &at("service-scripts"), &[], "write it")
                ]})
                .to_string();
            }
            if msg.contains("What changed since version") {
                let listed = format!("- {}: 跨境商品退货", at("crossborder-returns"));
                assert!(msg.contains(&listed), "the earlier pages are listed: {msg}");
                return json!({ "concepts": [concept("跨境商品退货", &at("crossborder-returns"),
                    &["跨境商品退货"], "the filing period and the tax refund changed")] })
                .to_string();
            }
            json!({ "concepts": [
                concept("七天无理由退货", &at("return-window"), &["七天无理由退货"], "write it"),
                concept("运费险", &at("shipping-insurance"), &["运费险"], "write it"),
                concept("跨境商品退货", &at("crossborder-returns"), &["跨境商品退货"], "write it"),
            ]})
            .to_string()
        }
        Step::Plan => {
            let rec = record_of(msg);
            let title = focus_title(msg).expect("one concept per call");
            let page = format!("{}/{}", folder(msg), page_of(&title));
            if rec["version"] == 1 {
                let body = match title.as_str() {
                    "跨境商品退货" => format!("# 跨境商品退货\n\n## 规则\n\n{V1}"),
                    t => format!("# {t}\n\n内容。"),
                };
                return json!({ "edits": [create(&page, &title, "一条规则。", &[], &body)] })
                    .to_string();
            }
            let content = rec["content"].as_str().unwrap();
            let rule = content.lines().last().unwrap();
            if related_pages(msg).first() == Some(&page) {
                json!({ "edits": [update(&page, Some("规则"), Some(rule))] })
            } else {
                json!({ "edits": [create(&format!("{page}-again"), "跨境退货", "重复。", &[], rule)] })
            }
            .to_string()
        }
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => answer(msg),
    })
}

async fn file(app: &TestApp, c: Caller<'_>, body: Value) -> (Value, Value) {
    let (status, queued) = app.post(c, "/ingest/expertise", body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    (queued, job["result"].clone())
}

#[tokio::test]
async fn new_versions_find_the_pages_of_earlier_ones() {
    let llm = librarian(|msg| {
        if msg.contains("客服话术") {
            NOT_THE_SAME.into()
        } else {
            same_as(&candidates(msg)[0])
        }
    });
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);

    // Version 1: a new document, filed as three concept pages, and given an id.
    let (queued, r) = file(
        &app,
        c,
        json!({ "title": "售后规则手册", "content": manual(V1) }),
    )
    .await;
    assert!(queued.get("document").is_none(), "no id was sent");
    assert_eq!(r["document_match"], "new");
    assert_eq!(r["version"], 1);
    let id = r["document"].as_str().unwrap().to_owned();
    assert!(id.starts_with("d_") && id.len() == 18, "{id}");
    assert_eq!(r["pages_created"].as_array().unwrap().len(), 3);
    // Read whole: one outline, then one call per concept, each with the whole document.
    assert_eq!(llm.calls(Step::Outline).len(), 1);
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 3);
    for plan in &plans {
        assert!(
            record_of(plan)["content"].as_str().unwrap().contains(V1),
            "the whole document"
        );
    }
    assert_eq!(
        llm.calls(Step::Match).len(),
        0,
        "nothing filed yet to compare with"
    );

    // Version 2, with its id: the page of the changed section is found and updated, with
    // what changed; the sections that didn't change are not filed again.
    llm.clear();
    let (queued, r) = file(
        &app,
        c,
        json!({ "title": "售后规则手册", "content": manual(V2), "document": id }),
    )
    .await;
    assert_eq!(
        queued["document"],
        id.as_str(),
        "the answer names the document"
    );
    assert_eq!(r["document_match"], "given");
    assert_eq!(r["version"], 2);
    assert_eq!(
        r["pages_updated"],
        json!(["expertise/crossborder-returns"]),
        "{r}"
    );
    assert_eq!(r["pages_created"], json!([]));
    let outline = &llm.calls(Step::Outline)[0];
    assert!(outline.contains(&format!("- {V1}")) && outline.contains(&format!("+ {V2}")));
    assert!(
        outline.contains("/crossborder-returns: 跨境商品退货"),
        "earlier pages listed"
    );
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 1, "only the concept the change touches");
    let related = related_pages(&plans[0]);
    for page in [
        "expertise/return-window",
        "expertise/shipping-insurance",
        "expertise/crossborder-returns",
    ] {
        assert!(
            related.contains(&page.to_owned()),
            "{page} not shown: {related:?}"
        );
    }
    assert!(plans[0].contains(&format!("version 2 of document {id}")));
    assert!(plans[0].contains(&format!("- {V1}")) && plans[0].contains(&format!("+ {V2}")));
    let (_, page) = app.get(c, "/pages/expertise/crossborder-returns").await;
    assert!(page["body"].as_str().unwrap().contains("7 天"));
    assert_eq!(page["front_matter"]["sources"].as_array().unwrap().len(), 2);

    // Version 3, unchanged: nothing to file, but the version is kept.
    llm.clear();
    let (_, r) = file(
        &app,
        c,
        json!({ "title": "售后规则手册", "content": manual(V2), "document": id }),
    )
    .await;
    assert_eq!(r["version"], 3);
    assert_eq!(llm.calls(Step::Outline).len(), 0, "no model call at all");
    assert_eq!(llm.calls(Step::Plan).len(), 0);
    assert_eq!(r["pages_updated"], json!([]));

    // Version 4 without the id: recognized from the wiki pages filed from it.
    llm.clear();
    let (_, r) = file(
        &app,
        c,
        json!({ "title": "售后规则手册（2026 修订）", "content": manual(V1) }),
    )
    .await;
    assert_eq!(r["document_match"], "matched", "{r}");
    assert_eq!(r["document"], id.as_str());
    assert_eq!(r["version"], 4);
    assert_eq!(r["pages_updated"], json!(["expertise/crossborder-returns"]));
    let matched = &llm.calls(Step::Match)[0];
    assert_eq!(candidates(matched)[0], id);
    assert!(
        matched.contains("#### expertise/crossborder-returns"),
        "shown as wiki pages"
    );
    // The candidates are shown as their pages, never their raw records: the manual's
    // detailed rules (细则) are in the records, not in any page.
    let shown = matched.split("## Candidates").nth(1).unwrap();
    assert!(!shown.contains("细则"), "never the raw records");

    // A different document sharing words with it: the librarian says so, and it is new.
    let (_, r) = file(
        &app,
        c,
        json!({ "title": "客服话术", "content": "退货时先致歉，再说明七天无理由退货和运费险的流程。" }),
    )
    .await;
    assert_eq!(r["document_match"], "new");
    assert_ne!(r["document"], id.as_str());
    assert_eq!(r["pages_created"], json!(["expertise/service-scripts"]));

    // Ids are the service's own: malformed is a 400, unknown a 404; other kinds take none.
    for (body, want) in [
        (
            json!({ "title": "t", "content": "c", "document": "manual-1" }),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({ "title": "t", "content": "c", "document": "d_ffffffffffffffff" }),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, resp) = app.post(c, "/ingest/expertise", body).await;
        assert_eq!(status, want, "{resp}");
    }
    let (status, _) = app
        .post(
            c,
            "/ingest/experience",
            json!({ "task": "t", "outcome": "o", "document": id }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A group's document can only be updated by members of the group, and its pages stay the
/// group's.
#[tokio::test]
async fn a_group_document_stays_in_its_group() {
    let llm = librarian(|_| NOT_THE_SAME.into());
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let ops = vec!["ops".to_owned()];
    let member = Caller::new(&key).user(Some("ann")).groups(&ops);
    let outsider = Caller::new(&key).user(Some("bob"));
    let (_, r) = file(
        &app,
        member,
        json!({ "title": "售后规则手册", "content": manual(V1), "group": "ops" }),
    )
    .await;
    let id = r["document"].as_str().unwrap().to_owned();
    assert!(
        r["pages_created"][0]
            .as_str()
            .unwrap()
            .starts_with("groups/g_")
    );
    let body = json!({ "title": "售后规则手册", "content": manual(V2), "document": id });
    let (status, _) = app.post(outsider, "/ingest/expertise", body.clone()).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not the outsider's to see");
    // The member needn't repeat the group: the document says where its pages are.
    let (_, r) = file(&app, member, body).await;
    assert_eq!(r["document_match"], "given");
    assert!(
        r["pages_updated"][0]
            .as_str()
            .unwrap()
            .starts_with("groups/g_"),
        "{r}"
    );
    // Documents of another audience are never candidates.
    let (_, r) = file(
        &app,
        Caller::new(&key),
        json!({ "title": "售后规则手册", "content": manual(V2) }),
    )
    .await;
    assert_eq!(r["document_match"], "new");
    assert_eq!(llm.calls(Step::Match).len(), 0);
}

/// A document is read whole and divided by meaning, not size: the outline's concepts may
/// span sections, each concept gets one call with the whole document, and later concepts see
/// the pages earlier ones wrote. A new version files only the concepts its changes touch.
#[tokio::test]
async fn documents_are_filed_one_concept_at_a_time() {
    let llm = FakeLlm::new(|step, msg, _| match step {
        Step::Outline if msg.contains("What changed since version") => json!({ "concepts": [
            concept("乙", "expertise/concept-2", &["二", "四"], "四 changed")
        ]})
        .to_string(),
        Step::Outline => json!({ "concepts": [
            concept("甲", "expertise/concept-1", &["一"], "write it"),
            // One concept explained in two sections apart.
            concept("乙", "expertise/concept-2", &["二", "四"], "write it"),
            concept("丙", "expertise/concept-3", &["三"], "write it"),
        ]})
        .to_string(),
        Step::Plan => {
            let title = focus_title(msg).unwrap();
            let n = match title.as_str() {
                "甲" => 1,
                "乙" => 2,
                _ => 3,
            };
            let page = format!("{}/concept-{n}", folder(msg));
            let edit = if related_pages(msg).contains(&page) {
                let rec = record_of(msg);
                update(
                    &page,
                    Some("最新"),
                    Some(rec["content"].as_str().unwrap().lines().last().unwrap()),
                )
            } else {
                create(&page, &title, "一个概念。", &[], "# 概念\n\n内容。")
            };
            json!({ "edits": [edit] }).to_string()
        }
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => NOT_THE_SAME.into(),
    });
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let doc = |last: &str| {
        format!(
            "# 大手册\n\n## 一\n\n{}\n\n## 二\n\n{}\n\n## 三\n\n{}\n\n## 四\n\n{last}\n",
            filler("第一节", 200),
            filler("第二节", 200),
            filler("第三节", 200)
        )
    };
    let (_, r) = file(
        &app,
        c,
        json!({ "title": "大手册", "content": doc("旧规定。") }),
    )
    .await;
    assert_eq!(llm.calls(Step::Outline).len(), 1);
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 3, "one call per concept");
    assert_eq!(focus_title(&plans[1]).as_deref(), Some("乙"));
    assert!(plans[1].contains("in the document: 二; 四"));
    for plan in &plans {
        assert!(
            record_of(plan)["content"]
                .as_str()
                .unwrap()
                .contains("旧规定。")
        );
        assert!(
            plan.contains("1. 甲 → [[expertise/concept-1]]")
                && plan.contains("3. 丙 → [[expertise/concept-3]]"),
            "the outline, with each concept's page id, is shown"
        );
    }
    assert!(
        related_pages(&plans[1]).contains(&"expertise/concept-1".to_owned()),
        "a later concept sees what an earlier one wrote"
    );
    assert_eq!(r["pages_created"].as_array().unwrap().len(), 3, "{r}");
    let id = r["document"].as_str().unwrap().to_owned();

    llm.clear();
    let (_, r) = file(
        &app,
        c,
        json!({ "title": "大手册", "content": doc("新规定。"), "document": id }),
    )
    .await;
    let outline = &llm.calls(Step::Outline)[0];
    assert!(outline.contains("+ 新规定。") && outline.contains("- 旧规定。"));
    assert!(outline.contains("Pages filed from its earlier versions"));
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 1, "only the concept the change touches");
    assert_eq!(
        related_pages(&plans[0]).first().map(String::as_str),
        Some("expertise/concept-2"),
        "the page the outline names comes first"
    );
    assert_eq!(r["pages_updated"], json!(["expertise/concept-2"]), "{r}");
}
