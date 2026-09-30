//! Search traces and replay (design, "Search traces and replay"): every `/search` and `/read`
//! leaves one line in the tenant's trace file, with folder names instead of raw ids, and a
//! replay reruns the recorded searches and reports what changed.

mod common;

use std::collections::HashMap;
use std::time::Duration;

use common::app::{Caller, TestApp};
use common::testset;
use kb::tenants::{Tenants, group_dir_name, tenant_dir_name, user_dir_name};
use kb::trace::replay::{TraceReader, WikiSource, replay, trace_files};
use kb::trace::{Endpoint, TraceRecord};
use kb::wiki::{Page, PagePath, WikiFs};
use serde_json::json;

async fn shopline() -> (TestApp, String) {
    let mut app = TestApp::new();
    let key = app.create_tenant("shopline").await;
    testset::install_wiki("shopline", &app.wiki_dir("shopline"));
    app.restart();
    (app, key)
}

fn traces(app: &TestApp) -> Vec<TraceRecord> {
    app.state.traces.flush();
    let mut reader = TraceReader::new(trace_files(&[app.config().tenants_dir()], Some(7)).unwrap());
    let records: Vec<TraceRecord> = reader.by_ref().collect();
    assert_eq!((reader.bad_lines, reader.errors.len()), (0, 0));
    records
}

#[tokio::test]
async fn searches_and_reads_leave_one_line_each() {
    let (app, key) = shopline().await;
    let groups = vec!["ops".to_string()];
    let caller = Caller::new(&key).user(Some("lily")).groups(&groups);
    app.post(
        caller,
        "/search",
        json!({ "query": "部分退款怎么处理", "keywords": ["部分退款"], "session": "s1" }),
    )
    .await;
    app.post(
        caller,
        "/read",
        json!({ "ids": ["expertise/refund-regulations", "expertise/nope"], "session": "s1" }),
    )
    .await;

    let records = traces(&app);
    assert_eq!(records.len(), 2);
    let (search, read) = (&records[0], &records[1]);

    assert_eq!(search.endpoint, Endpoint::Search);
    assert_eq!(search.tenant, tenant_dir_name("shopline"));
    assert_eq!(search.user, Some(user_dir_name("lily")));
    assert_eq!(search.groups, vec![group_dir_name("ops")]);
    assert_eq!(search.request["keywords"], json!(["部分退款"]));
    assert_eq!(
        search.result["memories"]["hits"][0],
        json!({ "page": "experience/partial-refund-after-shipment", "match": "alias" })
    );
    assert!(search.snapshot.is_some());
    assert_eq!(search.service, kb::SERVICE);
    assert_eq!(search.search_config, app.config().search.fingerprint());
    for step in [
        "prepare",
        "name_match",
        "keyword_search_memories",
        "assemble",
        "total",
    ] {
        assert!(search.timings_us.contains_key(step), "{step}");
    }

    assert_eq!(read.endpoint, Endpoint::Read);
    assert_eq!(read.result["pages"][1]["error"], "not_found");

    // Raw user and group ids never reach the file.
    let text = std::fs::read_to_string(kb::trace::trace_file(
        &app.config().tenants_dir(),
        &tenant_dir_name("shopline"),
        &search.at[..10],
    ))
    .unwrap();
    assert!(
        !text.contains("\"lily\"") && !text.contains("\"ops\""),
        "{text}"
    );
}

/// Replays read only recent files from folders, stream lines one at a time, and count lines
/// that are not records.
#[tokio::test]
async fn trace_files_by_age_and_streamed_lines() {
    let dir = tempfile::tempdir().unwrap();
    let logs = dir.path().join("t_0000000000000000").join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let today = TraceRecord::new(Endpoint::Search, "t_0000000000000000");
    let date = &today.at[..10];
    let line = serde_json::to_string(&today).unwrap();
    std::fs::write(
        logs.join(format!("search-{date}.jsonl")),
        format!("{line}\nnot json\n\n{line}\n"),
    )
    .unwrap();
    std::fs::write(logs.join("search-2020-01-01.jsonl"), format!("{line}\n")).unwrap();
    std::fs::write(logs.join("notes.txt"), "x").unwrap();

    let recent = trace_files(&[dir.path().to_path_buf()], Some(7)).unwrap();
    assert_eq!(recent.len(), 1);
    let all = trace_files(&[dir.path().to_path_buf()], None).unwrap();
    assert_eq!(all.len(), 2);
    // A file named directly is read whatever its age.
    let named = trace_files(&[logs.join("search-2020-01-01.jsonl")], Some(7)).unwrap();
    assert_eq!(named.len(), 1);

    let mut reader = TraceReader::new(recent);
    assert_eq!(reader.by_ref().count(), 2);
    assert_eq!(reader.bad_lines, 1);
}

#[tokio::test]
async fn disabled_traces_write_nothing() {
    let mut app = TestApp::with_config("[trace]\nenabled = false\n");
    let key = app.create_tenant("shopline").await;
    app.restart();
    app.post(Caller::new(&key), "/search", json!({ "query": "refund" }))
        .await;
    assert!(traces(&app).is_empty());
}

#[tokio::test]
async fn replay_reports_what_changed() {
    let (mut app, key) = shopline().await;
    let questions: HashMap<&str, serde_json::Value> = HashMap::from([
        (
            "refund",
            json!({ "query": "七天无理由退货有哪些例外？", "keywords": ["七天无理由", "例外商品"] }),
        ),
        (
            "coupon",
            json!({ "query": "优惠券叠加规则", "keywords": ["优惠券叠加"] }),
        ),
    ]);
    for body in questions.values() {
        app.post(Caller::new(&key), "/search", body.clone()).await;
    }
    let records = traces(&app);
    let cfg = app.config().search.clone();
    let (root, store_file) = (
        app.config().tenants_dir(),
        app.config().store.file_name.clone(),
    );
    // A fresh registry per replay, as the kb-replay tool opens one.
    let tenants = || Tenants::new(&root, store_file.clone(), Duration::from_secs(60));

    // Same code, same wiki: nothing changes. A record that no longer validates is skipped
    // without stopping the replay.
    let mut with_bad = records.clone();
    let mut bad = records[0].clone();
    bad.request["query"] = json!("");
    with_bad.insert(0, bad);
    let report = replay(&tenants(), &cfg, WikiSource::Recorded, with_bad)
        .await
        .unwrap();
    assert_eq!((report.searches, report.same, report.changed), (2, 2, 0));
    assert_eq!(report.skipped, 1);
    assert!(report.failed.is_empty());
    assert_eq!((report.wiki_changed, report.wiki_missing), (0, 0));
    assert_eq!(report.recorded_services.get(kb::SERVICE), Some(&2));
    assert_eq!(report.search_config, cfg.fingerprint());

    // A new page that outranks the old answer.
    let wiki = WikiFs::new(app.wiki_dir("shopline"));
    let mut page = Page::new(PagePath::parse("expertise/seven-day-exceptions").unwrap());
    page.front.title = Some("七天无理由例外商品".into());
    page.front.aliases = vec!["七天无理由".into(), "例外商品".into()];
    page.front.summary = Some("例外商品清单。".into());
    page.body = "# 七天无理由例外商品\n\n生鲜、定制商品不适用七天无理由。\n".into();
    wiki.write(&page).unwrap();
    app.restart();

    // On the recorded wiki, rebuilt from the history, nothing changes: the code didn't.
    let report = replay(&tenants(), &cfg, WikiSource::Recorded, records.clone())
        .await
        .unwrap();
    assert_eq!((report.searches, report.same, report.changed), (2, 2, 0));
    assert_eq!((report.wiki_changed, report.wiki_missing), (0, 0));

    // On today's wiki the new page shows up, flagged as another wiki.
    let report = replay(&tenants(), &cfg, WikiSource::Current, records.clone())
        .await
        .unwrap();
    assert_eq!(report.searches, 2);
    assert_eq!(report.wiki_changed, 2);
    assert_eq!(report.changed, 1, "{:#?}", report.diffs);
    let diff = &report.diffs[0];
    assert!(diff.query.contains("七天无理由"));
    let knowledge = diff.slots.iter().find(|s| s.slot == "knowledge").unwrap();
    assert!(
        knowledge
            .after
            .contains(&"expertise/seven-day-exceptions".to_string())
    );
    assert!(
        !knowledge
            .before
            .contains(&"expertise/seven-day-exceptions".to_string())
    );

    // A recorded version that is no longer kept falls back to today's wiki, and says so.
    std::fs::remove_dir_all(
        app.data
            .path()
            .join("tenants")
            .join(kb::tenants::tenant_dir_name("shopline"))
            .join("history"),
    )
    .unwrap();
    let report = replay(&tenants(), &cfg, WikiSource::Recorded, records)
        .await
        .unwrap();
    assert_eq!(
        (report.wiki_missing, report.wiki_changed, report.changed),
        (2, 2, 1)
    );
}

/// Another search config shows up in the report next to the recorded one.
#[tokio::test]
async fn replay_reports_the_search_config() {
    let (app, key) = shopline().await;
    app.post(
        Caller::new(&key),
        "/search",
        json!({ "query": "退款", "keywords": ["退款"] }),
    )
    .await;
    let records = traces(&app);
    let mut other = app.config().search.clone();
    other.ablation.pin_names = false;
    let tenants = Tenants::new(
        app.config().tenants_dir(),
        app.config().store.file_name.clone(),
        Duration::from_secs(60),
    );
    let report = replay(&tenants, &other, WikiSource::Recorded, records)
        .await
        .unwrap();
    assert_ne!(report.search_config, app.config().search.fingerprint());
    assert_eq!(
        report
            .recorded_search_configs
            .get(&app.config().search.fingerprint()),
        Some(&1)
    );
}
