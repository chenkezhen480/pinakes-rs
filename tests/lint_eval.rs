//! Lint with a real model, measured over every scenario of the test set (not a fixture written
//! for it): how many planted problems it finds, and what it reports on wikis nobody planted
//! anything in. Runs only when asked, since it calls the model for real:
//!
//! ```text
//! PINAKES_LINT_EVAL=1 cargo test --test lint_eval -- --nocapture
//! ```
//!
//! `PINAKES_LINT_RUNS` sets the number of runs (3 by default); with `PINAKES_TEST_EMBEDDING`
//! the related pages are found by meaning too, as in a deployment with the model loaded.
//!
//! Each run lints the six scenarios twice: as they are, and with problems planted by code,
//! on pages a seeded generator picks among each scenario's tenant playbooks and expertise pages:
//! - a contradiction: a new note page repeating one line of a page with its number doubled;
//! - a stale claim: a line of a page given a validity date that has passed;
//! - a duplicate: a copy of a page under another title.
//!
//! A planted problem counts as found when a finding of its kind names its pages. Every finding
//! of the model is written to `target/lint-eval/` for reading; the clean runs' findings are what
//! the model reports without anything planted, true problems of the scenarios or not.

mod common;

use std::collections::BTreeMap;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::testset;
use pinakes::config::Config;
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::{Value, json};

/// A small seeded generator, so a run plants the same problems on the same pages.
struct Rng(u64);

impl Rng {
    fn new(seed: &str) -> Self {
        Self(seed.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
        }))
    }
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % n as u64) as usize
    }
}

#[derive(Debug, Clone)]
struct Planted {
    kind: &'static str,
    pages: Vec<String>,
}

fn is_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// Body lines that state something: list items and sentences, not headings or table rules.
fn claim_lines(page: &Page) -> Vec<String> {
    page.body
        .lines()
        .map(str::trim)
        .filter(|l| l.chars().count() > 20 && !l.starts_with('#') && !l.starts_with("|-"))
        .map(str::to_owned)
        .collect()
}

/// The line with its first number (not a date, not a count of times seen) doubled.
fn doubled(line: &str) -> Option<String> {
    let lower = line.to_lowercase();
    if lower.contains("seen ") || line.contains("出现") || line.contains("见过") {
        return None;
    }
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_ascii_digit() && (i == 0 || !chars[i - 1].is_ascii_alphanumeric()) {
            let mut j = i;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let part_of_more = |k: Option<&char>| matches!(k, Some('-' | '/' | '.' | ':' | '年'));
            let before = if i > 0 { chars.get(i - 1) } else { None };
            if !part_of_more(before) && !part_of_more(chars.get(j)) && j - i <= 6 {
                let n: u64 = chars[i..j].iter().collect::<String>().parse().ok()?;
                let mut out: String = chars[..i].iter().collect();
                out.push_str(&(n.max(1) * 2).to_string());
                out.extend(&chars[j..]);
                return Some(out);
            }
            i = j;
        } else {
            i += 1;
        }
    }
    None
}

fn write(wiki: &WikiFs, path: &str, title: &str, from: &Page, body: &str) {
    let mut p = Page::new(PagePath::parse(path).unwrap());
    p.front.kind = from.front.kind;
    p.front.title = Some(title.to_owned());
    p.front.summary = Some(
        body.lines()
            .last()
            .unwrap_or("")
            .chars()
            .take(200)
            .collect(),
    );
    p.front.about = from.front.about.clone();
    p.body = format!("# {title}\n\n{body}\n");
    wiki.write(&p).unwrap();
}

/// Takes one of the pages that `ok` accepts, as the generator picks.
fn pick(tenant: &str, pages: &mut Vec<Page>, rng: &mut Rng, ok: &dyn Fn(&Page) -> bool) -> Page {
    let usable: Vec<usize> = (0..pages.len()).filter(|&i| ok(&pages[i])).collect();
    assert!(!usable.is_empty(), "{tenant}: no page to plant on");
    pages.remove(usable[rng.below(usable.len())])
}

/// Plants one problem of each kind on pages the generator picks.
fn plant(tenant: &str, wiki: &WikiFs) -> Vec<Planted> {
    let mut rng = Rng::new(tenant);
    let mut candidates: Vec<Page> = wiki
        .all_pages()
        .unwrap()
        .into_iter()
        .filter(|p| {
            let s = p.path.as_str();
            s.starts_with("expertise/") || s.starts_with("experience/")
        })
        .collect();
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    let mut out = Vec::new();

    // A contradiction: one line again, with its number doubled, on a note page of its own.
    let p = pick(tenant, &mut candidates, &mut rng, &|p: &Page| {
        claim_lines(p).iter().any(|l| doubled(l).is_some())
    });
    let lines: Vec<String> = claim_lines(&p)
        .into_iter()
        .filter_map(|l| doubled(&l))
        .collect();
    let line = lines[Rng::new(&format!("{tenant}-line")).below(lines.len())].clone();
    let title = p.front.title.clone().unwrap_or_default();
    let note = format!("{}-notes", p.path);
    let note_title = if is_cjk(&title) {
        format!("{title}补充说明")
    } else {
        format!("{title}: notes")
    };
    write(wiki, &note, &note_title, &p, &line);
    out.push(Planted {
        kind: "contradiction",
        pages: vec![p.path.to_string(), note],
    });

    // A stale claim: a line given a validity date that has passed.
    let mut q = pick(tenant, &mut candidates, &mut rng, &|p: &Page| {
        !claim_lines(p).is_empty()
    });
    let lines = claim_lines(&q);
    let line = lines[Rng::new(&format!("{tenant}-stale")).below(lines.len())].clone();
    let (m, d) = (1 + rng.below(8), 1 + rng.below(28));
    let until = if is_cjk(&line) {
        format!("（有效期至 2026年{m}月{d}日）")
    } else {
        format!(" (valid until 2026-{m:02}-{d:02})")
    };
    q.body = q.body.replacen(&line, &format!("{line}{until}"), 1);
    wiki.write(&q).unwrap();
    out.push(Planted {
        kind: "stale",
        pages: vec![q.path.to_string()],
    });

    // A duplicate: the same page under another title.
    let d = pick(tenant, &mut candidates, &mut rng, &|_: &Page| true);
    let title = d.front.title.clone().unwrap_or_default();
    let copy_title = d
        .front
        .aliases
        .first()
        .cloned()
        .unwrap_or_else(|| format!("{title} guide"));
    let copy = format!("{}-guide", d.path);
    let body = d
        .body
        .lines()
        .skip_while(|l| l.starts_with("# ") || l.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    write(wiki, &copy, &copy_title, &d, &body);
    out.push(Planted {
        kind: "duplicate",
        pages: vec![d.path.to_string(), copy],
    });
    out
}

/// Loads every scenario (planting problems when asked), lints each, and returns per tenant
/// the findings and what was planted.
async fn lint_all(
    llm: pinakes::llm::Llm,
    embedder: Option<std::sync::Arc<dyn pinakes::embed::Embedder>>,
    with_problems: bool,
) -> Vec<(String, Vec<Value>, Vec<Planted>)> {
    let mut app = match embedder {
        Some(e) => TestApp::with_llm_and_embedder(llm, e),
        None => TestApp::with_llm(llm),
    };
    let mut keys = Vec::new();
    let mut planted = Vec::new();
    for t in testset::tenants() {
        let key = app.create_tenant(&t.id).await;
        testset::install_wiki(&t.id, &app.wiki_dir(&t.id));
        let wiki = WikiFs::new(app.wiki_dir(&t.id));
        planted.push(if with_problems {
            plant(&t.id, &wiki)
        } else {
            Vec::new()
        });
        keys.push((t, key));
    }
    app.restart();
    let mut jobs = Vec::new();
    for (t, key) in &keys {
        for call in testset::graph_calls(t) {
            let caller = Caller::new(key)
                .user(call.user.as_deref())
                .groups(&call.groups);
            let (status, body) = app.post(caller, call.uri, call.body).await;
            assert!(status.is_success(), "{}: {status} {body}", call.uri);
        }
        let (status, queued) = app.post(Caller::new(key), "/lint", json!({})).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
        jobs.push(queued["job"].as_str().unwrap().to_owned());
    }
    let mut out = Vec::new();
    for (((t, key), job), planted) in keys.iter().zip(jobs).zip(planted) {
        let done = app.wait_job(Caller::new(key), &job).await;
        assert_eq!(done["status"], "done", "{}: {done}", t.id);
        // The whole report, user and group findings too: the admin's view.
        let db = rusqlite::Connection::open(
            app.wiki_dir(&t.id).parent().unwrap().join("pinakes.sqlite"),
        )
        .unwrap();
        let mut stmt = db.prepare("SELECT finding FROM lint_findings").unwrap();
        let findings: Vec<Value> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|f| serde_json::from_str(&f.unwrap()).unwrap())
            .filter(|f: &Value| {
                ["contradiction", "duplicate", "stale"].contains(&f["kind"].as_str().unwrap())
            })
            .collect();
        out.push((t.id.clone(), findings, planted));
    }
    out
}

fn found(p: &Planted, findings: &[Value]) -> bool {
    findings.iter().any(|f| {
        f["kind"] == p.kind
            && p.pages
                .iter()
                .all(|page| f["pages"].as_array().unwrap().iter().any(|x| x == page))
    })
}

fn involves(f: &Value, planted: &[Planted]) -> bool {
    planted
        .iter()
        .flat_map(|p| &p.pages)
        .any(|page| f["pages"].as_array().unwrap().iter().any(|x| x == page))
}

#[tokio::test]
async fn lint_over_every_scenario() {
    if std::env::var_os("PINAKES_LINT_EVAL").is_none() {
        eprintln!("PINAKES_LINT_EVAL is not set; skipping the lint evaluation");
        return;
    }
    let mut cfg = Config::default();
    cfg.load_model_file(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))
        .expect("model.conf");
    let key = cfg.librarian.llm.key();
    let llm = pinakes::llm::client(&cfg.librarian, key).expect("the model and its key");
    let embedder = common::embed::real();
    let runs: usize = std::env::var("PINAKES_LINT_RUNS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(3);
    let out_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/lint-eval");
    std::fs::create_dir_all(&out_dir).unwrap();

    let mut report = Vec::new();
    for run in 1..=runs {
        let clean = lint_all(llm.clone(), embedder.clone(), false).await;
        let planted = lint_all(llm.clone(), embedder.clone(), true).await;
        let mut recall: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        let (mut clean_n, mut other_n) = (0, 0);
        let mut missed = Vec::new();
        for ((tenant, clean_f, _), (_, planted_f, planted_p)) in clean.iter().zip(&planted) {
            clean_n += clean_f.len();
            other_n += planted_f.iter().filter(|f| !involves(f, planted_p)).count();
            for p in planted_p {
                let e = recall.entry(p.kind).or_default();
                e.1 += 1;
                if found(p, planted_f) {
                    e.0 += 1;
                } else {
                    missed.push(format!("{tenant} {} {:?}", p.kind, p.pages));
                }
            }
            for (name, findings) in [("clean", clean_f), ("planted", planted_f)] {
                std::fs::write(
                    out_dir.join(format!("run{run}-{name}-{tenant}.json")),
                    serde_json::to_string_pretty(findings).unwrap(),
                )
                .unwrap();
            }
            if run == 1 {
                std::fs::write(
                    out_dir.join(format!("planted-{tenant}.txt")),
                    planted_p
                        .iter()
                        .map(|p| format!("{} {:?}\n", p.kind, p.pages))
                        .collect::<String>(),
                )
                .unwrap();
            }
        }
        let line = format!(
            "run {run}: found {} | findings on clean wikis {clean_n}, on planted wikis besides the planted {other_n} | missed {missed:?}",
            recall
                .iter()
                .map(|(k, (f, n))| format!("{k} {f}/{n}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        eprintln!("{line}");
        report.push(line);
    }
    std::fs::write(out_dir.join("summary.txt"), report.join("\n") + "\n").unwrap();
}
