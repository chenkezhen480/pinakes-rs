//! Documents filed as expertise: ids issued by the service, documents read like a person reads
//! them (one concept per step, each concept filed as it is read, pages citing the lines they
//! were written from), new versions read again only where they changed, uploads without an id
//! recognized from the wiki, and a stopped reading resumed where it stopped.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::llm::{
    FakeLlm, NOT_THE_SAME, OK, Step, UNREACHABLE, all_ok, candidates, create, cursor_of,
    describe_from_definition, focus_title, folder, items, line_after, read_reply, reader,
    record_of, related_pages, same_as, shown, task_title, update,
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

/// The page each concept of the manual is filed into.
fn page_of(title: &str) -> &'static str {
    match title {
        "七天无理由退货" => "return-window",
        "运费险" => "shipping-insurance",
        "跨境商品退货" => "crossborder-returns",
        _ => "service-scripts",
    }
}

/// A librarian that reads the manual one section per concept (its headings mark where
/// concepts start, as a model would see) and a heading-less document as one concept. It
/// writes each concept's page; for a later version it updates the page when it is shown it,
/// writing a duplicate when it isn't, which the tests would catch. `answer` says which
/// candidate a match is.
fn librarian(answer: impl Fn(&str) -> String + Send + Sync + 'static) -> Arc<FakeLlm> {
    FakeLlm::new(move |step, msg, _| match step {
        Step::Rank | Step::Screen => unreachable!("not asked here"),
        Step::Lint => common::llm::NO_FINDINGS.into(),
        Step::Read => {
            let (lines, _) = shown(msg);
            if !lines.iter().any(|(_, l)| l.starts_with("## ")) && cursor_of(msg) == 1 {
                // No headings: the whole text is one concept.
                let title = line_after(msg, "Title: ");
                let page = format!("{}/{}", common::llm::read_folder(msg), page_of(title));
                let end = lines.last().unwrap().0;
                return read_reply("concept", end, Some(title), Some(&page), Some("话术"));
            }
            reader(
                msg,
                |l| l.starts_with("## "),
                |l| {
                    let t = l.trim_start_matches("## ").to_owned();
                    let p = page_of(&t).to_owned();
                    (t, p)
                },
                // As a model writes it: what the document covers.
                Some(
                    "售后规则手册：平台售后规则，面向客服，含七天无理由退货、运费险和跨境商品退货。",
                ),
            )
        }
        Step::Plan => {
            let dir = folder(msg);
            let edits: Vec<Value> = items(msg)
                .into_iter()
                .map(|it| {
                    let title = task_title(it.task.as_deref().unwrap()).unwrap();
                    let page = format!("{dir}/{}", page_of(&title));
                    let rec = &it.content;
                    let mut edit = if rec["version"] == 1 {
                        let body = match title.as_str() {
                            "跨境商品退货" => format!("# 跨境商品退货\n\n## 规则\n\n{V1}"),
                            t => format!("# {t}\n\n内容。"),
                        };
                        create(&page, &title, "一条规则。", &[], &body)
                    } else {
                        let extract = rec["extract"].as_str().unwrap();
                        let rule = extract.lines().last().unwrap();
                        if it.related.first() == Some(&page) {
                            update(&page, Some("规则"), Some(rule))
                        } else {
                            let again = format!("{page}-again");
                            create(&again, "跨境退货", "重复。", &[], rule)
                        }
                    };
                    edit["item"] = json!(it.id);
                    edit
                })
                .collect();
            json!({ "edits": edits }).to_string()
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

    // Version 1: a new document, read one concept at a time into three pages, and given an
    // id. Its title line is skipped as not knowledge.
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
    assert_eq!(r["pages_created"].as_array().unwrap().len(), 3, "{r}");
    // Sent without an id, its first read takes a small opening window (to recognize it, and,
    // as it is new, as its first window); the next read takes the rest. Each window's concepts
    // are filed as one batch, from their extracts.
    let reads = llm.calls(Step::Read);
    assert_eq!(reads.len(), 2);
    let opening_end = shown(&reads[0]).0.last().unwrap().0;
    assert!(
        !reads[0].contains("this is the end of the document"),
        "a small opening"
    );
    assert!(cursor_of(&reads[1]) > 1 && cursor_of(&reads[1]) <= opening_end + 1);
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 2);
    let filed: Vec<_> = plans.iter().flat_map(|p| items(p)).collect();
    assert_eq!(filed.len(), 3);
    // Each concept is filed from its own extract, never the text again.
    let freight = &filed[1].content;
    assert_eq!(freight["concept"], "运费险");
    assert!(freight["extract"].as_str().unwrap().contains("上限 25 元"));
    assert!(!freight["extract"].as_str().unwrap().contains("七天内"));
    assert!(freight.get("content").is_none() && freight.get("text").is_none());
    assert_eq!(
        llm.calls(Step::Match).len(),
        0,
        "nothing filed yet to compare with"
    );

    // Version 2, with its id: only the step the change touches is read and filed again,
    // with what changed; the steps it doesn't touch are kept without a model call.
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
    let reads = llm.calls(Step::Read);
    assert_eq!(reads.len(), 1, "only the changed step is read again");
    assert!(reads[0].contains("## How version 1 read these lines"));
    assert!(reads[0].contains(&format!("- {V1}")) && reads[0].contains(&format!("+ {V2}")));
    assert!(
        reads[0].contains("售后规则手册：平台售后规则"),
        "the main thread is kept"
    );
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 1);
    let related = related_pages(&plans[0]);
    assert_eq!(related[0], "expertise/crossborder-returns");
    for page in ["expertise/return-window", "expertise/shipping-insurance"] {
        assert!(
            related.contains(&page.to_owned()),
            "{page} not shown: {related:?}"
        );
    }
    assert!(plans[0].contains(&format!("version 2 of document {id}")));
    assert!(
        record_of(&plans[0])["extract"]
            .as_str()
            .unwrap()
            .contains(V2)
    );
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
    assert_eq!(llm.calls(Step::Read).len(), 0, "no model call at all");
    assert_eq!(llm.calls(Step::Plan).len(), 0);
    assert_eq!(r["pages_updated"], json!([]));

    // Version 4 without the id: recognized by what its first read says it is, against the wiki
    // pages filed from earlier versions, and compared with version 3, whose ledger it kept.
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
    // The first read, which recognized it, and the changed step read again.
    assert_eq!(llm.calls(Step::Read).len(), 2);
    let matched = &llm.calls(Step::Match)[0];
    assert_eq!(candidates(matched)[0], id);
    assert!(
        matched.contains("#### expertise/crossborder-returns"),
        "shown as wiki pages"
    );
    // The candidates are shown as their pages, never their raw records: the manual's
    // detailed rules (细则) are in the records, not in any page.
    let shown_pages = matched.split("## Candidates").nth(1).unwrap();
    assert!(!shown_pages.contains("细则"), "never the raw records");

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
        member.audience("group:ops"),
        json!({ "title": "售后规则手册", "content": manual(V1) }),
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

/// A heading-less text of rules, each a paragraph that opens with its name in 【】 and is
/// longer than a small read window.
fn rules_text(names: &[&str]) -> String {
    names
        .iter()
        .map(|n| format!("【{n}】{n}规定如下。\n{}", filler(n, 20)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A plan filing every item of a rules text as its own page; `fail` makes the call fail, as an
/// unreachable model does, when it says so for a batch's titles.
fn rule_plan(msg: &str, fail: impl Fn(&[String]) -> bool) -> String {
    let dir = folder(msg);
    let shown = items(msg);
    let titles: Vec<String> = shown
        .iter()
        .map(|it| task_title(it.task.as_deref().unwrap()).unwrap())
        .collect();
    if fail(&titles) {
        return UNREACHABLE.into();
    }
    let edits: Vec<Value> = shown
        .iter()
        .zip(&titles)
        .map(|(it, title)| {
            let page = format!("{dir}/{}", rule_name(&format!("【{title}】")).1);
            let body = format!(
                "# {title}\n\n{}规定的概括。",
                it.content["concept"].as_str().unwrap()
            );
            let mut e = create(&page, title, "一条规定。", &[], &body);
            e["item"] = json!(it.id);
            e
        })
        .collect();
    json!({ "edits": edits }).to_string()
}

fn rule_name(line: &str) -> (String, String) {
    let name = line
        .trim_start_matches('【')
        .split('】')
        .next()
        .unwrap()
        .to_owned();
    let n = match name.as_str() {
        "甲" => 1,
        "乙" => 2,
        "丙" => 3,
        _ => 4,
    };
    (name, format!("rule-{n}"))
}

/// Code never interprets a document's structure: the model reads from the cursor and says
/// where each concept ends. A concept longer than the window is shown with more text from the
/// same line; an end line outside what was shown is sent back with the reason; every line is
/// read once, in order; the main thread and the ledger travel with every read and write; and
/// each page cites the lines it was written from, which `/read` returns.
#[tokio::test]
async fn documents_are_read_one_concept_at_a_time() {
    let llm = FakeLlm::new(|step, msg, n| match step {
        Step::Rank | Step::Screen => unreachable!("not asked here"),
        // The very first answer ends outside the text shown.
        Step::Read if n == 0 => read_reply("skip", 9_999, None, None, None),
        Step::Lint => common::llm::NO_FINDINGS.into(),
        Step::Read => reader(
            msg,
            |l| l.starts_with('【'),
            rule_name,
            (cursor_of(msg) == 1).then_some("大手册：四条规定，适用于全体商家。"),
        ),
        Step::Plan => rule_plan(msg, |_| false),
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => NOT_THE_SAME.into(),
    });
    let app = TestApp::with_llm_and_config(llm.llm(), "[librarian]\nread_window_tokens = 600\n");
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let text = rules_text(&["甲", "乙", "丙", "丁"]);
    let (_, r) = file(&app, c, json!({ "title": "大手册", "content": text })).await;
    assert_eq!(r["pages_created"].as_array().unwrap().len(), 4, "{r}");

    let reads = llm.calls(Step::Read);
    assert!(
        reads[1].contains("## Why the last answer was rejected")
            && reads[1].contains("end_line 9999 is not between"),
        "{}",
        reads[1]
    );
    // Each concept (21 lines) is longer than the window: the same line is shown again with
    // more text, until the next concept's start is in sight.
    let cursors: Vec<usize> = reads.iter().map(|m| cursor_of(m)).collect();
    let mut starts = cursors.clone();
    starts.dedup();
    assert_eq!(starts, vec![1, 22, 43, 64], "every line once, in order");
    assert!(
        cursors.len() > starts.len() + 1,
        "windows grew: {cursors:?}"
    );
    let grown: Vec<usize> = reads
        .iter()
        .filter(|m| cursor_of(m) == 22)
        .map(|m| shown(m).0.len())
        .collect();
    assert!(grown.windows(2).all(|w| w[1] > w[0]), "{grown:?}");

    // The main thread from the opening, and the ledger, travel with every later read and write.
    let last = reads.last().unwrap();
    assert!(last.contains("大手册：四条规定"));
    assert!(
        last.contains("- L1-21: 甲 → [[expertise/rule-1]]"),
        "{last}"
    );
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 4);
    assert!(plans[3].contains("Main thread: 大手册：四条规定"));
    assert!(plans[3].contains("- L43-63: 丙 → [[expertise/rule-3]]"));
    assert_eq!(record_of(&plans[1])["lines"], "22-42");

    // Pages cite their lines, and /read returns exactly those lines.
    let source = r["source"].as_str().unwrap();
    let (_, page) = app.get(c, "/pages/expertise/rule-2").await;
    let cited = format!("{}#L22-42", source.strip_prefix("sources/").unwrap());
    assert_eq!(page["front_matter"]["sources"], json!([cited]));
    let (_, read) = app
        .post(c, "/read", json!({ "ids": [format!("sources/{cited}")] }))
        .await;
    let lines = read["pages"][0]["text"].as_str().unwrap();
    assert!(lines.starts_with("【乙】乙规定如下。"), "{lines}");
    assert_eq!(lines.lines().count(), 21);
    assert!(!lines.contains("丙"));
    // Only what a page cites: not the whole record, nor other lines of it.
    let record = source.strip_prefix("sources/").unwrap();
    for other in [
        format!("sources/{record}"),
        format!("sources/{record}#L1-84"),
    ] {
        // Not even with a cited id in the same request.
        let ids = json!([format!("sources/{cited}"), other]);
        let (_, read) = app.post(c, "/read", json!({ "ids": ids })).await;
        assert!(read["pages"][0]["text"].is_string(), "{read}");
        assert_eq!(read["pages"][1]["error"], "not_found", "{read}");
    }
    // Hits list the cited lines, ready for /read.
    let (_, hits) = app.post(c, "/search", json!({ "query": "一条规定" })).await;
    let cites = hits["knowledge"]["hits"][0]["sources"].to_string();
    assert!(cites.contains("#L"), "{hits}");
}

/// A reading that stops half way (here the model is unreachable while the third concept is
/// filed) is resumed by the next upload of the same text: the concepts already filed are not
/// read or written again, and the version is recorded under the first upload's record.
#[tokio::test]
async fn a_stopped_reading_resumes_where_it_stopped() {
    let down = Arc::new(AtomicBool::new(true));
    let flag = down.clone();
    let llm = FakeLlm::new(move |step, msg, _| match step {
        Step::Rank | Step::Screen => unreachable!("not asked here"),
        Step::Lint => common::llm::NO_FINDINGS.into(),
        Step::Read => reader(msg, |l| l.starts_with('【'), rule_name, None),
        Step::Plan => rule_plan(msg, |titles| {
            titles.iter().any(|t| t == "丙") && flag.swap(false, Ordering::SeqCst)
        }),
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => NOT_THE_SAME.into(),
    });
    // A small window: one rule per read, so the reading stops between them.
    let app = TestApp::with_llm_and_config(llm.llm(), "[librarian]\nread_window_tokens = 600\n");
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let body = json!({ "title": "规定", "content": rules_text(&["甲", "乙", "丙", "丁"]) });

    let (_, queued) = app.post(c, "/ingest/expertise", body.clone()).await;
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "failed", "{job}");
    assert!(!down.load(Ordering::SeqCst));
    for (page, there) in [("rule-1", true), ("rule-2", true), ("rule-3", false)] {
        let (status, _) = app.get(c, &format!("/pages/expertise/{page}")).await;
        assert_eq!(status == StatusCode::OK, there, "{page}");
    }

    llm.clear();
    let (queued_again, r) = file(&app, c, body).await;
    let reads = llm.calls(Step::Read);
    assert_eq!(cursor_of(&reads[0]), 43, "from the third concept on");
    let titles: Vec<String> = llm
        .calls(Step::Plan)
        .iter()
        .filter_map(|m| focus_title(m))
        .collect();
    assert_eq!(titles, vec!["丙", "丁"]);
    assert_eq!(r["version"], 1);
    assert_eq!(r["document_match"], "new");
    assert_eq!(
        r["pages_created"],
        json!(["expertise/rule-3", "expertise/rule-4"])
    );
    assert_ne!(queued["job"], queued_again["job"], "a second job");
    // All four pages cite the first upload's record, which is the version filed.
    let source = r["source"]
        .as_str()
        .unwrap()
        .strip_prefix("sources/")
        .unwrap();
    for (page, lines) in [("rule-1", "1-21"), ("rule-4", "64-84")] {
        let (_, p) = app.get(c, &format!("/pages/expertise/{page}")).await;
        assert_eq!(
            p["front_matter"]["sources"],
            json!([format!("{source}#L{lines}")])
        );
    }
}

/// An edit linking to a page whose creation the verifier holds is held with it, not written
/// with a link to nothing; approved, both are written, citing the concept's lines.
#[tokio::test]
async fn edits_linking_to_a_held_page_are_held_with_it() {
    let llm = FakeLlm::new(|step, msg, _| match step {
        Step::Rank | Step::Screen => unreachable!("not asked here"),
        Step::Lint => common::llm::NO_FINDINGS.into(),
        Step::Read => reader(msg, |l| l.starts_with('【'), rule_name, None),
        Step::Plan => {
            let b = create(
                "expertise/rule-b",
                "乙",
                "一条规定。",
                &[],
                "# 乙\n\n概括。",
            );
            let a_body = "# 甲\n\n见 [[expertise/rule-b]]。";
            let a = create("expertise/rule-a", "甲", "一条规定。", &[], a_body);
            let more = update("expertise/rule-b", Some("补充"), Some("一条补充。"));
            json!({ "edits": [b, a, more] }).to_string()
        }
        Step::Verify => {
            // Only the creation of rule-b is objected to.
            let v: Vec<Value> = common::llm::asked_edits(msg)
                .into_iter()
                .map(|i| {
                    let reason = (i == 0).then_some("unsupported");
                    json!({ "edit": i, "ok": i != 0, "reason": reason })
                })
                .collect();
            json!({ "verdicts": v }).to_string()
        }
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => NOT_THE_SAME.into(),
    });
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let body = json!({ "title": "规定", "content": rules_text(&["甲"]) });
    let (_, r) = file(&app, c, body).await;
    assert_eq!(r["pages_created"], json!([]), "{r}");
    let (_, list) = app.get(c, "/proposals").await;
    let edits = &list["proposals"][0]["edits"];
    assert_eq!(edits.as_array().unwrap().len(), 3, "{list}");
    for i in [1, 2] {
        let reason = edits[i]["reason"].as_str().unwrap();
        assert!(reason.contains("held for review"), "{reason}");
    }
    let record = r["source"].as_str().unwrap();
    let cited = format!("{}#L1-21", record.strip_prefix("sources/").unwrap());
    // One proposal per concept, citing its lines.
    assert_eq!(list["proposals"][0]["source"], cited.as_str());

    let proposal = list["proposals"][0]["proposal"].as_str().unwrap();
    let (status, body) = app
        .post_admin(&format!("/proposals/{proposal}/approve"), json!({}))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    for page in ["rule-a", "rule-b"] {
        let (_, p) = app.get(c, &format!("/pages/expertise/{page}")).await;
        assert_eq!(p["front_matter"]["sources"], json!([cited]));
    }
    let (_, b) = app.get(c, "/pages/expertise/rule-b").await;
    assert!(b["body"].as_str().unwrap().contains("一条补充。"));
}

/// The concepts of one window are filed as one batch, and each is retried on its own: the one
/// the verifier objects to is sent again alone, with why, while the other is written at once.
#[tokio::test]
async fn items_are_retried_on_their_own() {
    let llm = FakeLlm::new(|step, msg, n| match step {
        Step::Rank | Step::Screen => unreachable!("not asked here"),
        Step::Lint => common::llm::NO_FINDINGS.into(),
        Step::Read => reader(msg, |l| l.starts_with('【'), rule_name, Some("规定两条。")),
        Step::Plan => rule_plan(msg, |_| false),
        Step::Verify => {
            // The first time, the second rule's edit is objected to.
            let v: Vec<Value> = common::llm::asked_edits(msg)
                .into_iter()
                .map(|k| {
                    let second = msg.contains(&format!("### Edit {k} (item i2)"));
                    let ok = n > 0 || !second;
                    json!({ "edit": k, "ok": ok, "reason": (!ok).then_some("not in the extract") })
                })
                .collect();
            json!({ "verdicts": v }).to_string()
        }
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => NOT_THE_SAME.into(),
    });
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let (_, r) = file(
        &app,
        c,
        json!({ "title": "两条规定", "content": rules_text(&["甲", "乙"]) }),
    )
    .await;
    assert_eq!(
        r["pages_created"],
        json!(["expertise/rule-1", "expertise/rule-2"]),
        "{r}"
    );
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 2);
    let ids = |m: &str| items(m).into_iter().map(|i| i.id).collect::<Vec<_>>();
    assert_eq!(ids(&plans[0]), vec!["i1", "i2"], "one batch");
    assert_eq!(ids(&plans[1]), vec!["i2"], "only the refused item, again");
    assert!(plans[1].contains("not in the extract"));
    assert_eq!(llm.calls(Step::Read).len(), 1, "the text read once");
}

/// A concept is filed as the page its read named, which the ledger records: a plan that puts it
/// under another name is sent back.
#[tokio::test]
async fn a_concept_goes_to_the_page_its_read_named() {
    let llm = FakeLlm::new(|step, msg, n| match step {
        Step::Rank | Step::Screen => unreachable!("not asked here"),
        Step::Lint => common::llm::NO_FINDINGS.into(),
        Step::Read => reader(msg, |l| l.starts_with('【'), rule_name, None),
        Step::Plan if n == 0 => {
            let edit = create(
                "expertise/some-other-name",
                "甲",
                "一条规定。",
                &[],
                "# 甲\n\n概括。",
            );
            json!({ "edits": [edit] }).to_string()
        }
        Step::Plan => rule_plan(msg, |_| false),
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => NOT_THE_SAME.into(),
    });
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let (_, r) = file(
        &app,
        c,
        json!({ "title": "规定", "content": rules_text(&["甲"]) }),
    )
    .await;
    assert_eq!(r["pages_created"], json!(["expertise/rule-1"]), "{r}");
    let plans = llm.calls(Step::Plan);
    assert_eq!(plans.len(), 2);
    assert!(plans[1].contains("the concept is filed as `expertise/rule-1`"));
}
