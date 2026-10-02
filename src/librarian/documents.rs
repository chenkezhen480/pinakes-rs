//! Documents filed as expertise: their ids and versions, how each version was read, and
//! what changed between versions.
//!
//! A document's id is issued by the service (`d_` plus 16 hex digits) the first time the
//! document is filed and returned in the job's result. Sending it with the next upload says
//! "this is a new version". Without it, the librarian looks for the document among those
//! already filed (see `Librarian::identify`). The versions are raw records in `sources/`;
//! the pages filed from them cite them, which is how a new version finds every page the old
//! one wrote, however the librarian spread it over concept pages.
//!
//! A document is never cut into pieces by size or by its headings: the librarian reads it from
//! the top, one concept at a time, deciding where each ends (see `reading`). How a version was
//! read is its [`Reading`]: the ledger of steps (which lines, which concept, which page) and the
//! main thread. Pages cite the lines they were written from (`<record>#L<from>-<to>`).

use std::ops::Range;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentMatch {
    /// The caller sent the id.
    Given,
    /// The librarian recognized an earlier document from the wiki pages filed from it.
    Matched,
    /// A document the wiki didn't have; a new id was issued.
    New,
}

/// One step of reading a document: the lines it took, and the concept they explain or that
/// they were skipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerStep {
    /// The first and last line, from 1, in the version this ledger belongs to.
    pub from: usize,
    pub to: usize,
    /// The concept, or none for lines skipped as not knowledge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept: Option<String>,
    /// The page the concept is filed as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    /// What the page cites for these lines (`<record>#L<from>-<to>`): this version's, or an
    /// earlier one's for lines a new version left as they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cites: Option<String>,
}

/// How one version of a document was read: the main thread (a short running brief of what the
/// document is about, for whom, its scope and the terms it defines) and the ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    pub thread: String,
    pub steps: Vec<LedgerStep>,
}

impl Reading {
    /// The ledger as prompt lines: the last `max` steps, after a line saying how many earlier
    /// ones are left out.
    pub fn ledger_lines(&self, max: usize) -> Vec<String> {
        let skip = self.steps.len().saturating_sub(max);
        let mut out = Vec::new();
        if skip > 0 {
            out.push(format!(
                "- ({skip} earlier steps, to line {}, left out)",
                self.steps[skip - 1].to
            ));
        }
        out.extend(
            self.steps[skip..]
                .iter()
                .map(|s| match (&s.concept, &s.page) {
                    (Some(c), Some(p)) => format!("- L{}-{}: {c} → [[{p}]]", s.from, s.to),
                    (Some(c), None) => format!(
                        "- L{}-{}: {c} (no page: its edits were held for review or refused)",
                        s.from, s.to
                    ),
                    _ => format!("- L{}-{}: skipped (not knowledge)", s.from, s.to),
                }),
        );
        out
    }
}

/// A document part way through its reading, saved after every step: a job that stops (the
/// model unreachable, a restart) is resumed from here by the next upload of the same text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadingState {
    /// The raw record being read; the version is recorded under it.
    pub source: String,
    pub content_hash: String,
    pub document: Document,
    pub how: DocumentMatch,
    pub version: u32,
    /// The next line to read, from 1.
    pub cursor: usize,
    pub reading: Reading,
}

/// A short hash of a document's text, to find an unfinished reading of the same text.
pub fn content_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(text.as_bytes())[..8])
}

/// The source id a page cites for some lines of a record.
pub fn cite(record: &str, from: usize, to: usize) -> String {
    format!("{record}#L{from}-{to}")
}

/// A cited source split into its record and, when it names lines, the line range (from 1).
/// `None` when the line part is malformed.
pub fn parse_cite(id: &str) -> Option<(&str, Option<(usize, usize)>)> {
    let Some((record, lines)) = id.split_once('#') else {
        return Some((id, None));
    };
    let (a, b) = lines.strip_prefix('L')?.split_once('-')?;
    let (a, b): (usize, usize) = (a.parse().ok()?, b.parse().ok()?);
    (a >= 1 && a <= b).then_some((record, Some((a, b))))
}

/// The record a cited source is in, without its line range.
pub fn record_name(id: &str) -> &str {
    id.split_once('#').map_or(id, |(r, _)| r)
}

/// Lines `from..=to` (from 1) of a text, if it has them.
pub fn lines_of(text: &str, from: usize, to: usize) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    (from >= 1 && to <= lines.len() && from <= to).then(|| lines[from - 1..to].join("\n"))
}

/// What changed from one version of a document to the next, line by line, with which old line
/// each unchanged new line was.
pub struct Changes {
    /// Changed hunks: old and new line ranges, from 0.
    hunks: Vec<(Range<usize>, Range<usize>)>,
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    /// For each new line (from 0), the old line it was (from 1), when unchanged.
    old_of: Vec<Option<usize>>,
}

impl Changes {
    pub fn new(old: &str, new: &str) -> Self {
        // Split as the reading numbers lines (`str::lines`), so the numbers agree, and a last
        // line without a line end compares equal to the same line followed by more.
        let old_slices: Vec<&str> = old.lines().collect();
        let new_slices: Vec<&str> = new.lines().collect();
        let diff = TextDiff::from_slices(&old_slices, &new_slices);
        let old_lines: Vec<String> = old_slices.iter().map(|l| (*l).to_owned()).collect();
        let new_lines: Vec<String> = new_slices.iter().map(|l| (*l).to_owned()).collect();
        let mut old_of = vec![None; new_lines.len()];
        let mut hunks = Vec::new();
        for op in diff.ops() {
            let (tag, o, n) = op.as_tag_tuple();
            if tag == DiffTag::Equal {
                for (i, j) in n.clone().zip(o.clone()) {
                    old_of[i] = Some(j + 1);
                }
            } else {
                hunks.push((o, n));
            }
        }
        Self {
            hunks,
            old_lines,
            new_lines,
            old_of,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// The old line (from 1) a new line (from 1) was, if it is unchanged.
    pub fn old_line(&self, new_line: usize) -> Option<usize> {
        self.old_of.get(new_line.checked_sub(1)?).copied().flatten()
    }

    /// Whether a hunk belongs to the new lines `from..=to`: it changes one of them, or it
    /// removes lines right after one of them (or before the first line, for line 1).
    fn in_lines(n: &Range<usize>, from: usize, to: usize) -> bool {
        if n.is_empty() {
            (from..=to).contains(&n.start) || (n.start == 0 && from == 1)
        } else {
            n.start < to && n.end >= from
        }
    }

    /// The old lines (from 1) the hunks of new lines `from..=to` change or remove, sorted. A
    /// pure insertion touches the old line before it; a pure removal also touches the line
    /// before it (or after it, at the start), whose step is read again to see the removal.
    pub fn old_lines_touched(&self, from: usize, to: usize) -> Vec<usize> {
        let mut out: Vec<usize> = self
            .hunks
            .iter()
            .filter(|(_, n)| Self::in_lines(n, from, to))
            .flat_map(|(o, n)| {
                if o.is_empty() {
                    return vec![o.start.max(1)];
                }
                let mut lines: Vec<usize> = (o.start + 1..=o.end).collect();
                if n.is_empty() {
                    let neighbour = if o.start >= 1 { o.start } else { o.end + 1 };
                    if neighbour <= self.old_lines.len() {
                        lines.push(neighbour);
                    }
                }
                lines
            })
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Every old line (from 1) any hunk changes or removes, or inserts after.
    pub fn all_old_lines_touched(&self) -> Vec<usize> {
        self.old_lines_touched(1, self.new_lines.len().max(1))
    }

    /// The hunks of new lines `from..=to` as text (`-` removed, `+` added), whole; `None` when
    /// there are none. A step's lines are a window at most, so this is never the whole
    /// document's changes.
    pub fn within(&self, from: usize, to: usize) -> Option<String> {
        let mut out = String::new();
        for (o, n) in &self.hunks {
            if !Self::in_lines(n, from, to) {
                continue;
            }
            out.push_str(&format!("@@ line {}\n", n.start + 1));
            for l in &self.old_lines[o.clone()] {
                out.push_str(&format!("- {l}\n"));
            }
            for l in &self.new_lines[n.clone()] {
                out.push_str(&format!("+ {l}\n"));
            }
        }
        (!out.is_empty()).then_some(out)
    }
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

/// The text of a document in a raw expertise record (`content`), if it has one, as it was
/// filed: trimmed, as the insert endpoint takes it, so its line numbers are the ones read.
pub fn content_of(record: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(record).ok()?;
    v["content"].as_str().map(|c| c.trim().to_owned())
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
    fn changes_between_versions() {
        let old = "## A\nkeep\n## B\nold rule\n## C\ngone\nstay\n";
        let new = "## A\nkeep\n## B\nnew rule\n## C\nstay\n";
        let c = Changes::new(old, new);
        assert!(!c.is_empty());
        let all = c.within(1, 6).unwrap();
        assert!(
            all.contains("- old rule") && all.contains("+ new rule"),
            "{all}"
        );
        assert!(all.contains("- gone"));
        assert!(!all.contains("keep"), "unchanged lines are left out: {all}");
        // Unchanged lines map to their old numbers; changed ones don't.
        assert_eq!(c.old_line(2), Some(2));
        assert_eq!(c.old_line(4), None);
        assert_eq!(c.old_line(6), Some(7));
        // Each hunk belongs to the lines it is in: the new rule to line 4, the removed
        // line to line 5, after which it was.
        assert!(c.within(1, 3).is_none());
        assert!(c.within(4, 4).unwrap().contains("+ new rule"));
        assert!(c.within(5, 6).unwrap().contains("- gone"));
        assert_eq!(c.old_lines_touched(4, 4), vec![4]);
        // The removed line, and the line before it, whose step sees the removal.
        assert_eq!(c.old_lines_touched(5, 6), vec![5, 6]);
        assert_eq!(c.all_old_lines_touched(), vec![4, 5, 6]);
        // Text added after an unchanged last line leaves that line unchanged.
        let more = Changes::new("a\nlast", "a\nlast\nadded\n");
        assert_eq!(more.old_line(2), Some(2));
        assert_eq!(more.within(1, 2), None);
        assert!(Changes::new(new, new).is_empty());
    }

    #[test]
    fn cited_lines() {
        assert_eq!(cite("r.json", 3, 9), "r.json#L3-9");
        assert_eq!(parse_cite("r.json#L3-9"), Some(("r.json", Some((3, 9)))));
        assert_eq!(parse_cite("r.json"), Some(("r.json", None)));
        for bad in ["r.json#3-9", "r.json#L9-3", "r.json#L0-2", "r.json#Lx-2"] {
            assert_eq!(parse_cite(bad), None, "{bad}");
        }
        assert_eq!(record_name("r.json#L3-9"), "r.json");
        assert_eq!(lines_of("a\nb\nc\n", 2, 3).as_deref(), Some("b\nc"));
        assert_eq!(lines_of("a\nb\n", 2, 3), None);
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
