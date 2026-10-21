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
//! PINAKES_TEST_EMBEDDING=1 cargo test --test integration meaning_testset:: -- --nocapture
//! ```
//!
//! Without `PINAKES_TEST_EMBEDDING` it says so and passes. `PINAKES_TEST_GRAPH_ROWS=1` runs the
//! meaning-based questions with every slot searched through the graph index (`[search]
//! vector_graph_rows`); the answers must be the same. The graph's recall against the exact scan
//! on pages embedded by the real model is measured by
//! `the_graph_finds_what_the_exact_scan_finds_on_real_vectors` (`PINAKES_TEST_GRAPH_PAGES`
//! filler pages made from the test set's lines, 3,000 by default).

use crate::common;

use std::collections::HashMap;
use std::sync::Arc;

use crate::common::app::{Caller, TestApp};
use crate::common::testset::{self, Question, Tally, describe, judge, slot_pages};
use axum::http::StatusCode;
use pinakes::embed::Embedder;
use serde_json::{Value, json};

struct Loaded {
    app: TestApp,
    keys: HashMap<String, String>,
}

async fn load(embedder: Option<Arc<dyn Embedder>>) -> Loaded {
    let extra = match std::env::var("PINAKES_TEST_GRAPH_ROWS") {
        Ok(n) => format!("[search]\nvector_graph_rows = {}\n", n.trim()),
        Err(_) => String::new(),
    };
    let mut app = match embedder {
        Some(e) => TestApp::with_embedder(&extra, e),
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
    for mode in ["semantic", "auto"] {
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

/// The graph index against the exact scan, on vectors the real model made: filler pages built
/// from the test set's titles, headings and lines (as the benchmark's are), the test set's own
/// pages among them, searched with every question of the set.
#[tokio::test]
async fn the_graph_finds_what_the_exact_scan_finds_on_real_vectors() {
    use pinakes::search::SearchSnapshot;
    use pinakes::search::catalog::Slot;
    use pinakes::search::vectors::{self, Earlier, Method};
    use pinakes::wiki::{Page, PagePath, WikiFs};

    let Some(embedder) = common::embed::real() else {
        println!("PINAKES_TEST_EMBEDDING not set; the graph recall test was skipped");
        return;
    };
    let fillers: usize = std::env::var("PINAKES_TEST_GRAPH_PAGES")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(3000);
    let mut real: Vec<Page> = Vec::new();
    for t in testset::tenants() {
        let dir = tempfile::tempdir().unwrap();
        testset::install_wiki(&t.id, dir.path());
        for mut p in WikiFs::new(dir.path()).all_pages().unwrap() {
            if p.path
                .kind()
                .is_some_and(|k| k != pinakes::kinds::Kind::Graph)
            {
                // Each scenario's pages under names of their own; a fixed name (a user's
                // preferences) is left out.
                if let Ok(path) = PagePath::parse(&format!("{}-{}", p.path, t.id)) {
                    p.path = path;
                    real.push(p);
                }
            }
        }
    }
    let (mut titles, mut headings, mut lines) = (Vec::new(), Vec::new(), Vec::new());
    for p in &real {
        titles.extend(p.front.title.clone());
        for l in p.body.lines().map(str::trim).filter(|l| !l.is_empty()) {
            match l.strip_prefix("## ") {
                Some(h) => headings.push(h.to_owned()),
                None if !l.starts_with('#') => lines.push(l.to_owned()),
                None => {}
            }
        }
    }
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut below = |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    let mut pages = real.clone();
    for i in 0..fillers {
        let dir = if below(2) == 0 {
            "experience"
        } else {
            "expertise"
        };
        let mut p = Page::new(PagePath::parse(&format!("{dir}/filler-{i}")).unwrap());
        let title = format!("{} {i}", titles[below(titles.len())]);
        p.front.title = Some(title.clone());
        let mut body = format!("# {title}\n");
        for _ in 0..2 + below(2) {
            body.push_str(&format!("\n## {}\n\n", headings[below(headings.len())]));
            for _ in 0..3 + below(3) {
                body.push_str(&lines[below(lines.len())]);
                body.push('\n');
            }
        }
        p.body = body;
        pages.push(p);
    }
    let snap = SearchSnapshot::build(pages).unwrap();
    let started = std::time::Instant::now();
    let model = embedder.clone();
    let catalog_snap = Arc::new(snap);
    let s2 = catalog_snap.clone();
    let mut v = tokio::task::spawn_blocking(move || {
        vectors::build(&s2.catalog, Earlier::Stored(Vec::new()), &*model)
            .unwrap()
            .vectors
    })
    .await
    .unwrap();
    println!(
        "{} pages, {} rows embedded in {:.0} s",
        catalog_snap.catalog.pages.len(),
        v.len(),
        started.elapsed().as_secs_f64()
    );
    let started = std::time::Instant::now();
    vectors::index(&mut v, &catalog_snap.catalog, None, None, 1).unwrap();
    println!("graphs built in {:.1} s", started.elapsed().as_secs_f64());

    let (mut hits, mut wanted, mut top1) = (0, 0, 0);
    let mut queries = 0;
    for t in testset::tenants() {
        for q in testset::questions(&t.id) {
            let qv = embedder
                .embed(std::slice::from_ref(&q.query))
                .unwrap()
                .remove(0);
            queries += 1;
            for slot in [Slot::Memories, Slot::Knowledge] {
                let keep = |ord: usize| catalog_snap.catalog.pages[ord].slot == slot;
                let (graph, method) = v.nearest(&qv, slot, keep, 50);
                assert_eq!(method, Method::Graph);
                let exact = v.best(&qv, keep);
                let Some(&(_, tenth)) = exact.get(9).or(exact.last()) else {
                    continue;
                };
                wanted += exact.len().min(10);
                hits += graph
                    .iter()
                    .take(10)
                    .filter(|(_, s)| *s >= tenth - 1e-6)
                    .count();
                top1 += usize::from(graph.first().map(|g| g.0) == exact.first().map(|e| e.0));
            }
        }
    }
    let recall = hits as f64 / wanted.max(1) as f64;
    println!(
        "{queries} questions, two slots each: recall@10 {recall:.3} ({hits}/{wanted}), same first page {top1}/{}",
        queries * 2
    );
    assert!(recall >= 0.95, "recall@10 {recall:.3}");
}
