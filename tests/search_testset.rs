//! M2 "done when": the test set's questions come back in the right slot, in both languages.
//!
//! Every M2 question of `testset/questions/` is sent to `POST /search` as its caller (user and
//! groups headers), in keyword mode with the default slot sizes. A question passes when every
//! expected page is in its slot, nothing in `must_not` appears anywhere in the response, and
//! every slot in `expect_empty` is empty or weak (testset/README.md, "Questions").
//! Run with `-- --nocapture` for the per-category report.

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::testset::{self, Question, Tally};
use serde_json::{Value, json};

fn slot_pages(resp: &Value, slot: &str) -> Vec<String> {
    resp[slot]["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|h| h["page"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn slot_is_empty_or_weak(resp: &Value, slot: &str) -> bool {
    let s = &resp[slot];
    let empty = match slot {
        "relations" => s["edges"].as_array().is_none_or(|e| e.is_empty()),
        _ => s["hits"].as_array().is_none_or(|h| h.is_empty()),
    };
    empty || s["coverage"] == "weak"
}

/// Why a question failed, or `None` when it passed.
fn judge(q: &Question, resp: &Value) -> Option<String> {
    let mut problems = Vec::new();
    for (slot, expected) in [
        ("memories", &q.expect.memories),
        ("knowledge", &q.expect.knowledge),
    ] {
        let got = slot_pages(resp, slot);
        for page in expected {
            if !got.contains(&testset::page_path(page)) {
                problems.push(format!("{slot} lacks {page}"));
            }
        }
    }
    let text = resp.to_string();
    for page in &q.must_not {
        if text.contains(&testset::page_path(page)) {
            problems.push(format!("must_not {page} appears"));
        }
    }
    for slot in &q.expect_empty {
        if !slot_is_empty_or_weak(resp, slot) {
            problems.push(format!("{slot} should be empty or weak"));
        }
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

fn describe(resp: &Value) -> String {
    let mut out = String::new();
    for slot in ["memories", "knowledge"] {
        let hits: Vec<String> = resp[slot]["hits"]
            .as_array()
            .map(|hs| {
                hs.iter()
                    .map(|h| {
                        format!(
                            "{} [{}; {}]",
                            h["page"].as_str().unwrap_or(""),
                            h["match"].as_str().unwrap_or(""),
                            h["matched_keywords"]
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "\n      {slot} ({}): {}",
            resp[slot]["coverage"].as_str().unwrap_or("?"),
            hits.join(" | ")
        ));
    }
    out.push_str(&format!(
        "\n      unmatched: {}",
        resp["unmatched_keywords"]
    ));
    out
}

struct Outcome {
    passed: usize,
    total: usize,
    /// Hits that came back with `match: alias`, in every slot of every question.
    alias_hits: usize,
    /// Expected pages ranked first in their slot, out of all expected pages.
    first: usize,
    expected: usize,
    by_category: Tally,
    by_lang: Tally,
    failures: Vec<String>,
}

/// Loads every scenario into a fresh service with `extra_config`, then asks every M2
/// question. `with_keywords: false` sends only the query, as an agent that writes no keywords.
async fn run_m2(extra_config: &str, with_keywords: bool) -> Outcome {
    let mut app = TestApp::with_config(extra_config);
    let tenants = testset::tenants();
    let mut keys = HashMap::new();
    for t in &tenants {
        keys.insert(t.id.clone(), app.create_tenant(&t.id).await);
        testset::install_wiki(&t.id, &app.wiki_dir(&t.id));
    }
    // Load the installed pages the way a restarted service would.
    app.restart();

    let mut out = Outcome {
        passed: 0,
        total: 0,
        alias_hits: 0,
        first: 0,
        expected: 0,
        by_category: Tally::default(),
        by_lang: Tally::default(),
        failures: Vec::new(),
    };
    for t in &tenants {
        for q in testset::questions(&t.id) {
            if q.milestone != "M2" {
                continue;
            }
            let groups = q
                .user
                .as_deref()
                .map(|u| t.groups_of(u))
                .unwrap_or_default();
            let caller = Caller::new(&keys[&t.id])
                .user(q.user.as_deref())
                .groups(&groups);
            let mut body = json!({ "query": q.query });
            if with_keywords && !q.keywords.is_empty() {
                body["keywords"] = json!(q.keywords);
            }
            let (status, resp) = app.post(caller, "/search", body).await;
            assert_eq!(status, StatusCode::OK, "{}: {resp}", q.id);
            let verdict = judge(&q, &resp);
            out.alias_hits += resp.to_string().matches(r#""match":"alias""#).count();
            for (slot, pages) in [
                ("memories", &q.expect.memories),
                ("knowledge", &q.expect.knowledge),
            ] {
                for page in pages {
                    out.expected += 1;
                    if slot_pages(&resp, slot).first() == Some(&testset::page_path(page)) {
                        out.first += 1;
                    }
                }
            }
            out.total += 1;
            out.by_category.add(&q.category, verdict.is_none());
            out.by_lang.add(&q.lang, verdict.is_none());
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
    }
    out
}

#[tokio::test]
async fn m2_questions_come_back_in_the_right_slot() {
    let out = run_m2("", true).await;
    println!(
        "M2 questions by category:
{}",
        out.by_category.lines().join(
            "
"
        )
    );
    println!(
        "by language:
{}",
        out.by_lang.lines().join(
            "
"
        )
    );
    if !out.failures.is_empty() {
        println!(
            "failures:
{}",
            out.failures.join(
                "
"
            )
        );
    }
    assert!(
        out.failures.is_empty(),
        "{} M2 questions failed",
        out.failures.len()
    );
}

/// How much each part of keyword search carries, measured on the same questions. Most
/// expected pages are reached by an alias pin, so the runs without pins show what the
/// tokenizers and BM25 do on their own. The floors guard against regressions; raise them when
/// search improves. Run with `-- --nocapture` for the table and the misses.
#[tokio::test]
async fn m2_ablations() {
    let no_pins = "[search.ablation]
pin_names = false
";
    let runs = [
        ("full", "", true, 149),
        ("no name pins", no_pins, true, NO_PINS_FLOOR),
        (
            "no pins, words only",
            "[search.ablation]
pin_names = false
bigrams = false
",
            true,
            NO_PINS_WORDS_FLOOR,
        ),
        (
            "no pins, bigrams only",
            "[search.ablation]
pin_names = false
words = false
",
            true,
            NO_PINS_BIGRAMS_FLOOR,
        ),
        ("query only", "", false, QUERY_ONLY_FLOOR),
        (
            "query only, no pins",
            no_pins,
            false,
            QUERY_ONLY_NO_PINS_FLOOR,
        ),
    ];
    let mut table = Vec::new();
    let mut below = Vec::new();
    for (name, cfg, with_keywords, floor) in runs {
        let out = run_m2(cfg, with_keywords).await;
        table.push(format!(
            "  {name:<24} {:>3}/{}  ranked first {:>3}/{}  alias hits {:>3}   {}",
            out.passed,
            out.total,
            out.first,
            out.expected,
            out.alias_hits,
            out.by_lang
                .lines()
                .iter()
                .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        if name != "full" && !out.failures.is_empty() {
            println!(
                "misses, {name}:
{}",
                out.failures.join(
                    "
"
                )
            );
        }
        if cfg.contains("pin_names = false") {
            assert_eq!(out.alias_hits, 0, "{name}: pins are switched off");
        }
        if out.passed < floor {
            below.push(format!("{name}: {} < floor {floor}", out.passed));
        }
    }
    println!(
        "M2 ablations:
{}",
        table.join(
            "
"
        )
    );
    assert!(below.is_empty(), "{}", below.join("; "));
}

// Measured 2026-09-30. Without pins the set still passes in full: its keywords mostly equal the
// expected pages' aliases, which BM25 ranks first on its own.
const NO_PINS_FLOOR: usize = 149;
const NO_PINS_WORDS_FLOOR: usize = 149;
const NO_PINS_BIGRAMS_FLOOR: usize = 149;
// The six misses are first-person preference questions; abstracting them into keywords is the
// calling agent's job (design, "Tool descriptions teach the strategy").
const QUERY_ONLY_FLOOR: usize = 143;
const QUERY_ONLY_NO_PINS_FLOOR: usize = 143;
