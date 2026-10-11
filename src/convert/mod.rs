//! Uploaded files and fetched pages to Markdown (design, "Files are turned into Markdown
//! first" and "M10"). Markdown and plain text are taken as they are. HTML, Word (`.docx`),
//! spreadsheets (`.xlsx`, `.xlsm`, `.csv`) and PowerPoint (`.pptx`) are
//! converted by the service itself, in pure Rust. Other formats (PDF, `.doc`, `.xls`, `.ppt`) go to the
//! optional MarkItDown converter (`converter/`, `POST /convert`) when `[converter] url` names
//! it, and are refused with what to do otherwise.
//!
//! Files are untrusted, and the built-in readers run in the service's own process, so each
//! one is bounded: the file's size, a zip archive's entries, unpacked size and compression
//! ratio checked before anything is read (zip bombs), XML read without entities or external
//! references, nesting and cell caps, the Markdown's length, and a time limit. Bytes in,
//! Markdown out: no paths, no network.

mod docx;
mod html;
mod md;
mod package;
mod pptx;
mod sheet;
mod xml;

use std::time::Duration;

use axum::body::Bytes;
use serde::Deserialize;

use crate::config::ConverterConfig;
use crate::error::ApiError;

/// Extensions taken as they are, as UTF-8 text.
const TEXT: &[&str] = &["md", "markdown", "txt"];

/// Extensions the service converts itself.
pub const BUILTIN: &[&str] = &[
    "html", "htm", "xhtml", "docx", "xlsx", "xlsm", "csv", "pptx",
];

/// The converter name recorded with what the service converted itself.
pub const BUILTIN_NAME: &str = "builtin";

/// What a built-in conversion may take and make.
#[derive(Debug, Clone)]
pub struct Limits {
    /// The file's size (the 20 MB MarkItDown's container takes).
    pub bytes: usize,
    /// Entries in a zip archive.
    pub zip_entries: usize,
    /// Bytes a zip archive unpacks to, in all.
    pub unpacked: u64,
    /// How many times its compressed size one entry may unpack to.
    pub zip_ratio: u64,
    /// XML nesting.
    pub xml_depth: usize,
    /// XML elements in one part (each costs about a hundred bytes parsed).
    pub xml_elements: usize,
    /// Spreadsheet cells, in all.
    pub cells: usize,
    /// The Markdown's length.
    pub markdown_bytes: usize,
    /// When the conversion must have ended; the readers check it as they go, so the work
    /// stops (a timeout around it alone would leave it running).
    pub until: Option<std::time::Instant>,
}

impl Limits {
    /// Past the deadline: the reader stops here.
    pub fn late(&self) -> Result<(), Refused> {
        match self.until {
            Some(t) if std::time::Instant::now() > t => Err(Refused::TooLarge(
                "converting took longer than allowed".into(),
            )),
            _ => Ok(()),
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            bytes: 20 << 20,
            zip_entries: 10_000,
            unpacked: 256 << 20,
            zip_ratio: 200,
            xml_depth: 256,
            xml_elements: 1_000_000,
            cells: 200_000,
            markdown_bytes: 10 << 20,
            until: None,
        }
    }
}

/// Why the service didn't convert a file itself.
#[derive(Debug, thiserror::Error)]
pub enum Refused {
    /// A file over a limit, or one that would unpack or grow past one.
    #[error("{0}")]
    TooLarge(String),
    /// A damaged file, or not what its extension says.
    #[error("{0}")]
    Unreadable(String),
}

/// A file as Markdown, and what made it so.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Converted {
    pub markdown: String,
    /// `builtin`, `markitdown`, or `none` for a file taken as it is.
    pub converter: String,
    pub version: String,
    /// The addresses of an HTML page's links, its navigation included (which the Markdown
    /// leaves out), for the acquisition crawl. Empty for other formats.
    #[serde(default)]
    pub links: Vec<String>,
}

/// Converts a file the service reads itself. Blocking.
pub fn builtin(ext: &str, bytes: &[u8], limits: &Limits) -> Result<Converted, Refused> {
    if bytes.len() > limits.bytes {
        return Err(Refused::TooLarge(format!(
            "the file is larger than {} MB",
            limits.bytes >> 20
        )));
    }
    let mut links = Vec::new();
    let markdown = match ext {
        "html" | "htm" | "xhtml" => {
            let page = html::convert(bytes, limits)?;
            links = page.links;
            page.markdown
        }
        "docx" => docx::convert(bytes, limits)?,
        "pptx" => pptx::convert(bytes, limits)?,
        "xlsx" | "xlsm" => sheet::workbook(bytes, limits)?,
        "csv" => sheet::csv(bytes, limits)?,
        other => return Err(Refused::Unreadable(format!(".{other} is not read here"))),
    };
    if markdown.len() > limits.markdown_bytes {
        return Err(Refused::TooLarge(format!(
            "the text is longer than {} MB",
            limits.markdown_bytes >> 20
        )));
    }
    Ok(Converted {
        markdown,
        converter: BUILTIN_NAME.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        links,
    })
}

/// Text in UTF-8 (a byte order mark dropped), or else GB18030, as Chinese systems save it.
pub(crate) fn decode_text(bytes: &[u8]) -> String {
    if let Some((enc, _)) = encoding_rs::Encoding::for_bom(bytes) {
        return enc.decode_with_bom_removal(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => encoding_rs::GB18030
            .decode_without_bom_handling(bytes)
            .0
            .into_owned(),
    }
}

/// What to do with a file the service can't convert itself.
fn refusal(ext: &str) -> String {
    let markitdown = "or turn on the MarkItDown converter: `docker compose --profile markitdown up -d converter` and set `[converter] url` in config.toml";
    match ext {
        "doc" => format!(
            ".doc files (Word 97-2003) can't be converted by the service itself. Save the file as .docx, {markitdown}."
        ),
        "xls" | "ods" => format!(
            ".{ext} spreadsheets can't be converted by the service itself. Save the file as .xlsx, {markitdown}."
        ),
        "ppt" => format!(
            ".ppt files (PowerPoint 97-2003) can't be converted by the service itself. Save the file as .pptx, {markitdown}."
        ),
        "pdf" => format!(
            "PDF files can't be converted by the service itself. Save the document as .docx or HTML, {markitdown}."
        ),
        other => format!(
            ".{other} files can't be converted by the service itself: it reads Markdown, text, HTML, Word (.docx), spreadsheets (.xlsx, .csv) and PowerPoint (.pptx). Save the file in one of those formats, {markitdown}."
        ),
    }
}

pub struct Converter {
    url: Option<String>,
    /// MarkItDown's last health check and when, so the overview doesn't ask it every time.
    health: std::sync::Mutex<Option<(std::time::Instant, Result<String, String>)>>,
    timeout: Duration,
    limits: Limits,
    client: reqwest::Client,
}

impl Converter {
    pub fn new(cfg: &ConverterConfig) -> anyhow::Result<Self> {
        let timeout = Duration::from_secs(cfg.timeout_secs);
        let client = reqwest::Client::builder()
            .timeout(timeout)
            // A sidecar on the same host or network: never through a proxy.
            .no_proxy()
            .build()?;
        Ok(Self {
            url: cfg
                .url
                .as_deref()
                .map(|u| u.trim_end_matches('/').to_owned()),
            timeout,
            limits: Limits::default(),
            health: std::sync::Mutex::new(None),
            client,
        })
    }

    /// The MarkItDown converter's address, when one is set up.
    pub fn markitdown_url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// Whether MarkItDown answers, and its version; asked at most every 30 seconds.
    pub async fn markitdown_health(&self) -> Result<String, String> {
        const FRESH: Duration = Duration::from_secs(30);
        if let Some((at, last)) = &*self.health.lock().unwrap_or_else(|e| e.into_inner())
            && at.elapsed() < FRESH
        {
            return last.clone();
        }
        let now = self.ask_markitdown().await;
        *self.health.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((std::time::Instant::now(), now.clone()));
        now
    }

    async fn ask_markitdown(&self) -> Result<String, String> {
        let Some(url) = &self.url else {
            return Err("not set up".into());
        };
        #[derive(Deserialize)]
        struct Health {
            version: String,
        }
        let resp = self
            .client
            .get(format!("{url}/health"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|_| "can't be reached".to_owned())?;
        resp.json::<Health>()
            .await
            .map(|h| h.version)
            .map_err(|_| "answered, but not as a converter".to_owned())
    }

    /// The file as Markdown. `ext` is the file name's extension, lowercase.
    pub async fn convert(&self, ext: &str, bytes: Bytes) -> Result<Converted, ApiError> {
        if TEXT.contains(&ext) {
            let text = String::from_utf8(bytes.to_vec())
                .map_err(|_| ApiError::BadRequest(format!("the .{ext} file is not UTF-8 text")))?;
            return Ok(Converted {
                markdown: text.trim_start_matches('\u{feff}').to_owned(),
                converter: "none".into(),
                version: String::new(),
                links: Vec::new(),
            });
        }
        if BUILTIN.contains(&ext) {
            let (ext_owned, input, mut limits) =
                (ext.to_owned(), bytes.clone(), self.limits.clone());
            limits.until = Some(std::time::Instant::now() + self.timeout);
            let job = tokio::task::spawn_blocking(move || builtin(&ext_owned, &input, &limits));
            let why = match tokio::time::timeout(self.timeout, job).await {
                Ok(Ok(Ok(converted))) => return Ok(converted),
                Ok(Ok(Err(Refused::TooLarge(why)))) => return Err(ApiError::TooLarge(why)),
                Ok(Ok(Err(Refused::Unreadable(why)))) => why,
                Ok(Err(e)) => {
                    tracing::error!(ext, error = %e, "the built-in converter panicked");
                    "the file couldn't be read".to_owned()
                }
                Err(_) => {
                    return Err(ApiError::Unprocessable(format!(
                        "converting the file took over {} s",
                        self.timeout.as_secs()
                    )));
                }
            };
            // A file the service can't read may still be read by MarkItDown.
            if self.url.is_none() {
                return Err(ApiError::Unprocessable(format!(
                    "the .{ext} file can't be converted: {why}"
                )));
            }
            tracing::info!(
                ext,
                why,
                "not read by the built-in converter; trying MarkItDown"
            );
        }
        let Some(url) = &self.url else {
            return Err(ApiError::UnsupportedMediaType(refusal(ext)));
        };
        if bytes.len() > self.limits.bytes {
            return Err(ApiError::TooLarge(format!(
                "the file is larger than {} MB",
                self.limits.bytes >> 20
            )));
        }
        let resp = self
            .client
            .post(format!("{url}/convert"))
            .header("X-File-Extension", ext)
            .body(bytes)
            .send()
            .await
            .map_err(|e| {
                // The error names the converter's address: logged, not told to callers.
                tracing::warn!(error = %e, "the converter didn't answer");
                if e.is_timeout() {
                    ApiError::Unavailable("the converter is busy or slow; try again later".into())
                } else {
                    ApiError::BadGateway("the converter can't be reached".into())
                }
            })?;
        let status = resp.status();
        if status.is_success() {
            return resp
                .json::<Converted>()
                .await
                .map_err(|e| ApiError::BadGateway(format!("the converter's answer: {e}")));
        }
        #[derive(Deserialize, Default)]
        struct Failed {
            error: String,
        }
        let why = resp.json::<Failed>().await.unwrap_or_default().error;
        Err(match status.as_u16() {
            415 => ApiError::UnsupportedMediaType(format!("{why}; {}", refusal(ext))),
            413 => ApiError::TooLarge(why),
            422 | 504 => ApiError::Unprocessable(why),
            _ => ApiError::BadGateway(format!("the converter answered {status}: {why}")),
        })
    }
}
