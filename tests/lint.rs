//! M8 "done when", lint: findings with page links, for every kind of problem, each visible only
//! to callers who may see its pages; fixes held as proposals; later runs review only what
//! changed and keep what they didn't review again.
//!
//! The model is scripted ([`common::llm::FakeLlm`]): it reports the contradiction and the stale
//! claim planted here and proposes the fixes it is asked for. What a real model finds is the
//! subject of `tests/librarian_live.rs`.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::llm::{FakeLlm, OK, Step, all_ok, describe_from_definition};
use pinakes::tenants::user_dir_name;
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::{Value, json};

const SHORT: &str = "Customers may return items within 7 days of delivery.";
const LONG: &str = "Customers may return items within 30 days of delivery.";
const PROMO: &str = "The spring promotion is valid until 2026-03-31.";

/// The tasks a lint message asks about: id, kind, the page ids, and the keyword of a miss.
fn tasks(msg: &str) -> Vec<(String, String, Vec<String>, String)> {
    msg.split("\n### Task ")
        .skip(1)
        .map(|part| {
            let head = part.lines().next().unwrap();
            let (id, kind) = head.split_once(": ").unwrap();
            let line = |label: &str| {
                part.lines()
                    .find_map(|l| l.strip_prefix(label))
                    .unwrap_or("")
                    .to_owned()
            };
            let pages = if kind == "anchor" {
                vec![line("The memory page without anchors: ")]
            } else {
                line("Pages: ").split(", ").map(str::to_owned).collect()
            };
            (id.to_owned(), kind.to_owned(), pages, line("Keyword: "))
        })
        .collect()
}

fn update(page: &str) -> Value {
    json!({ "op": "update", "page": page, "heading": null, "text": null, "summary": null,
            "add_aliases": [], "add_keywords": [], "add_about": [] })
}

/// Finds what is planted and fixes what it is asked to.
fn linter() -> Arc<FakeLlm> {
    FakeLlm::new(|step, msg, _| match step {
        Step::Rank | Step::Screen => unreachable!("not asked here"),
        Step::Lint => {
            let mut findings = Vec::new();
            for (id, kind, pages, keyword) in tasks(msg) {
                let has = |p: &str| pages.iter().any(|x| x == p);
                match kind.as_str() {
                    "review"
                        if has("expertise/return-window") && has("expertise/return-policy") =>
                    {
                        findings.push(json!({ "task": id, "type": "contradiction",
                            "pages": ["expertise/return-window", "expertise/return-policy"],
                            "claims": [{ "page": "expertise/return-window", "text": SHORT },
                                       { "page": "expertise/return-policy", "text": LONG },
                                       { "page": "expertise/return-policy", "text": "made up" }],
                            "reason": "7 days or 30 days?", "problem": true,
                            // Ignored: a contradiction is for a person to decide.
                            "edits": [{ "op": "supersede", "page": "expertise/return-policy",
                                        "claim": LONG, "replacement": null, "reason": "x" }] }));
                    }
                    "review" if has("expertise/promo") => {
                        findings.push(json!({ "task": id, "type": "stale",
                            "pages": ["expertise/promo"],
                            "claims": [{ "page": "expertise/promo", "text": PROMO }],
                            "reason": "The promotion ended on 2026-03-31.", "problem": true,
                            "edits": [{ "op": "supersede", "page": "expertise/promo",
                                        "claim": PROMO, "replacement": null,
                                        "reason": "the promotion has ended" }] }));
                    }
                    "review" if has("expertise/unknown-claims") => {
                        // A claim not on the page: not a finding.
                        findings.push(json!({ "task": id, "type": "stale",
                            "pages": ["expertise/unknown-claims"],
                            "claims": [{ "page": "expertise/unknown-claims", "text": "not there" }],
                            "reason": "?", "problem": true, "edits": [] }));
                        // Thought better of: not a finding.
                        findings.push(json!({ "task": id, "type": "duplicate",
                            "pages": ["expertise/unknown-claims", "expertise/promo"],
                            "claims": [], "reason": "They only overlap.", "problem": false,
                            "edits": [] }));
                    }
                    "anchor" => {
                        let mut e = update(&pages[0]);
                        e["add_about"] = json!(["commerce:order"]);
                        findings.push(json!({ "task": id, "type": "fix", "pages": pages,
                            "claims": [], "reason": "It is about orders.", "problem": true,
                            "edits": [e] }));
                    }
                    "miss" => {
                        let mut e = update(&pages[0]);
                        e["add_aliases"] = json!([keyword]);
                        findings.push(json!({ "task": id, "type": "fix", "pages": [pages[0]],
                            "claims": [], "reason": "It covers this.", "problem": true,
                            "edits": [e] }));
                    }
                    _ => {}
                }
            }
            json!({ "findings": findings }).to_string()
        }
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => common::llm::NOT_THE_SAME.into(),
        Step::Plan => common::llm::NO_EDITS.into(),
        Step::Read => common::llm::skip_to_end(msg),
    })
}

fn page(path: &str, title: &str, about: &[&str], sources: &[&str], body: &str) -> Page {
    let mut p = Page::new(PagePath::parse(path).unwrap());
    p.front.kind = p.path.kind();
    p.front.title = Some(title.into());
    p.front.summary = Some(format!("About {title}."));
    p.front.about = about.iter().map(|a| (*a).into()).collect();
    p.front.sources = sources.iter().map(|s| (*s).into()).collect();
    p.body = format!("# {title}\n\n{body}\n");
    p
}

async fn ok(app: &TestApp, caller: Caller<'_>, uri: &str, body: Value) -> Value {
    let (status, resp) = app.post(caller, uri, body).await;
    assert!(status.is_success(), "{uri}: {status} {resp}");
    resp
}

/// Runs lint and returns the job and the report as `caller` sees it.
async fn lint(app: &TestApp, caller: Caller<'_>) -> (Value, Value) {
    app.state.traces.flush();
    let (status, resp) = app.post(caller, "/lint", json!({})).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resp}");
    let job = app.wait_job(caller, resp["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    let (status, report) = app.get(caller, "/lint").await;
    assert_eq!(status, StatusCode::OK);
    (job, report)
}

fn of_kind<'a>(report: &'a Value, kind: &str) -> Vec<&'a Value> {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["kind"] == kind)
        .collect()
}

#[tokio::test]
async fn lint_reports_findings_and_proposes_fixes() {
    let llm = linter();
    let mut app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let host = Caller::new(&key);
    let lily = host.user(Some("lily"));
    let (status, report) = app.get(host, "/lint").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        report,
        json!({ "findings": [] }),
        "nothing before the first run"
    );

    ok(
        &app,
        host,
        "/ingest/graph/schema",
        json!({ "schema": "commerce", "summary": "Orders and buyers.",
                "nodes": { "order": { "key": "order_id" }, "customer": { "key": "customer_id" } },
                "relations": { "placed_by": { "from": "order", "to": "customer" } } }),
    )
    .await;
    ok(
        &app,
        host,
        "/ingest/graph/space",
        json!({ "space": "mainland", "schema": "commerce" }),
    )
    .await;
    ok(
        &app,
        host,
        "/ingest/graph/data",
        json!({ "space": "mainland",
                "nodes": [{ "type": "customer", "key": "c1", "name": "Northwind Traders" }] }),
    )
    .await;

    let wiki = WikiFs::new(app.wiki_dir("acme"));
    let lily_page = format!("users/{}/decisions/refunds", user_dir_name("lily"));
    let orphan = "2026-01-01-experience-deadbeef.json";
    let gone = "2026-01-02-decision-0badf00d.json";
    for p in [
        page("expertise/return-window", "Return window", &[], &[], SHORT),
        page(
            "expertise/return-policy",
            "Return policy",
            &[],
            &[],
            &format!("{LONG}\n\nSee [[expertise/return-window]]."),
        ),
        page("expertise/promo", "Spring promotion", &[], &[], PROMO),
        page(
            "expertise/unknown-claims",
            "Gift cards",
            &["commerce:ghost", "mainland:customer/c1"],
            &[gone],
            "Gift cards never expire. See [[expertise/nowhere]].",
        ),
        page(
            "experience/order-lookup",
            "Looking up orders",
            &[],
            &[],
            "Search by the order id first.",
        ),
        page(
            &lily_page,
            "Lily's refund rule",
            &["commerce:order"],
            &[],
            "Refund at once. See [[expertise/missing]].",
        ),
        page(
            "expertise/guarantee",
            "Guarantee terms",
            &[],
            &[],
            "The guarantee lasts one year.",
        ),
    ] {
        wiki.write(&p).unwrap();
    }
    std::fs::write(app.tenant_dir("acme").join("sources").join(orphan), "{}").unwrap();
    app.restart();

    // Searches that name a node by part of its name.
    for _ in 0..3 {
        ok(
            &app,
            host,
            "/search",
            json!({ "query": "Northwind", "keywords": ["Northwind"] }),
        )
        .await;
    }
    // And searches that came back weak, as their trace lines say.
    app.state.traces.flush();
    let today = &pinakes::trace::now_rfc3339()[..10];
    let traces = app
        .tenant_dir("acme")
        .join("logs")
        .join(format!("search-{today}.jsonl"));
    let weak = json!({ "v": 1, "at": format!("{today}T00:00:00.000Z"), "endpoint": "search",
        "request": { "query": "guarantee period?", "keywords": ["guarantee period"] },
        "result": { "memories": { "coverage": "weak", "hits": [] },
                    "knowledge": { "coverage": "weak", "hits": [] },
                    "unmatched_keywords": ["guarantee period"], "partial_names": [] } });
    let mut lines = std::fs::read_to_string(&traces).unwrap();
    // Lily's own weak searches are hers alone.
    let mut hers = weak.clone();
    hers["user"] = json!(user_dir_name("lily"));
    hers["request"]["keywords"] = json!(["private ailment"]);
    hers["result"]["unmatched_keywords"] = json!(["private ailment"]);
    for _ in 0..3 {
        lines.push_str(&format!("{weak}\n{hers}\n"));
    }
    std::fs::write(&traces, lines).unwrap();

    let (job, report) = lint(&app, host).await;
    let run = &job["result"]["lint"];
    assert_eq!(report["run"]["at"], run["at"], "{report}");
    assert!(run["pages_reviewed"].as_u64().unwrap() >= 6, "{run}");

    assert!(
        of_kind(&report, "duplicate").is_empty(),
        "thought better of, or of two kinds: {report}"
    );
    let contradiction = of_kind(&report, "contradiction");
    assert_eq!(contradiction.len(), 1, "{report}");
    assert_eq!(
        contradiction[0]["pages"],
        json!(["expertise/return-window", "expertise/return-policy"])
    );
    assert_eq!(
        contradiction[0]["claims"].as_array().unwrap().len(),
        2,
        "a claim not on its page is dropped: {}",
        contradiction[0]
    );
    assert!(contradiction[0].get("proposal").is_none(), "only reported");
    let stale = of_kind(&report, "stale");
    assert_eq!(stale.len(), 1, "{report}");
    assert_eq!(stale[0]["pages"], json!(["expertise/promo"]));
    let stale_fix = stale[0]["proposal"].as_str().unwrap().to_owned();

    let one = |kind: &str| {
        let f = of_kind(&report, kind);
        assert_eq!(f.len(), 1, "{kind}: {report}");
        f[0].clone()
    };
    let broken = one("broken_link");
    assert_eq!(broken["pages"], json!(["expertise/unknown-claims"]));
    assert_eq!(broken["target"], "expertise/nowhere");
    let bad = one("bad_anchor");
    assert_eq!(bad["target"], "commerce:ghost", "{bad}");
    assert_eq!(one("missing_source")["target"], format!("sources/{gone}"));
    assert_eq!(one("orphan_record")["target"], format!("sources/{orphan}"));
    let unanchored = one("unanchored");
    assert_eq!(unanchored["pages"], json!(["experience/order-lookup"]));
    let anchor_fix = unanchored["proposal"].as_str().unwrap().to_owned();
    let miss = of_kind(&report, "frequent_miss")
        .into_iter()
        .find(|f| f["target"] == "guarantee period")
        .unwrap_or_else(|| panic!("{report}"))
        .clone();
    assert_eq!(miss["count"], 3);
    let miss_fix = miss["proposal"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "{miss}
{}",
                llm.calls(Step::Lint).join(
                    "
=====
"
                )
            )
        })
        .to_owned();
    let partial = one("partial_name");
    assert_eq!(partial["target"], "northwind", "as the trace has it");
    assert_eq!(partial["nodes"], json!(["mainland:customer/c1"]));

    // Lily's page's broken link is hers to see.
    let (_, mine) = app.get(lily, "/lint").await;
    let theirs = of_kind(&mine, "broken_link");
    assert_eq!(theirs.len(), 2, "{mine}");
    assert!(theirs.iter().any(|f| f["pages"] == json!([lily_page])));
    let private = |r: &Value| {
        of_kind(r, "frequent_miss")
            .iter()
            .any(|f| f["target"] == "private ailment")
    };
    assert!(private(&mine), "{mine}");
    assert!(!private(&report), "another user's search terms: {report}");

    // The fixes wait for approval, then apply.
    let (_, held) = app.get(host, "/proposals").await;
    let held: Vec<&str> = held["proposals"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["proposal"].as_str().unwrap())
        .collect();
    for id in [&stale_fix, &anchor_fix, &miss_fix] {
        assert!(held.contains(&id.as_str()), "{id} in {held:?}");
    }
    for id in [&anchor_fix, &miss_fix] {
        let (status, body) = app
            .post_admin(&format!("/proposals/{id}/approve"), json!({}))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let lookup = wiki
        .read(&PagePath::parse("experience/order-lookup").unwrap())
        .unwrap();
    assert_eq!(lookup.front.about, vec!["commerce:order"]);
    let guarantee = wiki
        .read(&PagePath::parse("expertise/guarantee").unwrap())
        .unwrap();
    assert!(
        guarantee
            .front
            .aliases
            .contains(&"guarantee period".to_owned())
    );

    // The next run reviews only what changed: the two approved pages. The contradiction it
    // didn't review again stays, and the stale claim's fix is still the one held.
    llm.clear();
    let (job, report) = lint(&app, host).await;
    assert_eq!(job["result"]["lint"]["pages_reviewed"], 2, "{job}");
    assert_eq!(of_kind(&report, "contradiction").len(), 1, "{report}");
    assert_eq!(of_kind(&report, "stale")[0]["proposal"], stale_fix);
    assert!(of_kind(&report, "unanchored").is_empty(), "{report}");
    assert!(
        !of_kind(&report, "frequent_miss")
            .iter()
            .any(|f| f["target"] == "guarantee period"),
        "a page has taken the keyword since: {report}"
    );
    assert!(
        job["result"]["lint"]["proposals"] == 0,
        "{job}
{report}
{}",
        llm.calls(Step::Lint).join(
            "
=====
"
        )
    );

    // A changed page is reviewed again, with what it relates to.
    wiki.write(&page(
        "expertise/return-policy",
        "Return policy",
        &[],
        &[],
        "Customers may return items within 7 days of delivery.",
    ))
    .unwrap();
    app.state.tenants.open_tenants().await[0]
        .reindex()
        .await
        .unwrap();
    llm.clear();
    let (job, _) = lint(&app, host).await;
    assert_eq!(job["result"]["lint"]["pages_reviewed"], 1, "{job}");
    let asked = llm.calls(Step::Lint);
    assert!(
        asked.iter().any(|m| m.contains("expertise/return-window")),
        "reviewed with its related page"
    );
}

/// Lint is the librarian's work: without it, 503, as for every write.
#[tokio::test]
async fn no_librarian_means_503() {
    let app = TestApp::without_llm();
    let key = app.create_tenant("acme").await;
    let (status, body) = app.post(Caller::new(&key), "/lint", json!({})).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
}
