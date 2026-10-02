//! Uploaded files to Markdown (design, "Files are turned into Markdown first"). Markdown and
//! plain text are taken as they are; other files go to the converter, a service of its own
//! (`converter/`, MarkItDown behind `POST /convert`), so this service never links a converter
//! and one can be swapped for another. Only the file's bytes and its extension are sent.

use std::time::Duration;

use axum::body::Bytes;
use serde::Deserialize;

use crate::config::ConverterConfig;
use crate::error::ApiError;

/// Extensions taken as they are, as UTF-8 text.
const TEXT: &[&str] = &["md", "markdown", "txt"];

/// A file as Markdown, and what made it so.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Converted {
    pub markdown: String,
    /// `markitdown`, or `none` for a file taken as it is.
    pub converter: String,
    pub version: String,
}

pub struct Converter {
    url: Option<String>,
    client: reqwest::Client,
}

impl Converter {
    pub fn new(cfg: &ConverterConfig) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            // A sidecar on the same host or network: never through a proxy.
            .no_proxy()
            .build()?;
        Ok(Self {
            url: cfg
                .url
                .as_deref()
                .map(|u| u.trim_end_matches('/').to_owned()),
            client,
        })
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
            });
        }
        let Some(url) = &self.url else {
            return Err(ApiError::Unavailable(format!(
                "no converter is set up for .{ext} files ([converter] url); Markdown and text \
                 files are taken as they are"
            )));
        };
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
            415 => ApiError::UnsupportedMediaType(why),
            413 => ApiError::TooLarge(why),
            422 | 504 => ApiError::Unprocessable(why),
            _ => ApiError::BadGateway(format!("the converter answered {status}: {why}")),
        })
    }
}
