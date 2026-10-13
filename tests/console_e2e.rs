//! The service the console's browser tests run against (`ui/tests`, Playwright starts it):
//! the real router on 127.0.0.1:8097, a personal deployment read from a config file, a
//! scripted librarian, an embedding switch whose stand-in model downloads slowly, and a tenant
//! seeded with pages, a graph, a graph draft, a held hand edit and a held external source. Ignored in normal runs:
//! `cargo test --test integration console_e2e:: -- --ignored --nocapture` serves until stopped.

use crate::common;

use std::sync::Arc;
use std::time::Duration;

use crate::common::embed::{FAKE_DIM, Fake};
use crate::common::llm::{
    FakeLlm, NO_FINDINGS, NOT_THE_SAME, OK, Step, all_ok, all_rejected, create,
    describe_from_definition, folder, read_reply, read_step, read_steps, record_of, shown,
};
use axum::body::Body;
use axum::http::Request;
use pinakes::AppState;
use pinakes::config::Config;
use pinakes::embed::{Embedder, Status, Switch};
use pinakes::wiki::{Page, PagePath, WikiFs};
use serde_json::{Value, json};
use tower::ServiceExt;

const PORT: u16 = 8097;
const ADMIN: &str = "e2e-admin";

/// The page a document is filed as: named after its title when that is plain ASCII (the seeded
/// external source), else `uploaded-document`.
fn filed_page(folder: &str, title: &str) -> String {
    let slug: String = title
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    if title.is_ascii() && !title.is_empty() {
        format!("{folder}/{slug}")
    } else {
        format!("{folder}/uploaded-document")
    }
}

/// A librarian that files a document as one page named after its first line, accepts every
/// edit except a hand edit holding the word HOLD, and describes schemas from their definitions.
fn librarian() -> Arc<FakeLlm> {
    FakeLlm::new(|step, msg, _| match step {
        Step::Rank | Step::Screen => unreachable!("not asked here"),
        Step::Lint => NO_FINDINGS.into(),
        Step::Read => {
            let (lines, at_end) = shown(msg);
            if !at_end {
                return read_reply("more", 0, None, None, None);
            }
            let end = lines.last().map(|l| l.0).unwrap_or(1);
            let text: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
            let title = text
                .iter()
                .map(|l| l.trim_start_matches('#').trim())
                .find(|l| !l.is_empty())
                .unwrap_or("Uploaded document")
                .to_owned();
            let page = filed_page(&common::llm::read_folder(msg), &title);
            read_steps(
                vec![read_step(end, Some((&title, &page, &text.join("\n"))))],
                None,
            )
        }
        Step::Plan => {
            let rec = record_of(msg);
            let title = rec["title"]
                .as_str()
                .unwrap_or("Uploaded document")
                .to_owned();
            let page = filed_page(&folder(msg), &title);
            let extract = rec["extract"].as_str().unwrap_or_default().to_owned();
            json!({ "edits": [create(&page, &title, "An uploaded document.", &[], &extract)] })
                .to_string()
        }
        Step::Verify if msg.contains("HOLD") => all_rejected(msg, "a person should look at this"),
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => NOT_THE_SAME.into(),
    })
}

/// An embedding switch whose model "downloads" in steps a browser can watch.
fn switch() -> Arc<Switch> {
    Arc::new(Switch::new(
        "fake",
        FAKE_DIM,
        Arc::new(|progress| {
            let total = 641_000_000u64;
            for i in 0..=20u64 {
                let go_on = progress(Status::Downloading {
                    file: "onnx/model.onnx".into(),
                    done_bytes: total / 20 * i,
                    total_bytes: total,
                    files_done: 4,
                    files: 5,
                });
                if !go_on {
                    anyhow::bail!("turned off");
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            Ok(Fake::new() as Arc<dyn Embedder>)
        }),
    ))
}

fn page(path: &str, title: &str, summary: &str, body: &str) -> Page {
    let mut p = Page::new(PagePath::parse(path).unwrap());
    p.front.title = Some(title.into());
    p.front.summary = Some(summary.into());
    p.front.keywords = vec!["退货".into(), "returns".into()];
    p.body = body.into();
    p
}

async fn call(
    router: &axum::Router,
    method: &str,
    uri: &str,
    key: &str,
    user: Option<&str>,
    body: Value,
) -> Value {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json");
    if let Some(u) = user {
        req = req.header("X-User-Id", u);
    }
    let resp = router
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .unwrap()
        .to_bytes();
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert!(status.is_success(), "{method} {uri}: {status} {v}");
    v
}

#[tokio::test]
#[ignore = "serves the console's browser tests until stopped"]
async fn serve() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let file = dir.path().join("config.toml");
    std::fs::write(
        &file,
        format!(
            "# The browser tests' service.\n[server]\nhost = \"127.0.0.1\"   # this machine only\nport = {PORT}\n\n[data]\ndir = {:?}\n\n[identity]\ndeployment = \"personal\"\n\n[embedding]\nenabled = false   # off: keywords only\nmodel = \"jina-embeddings-v2-base-zh\"\n",
            data.to_str().unwrap()
        ),
    )
    .unwrap();
    let mut config = Config::load(&file).unwrap();
    config.converter.pdf_reader = Some(env!("CARGO_BIN_EXE_pinakes-pdf").into());
    std::fs::create_dir_all(config.tenants_dir()).unwrap();
    let state = AppState::with_switch(config, Some(ADMIN), Ok(librarian().llm()), Some(switch()));
    state.prepare().await.unwrap();
    let key = std::fs::read_to_string(data.join(pinakes::PERSONAL_KEY_FILE))
        .unwrap()
        .trim()
        .to_owned();
    let router = pinakes::router(state.clone());

    // Pages
    let wiki = WikiFs::new(
        data.join("tenants")
            .join(pinakes::tenants::tenant_dir_name("000000"))
            .join("wiki"),
    );
    wiki.write(&page(
        "expertise/returns-policy",
        "退货政策",
        "会员可在收货后七天内退货。",
        "# 退货政策\n\n会员可在收货后**七天**内退货，见 [[expertise/refund-times]]。\n\n| 支付方式 | 退款时限 |\n| --- | --- |\n| 银行卡 | 5 天 |\n| 钱包 | 1 天 |\n\n<script>alert('x')</script>\n",
    ))
    .unwrap();
    wiki.write(&page(
        "expertise/refund-times",
        "退款时限",
        "不同支付方式的退款时限。",
        "# 退款时限\n\n银行卡 5 天，钱包 1 天。\n",
    ))
    .unwrap();
    // The graph: a schema (described by the librarian), a space, nodes and edges.
    call(&router, "POST", "/ingest/graph/schema", &key, None, json!({
        "schema": "commerce", "title": "订单关系", "summary": "订单、客户与商品。",
        "nodes": { "order": { "key": "order_id", "zh": "订单" }, "customer": { "key": "customer_id", "zh": "客户" } },
        "relations": { "placed_by": { "from": "order", "to": "customer", "zh": "下单人" } } })).await;
    call(
        &router,
        "POST",
        "/ingest/graph/space",
        &key,
        None,
        json!({ "space": "mainland", "schema": "commerce", "name": "国内商城" }),
    )
    .await;
    call(&router, "POST", "/ingest/graph/data", &key, None, json!({ "space": "mainland",
        "nodes": [{ "type": "order", "key": "1042", "name": "订单 1042" }, { "type": "customer", "key": "c1", "name": "林晓" }],
        "edges": [{ "from": "order/1042", "type": "placed_by", "to": "customer/c1" }] })).await;
    // An agent's draft, and a hand edit the librarian holds.
    call(&router, "POST", "/graph/draft", &key, Some("agent"), json!({ "space": "mainland",
        "changes": [{ "op": "put_node", "type": "customer", "key": "c2", "name": "王芳" },
                    { "op": "add_edge", "from": "order/1042", "type": "placed_by", "to": "customer/c2" }],
        "reason": "订单 1042 实为王芳代下单" })).await;
    let current = call(
        &router,
        "GET",
        "/pages/expertise/refund-times",
        &key,
        None,
        Value::Null,
    )
    .await;
    let job = call(
        &router,
        "PUT",
        "/pages/expertise/refund-times",
        &key,
        None,
        json!({
        "version": current["version"], "front_matter": current["front_matter"],
        "body": "# 退款时限\n\n银行卡 5 天，钱包 1 天。HOLD 现金到付 7 天。\n" }),
    )
    .await;
    for _ in 0..200 {
        let j = call(
            &router,
            "GET",
            &format!("/jobs/{}", job["job"].as_str().unwrap()),
            &key,
            None,
            Value::Null,
        )
        .await;
        if j["status"] == "done" || j["status"] == "failed" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // A page acquisition found on a trusted site, held as a source to approve (saved as
    // `acquisition::propose` saves one).
    let tenant = state.tenants.by_id("000000").await.unwrap().unwrap();
    let today = &pinakes::trace::now_rfc3339()[..10];
    let name = pinakes::librarian::sources::new_external_name(today);
    let url = "https://help.example.com/returns-abroad";
    tenant.store.add_record(&name, "tenant").await.unwrap();
    pinakes::librarian::sources::save(
        &tenant.dir,
        &name,
        &json!({
            "title": "Returns abroad", "content": "# Returns abroad\n\nOrders shipped abroad can be returned within 14 days.\n",
            "source_url": url, "date": today, "entities": [],
            "external": { "url": url, "fetched_at": today, "license": "CC BY 4.0", "copy": true, "kept": true },
        }),
    )
    .unwrap();
    let proposal: pinakes::librarian::Proposal = serde_json::from_value(json!({
        "proposal": pinakes::librarian::new_proposal_id(&tenant.dir_name),
        "status": "held", "created_at": pinakes::trace::now_rfc3339(), "kind": "expertise",
        "audience": "tenant", "source": name, "edits": [],
        "candidate": {
            "gap": { "owner": "tenant", "kind": "frequent_miss", "target": "海外退货", "detail": "searched 5 times, nothing found" },
            "url": url, "title": "Returns abroad", "snippet": "Returns abroad", "why": "它回答了海外订单的退货时限",
            "summary": "Return windows for orders shipped abroad.", "license": "CC BY 4.0", "copy": true, "fetched_at": today,
        },
    }))
    .unwrap();
    tenant.store.put_proposal(&proposal).await.unwrap();

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT))
        .await
        .unwrap();
    println!("console e2e service on http://127.0.0.1:{PORT}/ui/ (admin key {ADMIN})");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .unwrap();
    drop(dir);
}
