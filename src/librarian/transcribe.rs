//! A scanned PDF read by the model (design, "Scans read by the model"). The PDF reader's own
//! process draws each page as a picture, and the model writes what the page says as Markdown,
//! a few pages at a time. The pages joined, without a line that heads or ends most of them (a
//! running header or footer), are the document's text, recorded as converted by the model. A
//! page that can't be drawn or read fails the whole document with why, the model's own answer
//! included: nothing is filed from part of a scan.

use anyhow::Context;
use futures_util::{StreamExt, TryStreamExt, stream};
use serde::Deserialize;
use serde_json::json;

use super::{Librarian, prompts};
use crate::convert::Converter;
use crate::llm::{Image, Prompt};

/// Pages transcribed at once.
const AT_ONCE: usize = 4;

/// The most tokens a page's reply may take: a dense page of Chinese is about 1,000.
const PAGE_TOKENS: u32 = 8_192;

/// What a scan needs of the model, told when the model refuses a page.
const NEEDS_IMAGES: &str = "reading a scan needs a model that takes images (such as DeepSeek's \
     deepseek-flash, or Claude); change the model, or upload the document with its text (a \
     .docx, or a PDF with a text layer)";

#[derive(Deserialize)]
struct PageReply {
    markdown: String,
}

fn schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": { "markdown": { "type": "string" } },
        "required": ["markdown"],
        "additionalProperties": false,
    })
}

impl Librarian {
    /// The Markdown of a scanned PDF of `pages` pages, read by the model page by page.
    #[tracing::instrument(name = "librarian_transcribe", skip_all, fields(pages))]
    pub async fn transcribe(
        &self,
        converter: &Converter,
        pdf: &[u8],
        pages: usize,
    ) -> anyhow::Result<String> {
        // In order, a few at a time; the first failure stops the rest.
        let texts: Vec<String> = stream::iter(1..=pages)
            .map(|n| self.transcribe_page(converter, pdf, n, pages))
            .buffered(AT_ONCE)
            .try_collect()
            .await?;
        if !texts.iter().any(|t| t.chars().any(char::is_alphanumeric)) {
            anyhow::bail!(
                "the model found no text on the scan's {pages} page(s); if it doesn't take \
                 images, it saw none: {NEEDS_IMAGES}"
            );
        }
        Ok(join_pages(texts))
    }

    async fn transcribe_page(
        &self,
        converter: &Converter,
        pdf: &[u8],
        n: usize,
        pages: usize,
    ) -> anyhow::Result<String> {
        let png = converter
            .pdf_page(pdf, n)
            .await
            .map_err(|e| anyhow::anyhow!("page {n} of the scan couldn't be drawn: {e}"))?;
        let prompt = Prompt {
            system: prompts::TRANSCRIBE.to_owned(),
            user: format!("Page {n} of {pages}."),
            max_tokens: self.cfg.max_tokens.max(PAGE_TOKENS),
            schema: schema(),
            images: vec![Image {
                media_type: "image/png",
                data: png,
            }],
        };
        let reply = self.llm.complete(&prompt).await.map_err(|e| {
            let model = self.cfg.llm.model().unwrap_or_default();
            let why = format!("{e:#}");
            // A request the model's API refused (4xx) is most likely the picture.
            if why.contains("HTTP 4") {
                anyhow::anyhow!(
                    "the model ({model}) didn't take page {n} of {pages} of the scan: {why}; \
                     {NEEDS_IMAGES}"
                )
            } else {
                anyhow::anyhow!(
                    "the model ({model}) didn't read page {n} of {pages} of the scan: {why}"
                )
            }
        })?;
        let page: PageReply = serde_json::from_str(&reply)
            .with_context(|| format!("the model's transcription of page {n} can't be read"))?;
        Ok(page.markdown.trim().to_owned())
    }
}

/// A line's likeness across pages: without digits (a page's number) or spaces.
fn likeness(line: &str) -> String {
    line.chars()
        .filter(|c| !c.is_ascii_digit() && !c.is_whitespace())
        .collect()
}

/// The pages' Markdown as one text, without a running header or footer: a first or last line
/// alike (numbers aside) on at least two pages and two thirds of them.
fn join_pages(mut pages: Vec<String>) -> String {
    for last in [false, true] {
        let edge = |page: &str| -> Option<String> {
            let mut lines = page.lines().filter(|l| !l.trim().is_empty());
            let line = if last {
                lines.next_back()
            } else {
                lines.next()
            }?;
            let like = likeness(line);
            // A heading is the document's, never furniture.
            (!like.is_empty() && !line.starts_with('#')).then_some(like)
        };
        let edges: Vec<Option<String>> = pages.iter().map(|p| edge(p)).collect();
        let mut counts = std::collections::HashMap::<&str, usize>::new();
        for e in edges.iter().flatten() {
            *counts.entry(e).or_default() += 1;
        }
        let running: Vec<String> = counts
            .into_iter()
            .filter(|(_, n)| *n >= 2 && *n * 3 >= pages.len() * 2)
            .map(|(e, _)| e.to_owned())
            .collect();
        for (page, edge) in pages.iter_mut().zip(&edges) {
            if edge.as_ref().is_some_and(|e| running.contains(e)) {
                let lines: Vec<&str> = page.lines().collect();
                let at = if last {
                    lines.iter().rposition(|l| !l.trim().is_empty())
                } else {
                    lines.iter().position(|l| !l.trim().is_empty())
                };
                if let Some(at) = at {
                    let kept: Vec<&str> = lines
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| *i != at)
                        .map(|(_, l)| *l)
                        .collect();
                    *page = kept.join("\n").trim().to_owned();
                }
            }
        }
    }
    pages
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn running_headers_and_footers_are_left_out() {
        let pages = vec![
            "华东仓储有限公司 内部资料\n\n# 仓库作业规范\n\n本规范适用于华东区。".to_owned(),
            "华东仓储有限公司 内部资料\n\n## 二、库存\n\n每月盘点。\n\n仓库作业规范 第 2 页"
                .to_owned(),
            "华东仓储有限公司  内部资料\n\n## 三、出库\n\n先进先出。\n\n仓库作业规范 第 3 页"
                .to_owned(),
        ];
        assert_eq!(
            join_pages(pages),
            "# 仓库作业规范\n\n本规范适用于华东区。\n\n## 二、库存\n\n每月盘点。\n\n## 三、出库\n\n先进先出。"
        );
    }

    #[test]
    fn a_line_on_one_page_and_headings_stay() {
        let pages = vec![
            "## Returns\n\nWithin 30 days.".to_owned(),
            "## Returns\n\nRefunds in 5 days.\n\nSigned: the manager".to_owned(),
            "".to_owned(),
        ];
        assert_eq!(
            join_pages(pages),
            "## Returns\n\nWithin 30 days.\n\n## Returns\n\nRefunds in 5 days.\n\nSigned: the manager"
        );
        assert_eq!(join_pages(vec!["Only page".to_owned()]), "Only page");
    }
}
