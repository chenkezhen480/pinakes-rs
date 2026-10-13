//! Files uploaded to `/ingest/expertise` are turned into Markdown first (design, "Files are
//! turned into Markdown first" and "M10"): Markdown and text as they are, HTML, Word,
//! spreadsheets, PowerPoint and PDF by the service itself (PDF in its `pinakes-pdf` process),
//! a scanned PDF by the model in the job, the old binary Office formats by the optional
//! MarkItDown converter. The original file is kept next to its record, which says what
//! converted it.
//!
//! With `PINAKES_TEST_CONVERTER` set to the real converter's URL (`docker compose --profile
//! dev up -d --build converter-dev`, then `PINAKES_TEST_CONVERTER=http://127.0.0.1:8090`),
//! every fixture is converted by both and compared.

use crate::common;

use std::sync::Arc;

use crate::common::app::{Caller, TestApp};
use crate::common::llm::{
    FAILS, FakeLlm, NOT_THE_SAME, OK, Step, all_ok, create, describe_from_definition, folder,
    read_reply, read_step, read_steps, record_of, shown,
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

/// Without MarkItDown, Word, spreadsheets, PowerPoint, HTML and PDF are converted by the
/// service itself; a damaged file is a 422 saying why, and a format it can't read (`.doc`,
/// `.ppt`) a 415 that says what to do.
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
    // A PDF, read in the PDF reader's own process: its table and list come back.
    let (status, queued) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some(("仓库作业规范.pdf", &fixture("policy-zh.pdf"))),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
    let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
    assert_eq!(job["status"], "done", "{job}");
    let (rec, dir) = record(&app, &job["result"]);
    let md = rec["content"].as_str().unwrap();
    assert!(md.contains("| B 类 | 0.5% | 3 个工作日 | 主管 |"), "{md}");
    assert!(md.contains("- 外包装破损的箱子单独拍照"), "{md}");
    assert_eq!(rec["file"]["converter"], "builtin");
    let original = dir.join(rec["file"]["original"].as_str().unwrap());
    assert_eq!(std::fs::read(original).unwrap(), fixture("policy-zh.pdf"));
    // A damaged PDF can't be read, and says so.
    let mut damaged = fixture("policy-zh.pdf");
    damaged.truncate(damaged.len() / 3);
    let (status, resp) = app
        .post_form(c, "/ingest/expertise", &[], Some(("broken.pdf", &damaged)))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{resp}");
    let message = resp["error"]["message"].as_str().unwrap();
    assert!(message.contains("couldn't be read"), "{message}");
    for (name, says) in [
        ("old.doc", "Save the file as .docx"),
        ("old.ppt", "Save the file as .pptx"),
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

/// A one-page PDF whose content stream shows `lines` text lines (uncompressed): the PDF reader
/// takes about 7 KB a line, so 300,000 would take 2 GB.
fn pdf_of_lines(lines: usize) -> Vec<u8> {
    let mut content = String::from("BT /F1 6 Tf ");
    for i in 0..lines {
        content.push_str(&format!(
            "1 0 0 1 10 {} Tm (line {i} of a crafted page) Tj ",
            i % 780
        ));
    }
    content.push_str("ET");
    pdf_of_pages(1, &content)
}

/// A PDF of `pages` pages, each drawn by the same content stream (Helvetica as `/F1`).
fn pdf_of_pages(pages: usize, content: &str) -> Vec<u8> {
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", i + 5)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    for _ in 0..pages {
        objects.push("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 3 0 R >> >> >>".to_owned());
    }
    let mut out = String::from("%PDF-1.7\n");
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
    }
    let xref = out.len();
    out.push_str(&format!(
        "xref\n0 {}\n0000000000 65535 f \n",
        objects.len() + 1
    ));
    for o in offsets {
        out.push_str(&format!("{o:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

/// A crafted PDF that would take gigabytes stops the PDF reader's own process at its memory cap,
/// and one that takes longer than the converter's time limit is killed; either is refused,
/// and the service goes on reading PDFs.
#[tokio::test]
async fn a_pdf_past_the_readers_memory_or_time_is_refused() {
    let app = TestApp::with_llm_and_config(librarian().llm(), "[converter]\ntimeout_secs = 2\n");
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let started = std::time::Instant::now();
    let (status, resp) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some(("huge.pdf", &pdf_of_lines(300_000))),
        )
        .await;
    let message = resp["error"]["message"].as_str().unwrap_or_default();
    assert!(
        (status == StatusCode::UNPROCESSABLE_ENTITY
            && message.contains("more memory than allowed"))
            || (status == StatusCode::PAYLOAD_TOO_LARGE
                && message.contains("took longer than 2 s")),
        "{status} {resp}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    let (status, queued) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some(("仓库作业规范.pdf", &fixture("policy-zh.pdf"))),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
}

/// The reader's own process stops at its memory cap (not killed by the deadline): a page that
/// would take 2 GB is refused for its memory.
#[test]
fn the_pdf_readers_memory_is_capped() {
    use std::io::Write;
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_pinakes-pdf"))
        .args([
            "text".to_owned(),
            (256u64 << 20).to_string(),
            (10u64 << 20).to_string(),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let input = pdf_of_lines(300_000);
    let mut stdin = child.stdin.take().unwrap();
    let feed = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().unwrap();
    feed.join().unwrap();
    assert!(!out.status.success());
    let says = String::from_utf8_lossy(&out.stderr);
    assert!(says.contains("memory allocation"), "{says}");
}

/// A scan as a scanner makes it, as far as text goes: `pages` pages with a box drawn on each,
/// and, with `stamp`, a page number stamped in text (too little to be a text layer).
fn scan_pdf(pages: usize, stamp: bool) -> Vec<u8> {
    let mut content = String::from("0.5 g 72 500 468 200 re f ");
    if stamp {
        content.push_str("BT /F1 9 Tf 300 30 Td (- 1 -) Tj ET");
    }
    pdf_of_pages(pages, &content)
}

/// What the model writes for each of a scan's pages: a header every page repeats, a heading,
/// and the page's own sentence.
fn transcribed_page(msg: &str) -> String {
    let n: usize = msg
        .strip_prefix("Page ")
        .and_then(|m| m.split(' ').next())
        .and_then(|n| n.parse().ok())
        .unwrap();
    let said = [
        "冷链货物到货温度须在 2℃ 至 8℃ 之间。",
        "外包装破损的箱子单独拍照。",
        "危险品须放入隔离区。",
    ];
    format!(
        "华东仓储有限公司 内部资料\n\n## 第{n}节\n\n{}",
        said[(n - 1) % 3]
    )
}

/// A scanned PDF (no text layer, or only a stamped page number) is read by the model in the
/// job: each page drawn as a picture by the PDF reader's process and transcribed, in order; a
/// header on every page is left out, and the record keeps the transcription and says the model
/// made it. The real scan fixture is one page.
#[tokio::test]
async fn a_scan_is_read_by_the_model() {
    let llm = librarian();
    llm.transcribe_with(transcribed_page);
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    for (name, bytes, pages) in [
        ("仓库扫描件.pdf", scan_pdf(3, false), 3),
        ("stamped.pdf", scan_pdf(2, true), 2),
        ("scanned-zh.pdf", fixture("scanned-zh.pdf"), 1),
    ] {
        llm.clear();
        let before = llm.transcribed().len();
        let (status, queued) = app
            .post_form(c, "/ingest/expertise", &[], Some((name, &bytes)))
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{name}: {queued}");
        let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
        assert_eq!(job["status"], "done", "{name}: {job}");
        let mut asked = llm.transcribed()[before..].to_vec();
        asked.sort();
        let want: Vec<String> = (1..=pages)
            .map(|n| format!("Page {n} of {pages}."))
            .collect();
        assert_eq!(asked, want, "{name}: every page once");
        let (rec, dir) = record(&app, &job["result"]);
        let md = rec["content"].as_str().unwrap();
        if pages > 1 {
            assert!(
                !md.contains("内部资料"),
                "{name}: the running header is left out: {md}"
            );
        }
        let expected: Vec<String> = (1..=pages)
            .map(|n| {
                let page = transcribed_page(&format!("Page {n} of {pages}."));
                if pages > 1 {
                    page.split_once("\n\n").unwrap().1.to_owned()
                } else {
                    page
                }
            })
            .collect();
        assert_eq!(md, expected.join("\n\n"), "{name}: the pages in order");
        assert_eq!(rec["file"]["converter"], "model", "{name}");
        let original = dir.join(rec["file"]["original"].as_str().unwrap());
        assert_eq!(std::fs::read(original).unwrap(), bytes, "{name}");
        // The librarian reads the transcription as any document.
        let plan = record_of(&llm.calls(Step::Plan)[0]);
        assert!(
            plan["extract"].as_str().unwrap().contains("第1节"),
            "{name}: {plan}"
        );
    }
}

/// A model that doesn't take pictures fails the scan's job at once, with its own answer and
/// what to do; so does a model that sees no text on any page. Nothing of the scan is kept.
#[tokio::test]
async fn a_model_that_reads_no_pictures_fails_the_scan_with_its_answer() {
    let llm = librarian();
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let refusal = "anthropic: HTTP 400 Bad Request: {\"error\":{\"message\":\"unknown variant `image`, expected `text`\"}}";
    for (reply, says) in [
        (format!("{FAILS}{refusal}"), "unknown variant `image`"),
        (String::new(), "found no text"),
    ] {
        llm.transcribe_with(move |_| reply.clone());
        let started = std::time::Instant::now();
        let (status, queued) = app
            .post_form(
                c,
                "/ingest/expertise",
                &[],
                Some(("scan.pdf", &scan_pdf(6, false))),
            )
            .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{queued}");
        let job = app.wait_job(c, queued["job"].as_str().unwrap()).await;
        assert_eq!(job["status"], "failed", "{job}");
        let error = job["error"].as_str().unwrap();
        assert!(error.contains(says), "{error}");
        assert!(error.contains("a model that takes images"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }
    assert!(llm.calls(Step::Read).is_empty(), "nothing was read");
    let sources = app.wiki_dir("acme").parent().unwrap().join("sources");
    let kept = std::fs::read_dir(&sources).map_or(0, |d| d.count());
    assert_eq!(kept, 0, "nothing of the scans is kept");
}

/// A scan of more pages than the model reads is refused at once, with how many it reads.
#[tokio::test]
async fn a_scan_past_the_page_limit_is_refused() {
    let llm = librarian();
    let app = TestApp::with_llm(llm.llm());
    let key = app.create_tenant("acme").await;
    let c = Caller::new(&key);
    let (status, resp) = app
        .post_form(
            c,
            "/ingest/expertise",
            &[],
            Some(("long.pdf", &scan_pdf(201, false))),
        )
        .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{resp}");
    let message = resp["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("201 pages") && message.contains("at most 200"),
        "{message}"
    );
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

/// With the real converter set up, a PDF is still read by the service itself, its table
/// included; MarkItDown is only asked for what the service can't read.
#[tokio::test]
async fn with_markitdown_a_pdf_is_still_read_by_the_service() {
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
    assert!(md.contains("二、盘点责任人"), "{md}");
    assert!(md.contains("| 冷库 | 赵磊 | 周敏 |"), "{md}");
    assert_eq!(rec["file"]["converter"], "builtin");
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

/// PDFs the built-in reader reads (in-process here; the service runs it in its own process),
/// with what each must come back with.
const PDF_FIXTURES: &[(&str, &[&str])] = &[
    (
        "policy-zh.pdf",
        &[
            "# 仓库作业规范（2026 年版）",
            "| 货物类别 | 允许差异 | 处理时限 | 审批人 |\n| --- | --- | --- | --- |",
            "| 30 至 70 公斤 | 重货通 | 45 |",
            "- 冷链货物到货温度须在 2℃ 至 8℃ 之间，超出范围一律拒收。",
            "1. 拣货完成后须二次复核，复核人与拣货人不得为同一人。",
        ],
    ),
    (
        "returns-policy.pdf",
        &[
            "Returns policy",
            "| Card | 5 days | to the same card |",
            "- Unopened goods",
            "Fresh food cannot be returned.",
        ],
    ),
    (
        "warehouse-rules-zh.pdf",
        &[
            "二、盘点责任人",
            "| 冷库 | 赵磊 | 周敏 |",
            "1. 核对送货单与采购单",
        ],
    ),
];

#[test]
fn the_service_reads_every_pdf_fixture_itself() {
    for (name, wanted) in PDF_FIXTURES {
        let md = builtin(name);
        for w in *wanted {
            assert!(md.contains(w), "{name} lacks {w:?}:\n{md}");
        }
    }
}

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
    // MarkItDown splits the Chinese lines of a PDF Word made into broken tables (numbers in
    // cells of their own), which the comparison would count as items; the PDFs PyMuPDF made
    // it reads well.
    let pdfs = PDF_FIXTURES.iter().filter(|(n, _)| *n != "policy-zh.pdf");
    for (name, _) in FIXTURES.iter().chain(pdfs) {
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
