//! Files uploaded to `/ingest/expertise` are turned into Markdown first (design, "Files are
//! turned into Markdown first"): Markdown and text as they are, other formats by the converter
//! service. The original file is kept next to its record, which says what converted it.
//!
//! With `PINAKES_TEST_CONVERTER` set to the real converter's URL (`docker compose up -d
//! --build converter`, then `PINAKES_TEST_CONVERTER=http://127.0.0.1:8090`), a Word file also
//! goes through MarkItDown itself.

mod common;

use std::sync::Arc;

use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use common::app::{Caller, TestApp};
use common::llm::{
    FakeLlm, NOT_THE_SAME, OK, Step, all_ok, create, describe_from_definition, folder, read_reply,
    read_step, read_steps, record_of, shown,
};
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

/// Without a converter, Markdown and text files still work; other files are a 503 that says
/// what to set up.
#[tokio::test]
async fn without_a_converter_only_text_is_taken() {
    let app = TestApp::with_llm(librarian().llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let (status, resp) = app
        .post_form(c, "/ingest/expertise", &[], Some(("a.docx", &b"PK"[..])))
        .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{resp}");
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("[converter] url")
    );
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

/// The real converter, when it runs: a Word file with a heading, a table and a list comes back
/// as Markdown with all three, and the record names MarkItDown and its version.
#[tokio::test]
async fn a_word_file_through_markitdown() {
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
    let docx = include_bytes!("fixtures/member-benefits.docx");
    let (status, queued) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some(("member-benefits.docx", docx)),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    let (rec, _) = record(&app, &job["result"]);
    let md = rec["content"].as_str().unwrap();
    eprintln!("{md}");
    assert!(md.contains("# 一、会员等级"), "headings");
    assert!(md.contains("| 金卡 | 5000–19999 元 | 1.5 倍 |"), "tables");
    assert!(md.contains("* 黑卡会员享专属客服"), "lists");
    assert_eq!(rec["file"]["converter"], "markitdown");
    assert_eq!(rec["file"]["version"], "0.1.8");
}
