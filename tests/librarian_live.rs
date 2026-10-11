//! The librarian with a real model, on the `shopline` scenario: the schema is described by the
//! model, and a record about an existing playbook must update that playbook, not add a page;
//! every page it writes must be anchored. Uses the model in the repository's `model.conf` (see
//! `model.conf.example`), and runs only when asked, since it calls the model for real (and
//! costs money with a paid API):
//!
//! ```text
//! PINAKES_TEST_LLM=1 cargo test --test integration librarian_live:: -- --nocapture
//! ```

use crate::common;

use crate::common::app::{Caller, TestApp};
use crate::common::testset;
use axum::http::StatusCode;
use pinakes::config::Config;
use pinakes::llm::Llm;
use serde_json::json;

fn live_llm() -> Option<Llm> {
    std::env::var("PINAKES_TEST_LLM").ok()?;
    // What the librarian decides at each step, with `--nocapture`.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("pinakes=info"))
        .with_test_writer()
        .try_init();
    let mut cfg = Config::default();
    cfg.load_model_file(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        .expect("model.conf");
    let key = cfg.librarian.llm.key();
    Some(pinakes::llm::client(&cfg.librarian, key).expect("the model and its key"))
}

#[tokio::test]
async fn a_real_model_updates_instead_of_duplicating() {
    let Some(llm) = live_llm() else {
        eprintln!("PINAKES_TEST_LLM is not set; skipping the live librarian test");
        return;
    };
    let mut app = TestApp::with_llm(llm);
    let key = app.create_tenant("shopline").await;
    testset::install_wiki("shopline", &app.wiki_dir("shopline"));
    app.restart();
    // The schema goes through the model like any host's would.
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
    let wang = Caller::new(&key).user(Some("wang"));
    let (_, before) = app.get(wang, "/pages?kind=experience").await;

    let (status, queued) = app
        .post(
            wang,
            "/ingest/experience",
            json!({
                "task": "partial refund after shipment",
                "outcome": "The buyer asked for a partial refund on order 1042 after it shipped. We issued it and added the manual ledger entry, but finance rejected the entry because it had no shipment tracking number. Adding the tracking number fixed it.",
                "succeeded": true,
                "entities": ["commerce:order"],
                "date": "2026-10-01"
            }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(wang, queued["job"].as_str().unwrap()).await;
    eprintln!("{}", serde_json::to_string_pretty(&job).unwrap());
    assert_eq!(job["status"], "done", "{job}");
    let r = &job["result"];
    let (_, after) = app.get(wang, "/pages?kind=experience").await;
    assert_eq!(
        after["pages"].as_array().unwrap().len(),
        before["pages"].as_array().unwrap().len(),
        "no new playbook: {job}"
    );
    let updated = r["pages_updated"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "experience/partial-refund-after-shipment");
    assert!(
        updated || r["proposal"].is_string(),
        "the playbook was updated (or the update held): {job}"
    );
    let (_, page) = app
        .get(wang, "/pages/experience/partial-refund-after-shipment")
        .await;
    eprintln!("{}", page["body"].as_str().unwrap());
    assert!(!page["front_matter"]["about"].as_array().unwrap().is_empty());
}

/// A document uploaded twice without its id: the second upload must be recognized as the
/// first one's new version and update its pages, not add new ones.
#[tokio::test]
async fn a_real_model_recognizes_a_new_version() {
    let Some(llm) = live_llm() else {
        eprintln!("PINAKES_TEST_LLM is not set; skipping the live document test");
        return;
    };
    let app = TestApp::with_llm(llm);
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let manual = |cross: &str| {
        format!(
            "# 售后规则手册\n\n## 七天无理由退货\n\n签收后七天内可无理由退货，商品需完好、配件齐全。定制商品和生鲜不适用。\n\n## 运费险\n\n买家退货时，运费险赔付寄回运费，单笔上限 25 元，退款完成后 48 小时内到账。\n\n## 跨境商品退货\n\n{cross}\n"
        )
    };
    let mut results = Vec::new();
    for cross in [
        "跨境商品退货需在签收后 3 天内申报，商品退回保税仓，已缴关税不退。",
        "自 2026-11-01 起，跨境商品退货申报期延长到 7 天，商品退回保税仓，关税按退货金额比例退还。",
    ] {
        let (status, queued) = app
            .post(
                c,
                "/ingest/expertise",
                json!({ "title": "售后规则手册", "content": manual(cross) }),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
        let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
        eprintln!("{}", serde_json::to_string_pretty(&job).unwrap());
        assert_eq!(job["status"], "done", "{job}");
        results.push(job["result"].clone());
    }
    let (v1, v2) = (&results[0], &results[1]);
    assert_eq!(v1["document_match"], "new");
    assert!(!v1["pages_created"].as_array().unwrap().is_empty());
    assert_eq!(v2["document_match"], "matched", "recognized: {v2}");
    assert_eq!(v2["document"], v1["document"]);
    assert_eq!(v2["version"], 2);
    assert_eq!(v2["pages_created"], json!([]), "no duplicate: {v2}");
    let updated = v2["pages_updated"].as_array().unwrap();
    assert!(!updated.is_empty(), "{v2}");
    let mut found = false;
    for page in updated {
        let (_, p) = app
            .get(c, &format!("/pages/{}", page.as_str().unwrap()))
            .await;
        let body = p["body"].as_str().unwrap();
        eprintln!("--- {page}\n{body}");
        found |= body.contains('7') && body.contains("关税");
    }
    assert!(found, "an updated page has the new rule");
}

/// A longer manual, read from the top one concept at a time, each concept filed as it is read;
/// then a new version without its id that changes one rule must be recognized, read again only
/// where it changed, and only touch the pages of that rule.
#[tokio::test]
async fn a_real_model_files_a_manual_by_concept() {
    let Some(llm) = live_llm() else {
        eprintln!("PINAKES_TEST_LLM is not set; skipping the live manual test");
        return;
    };
    let app = TestApp::with_llm(llm);
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let v1 = include_str!("fixtures/after-sales-manual.md").to_owned();
    let v2 = v1.replace(
        "跨境商品退货需在签收后 3 天内在平台申报",
        "跨境商品退货需在签收后 7 天内在平台申报",
    );
    assert_ne!(v1, v2);
    let mut results = Vec::new();
    for content in [&v1, &v2] {
        let started = std::time::Instant::now();
        let (status, queued) = app
            .post(
                c,
                "/ingest/expertise",
                json!({ "title": "售后服务手册", "content": content }),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
        let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
        eprintln!(
            "{} s\n{}",
            started.elapsed().as_secs(),
            serde_json::to_string_pretty(&job).unwrap()
        );
        assert_eq!(job["status"], "done", "{job}");
        results.push(job["result"].clone());
    }
    let (r1, r2) = (&results[0], &results[1]);
    assert!(
        r1["pages_created"].as_array().unwrap().len() >= 3,
        "filed as several concept pages: {r1}"
    );
    assert_eq!(r2["document_match"], "matched", "{r2}");
    assert_eq!(r2["pages_created"], json!([]), "no duplicate: {r2}");
    let updated = r2["pages_updated"].as_array().unwrap();
    assert!(
        !updated.is_empty() && updated.len() <= 2,
        "only the changed rule: {r2}"
    );
    let mut found = false;
    for page in updated {
        let (_, p) = app
            .get(c, &format!("/pages/{}", page.as_str().unwrap()))
            .await;
        let body = p["body"].as_str().unwrap();
        eprintln!("--- {page}\n{body}");
        found |= body.contains("7 天") || body.contains("7天");
    }
    assert!(found, "the new rule is on an updated page");
    let (_, list) = app.get(c, "/pages?kind=expertise").await;
    for p in list["pages"].as_array().unwrap() {
        eprintln!("{}: {}", p["page"], p["summary"]);
    }
}

/// A text with no headings at all, a meeting transcript: the model must still find where each
/// topic ends, file each as a condensed page (not a copy) with its numbers exactly as said, and
/// cite the lines each page was written from.
#[tokio::test]
async fn a_real_model_reads_a_transcript_without_headings() {
    let Some(llm) = live_llm() else {
        eprintln!("PINAKES_TEST_LLM is not set; skipping the live transcript test");
        return;
    };
    let app = TestApp::with_llm(llm);
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let text = include_str!("fixtures/ops-meeting-transcript.txt");
    let started = std::time::Instant::now();
    let (status, queued) = app
        .post(
            c,
            "/ingest/expertise",
            json!({ "title": "运营周会记录 2026-09-28", "content": text }),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    eprintln!(
        "{} s\n{}",
        started.elapsed().as_secs(),
        serde_json::to_string_pretty(&job).unwrap()
    );
    assert_eq!(job["status"], "done", "{job}");
    let r = &job["result"];
    let pages = r["pages_created"].as_array().unwrap();
    assert!(pages.len() >= 3, "the topics are filed apart: {r}");
    let mut all = String::new();
    let mut page_chars = 0;
    for page in pages {
        let (_, p) = app
            .get(c, &format!("/pages/{}", page.as_str().unwrap()))
            .await;
        let body = p["body"].as_str().unwrap();
        eprintln!("--- {page} {}\n{body}", p["front_matter"]["sources"]);
        let sources = p["front_matter"]["sources"].to_string();
        assert!(sources.contains("#L"), "cites its lines: {sources}");
        page_chars += body.chars().count();
        all.push_str(body);
    }
    // The decisions' numbers, exactly as said.
    for fact in ["24 小时", "48 小时", "2000", "15 元", "两年"] {
        let squeezed = fact.replace(' ', "");
        assert!(
            all.contains(fact) || all.contains(&squeezed),
            "{fact} is on a page"
        );
    }
    eprintln!(
        "pages: {page_chars} characters for a {}-character transcript",
        text.chars().count()
    );
}

/// A Word file end to end: the service's own converter turns it into Markdown, table
/// included, and the model reads and files it; the table's numbers reach the pages.
#[tokio::test]
async fn a_real_model_files_a_word_file() {
    let Some(llm) = live_llm() else {
        eprintln!("PINAKES_TEST_LLM is not set; skipping the live librarian test");
        return;
    };
    let app = TestApp::with_llm(llm);
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let docx = include_bytes!("fixtures/member-benefits.docx");
    let (status, queued) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some(("会员权益说明.docx", docx)),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    eprintln!("{}", serde_json::to_string_pretty(&job).unwrap());
    assert_eq!(job["status"], "done", "{job}");
    let source = job["result"]["source"].as_str().unwrap();
    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(app.tenant_dir("acme").join(source)).unwrap(),
    )
    .unwrap();
    assert_eq!(record["file"]["converter"], "builtin");
    let mut all = String::new();
    for page in job["result"]["pages_created"].as_array().unwrap() {
        let (_, p) = app
            .get(c, &format!("/pages/{}", page.as_str().unwrap()))
            .await;
        eprintln!("--- {page}\n{}", p["body"].as_str().unwrap());
        all.push_str(p["body"].as_str().unwrap());
    }
    assert!(
        all.contains("1.5"),
        "the table's multiplier for gold members"
    );
    assert!(all.contains("5000"), "a threshold from the table");
}

/// A memory recorded with the session its task read with: the real model is offered the
/// knowledge page the session read, should anchor the new playbook to it, and a later search
/// brings that page along. The same record without a session runs in a second tenant, for
/// comparison (printed, not asserted). Add `PINAKES_TEST_EMBEDDING=1` to search related pages
/// by meaning too, which finds the Chinese page for the English record without the session.
#[tokio::test]
async fn a_real_model_anchors_the_knowledge_its_task_read() {
    let Some(llm) = live_llm() else {
        eprintln!("PINAKES_TEST_LLM is not set; skipping the live librarian test");
        return;
    };
    // With PINAKES_TEST_EMBEDDING the related-page search ranks by meaning too.
    let mut app = match common::embed::real() {
        Some(e) => TestApp::with_llm_and_embedder(llm, e),
        None => TestApp::with_llm(llm),
    };
    let mut keys = Vec::new();
    for id in ["shopline", "shopline2"] {
        keys.push(app.create_tenant(id).await);
        testset::install_wiki("shopline", &app.wiki_dir(id));
    }
    app.restart();
    let t = testset::tenants()
        .into_iter()
        .find(|t| t.id == "shopline")
        .unwrap();
    for key in &keys {
        for call in testset::graph_calls(&t) {
            let caller = Caller::new(key)
                .user(call.user.as_deref())
                .groups(&call.groups);
            let (status, body) = app.post(caller, call.uri, call.body).await;
            assert!(status.is_success(), "{}: {status} {body}", call.uri);
        }
    }
    let record = |session: Option<&str>| {
        let mut r = json!({
            "task": "business buyer asks for a VAT special invoice",
            "outcome": "A company buyer needed a VAT special invoice to claim the purchase. We asked for the company name, tax number, bank and account, and registered address before issuing it; it was sent three working days later and the buyer could claim it.",
            "succeeded": true,
            "entities": ["commerce:order"],
            "date": "2026-10-04"
        });
        if let Some(s) = session {
            r["session"] = json!(s);
        }
        r
    };
    let mut abouts = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let wang = Caller::new(key).user(Some("wang"));
        let session = (i == 0).then_some("task-invoice");
        if session.is_some() {
            let (status, _) = app
                .post(
                    wang,
                    "/read",
                    json!({ "ids": ["expertise/invoice-issuing"], "session": "task-invoice" }),
                )
                .await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, queued) = app.post(wang, "/ingest/experience", record(session)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
        let job = app.wait_job(wang, queued["job"].as_str().unwrap()).await;
        eprintln!(
            "with session: {}\n{}",
            session.is_some(),
            serde_json::to_string_pretty(&job["result"]).unwrap()
        );
        assert_eq!(job["status"], "done", "{job}");
        let r = &job["result"];
        let written: Vec<String> = r["pages_created"]
            .as_array()
            .unwrap()
            .iter()
            .chain(r["pages_updated"].as_array().unwrap())
            .filter_map(|p| p.as_str().map(str::to_owned))
            .filter(|p| p.starts_with("experience/"))
            .collect();
        let mut about = Vec::new();
        for p in &written {
            let (_, page) = app.get(wang, &format!("/pages/{p}")).await;
            eprintln!("{p} about: {}", page["front_matter"]["about"]);
            about.extend(
                page["front_matter"]["about"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        let (_, resp) = app
            .post(
                wang,
                "/search",
                json!({ "query": "a company buyer wants a VAT special invoice", "keywords": ["VAT special invoice"] }),
            )
            .await;
        eprintln!(
            "search: memories {:?} knowledge {:?}",
            resp["memories"]["hits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["page"].clone())
                .collect::<Vec<_>>(),
            resp["knowledge"]["hits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| (h["page"].clone(), h["match"].clone()))
                .collect::<Vec<_>>()
        );
        abouts.push((session.is_some(), written, about));
    }
    let (_, written, about) = &abouts[0];
    assert!(!written.is_empty(), "the playbook was written");
    assert!(
        about.iter().any(|a| a == "expertise/invoice-issuing"),
        "with the session the playbook is anchored to the page the task read: {about:?}"
    );
    // Searching related pages by meaning finds the Chinese page without the session too.
    if common::embed::real().is_some() {
        let (_, written, about) = &abouts[1];
        assert!(!written.is_empty(), "the playbook was written");
        assert!(
            about.iter().any(|a| a == "expertise/invoice-issuing"),
            "without the session, by meaning: {about:?}"
        );
    }
}

/// Lint with a real model, on the `shopline` scenario with a contradiction, a stale claim and an
/// unanchored playbook planted: the contradiction and the stale claim are reported with their
/// claims quoted, and the playbook gets anchors proposed.
#[tokio::test]
async fn a_real_model_lints_the_wiki() {
    use pinakes::wiki::{Page, PagePath, WikiFs};
    let Some(llm) = live_llm() else {
        eprintln!("PINAKES_TEST_LLM is not set; skipping the live lint test");
        return;
    };
    let mut app = TestApp::with_llm(llm);
    let key = app.create_tenant("shopline").await;
    testset::install_wiki("shopline", &app.wiki_dir("shopline"));
    let page = |path: &str, title: &str, about: &[&str], body: &str| {
        let mut p = Page::new(PagePath::parse(path).unwrap());
        p.front.kind = p.path.kind();
        p.front.title = Some(title.into());
        p.front.summary = Some(body.lines().next().unwrap().into());
        p.front.about = about.iter().map(|a| (*a).into()).collect();
        p.body = format!("# {title}\n\n{body}\n");
        p
    };
    let wiki = WikiFs::new(app.wiki_dir("shopline"));
    for p in [
        page(
            "expertise/return-window-cn",
            "国内订单退货期限",
            &[],
            "国内订单签收后 7 天内可无理由退货。\n\n超过 7 天只能走售后维修。",
        ),
        page(
            "expertise/domestic-returns",
            "国内退货规则",
            &[],
            "国内订单签收后 15 天内可无理由退货，运费由买家承担。",
        ),
        page(
            "expertise/spring-sale",
            "春季大促规则",
            &[],
            "春季大促满 300 减 50，活动有效期至 2026-04-30。\n\n大促期间订单发货时间延长到 72 小时。",
        ),
        page(
            "experience/order-address-change",
            "订单改地址",
            &[],
            "## 可行步骤\n\n1. 未发货的订单可以直接在后台改收货地址。\n2. 已发货的订单要先联系快递拦截。",
        ),
    ] {
        wiki.write(&p).unwrap();
    }
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
    let host = Caller::new(&key);
    let (status, queued) = app.post(host, "/lint", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(host, queued["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    let (_, report) = app.get(host, "/lint").await;
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    let findings = report["findings"].as_array().unwrap();
    let about = |kind: &str, page: &str| {
        findings
            .iter()
            .find(|f| f["kind"] == kind && f["pages"].as_array().unwrap().iter().any(|p| p == page))
    };
    let contradiction = about("contradiction", "expertise/domestic-returns")
        .expect("the two return windows contradict each other");
    assert!(
        !contradiction["claims"].as_array().unwrap().is_empty(),
        "{contradiction}"
    );
    let stale = about("stale", "expertise/spring-sale").expect("the sale ended on 2026-04-30");
    assert!(!stale["claims"].as_array().unwrap().is_empty(), "{stale}");
    let unanchored = about("unanchored", "experience/order-address-change").unwrap();
    assert!(
        unanchored["proposal"].is_string(),
        "anchors proposed: {unanchored}"
    );
}
