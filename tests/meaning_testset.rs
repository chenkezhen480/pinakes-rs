//! M5 "done when": questions that need a memory plus the knowledge it leads to are answered, and
//! semantic mode recovers cross-language misses.
//!
//! Every scenario's wiki is loaded as in `search_testset.rs`, then the M5 questions are asked as
//! their callers: `hop` questions in keyword mode (the hop is on by default), `cross_lang`
//! questions in keyword mode (where they miss) and in the meaning-based modes. The hop part
//! always runs. The meaning part needs the embedding model, downloaded into `target/models` on
//! first use:
//!
//! ```text
//! PINAKES_TEST_EMBEDDING=1 cargo test --test meaning_testset -- --nocapture
//! ```
//!
//! Without `PINAKES_TEST_EMBEDDING` it says so and passes.

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::testset::{self, Question, Tally, describe, judge, slot_pages};
use pinakes::embed::Embedder;
use serde_json::{Value, json};

struct Loaded {
    app: TestApp,
    keys: HashMap<String, String>,
}

async fn load(embedder: Option<Arc<dyn Embedder>>) -> Loaded {
    let mut app = match embedder {
        Some(e) => TestApp::with_embedder("", e),
        None => TestApp::new(),
    };
    let mut keys = HashMap::new();
    for t in testset::tenants() {
        keys.insert(t.id.clone(), app.create_tenant(&t.id).await);
        testset::install_wiki(&t.id, &app.wiki_dir(&t.id));
    }
    // Load the installed pages the way a restarted service would.
    app.restart();
    Loaded { app, keys }
}

/// Every question of the set with its tenant, for the given categories.
fn questions(categories: &[&str]) -> Vec<(String, Vec<String>, Question)> {
    let mut out = Vec::new();
    for t in testset::tenants() {
        for q in testset::questions(&t.id) {
            if categories.contains(&q.category.as_str()) {
                let groups = q
                    .user
                    .as_deref()
                    .map(|u| t.groups_of(u))
                    .unwrap_or_default();
                out.push((t.id.clone(), groups, q));
            }
        }
    }
    out
}

struct Outcome {
    passed: usize,
    total: usize,
    by_category: Tally,
    failures: Vec<String>,
}

/// Asks each question with `extra` merged into the request body.
async fn ask(
    w: &Loaded,
    qs: &[(String, Vec<String>, Question)],
    extra: Value,
    check: impl Fn(&Question, &Value) -> Option<String>,
) -> Outcome {
    let mut out = Outcome {
        passed: 0,
        total: 0,
        by_category: Tally::default(),
        failures: Vec::new(),
    };
    for (tenant, groups, q) in qs {
        let caller = Caller::new(&w.keys[tenant])
            .user(q.user.as_deref())
            .groups(groups);
        let mut body = json!({ "query": q.query });
        if !q.keywords.is_empty() {
            body["keywords"] = json!(q.keywords);
        }
        for (k, v) in extra.as_object().unwrap() {
            body[k] = v.clone();
        }
        let (status, resp) = w.app.post(caller, "/search", body).await;
        assert_eq!(status, StatusCode::OK, "{}: {resp}", q.id);
        let verdict = check(q, &resp);
        out.total += 1;
        out.by_category.add(&q.category, verdict.is_none());
        match verdict {
            None => out.passed += 1,
            Some(why) => out.failures.push(format!(
                "  {} ({}): {why}{}",
                q.id,
                q.query,
                describe(&resp)
            )),
        }
    }
    out
}

fn report(name: &str, out: &Outcome) {
    println!(
        "{name}: {}/{}\n{}",
        out.passed,
        out.total,
        out.by_category.lines().join("\n")
    );
    if !out.failures.is_empty() {
        println!("misses, {name}:\n{}", out.failures.join("\n"));
    }
}

/// The expected knowledge pages of a hop question are reached through the memory: present,
/// and marked `via: memory` unless keyword search found them itself.
fn hop_check(q: &Question, resp: &Value) -> Option<String> {
    if let Some(why) = judge(q, resp) {
        return Some(why);
    }
    for hit in resp["knowledge"]["hits"].as_array().unwrap() {
        let via = hit.get("via").and_then(Value::as_str);
        let anchor = hit["match"] == "anchor";
        if anchor != (via == Some("memory")) {
            return Some(format!(
                "{}: match {} with via {via:?}",
                hit["page"], hit["match"]
            ));
        }
    }
    None
}

#[tokio::test]
async fn hop_questions_reach_the_knowledge_their_memory_leads_to() {
    let w = load(None).await;
    let qs = questions(&["hop"]);
    let on = ask(&w, &qs, json!({}), hop_check).await;
    report("hop questions", &on);
    let off = ask(&w, &qs, json!({ "hop": false }), judge).await;
    report("hop questions with hop: false", &off);
    assert_eq!(
        on.passed,
        on.total,
        "{} hop questions failed",
        on.total - on.passed
    );
    // Measured 2026-10-03: without the hop, keyword search alone answers 3 of the 12.
    assert!(
        off.passed <= HOP_OFF_CEILING,
        "the questions no longer need the hop: {} pass without it",
        off.passed
    );
    // Every knowledge page the hop adds is marked as such.
    let mut via = 0;
    for (tenant, groups, q) in &qs {
        let caller = Caller::new(&w.keys[tenant])
            .user(q.user.as_deref())
            .groups(groups);
        let (_, resp) = w
            .app
            .post(
                caller,
                "/search",
                json!({ "query": q.query, "keywords": q.keywords }),
            )
            .await;
        via += resp["knowledge"]["hits"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|h| h["via"] == "memory")
            .count();
    }
    assert!(via >= 9, "only {via} hits came through the hop");
}

const HOP_OFF_CEILING: usize = 3;

#[tokio::test]
async fn semantic_mode_recovers_cross_language_misses() {
    let Some(embedder) = common::embed::real() else {
        println!("PINAKES_TEST_EMBEDDING not set; the meaning-based test set was skipped");
        return;
    };
    let w = load(Some(embedder)).await;
    let xl = questions(&["cross_lang"]);
    let keyword = ask(&w, &xl, json!({ "mode": "keyword" }), judge).await;
    report("cross-language, keyword", &keyword);
    let mut table = Vec::new();
    for mode in ["semantic", "both", "auto"] {
        let out = ask(&w, &xl, json!({ "mode": mode }), judge).await;
        report(&format!("cross-language, {mode}"), &out);
        table.push((mode, out.passed));
    }
    // Every M2 question again in `auto` mode: meaning must not push out what keywords find,
    // and off-topic questions stay weak.
    let m2: Vec<_> = questions(&[
        "experience",
        "expertise",
        "decision",
        "preference",
        "negative",
        "isolation",
    ])
    .into_iter()
    .filter(|(_, _, q)| q.milestone == "M2")
    .collect();
    let auto_m2 = ask(&w, &m2, json!({ "mode": "auto" }), judge).await;
    report("M2 questions, auto", &auto_m2);
    // Off-topic questions find nothing close enough in meaning either.
    let negatives = questions(&["negative"]);
    let semantic_neg = ask(&w, &negatives, json!({ "mode": "semantic" }), |q, resp| {
        judge(q, resp).or_else(|| {
            let shown = slot_pages(resp, "memories").len() + slot_pages(resp, "knowledge").len();
            (shown > 0).then(|| format!("{shown} pages shown"))
        })
    })
    .await;
    report("off-topic questions, semantic", &semantic_neg);
    println!(
        "cross-language passed: keyword {}, {}",
        keyword.passed,
        table
            .iter()
            .map(|(m, n)| format!("{m} {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    assert!(keyword.passed <= 1, "keyword mode should miss these");
    for (mode, passed) in table {
        assert!(
            passed >= CROSS_LANG_FLOOR,
            "{mode}: {passed} < floor {CROSS_LANG_FLOOR}"
        );
    }
    assert_eq!(
        semantic_neg.passed, semantic_neg.total,
        "off-topic questions found pages by meaning"
    );
    assert_eq!(
        auto_m2.passed, auto_m2.total,
        "auto mode changed M2 results"
    );
}

// Measured 2026-10-03 with jina-embeddings-v2-base-zh, default slot sizes.
const CROSS_LANG_FLOOR: usize = 12;
