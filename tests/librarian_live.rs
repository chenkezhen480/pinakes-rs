//! The librarian with a real model, on the `shopline` scenario: the schema is described by the
//! model, and a record about an existing playbook must update that playbook, not add a page;
//! every page it writes must be anchored. Uses the model in the repository's `model.conf` (see
//! `model.conf.example`), and runs only when asked, since it calls the model for real (and
//! costs money with a paid API):
//!
//! ```text
//! PINAKES_TEST_LLM=1 cargo test --test librarian_live -- --nocapture
//! ```

mod common;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::testset;
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
