//! Documents filed as expertise: their ids and versions, and what changed between versions.
//!
//! A document's id is issued by the service (`d_` plus 16 hex digits) the first time the
//! document is filed and returned in the job's result. Sending it with the next upload says
//! "this is a new version". Without it, the librarian looks for the document among those
//! already filed (see `Librarian::identify`). The versions are raw records in `sources/`;
//! the pages filed from them cite them, which is how a new version finds every page the old
//! one wrote, however the librarian spread it over concept pages.
//!
//! A document is never cut into pieces by size: the librarian reads it whole, outlines the
//! concepts in it, and writes one concept at a time (see `Librarian::ingest`).

use serde::{Deserialize, Serialize};
use similar::{DiffTag, TextDiff};

/// A document the service has filed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    /// `tenant`, or the group folder its pages are for.
    pub audience: String,
    pub title: String,
}

/// How a job knew which document it was filing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentMatch {
    /// The caller sent the id.
    Given,
    /// The librarian recognized an earlier document from the wiki pages filed from it.
    Matched,
    /// A document the wiki didn't have; a new id was issued.
    New,
}

pub fn new_document_id() -> String {
    format!("d_{}", hex::encode(rand::random::<[u8; 8]>()))
}

pub fn valid_document_id(id: &str) -> bool {
    id.strip_prefix("d_").is_some_and(|h| {
        h.len() == 16
            && h.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Pieces of a text of at most `max` characters, split at line ends, for searching with:
/// a whole document is too long to be one query. When there are more than `count`, that many
/// are taken evenly across the text, so every part of it is searched.
pub fn chunks(text: &str, max: usize, count: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for line in text.lines() {
        let line: String = line.chars().take(max).collect();
        if !cur.is_empty() && cur.chars().count() + line.chars().count() + 1 > max {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push('\n');
        }
        cur.push_str(&line);
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out.retain(|c| !c.trim().is_empty());
    if out.len() > count && count > 0 {
        let n = out.len();
        out = (0..count).map(|i| out[i * n / count].clone()).collect();
    }
    out
}

/// What changed from one version's text to the next: the changed lines (`-` removed, `+`
/// added) with the line they are at, at most `max` characters; `None` when nothing changed.
pub fn changes(old: &str, new: &str, max: usize) -> Option<String> {
    let diff = TextDiff::from_lines(old, new);
    let old_lines: Vec<&str> = diff.iter_old_slices().collect();
    let new_lines: Vec<&str> = diff.iter_new_slices().collect();
    let mut out = String::new();
    for op in diff.ops() {
        let (tag, old_r, new_r) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            continue;
        }
        out.push_str(&format!("@@ line {}\n", new_r.start + 1));
        for l in &old_lines[old_r] {
            out.push_str(&format!("- {}\n", l.trim_end_matches(['\n', '\r'])));
        }
        for l in &new_lines[new_r] {
            out.push_str(&format!("+ {}\n", l.trim_end_matches(['\n', '\r'])));
        }
    }
    if out.is_empty() {
        return None;
    }
    if out.chars().count() > max {
        let mut cut: String = out.chars().take(max).collect();
        cut.push_str("\n[… more changes left out]\n");
        out = cut;
    }
    Some(out)
}

/// The text of a document in a raw expertise record (`content`), if it has one.
pub fn content_of(record: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(record).ok()?;
    v["content"].as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let id = new_document_id();
        assert!(valid_document_id(&id), "{id}");
        assert!(!valid_document_id("d_123"));
        assert!(!valid_document_id("p_0011223344556677"));
        assert!(!valid_document_id("d_00112233445566GG"));
    }

    #[test]
    fn chunks_cover_the_whole_text() {
        let text: String = (0..100).map(|i| format!("line {i}\n")).collect();
        let c = chunks(&text, 50, 100);
        assert!(c.iter().all(|c| c.chars().count() <= 50));
        assert!(c[0].starts_with("line 0") && c.last().unwrap().ends_with("line 99"));
        let few = chunks(&text, 50, 4);
        assert_eq!(few.len(), 4);
        assert!(
            few[3].contains("line 7"),
            "the last pick is near the end: {few:?}"
        );
    }

    #[test]
    fn changes_between_versions() {
        let old = "## A\nkeep\n## B\nold rule\n## C\ngone\nstay\n";
        let new = "## A\nkeep\n## B\nnew rule\n## C\nstay\n";
        let c = changes(old, new, 1_000).unwrap();
        assert!(c.contains("- old rule") && c.contains("+ new rule"), "{c}");
        assert!(c.contains("- gone"));
        assert!(!c.contains("keep"), "unchanged lines are left out: {c}");
        assert_eq!(changes(new, new, 1_000), None);
        let long = changes("", &"x\n".repeat(1_000), 50).unwrap();
        assert!(long.ends_with("[… more changes left out]\n"));
    }

    #[test]
    fn content_of_a_record() {
        assert_eq!(
            content_of(r#"{"title": "T", "content": "x"}"#).as_deref(),
            Some("x")
        );
        assert_eq!(content_of("not json"), None);
    }
}
