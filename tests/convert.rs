//! Files uploaded to `/ingest/expertise` are turned into Markdown first (design, "Files are
//! turned into Markdown first" and "M10"): Markdown and text as they are, HTML, Word,
//! spreadsheets and PowerPoint by the service itself, other formats by the optional MarkItDown
//! converter. The original file is kept next to its record, which says what converted it.
//!
//! With `PINAKES_TEST_CONVERTER` set to the real converter's URL (`docker compose --profile
//! dev up -d --build converter-dev`, then `PINAKES_TEST_CONVERTER=http://127.0.0.1:8090`), a
//! PDF also goes through MarkItDown itself, and every fixture is converted by both and
//! compared.

use crate::common;

use std::sync::Arc;

use crate::common::app::{Caller, TestApp};
use crate::common::llm::{
    FakeLlm, NOT_THE_SAME, OK, Step, all_ok, create, describe_from_definition, folder, read_reply,
    read_step, read_steps, record_of, shown,
};
use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use serde_json::{Value, json};

/// A model that reads the whole text as one concept and files it as one page.
fn librarian() -> Arc<FakeLlm> {
    FakeLlm::new(|step, msg, _| match step {
        Step::Rank | Step::Screen => unreachable!("not asked here"),
        Step::Lint => common::llm::NO_FINDINGS.into(),
        Step::Read => {
            let (lines, at_end) = shown(msg);
            if !at_end {
                return read_reply("more", 0, None, None, None);
            }
            let page = format!("{}/benefits", common::llm::read_folder(msg));
            let end = lines.last().unwrap().0;
            // The extract: the lines themselves, which hold every number they have.
            let extract: Vec<&str> = lines.iter().map(|(_, l)| l.as_str()).collect();
            let step = read_step(
                end,
                Some((
                    "会员权益",
                    &page,
                    &extract.join(
                        "
",
                    ),
                )),
            );
            read_steps(vec![step], None)
        }
        Step::Plan => {
            let rec = record_of(msg);
            let page = format!("{}/benefits", folder(msg));
            let body = format!("# 会员权益\n\n{}", rec["extract"].as_str().unwrap());
            json!({ "edits": [create(&page, "会员权益", "会员等级与权益。", &[], &body)] })
                .to_string()
        }
        Step::Verify => all_ok(msg),
        Step::Describe => describe_from_definition(msg),
        Step::VerifyDescribe => OK.into(),
        Step::Match => NOT_THE_SAME.into(),
    })
}

/// A stand-in converter: `.docx` becomes fixed Markdown, `.bad` can't be converted, and
/// anything else is a format it doesn't take. Returns its URL.
async fn fake_converter() -> String {
    async fn convert(headers: HeaderMap, body: axum::body::Bytes) -> (StatusCode, Json<Value>) {
        let ext = headers
            .get("X-File-Extension")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        match ext {
            "docx" if !body.is_empty() => (
                StatusCode::OK,
                Json(
                    json!({ "markdown": "# 会员权益\n\n金卡每月 2 张免运费券。\n",
                             "converter": "fake", "version": "1.0" }),
                ),
            ),
            "bad" => (
                StatusCode::UNPROCESSABLE_ENTITY,
                Json(json!({ "error": "could not convert the file: BadZipFile" })),
            ),
            _ => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                Json(json!({ "error": format!("unsupported format: {ext}") })),
            ),
        }
    }
    let app = axum::Router::new().route("/convert", axum::routing::post(convert));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// The record a job filed, and the folder it is in.
fn record(app: &TestApp, result: &Value) -> (Value, std::path::PathBuf) {
    let name = result["source"]
        .as_str()
        .unwrap()
        .strip_prefix("sources/")
        .unwrap();
    let dir = app.wiki_dir("acme").parent().unwrap().join("sources");
    let text = std::fs::read_to_string(dir.join(name)).unwrap();
    (serde_json::from_str(&text).unwrap(), dir)
}

#[tokio::test]
async fn uploaded_files_are_filed_as_markdown() {
    let url = fake_converter().await;
    let llm = librarian();
    let app = TestApp::with_llm_and_config(llm.llm(), &format!("[converter]\nurl = \"{url}\"\n"));
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);

    // A Word file: converted, filed, kept, and the record says by what.
    let docx = b"PK\x03\x04 pretend this is a Word file";
    let (status, queued) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[
                ("source_url", "https://example.com/benefits"),
                ("entities", "commerce:order"),
            ],
            Some(("会员权益说明.docx", docx)),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    let result = &job["result"];
    assert_eq!(result["pages_created"], json!(["expertise/benefits"]));
    let plan = record_of(&llm.calls(Step::Plan)[0]);
    assert_eq!(
        plan["title"], "会员权益说明",
        "the title is the file's name"
    );
    assert!(plan["extract"].as_str().unwrap().contains("2 张免运费券"));
    assert_eq!(plan["source_url"], "https://example.com/benefits");
    let (rec, dir) = record(&app, result);
    // The converter's Markdown, kept as it came.
    assert_eq!(rec["content"], "# 会员权益\n\n金卡每月 2 张免运费券。\n");
    assert_eq!(rec["file"]["name"], "会员权益说明.docx");
    assert_eq!(rec["file"]["converter"], "fake");
    assert_eq!(rec["file"]["version"], "1.0");
    assert_eq!(rec["entities"], json!(["commerce:order"]));
    let original = rec["file"]["original"].as_str().unwrap();
    assert!(original.ends_with(".docx"), "{original}");
    assert_eq!(std::fs::read(dir.join(original)).unwrap(), docx);

    // Markdown is taken as it is, with the title given; a form's blank inputs are left out.
    let (status, queued) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[("title", "退货须知"), ("date", ""), ("document", "")],
            Some(("notes.md", "# 退货\n\n七天无理由。\n".as_bytes())),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    let (rec, _) = record(&app, &job["result"]);
    assert_eq!(rec["title"], "退货须知");
    assert_eq!(rec["file"]["converter"], "none");

    // A long name keeps its extension.
    let long = format!("{}.md", "很长的文件名".repeat(50));
    let (status, resp) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some((&long, "规则。".as_bytes())),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resp}");

    // What can't be taken: a format the converter doesn't take, a file it can't read, a file
    // without an extension, text that isn't UTF-8, no file, an unknown field.
    for (fields, file, want) in [
        (
            vec![],
            Some(("tool.exe", &b"MZ"[..])),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            vec![],
            Some(("broken.bad", &b"x"[..])),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            vec![],
            Some(("README", &b"x"[..])),
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
        ),
        (
            vec![],
            Some(("latin1.txt", &b"caf\xe9"[..])),
            StatusCode::BAD_REQUEST,
        ),
        (vec![("title", "t")], None, StatusCode::BAD_REQUEST),
        (
            vec![],
            Some(("empty.docx", &b""[..])),
            StatusCode::BAD_REQUEST,
        ),
        (
            vec![("tenant", "x")],
            Some(("a.md", &b"x"[..])),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, resp) = app.post_form(c, "/ingest/expertise", &fields, file).await;
        assert_eq!(status, want, "{file:?}: {resp}");
    }
}

/// Without MarkItDown, Word, spreadsheets, PowerPoint and HTML are converted by the service
/// itself; a damaged file is a 422 saying why, and a format it can't read (PDF, `.doc`) a 415
/// that says what to do.
#[tokio::test]
async fn without_markitdown_the_service_converts_office_files_itself() {
    let llm = librarian();
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let (status, queued) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some(("会员权益说明.docx", &fixture("member-benefits.docx"))),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    let (rec, _) = record(&app, &job["result"]);
    let md = rec["content"].as_str().unwrap();
    assert!(md.contains("# 一、会员等级"), "{md}");
    assert!(md.contains("| 金卡 | 5000–19999 元 | 1.5 倍 |"), "{md}");
    assert!(md.contains("- 黑卡会员享专属客服"), "{md}");
    assert_eq!(rec["file"]["converter"], "builtin");
    assert_eq!(rec["file"]["version"], env!("CARGO_PKG_VERSION"));

    let (status, resp) = app
        .post_form(c, "/ingest/expertise", &[], Some(("a.docx", &b"PK"[..])))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not a valid Office file"),
        "{resp}"
    );
    for (name, says) in [
        ("old.doc", "Save the file as .docx"),
        ("old.ppt", "Save the file as .pptx"),
        (
            "report.pdf",
            "PDF files can't be converted by the service itself",
        ),
        ("photo.png", "it reads Markdown, text, HTML, Word"),
    ] {
        let (status, resp) = app
            .post_form(c, "/ingest/expertise", &[], Some((name, &b"%PDF-1.7"[..])))
            .await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{name}: {resp}");
        let message = resp["error"]["message"].as_str().unwrap();
        assert!(message.contains(says), "{name}: {message}");
        assert!(
            message.contains("docker compose --profile markitdown up -d converter")
                && message.contains("[converter] url"),
            "{name}: {message}"
        );
    }
    let (status, resp) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some(("a.txt", "规则。".as_bytes())),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resp}");
}

/// The built-in converter's limits, each against a crafted file: a zip bomb is refused before
/// it is unpacked, and a file over the size limit is refused, both as 413.
#[tokio::test]
async fn crafted_files_are_refused_at_the_limits() {
    let app = TestApp::with_llm(librarian().llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    // 50 MB of zeros as the document, deflated to well under a megabyte.
    let bomb = {
        use std::io::Write;
        let mut out = std::io::Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("word/document.xml", opts).unwrap();
        let block = vec![b' '; 1 << 20];
        for _ in 0..50 {
            w.write_all(&block).unwrap();
        }
        w.finish().unwrap();
        out.into_inner()
    };
    assert!(bomb.len() < 1 << 20);
    let (status, resp) = app
        .post_form(c, "/ingest/expertise", &[], Some(("bomb.docx", &bomb)))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{resp}");
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("zip bomb"),
        "{resp}"
    );
    let big = vec![b'a'; (20 << 20) + 1];
    let (status, resp) = app
        .post_form(c, "/ingest/expertise", &[], Some(("big.html", &big)))
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{resp}");
}

/// The real converter, when it runs: a PDF (which the service doesn't read itself) comes back
/// as Markdown with its headings and table, and the record names MarkItDown and its version.
#[tokio::test]
async fn a_pdf_through_markitdown() {
    let Ok(url) = std::env::var("PINAKES_TEST_CONVERTER") else {
        eprintln!("PINAKES_TEST_CONVERTER is not set; skipping the converter test");
        return;
    };
    let app = TestApp::with_llm_and_config(
        librarian().llm(),
        &format!("[converter]\nurl = \"{url}\"\n"),
    );
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let (status, queued) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some(("warehouse-rules.pdf", &fixture("warehouse-rules-zh.pdf"))),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    let (rec, _) = record(&app, &job["result"]);
    let md = rec["content"].as_str().unwrap();
    eprintln!("{md}");
    assert!(md.contains("二、盘点责任人"), "{md}");
    assert!(md.contains("赵磊"), "{md}");
    assert_eq!(rec["file"]["converter"], "markitdown");
    assert_eq!(rec["file"]["version"], "0.1.8");
}

/// The fixtures the built-in converter reads, Chinese and English, with what each must come
/// back with: headings, list items and table rows.
const FIXTURES: &[(&str, &[&str])] = &[
    (
        "member-benefits.docx",
        &[
            "# 会员权益说明（2026 版）",
            "# 一、会员等级",
            "| 等级 | 累计消费 | 积分倍率 |\n| --- | --- | --- |",
            "| 黑卡 | 20000 元以上 | 2 倍 |",
            "- 生日当月积分按 3 倍累计，上限 5000 分。",
        ],
    ),
    (
        "returns-policy.docx",
        &[
            "# Returns policy",
            "## How to return",
            "within **seven days** of delivery; see [the full terms](https://example.com/terms).",
            "- Goods with their original receipt",
            "3. Hand the parcel to the courier.",
            "| Cash on delivery | 7 days | by bank transfer |",
        ],
    ),
    (
        "warehouse-rules-zh.docx",
        &[
            "# 仓储作业规范",
            "## 2.1 周期",
            "1. 核对送货单与采购单",
            "每月最后一个周五进行**全盘**",
            "| 冷库 | 赵磊 | 周敏 |",
            "- 每四小时记录一次",
        ],
    ),
    (
        "inventory.xlsx",
        &[
            "## 库存\n\n| 品名 | 仓库 | 数量 | 入库日期 |",
            "| 矿泉水 550ml | A 区 | 1200 | 2026-09-01 |\n| 洗衣液 2L | B 区 | 340 | 2026-09-15 |",
            "| 冷冻水饺 | 冷库 | 85.5 | 2026-10-02 |",
            "## Suppliers",
            "| CleanCo | sales@cleanco.example | 7 |",
        ],
    ),
    (
        "orders.csv",
        &[
            "| order_id | customer | amount | status |",
            "| A1003 | 王芳 |  | pending |",
        ],
    ),
    (
        "quarterly-review-zh.pptx",
        &[
            "# 第三季度经营回顾\n\n华东大区 · 2026 年 10 月",
            "# 销售亮点\n\n- 销售额同比增长 18%\n  - 华东贡献过半",
            "### Notes\n\n重点讲华东的增长原因：会员日活动。",
            "| 华东 | 52 | +21% |",
        ],
    ),
    (
        "product-launch.pptx",
        &[
            "# What members get",
            "  - Used on orders over 49 yuan",
            "| Gold | 99 a year | 24 |",
            "Stress that coupons do not roll over.",
        ],
    ),
    (
        "shipping-faq.html",
        &[
            "# Shipping FAQ",
            "## How long does delivery take?",
            "| Remote areas | 5 to 7 days, no express |  |",
            "- Gold members: two free shipping coupons a month\n  - Coupons do not roll over",
            "1. Open [your orders](/account/orders).",
            "> Tip: tracking numbers appear within an hour of dispatch.",
        ],
    ),
    (
        "member-service-zh.html",
        &[
            "# 会员服务说明",
            "### 如何升级",
            "| 金卡 | 99 元 | 每月 2 张 |",
            "- 当月未用完的券不累积",
        ],
    ),
];

fn fixture(name: &str) -> Vec<u8> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::read(dir.join(name)).unwrap()
}

fn builtin(name: &str) -> String {
    let ext = name.rsplit_once('.').unwrap().1;
    pinakes::convert::builtin(ext, &fixture(name), &Default::default())
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .markdown
}

#[test]
fn the_service_reads_every_fixture_itself() {
    for (name, wanted) in FIXTURES {
        let md = builtin(name);
        for w in *wanted {
            assert!(md.contains(w), "{name} lacks {w:?}:\n{md}");
        }
        // Navigation chrome stays out of a web page's text.
        for chrome in [
            "Example Shop",
            "Help topics",
            "示例商城首页",
            "关于我们",
            "Privacy",
        ] {
            assert!(!md.contains(chrome), "{name} keeps {chrome:?}:\n{md}");
        }
    }
}

/// The parts of a Markdown text that carry its structure: headings, list items and table
/// cells, as plain text (marks and link addresses dropped).
fn outline(md: &str) -> Vec<String> {
    let plain = |s: &str| {
        let mut out = String::new();
        let mut rest = s;
        // `[text](address)` as its text.
        while let Some(open) = rest.find('[') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            match (after.find("]("), after.find(')')) {
                (Some(close), Some(end)) if close < end => {
                    out.push_str(&after[..close]);
                    rest = &after[end + 1..];
                }
                _ => {
                    out.push('[');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        let t = out.replace(['*', '`'], "");
        let t = t.trim().trim_end_matches(':');
        // pandas writes whole numbers as `1200.0` in MarkItDown's tables.
        match t.strip_suffix(".0") {
            Some(n) if n.parse::<i64>().is_ok() => n.to_owned(),
            _ => t.to_owned(),
        }
    };
    let mut items = Vec::new();
    for line in md.lines() {
        let t = line.trim();
        if t.starts_with('|') {
            if t.contains("---") {
                continue;
            }
            items.extend(t.trim_matches('|').split('|').map(plain).filter(|c| {
                !c.is_empty() && c != "NaN" && c != "NaT" && !c.starts_with("Unnamed")
            }));
        } else if let Some(h) = t.strip_prefix('#') {
            items.push(plain(h.trim_start_matches('#')));
        } else if let Some(i) = t
            .strip_prefix("* ")
            .or_else(|| t.strip_prefix("- "))
            .or_else(|| t.strip_prefix("+ "))
        {
            items.push(plain(i));
        } else if t.len() > 2 && t.as_bytes()[0].is_ascii_digit() && t.contains(". ") {
            items.push(plain(t.split_once(". ").unwrap().1));
        }
    }
    items
}

/// Every heading, list item and table cell MarkItDown finds in a fixture, the built-in
/// converter finds too, in the main content of a web page (MarkItDown keeps the navigation,
/// which the built-in one leaves out on purpose). Over all fixtures, never tuned on one.
#[tokio::test]
async fn the_builtin_converter_keeps_what_markitdown_keeps() {
    let Ok(url) = std::env::var("PINAKES_TEST_CONVERTER") else {
        eprintln!("PINAKES_TEST_CONVERTER is not set; skipping the comparison with MarkItDown");
        return;
    };
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let mut missing = Vec::new();
    let mut compared = 0;
    for (name, _) in FIXTURES {
        let ext = name.rsplit_once('.').unwrap().1;
        if ext == "csv" {
            continue; // MarkItDown's container doesn't take CSV
        }
        let theirs: Value = client
            .post(format!("{url}/convert"))
            .header("X-File-Extension", ext)
            .body(fixture(name))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let theirs = theirs["markdown"].as_str().unwrap().to_owned();
        let main_text = if ext == "html" {
            let html = String::from_utf8(fixture(name)).unwrap();
            let start = html
                .find("<main")
                .or_else(|| html.find("<article"))
                .unwrap();
            let end = html
                .find("</main>")
                .or_else(|| html.find("</article>"))
                .unwrap();
            html[start..end].to_owned()
        } else {
            String::new()
        };
        let ours = outline(&builtin(name)).join("\n");
        for item in outline(&theirs) {
            if ext == "html" && !main_text.contains(&item) {
                continue;
            }
            compared += 1;
            if !ours.contains(&item) {
                missing.push(format!("{name}: {item}"));
            }
        }
    }
    assert!(compared > 80, "{compared} items compared");
    assert!(
        missing.is_empty(),
        "the built-in converter lacks: {missing:#?}"
    );
}
