//! `/search` and `/read` over HTTP: request checks, user, group and tenant isolation, routes,
//! headings, links and the read tracker. Uses the `shopline` and `factoryops` scenarios.

mod common;

use std::collections::HashMap;

use axum::http::StatusCode;
use common::app::{Caller, TestApp};
use common::testset;
use pinakes::tenants::{group_dir_name, user_dir_name};
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::{Value, json};

struct World {
    app: TestApp,
    keys: HashMap<String, String>,
}

async fn world() -> World {
    let mut app = TestApp::new();
    let mut keys = HashMap::new();
    for t in ["shopline", "factoryops"] {
        keys.insert(t.to_owned(), app.create_tenant(t).await);
        testset::install_wiki(t, &app.wiki_dir(t));
    }
    // A third tenant with one page of its own, sharing words with shopline's pages.
    keys.insert("other".into(), app.create_tenant("other").await);
    let wiki = WikiFs::new(app.wiki_dir("other"));
    let mut page = Page::new(PagePath::parse("expertise/secret-refunds").unwrap());
    page.front.title = Some("Other tenant refund secrets".into());
    page.front.aliases = vec!["partial refund".into(), "部分退款".into()];
    page.front.summary = Some("Only the other tenant may see this.".into());
    page.body = "# Secrets\n\npartial refund 部分退款\n\nzebrafish notes\n".into();
    wiki.write(&page).unwrap();
    app.restart();
    World { app, keys }
}

impl World {
    fn caller<'a>(&'a self, tenant: &str, user: Option<&'a str>) -> Caller<'a> {
        Caller::new(&self.keys[tenant]).user(user)
    }

    async fn search(&self, caller: Caller<'_>, body: Value) -> Value {
        let (status, resp) = self.app.post(caller, "/search", body).await;
        assert_eq!(status, StatusCode::OK, "{resp}");
        resp
    }
}

fn pages(resp: &Value) -> Vec<String> {
    ["memories", "knowledge"]
        .iter()
        .flat_map(|slot| resp[slot]["hits"].as_array().unwrap().iter())
        .map(|h| h["page"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn bad_requests_are_rejected() {
    let w = world().await;
    let c = w.caller("shopline", None);
    for body in [
        json!({ "query": "" }),
        json!({ "query": "refund", "mode": "meaning" }),
        json!({ "query": "refund", "slots": { "memories": 21 } }),
        json!({ "query": "refund", "tenant": "other" }),
        json!({ "query": "refund", "keywords": ["x".repeat(101)] }),
        json!({ "query": "refund", "session": "" }),
    ] {
        let (status, resp) = w.app.post(c, "/search", body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} → {resp}");
    }
    // Without an embedding model the meaning-based modes are off; `auto` runs on keywords.
    for mode in ["semantic", "both"] {
        let (status, resp) = w
            .app
            .post(c, "/search", json!({ "query": "refund", "mode": mode }))
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{mode}: {resp}");
    }
    let (status, resp) = w
        .app
        .post(c, "/search", json!({ "query": "refund", "mode": "auto" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["mode"], "auto");
    let (status, _) = w
        .app
        .post(
            Caller::new("pnx_nope"),
            "/search",
            json!({ "query": "refund" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn hits_carry_summary_lines_and_route() {
    let w = world().await;
    let resp = w
        .search(
            w.caller("shopline", None),
            json!({ "query": "七天无理由退货有哪些例外商品？", "keywords": ["七天无理由", "例外商品"] }),
        )
        .await;
    let top = &resp["knowledge"]["hits"][0];
    assert_eq!(top["page"], "expertise/refund-regulations");
    assert_eq!(resp["knowledge"]["coverage"], "strong");
    assert!(!top["summary"].as_str().unwrap().is_empty());
    assert!(!top["lines"].as_array().unwrap().is_empty());
    assert_eq!(top["route"]["read"], "expertise/refund-regulations");
    assert_eq!(top["route"]["heading"], "七天无理由退货");
    assert_eq!(top["read"], false);
    // The relations slot waits for M3; scores are never exposed.
    assert_eq!(resp["relations"]["edges"], json!([]));
    assert!(!resp.to_string().contains("score"));
}

/// A keyword found only on a page too weak to show is still reported as unmatched, so the
/// agent knows to rephrase it.
#[tokio::test]
async fn keywords_only_on_dropped_pages_are_unmatched() {
    let w = world().await;
    let resp = w
        .search(
            w.caller("other", None),
            json!({ "query": "zebrafish", "keywords": ["zebrafish", "qqqq-none"] }),
        )
        .await;
    assert!(pages(&resp).is_empty(), "{resp}");
    assert_eq!(
        resp["unmatched_keywords"],
        json!(["zebrafish", "qqqq-none"])
    );
}

#[tokio::test]
async fn slot_sizes_and_unmatched_keywords() {
    let w = world().await;
    let resp = w
        .search(
            w.caller("shopline", None),
            json!({ "query": "退款", "keywords": ["退款", "zzqx-unknown"], "slots": { "memories": 1, "knowledge": 0 } }),
        )
        .await;
    assert!(resp["memories"]["hits"].as_array().unwrap().len() <= 1);
    assert_eq!(resp["knowledge"]["hits"], json!([]));
    assert_eq!(resp["unmatched_keywords"], json!(["zzqx-unknown"]));
}

#[tokio::test]
async fn other_users_and_tenants_stay_invisible() {
    let w = world().await;
    let lily_page = format!(
        "users/{}/decisions/double11-coupon-budget",
        user_dir_name("lily")
    );
    let body =
        json!({ "query": "双11优惠券预算上限是多少？", "keywords": ["双11预算", "优惠券预算"] });

    let lily = w
        .search(w.caller("shopline", Some("lily")), body.clone())
        .await;
    assert_eq!(lily["memories"]["hits"][0]["page"], lily_page.as_str());
    for caller in [
        w.caller("shopline", Some("wang")),
        w.caller("shopline", None),
    ] {
        let resp = w.search(caller, body.clone()).await;
        assert!(!resp.to_string().contains(&lily_page), "{resp}");
    }

    // /read reports another user's page exactly like a missing one.
    for (user, id, expect) in [
        (Some("lily"), lily_page.as_str(), None),
        (Some("wang"), lily_page.as_str(), Some("not_found")),
        (Some("wang"), "expertise/does-not-exist", Some("not_found")),
    ] {
        let (status, resp) = w
            .app
            .post(w.caller("shopline", user), "/read", json!({ "ids": [id] }))
            .await;
        assert_eq!(status, StatusCode::OK);
        let item = &resp["pages"][0];
        assert_eq!(item["error"].as_str(), expect, "{resp}");
        assert_eq!(item["text"].is_null(), expect.is_some());
    }

    // Another tenant's pages never appear, even with matching aliases.
    let resp = w
        .search(
            w.caller("shopline", None),
            json!({ "query": "partial refund", "keywords": ["partial refund", "部分退款"] }),
        )
        .await;
    assert!(!resp.to_string().contains("secret-refunds"));
    let other = w
        .search(
            w.caller("other", None),
            json!({ "query": "partial refund", "keywords": ["partial refund"] }),
        )
        .await;
    assert_eq!(pages(&other), vec!["expertise/secret-refunds"]);
    let (_, resp) = w
        .app
        .post(
            w.caller("shopline", None),
            "/read",
            json!({ "ids": ["expertise/secret-refunds"] }),
        )
        .await;
    assert_eq!(resp["pages"][0]["error"], "not_found");
}

#[tokio::test]
async fn group_pages_follow_the_groups_header() {
    let w = world().await;
    let page = format!(
        "groups/{}/experience/wuhan-cnc-bearing-swap",
        group_dir_name("wuhan-maintenance")
    );
    let body = json!({ "query": "武汉CNC-07换轴承要注意什么？", "keywords": ["武汉CNC-07换轴承", "6206轴承"] });
    let team = vec!["wuhan-maintenance".to_string()];
    let zhou = w
        .search(
            w.caller("factoryops", Some("zhou")).groups(&team),
            body.clone(),
        )
        .await;
    assert!(pages(&zhou).contains(&page), "{zhou}");
    // Same user without the group header: the host stopped sending it.
    let without = w
        .search(w.caller("factoryops", Some("zhou")), body.clone())
        .await;
    assert!(!without.to_string().contains(&page));
    let other_group = vec!["suzhou-maintenance".to_string()];
    let sarah = w
        .search(
            w.caller("factoryops", Some("sarah")).groups(&other_group),
            body,
        )
        .await;
    assert!(!sarah.to_string().contains(&page));
}

#[tokio::test]
async fn read_tracker_and_headings() {
    let w = world().await;
    let c = w.caller("shopline", Some("lily"));
    let id = "expertise/refund-regulations";

    // A heading, then the same heading again in the same session.
    let (status, first) = w
        .app
        .post(
            c,
            "/read",
            json!({ "ids": [id], "heading": "退款方式", "session": "s1" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let item = &first["pages"][0];
    assert_eq!(item["heading"], "退款方式");
    let text = item["text"].as_str().unwrap();
    assert!(text.starts_with("## 退款方式"), "{text}");
    assert!(!text.contains("## 到账时间"));
    let (_, again) = w
        .app
        .post(
            c,
            "/read",
            json!({ "ids": [id], "heading": "退款方式", "session": "s1" }),
        )
        .await;
    assert_eq!(again["pages"][0]["already_read"], true);
    assert!(again["pages"][0]["text"].is_null());

    // The whole page hasn't been read yet; after reading it, search marks the hit.
    let (_, whole) = w
        .app
        .post(c, "/read", json!({ "ids": [id], "session": "s1" }))
        .await;
    assert!(
        whole["pages"][0]["text"]
            .as_str()
            .unwrap()
            .contains("## 到账时间")
    );
    let body = json!({ "query": "七天无理由退货", "keywords": ["七天无理由"], "session": "s1" });
    let resp = w.search(c, body.clone()).await;
    assert_eq!(resp["knowledge"]["hits"][0]["page"], id);
    assert_eq!(resp["knowledge"]["hits"][0]["read"], true);
    // Another session, or another user with the same session id, starts fresh.
    let mut other = body.clone();
    other["session"] = json!("s2");
    assert_eq!(
        w.search(c, other).await["knowledge"]["hits"][0]["read"],
        false
    );
    let wang = w.search(w.caller("shopline", Some("wang")), body).await;
    assert_eq!(wang["knowledge"]["hits"][0]["read"], false);

    // A missing heading, `index`, and malformed ids.
    let (_, resp) = w
        .app
        .post(
            c,
            "/read",
            json!({ "ids": [id, "index"], "heading": "Nope" }),
        )
        .await;
    assert_eq!(resp["pages"][0]["error"], "heading_not_found");
    assert_eq!(resp["pages"][1]["error"], "not_found");
    for ids in [
        json!([]),
        json!(["../secret"]),
        json!(["sources/../x.json"]),
    ] {
        let (status, _) = w.app.post(c, "/read", json!({ "ids": ids })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{ids}");
    }
    // A source id no visible page cites is not found, like a hidden page.
    let (_, resp) = w
        .app
        .post(c, "/read", json!({ "ids": ["sources/x.json"] }))
        .await;
    assert_eq!(resp["pages"][0]["error"], "not_found");
}

/// A page whose file can't be parsed fails on its own; the other ids are still read.
#[tokio::test]
async fn unreadable_pages_fail_alone() {
    let w = world().await;
    let file = w
        .app
        .wiki_dir("shopline")
        .join("expertise")
        .join("broken.md");
    std::fs::write(file, [0xff, 0xfe, 0x00]).unwrap();
    let (status, resp) = w
        .app
        .post(
            w.caller("shopline", None),
            "/read",
            json!({ "ids": ["expertise/broken", "expertise/refund-regulations"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{resp}");
    assert_eq!(resp["pages"][0]["error"], "unreadable");
    assert!(resp["pages"][1]["text"].is_string());
}

#[tokio::test]
async fn read_with_links_returns_visible_summaries() {
    let mut w = world().await;
    let wiki = WikiFs::new(w.app.wiki_dir("shopline"));
    let lily = user_dir_name("lily");
    let mut page = Page::new(PagePath::parse("expertise/linking").unwrap());
    page.front.title = Some("Linking".into());
    page.body = format!(
        "# Linking\n\nSee [[refund-regulations]], [[expertise/shipping-sla]], \
         [[users/{lily}/decisions/double11-coupon-budget]] and [[missing]].\n"
    );
    wiki.write(&page).unwrap();
    w.app.restart();

    let read = |user: Option<&'static str>| {
        let w = &w;
        async move {
            let (_, resp) = w
                .app
                .post(
                    w.caller("shopline", user),
                    "/read",
                    json!({ "ids": ["expertise/linking"], "with_links": true }),
                )
                .await;
            resp["pages"][0]["links"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l["page"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>()
        }
    };
    let own = format!("users/{lily}/decisions/double11-coupon-budget");
    assert_eq!(
        read(Some("lily")).await,
        vec![
            "expertise/refund-regulations".to_string(),
            "expertise/shipping-sla".to_string(),
            own
        ]
    );
    assert_eq!(
        read(Some("wang")).await,
        vec!["expertise/refund-regulations", "expertise/shipping-sla"]
    );
}
