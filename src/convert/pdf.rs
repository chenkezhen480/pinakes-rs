//! PDF to Markdown by the service itself (design, "PDF read by the service"). The text layer
//! is read by pdfni (pure Rust, tables as Markdown tables, headers and footers repeated on every
//! page left out), chosen against MarkItDown and six other Rust readers on Word, PowerPoint and
//! PyMuPDF PDFs in Chinese and English, and against crafted files (the comparison is in the
//! design).
//!
//! A PDF reader is a large parser of untrusted input that isn't ours, and on crafted files it
//! can take gigabytes (a page of a million text operators) where our own readers check their
//! limits as they go. So the service never runs it in its own process: `pinakes-pdf` (the
//! service's second binary) reads one PDF from its stdin and writes the Markdown to its stdout,
//! with its memory capped by its allocator ([`MEMORY`]); the service gives it the file, a
//! deadline, and kills it past that. A crash, a cap reached or a hang ends that process only.
//! A PDF with no text layer (a scan) has no text here; an upload's scan is read by the model
//! instead (`librarian::transcribe`), from its pages rendered as images, in the same process of
//! its own, by hayro (pure Rust, the renderer's crafted files measured in the design).

use std::path::Path;
use std::time::Duration;

use super::{Limits, Refused};

/// Memory the reader process may allocate; past it, it stops.
pub const MEMORY: usize = 1 << 30;

/// How `pinakes-pdf` ends, besides 0 (its answer on stdout): the reason is on stderr.
pub const EXIT_UNREADABLE: i32 = 2;
pub const EXIT_NO_TEXT: i32 = 4;
pub const EXIT_TOO_LARGE: i32 = 5;

/// A page rendered for the model to read: its longer side, in pixels (about what the models
/// look at: DeepSeek scales an image down to about 1300 by 1300, Claude to 1568 on the longer
/// side).
pub const PAGE_PIXELS: f32 = 1600.0;
/// The most a rendered page's PNG may take.
const MAX_PAGE_PNG: usize = 16 << 20;

/// Fewer letters and digits than this a page, on average, is no text layer to speak of: a scan
/// whose pages carry only a stamped number or a file name is read as a scan.
const LETTERS_PER_PAGE: usize = 20;

/// The Markdown of a PDF's text layer, read in this process. Its memory isn't bounded on
/// crafted files: only `pinakes-pdf` (and tests) call it.
pub fn markdown(bytes: &[u8], limits: &Limits) -> Result<String, Refused> {
    if bytes.len() > limits.bytes {
        return Err(Refused::TooLarge(format!(
            "the file is larger than {} MB",
            limits.bytes >> 20
        )));
    }
    let options = pdfni::ExtractOptions {
        detect_header_footer: true,
        // A table without lines and without a column of numbers or dates is taken from 3 rows
        // on, not 4: on the twelve documents of the comparison only two such tables changed,
        // both into the tables they are.
        text: pdfni::TextSettings {
            untyped_min_rows: 3,
            ..Default::default()
        },
        reader: pdfni::ReaderSettings {
            max_decoded_bytes: usize::try_from(limits.unpacked).unwrap_or(usize::MAX),
            ..Default::default()
        },
        ..Default::default()
    };
    let md = pdfni::extract_markdown_from_bytes(bytes, None, &options).map_err(|e| match e {
        pdfni::Error::BadPassword | pdfni::Error::SecurityRestricted => {
            Refused::Unreadable("the PDF is protected by a password".into())
        }
        e => Refused::Unreadable(format!("the PDF couldn't be read ({e})")),
    })?;
    let md = tidy(&md);
    let letters = md.chars().filter(|c| c.is_alphanumeric()).count();
    if letters < LETTERS_PER_PAGE * open(bytes).map_or(1, |pdf| pdf.pages().len()) {
        return Err(Refused::NoText(
            "the PDF has no text layer (a scanned document?)".into(),
        ));
    }
    if md.len() > limits.markdown_bytes {
        return Err(Refused::TooLarge(format!(
            "the text is longer than {} MB",
            limits.markdown_bytes >> 20
        )));
    }
    Ok(md)
}

/// List items whose bullet came out as the symbol font's private-use character (Word's bullets
/// do), a soft hyphen (some generators' bullet) or `•`, become Markdown list items; no-break
/// spaces, which carry nothing in a PDF's text, become spaces, so keywords and comparisons meet
/// the words as typed.
fn tidy(md: &str) -> String {
    let md = md.replace('\u{a0}', " ");
    let mut out = String::with_capacity(md.len());
    for line in md.lines() {
        let indent = line.len() - line.trim_start().len();
        let rest = &line[indent..];
        let mut chars = rest.chars();
        match (chars.next(), chars.clone().next()) {
            (Some(c), Some(next))
                if (('\u{f000}'..='\u{f0ff}').contains(&c) || c == '•' || c == '\u{ad}')
                    && next.is_whitespace() =>
            {
                out.push_str(&line[..indent]);
                out.push_str("- ");
                out.push_str(chars.as_str().trim_start());
            }
            _ => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// The PDF as hayro reads it.
fn open(bytes: &[u8]) -> Result<hayro::hayro_syntax::Pdf, Refused> {
    use hayro::hayro_syntax::LoadPdfError;
    hayro::hayro_syntax::Pdf::new(bytes.to_vec()).map_err(|e| match e {
        LoadPdfError::Decryption(_) => {
            Refused::Unreadable("the PDF is protected by a password".into())
        }
        LoadPdfError::Invalid => Refused::Unreadable("the PDF couldn't be read".into()),
    })
}

/// How many pages the PDF has. In this process: only `pinakes-pdf` (and tests) call it.
pub fn count_pages(bytes: &[u8]) -> Result<usize, Refused> {
    Ok(open(bytes)?.pages().len())
}

/// Page `n` (from 1) as a PNG, its longer side [`PAGE_PIXELS`], on white. In this process, its
/// memory not bounded on crafted files: only `pinakes-pdf` (and tests) call it.
pub fn page_png(bytes: &[u8], n: usize) -> Result<Vec<u8>, Refused> {
    use hayro::vello_cpu::color::palette::css::WHITE;
    let pdf = open(bytes)?;
    let page = n
        .checked_sub(1)
        .and_then(|i| pdf.pages().get(i))
        .ok_or_else(|| Refused::Unreadable(format!("the PDF has no page {n}")))?;
    let (w, h) = page.render_dimensions();
    // A page drawn tiny isn't blown up past ten times.
    let scale = (PAGE_PIXELS / w.max(h).max(1.0)).min(10.0);
    let pixmap = hayro::render(
        page,
        &hayro::RenderCache::new(),
        &hayro::hayro_interpret::InterpreterSettings::default(),
        &hayro::RenderSettings::default(),
        &hayro::PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    pixmap
        .into_png()
        .map_err(|e| Refused::Unreadable(format!("page {n} couldn't be drawn ({e})")))
}

/// The number of pages of a PDF, counted by `reader` (the `pinakes-pdf` binary) in a process
/// of its own, within `timeout`.
pub async fn pages(
    reader: &Path,
    bytes: &[u8],
    limits: &Limits,
    timeout: Duration,
) -> Result<usize, Refused> {
    let out = run(reader, &["pages".into()], bytes, limits, 32, timeout).await?;
    std::str::from_utf8(&out)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .ok_or_else(|| Refused::Unreadable("the PDF reader's page count can't be read".into()))
}

/// Page `n` (from 1) of a PDF as a PNG, drawn by `reader` in a process of its own, within
/// `timeout`.
pub async fn page(
    reader: &Path,
    bytes: &[u8],
    n: usize,
    limits: &Limits,
    timeout: Duration,
) -> Result<Vec<u8>, Refused> {
    let args = ["page".to_owned(), n.to_string()];
    let png = run(reader, &args, bytes, limits, MAX_PAGE_PNG, timeout).await?;
    if !png.starts_with(b"\x89PNG") {
        return Err(Refused::Unreadable(format!(
            "page {n} wasn't drawn as a picture"
        )));
    }
    Ok(png)
}

/// A PDF as Markdown, read by `reader` (the `pinakes-pdf` binary) in a process of its own,
/// within `timeout`.
pub async fn convert(
    reader: &Path,
    bytes: &[u8],
    limits: &Limits,
    timeout: Duration,
) -> Result<String, Refused> {
    let args = [
        "text".to_owned(),
        limits.unpacked.to_string(),
        limits.markdown_bytes.to_string(),
    ];
    let out = run(reader, &args, bytes, limits, limits.markdown_bytes, timeout).await?;
    String::from_utf8(out)
        .map_err(|_| Refused::Unreadable("the PDF reader's text isn't UTF-8".into()))
}

/// Runs `reader` with `args` on the PDF (its stdin), within `timeout`, and returns its stdout:
/// at most `max` bytes, or the reason it gave (or that it stopped) as a refusal.
async fn run(
    reader: &Path,
    args: &[String],
    bytes: &[u8],
    limits: &Limits,
    max: usize,
    timeout: Duration,
) -> Result<Vec<u8>, Refused> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if bytes.len() > limits.bytes {
        return Err(Refused::TooLarge(format!(
            "the file is larger than {} MB",
            limits.bytes >> 20
        )));
    }
    let mut child = tokio::process::Command::new(reader)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            tracing::error!(reader = %reader.display(), error = %e, "the PDF reader can't be started");
            Refused::Unreadable(format!(
                "the service's PDF reader ({}) can't be started",
                reader.file_name().unwrap_or_default().to_string_lossy()
            ))
        })?;
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Err(Refused::Unreadable("the PDF reader didn't start".into()));
    };
    let input = bytes.to_vec();
    // The child dropped with this future (past the deadline) is killed.
    let work = async move {
        let feed = async move {
            // A reader that stops early closes its stdin; that is its answer, not an error.
            let _ = stdin.write_all(&input).await;
            drop(stdin);
        };
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let read_out = async { stdout.take(max as u64 + 1).read_to_end(&mut out).await };
        let read_err = async { stderr.take(16 << 10).read_to_end(&mut err).await };
        let ((), r_out, _) = tokio::join!(feed, read_out, read_err);
        if r_out.is_err() || out.len() > max {
            let _ = child.kill().await;
            return Err(Refused::TooLarge(format!(
                "what the PDF reader made of it is larger than {} MB",
                max >> 20
            )));
        }
        let status = child
            .wait()
            .await
            .map_err(|_| Refused::Unreadable("the PDF reader stopped".into()))?;
        Ok((status, out, err))
    };
    let (status, out, err) = tokio::time::timeout(timeout, work).await.map_err(|_| {
        Refused::TooLarge(format!(
            "reading the PDF took longer than {} s",
            timeout.as_secs()
        ))
    })??;
    let why = String::from_utf8_lossy(&err).trim().to_owned();
    match status.code() {
        Some(0) => Ok(out),
        Some(EXIT_NO_TEXT) => Err(Refused::NoText(why)),
        Some(EXIT_TOO_LARGE) => Err(Refused::TooLarge(why)),
        Some(EXIT_UNREADABLE) => Err(Refused::Unreadable(why)),
        code => {
            tracing::warn!(?code, why, "the PDF reader stopped without an answer");
            Err(Refused::Unreadable(
                "the PDF couldn't be read: reading it needed more memory than allowed, or the reader failed".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_bullets_become_list_items() {
        let md = "Rules\n\u{f0b7}  Goods must be unused.\n  \u{f0a7} Nested one\n• Third\n\u{f0b7}not a bullet\n";
        assert_eq!(
            tidy(md),
            "Rules\n- Goods must be unused.\n  - Nested one\n- Third\n\u{f0b7}not a bullet\n"
        );
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn a_word_pdf_keeps_its_table_and_numbers() {
        let md = markdown(&fixture("policy-zh.pdf"), &Limits::default()).unwrap();
        assert!(
            md.contains("| 货物类别 | 允许差异 | 处理时限 | 审批人 |"),
            "{md}"
        );
        assert!(md.contains("- 冷链货物到货温度须在 2℃ 至 8℃ 之间"), "{md}");
        assert!(md.contains("连续 90 天无出库的货物视为呆滞库存"), "{md}");
        // The header repeated on every page is left out.
        assert!(!md.contains("华东仓储有限公司 内部资料"), "{md}");
    }

    #[test]
    fn a_scan_has_no_text() {
        let err = markdown(&fixture("scanned-zh.pdf"), &Limits::default()).unwrap_err();
        assert!(matches!(err, Refused::NoText(_)), "{err}");
    }

    #[test]
    fn a_damaged_pdf_is_unreadable() {
        let mut bytes = fixture("policy-zh.pdf");
        bytes.truncate(bytes.len() / 3);
        assert!(matches!(
            markdown(&bytes, &Limits::default()),
            Err(Refused::Unreadable(_))
        ));
        assert!(matches!(
            markdown(b"%PDF-1.7\nnot really", &Limits::default()),
            Err(Refused::Unreadable(_))
        ));
    }
}
