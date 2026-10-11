//! A page: YAML front matter between `---` lines, then a Markdown body.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{PagePath, PageSummary, WikiError};
use crate::kinds::Kind;

/// The front matter fields the service reads and writes. Fields it doesn't model (such as a
/// graph space's `nodes` and `relations`) are kept in `extra` and written back unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct FrontMatter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Search terms in both languages, written by the librarian.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Anchors: the entities and pages this page is about.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub about: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    #[serde(flatten)]
    #[cfg_attr(test, ts(skip))]
    pub extra: BTreeMap<String, serde_norway::Value>,
}

impl FrontMatter {
    fn is_empty(&self) -> bool {
        *self == FrontMatter::default()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Page {
    pub path: PagePath,
    pub front: FrontMatter,
    pub body: String,
}

impl Page {
    pub fn new(path: PagePath) -> Self {
        Self {
            path,
            front: FrontMatter::default(),
            body: String::new(),
        }
    }

    /// Parses page text. Text without a leading `---` line is all body (as in `index.md`).
    pub fn parse(path: PagePath, text: &str) -> Result<Self, WikiError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let malformed = |reason: String| WikiError::Malformed {
            path: path.to_string(),
            reason,
        };

        let mut lines = text.split_inclusive('\n');
        let first = lines.next().unwrap_or("");
        if first.trim_end() != "---" {
            return Ok(Self {
                path,
                front: FrontMatter::default(),
                body: text.to_owned(),
            });
        }

        let mut yaml_len = 0;
        let mut closed = false;
        for line in lines.by_ref() {
            if line.trim_end() == "---" {
                closed = true;
                break;
            }
            yaml_len += line.len();
        }
        if !closed {
            return Err(malformed("front matter has no closing ---".into()));
        }
        let yaml = &text[first.len()..first.len() + yaml_len];
        let body_start = first.len() + yaml_len;
        let rest = &text[body_start..];
        // Skip the closing `---` line itself and one blank line after it.
        let rest = rest.split_once('\n').map_or("", |(_, r)| r);
        let body = rest
            .strip_prefix("\r\n")
            .or_else(|| rest.strip_prefix('\n'))
            .unwrap_or(rest);

        let front: FrontMatter = if yaml.trim().is_empty() {
            FrontMatter::default()
        } else {
            serde_norway::from_str(yaml).map_err(|e| malformed(e.to_string()))?
        };
        Ok(Self {
            path,
            front,
            body: body.to_owned(),
        })
    }

    pub fn render(&self) -> String {
        if self.front.is_empty() {
            return self.body.clone();
        }
        let yaml = serde_norway::to_string(&self.front).expect("front matter serializes");
        format!("---\n{yaml}---\n\n{}", self.body)
    }

    /// A short hash of the page as stored. `GET /pages` returns it and `PUT /pages` sends it
    /// back, so a hand edit can't overwrite a page that changed since it was read.
    pub fn version(&self) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(&Sha256::digest(self.render().as_bytes())[..8])
    }

    pub fn summary(&self) -> PageSummary {
        PageSummary {
            path: self.path.to_string(),
            kind: self.path.kind(),
            title: self.front.title.clone(),
            summary: self.front.summary.clone(),
            updated: self.front.updated.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---
kind: experience
title: Handling partial refunds
aliases: [partial refund, 部分退款, refund after shipment]
summary: Check shipment status first; partial refunds after shipment need a manual ledger entry.
about: [commerce:order, expertise/refund-regulations]
sources: [2026-09-28-task-4812.json]
updated: 2026-09-28
---

# Handling partial refunds

Steps that work.
";

    fn path(p: &str) -> PagePath {
        PagePath::parse(p).unwrap()
    }

    #[test]
    fn parses_the_design_doc_example() {
        let page = Page::parse(path("experience/refund-handling"), SAMPLE).unwrap();
        assert_eq!(page.front.kind, Some(Kind::Experience));
        assert_eq!(page.front.aliases[1], "部分退款");
        assert_eq!(page.front.about.len(), 2);
        assert_eq!(page.front.updated.as_deref(), Some("2026-09-28"));
        assert!(page.body.starts_with("# Handling partial refunds"));
    }

    #[test]
    fn render_roundtrips() {
        let page = Page::parse(path("experience/refund-handling"), SAMPLE).unwrap();
        let again = Page::parse(page.path.clone(), &page.render()).unwrap();
        assert_eq!(page, again);
    }

    #[test]
    fn keeps_unknown_fields() {
        let text = "---\nkind: graph\nspace: commerce\nnodes:\n  order: { key: order_id }\n---\n";
        let page = Page::parse(path("graph/commerce"), text).unwrap();
        assert!(page.front.extra.contains_key("space"));
        assert!(page.front.extra.contains_key("nodes"));
        let again = Page::parse(page.path.clone(), &page.render()).unwrap();
        assert_eq!(page.front, again.front);
    }

    #[test]
    fn crlf_and_bom() {
        let text = "\u{feff}---\r\ntitle: T\r\n---\r\n\r\nbody\r\n";
        let page = Page::parse(path("expertise/t"), text).unwrap();
        assert_eq!(page.front.title.as_deref(), Some("T"));
        assert_eq!(page.body, "body\r\n");
    }

    #[test]
    fn no_front_matter_is_all_body() {
        let page = Page::parse(path("index"), "# Index\n").unwrap();
        assert_eq!(page.front, FrontMatter::default());
        assert_eq!(page.render(), "# Index\n");
    }

    #[test]
    fn unclosed_front_matter_is_malformed() {
        let err = Page::parse(path("expertise/x"), "---\ntitle: T\n").unwrap_err();
        assert!(matches!(err, WikiError::Malformed { .. }));
    }
}
