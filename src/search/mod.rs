//! The one search entry (design, "One way in: the wiki as the only search entry"), keyword mode.
//!
//! A search reads only the tenant's in-memory [`SearchSnapshot`]: the page catalog, the name
//! matcher and the tantivy index. It never touches SQLite, Neo4j or an LLM. Steps:
//!
//! 1. prepare: normalize the query and keywords, infer the intent;
//! 2. name match: known titles and aliases pin their pages, node type names become entities;
//! 3. keyword search: BM25 per channel (memories, knowledge), filtered to the caller's audiences;
//! 4. rank: pinned pages first, then by score; pages that match too little are dropped;
//! 5. assemble: summaries, matching lines, routes, coverage and unmatched keywords.
//!
//! The relations slot stays empty until the graph arrives in M3.

pub mod catalog;
pub mod index;
pub mod names;
pub mod text;
pub mod tokenize;
pub mod tracker;

use std::collections::{BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use catalog::{Catalog, FieldText, PageEntry, Slot};
use index::PageIndex;
use names::{NameMatcher, Target};
use text::{RunKind, all_cjk, is_stopword, normalize, on_word_boundary, runs, word_parts};
use tokenize::{Analyzer, Tok, stem};
use tracker::ReadState;

use crate::config::SearchConfig;
use crate::identity::Scope;
use crate::kinds::Kind;
use crate::wiki::{Page, PagePath};

/// Most hits a caller may ask for in one slot.
pub const MAX_SLOT: usize = 20;
const MAX_QUERY_CHARS: usize = 2_000;
const MAX_KEYWORDS: usize = 20;
const MAX_KEYWORD_CHARS: usize = 100;
/// Candidates fetched from the index per channel before ranking.
const CANDIDATES: usize = 50;
/// Matching lines returned per hit, and their length.
const LINES_PER_HIT: usize = 3;
const MAX_LINE_CHARS: usize = 240;
/// Weight of query words when the agent also passed keywords.
const QUERY_WEIGHT: f32 = 0.3;
/// A keyword counts as matched in a field when this share of its parts is found there.
const MATCH_STRENGTH: f32 = 0.6;
/// A slot is strong when its top hit matched at least this share of the keywords, at least one
/// of them in its title, aliases, keywords or summary.
const STRONG_SHARE: f32 = 0.5;

/// Everything a search reads, built from a tenant's wiki. Replaced whole when pages change, so
/// running searches keep the snapshot they started with.
pub struct SearchSnapshot {
    /// A hash of the pages it was built from, so a trace says which wiki a search ran on.
    pub version: String,
    pub catalog: Catalog,
    analyzer: Analyzer,
    names: NameMatcher,
    index: PageIndex,
}

impl SearchSnapshot {
    pub fn build(pages: Vec<Page>) -> anyhow::Result<Self> {
        let version = version_of(&pages);
        let analyzer = Analyzer::new(Catalog::terms_of(&pages));
        let catalog = Catalog::build(pages, &analyzer);
        let names = NameMatcher::build(&catalog);
        let index = PageIndex::build(&catalog, &analyzer)?;
        Ok(Self {
            version,
            catalog,
            analyzer,
            names,
            index,
        })
    }

    pub fn empty() -> anyhow::Result<Self> {
        Self::build(Vec::new())
    }
}

/// The version of a set of pages: 16 hex digits of a SHA-256 over every page's path and text.
/// A search snapshot built from the pages has this version, and the wiki history stores them
/// under it.
pub fn version_of(pages: &[Page]) -> String {
    use sha2::{Digest, Sha256};
    let mut sorted: Vec<&Page> = pages.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = Sha256::new();
    for page in sorted {
        h.update(page.path.as_str().as_bytes());
        h.update([0u8]);
        h.update(page.render().as_bytes());
        h.update([0u8]);
    }
    hex::encode(&h.finalize()[..8])
}

// ---- request and response ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Keyword,
    Semantic,
    Both,
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Intent {
    Relation,
    Procedure,
    History,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memories: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relations: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlotSizes {
    pub memories: usize,
    pub knowledge: usize,
    pub relations: usize,
}

impl Default for SlotSizes {
    fn default() -> Self {
        Self {
            memories: 3,
            knowledge: 3,
            relations: 5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub intent: Option<Intent>,
    #[serde(default)]
    pub slots: SlotRequest,
    /// Follow the top memories to their knowledge. Accepted now; takes effect in M5.
    #[serde(default = "yes")]
    pub hop: bool,
    #[serde(default)]
    pub session: Option<String>,
}

fn yes() -> bool {
    true
}

impl SearchRequest {
    /// Checks limits. Errors are messages for a 400 response.
    pub fn validate(&self) -> Result<(), String> {
        if self.query.trim().is_empty() {
            return Err("query is empty".into());
        }
        if self.query.chars().count() > MAX_QUERY_CHARS {
            return Err(format!("query is longer than {MAX_QUERY_CHARS} characters"));
        }
        if self.mode != Mode::Keyword {
            return Err(
                "only mode \"keyword\" is available; semantic, both and auto arrive in M5".into(),
            );
        }
        if self.keywords.len() > MAX_KEYWORDS || self.entities.len() > MAX_KEYWORDS {
            return Err(format!(
                "at most {MAX_KEYWORDS} keywords and {MAX_KEYWORDS} entities"
            ));
        }
        if self
            .keywords
            .iter()
            .chain(&self.entities)
            .any(|k| k.chars().count() > MAX_KEYWORD_CHARS)
        {
            return Err(format!(
                "keywords and entities are at most {MAX_KEYWORD_CHARS} characters"
            ));
        }
        for n in [
            self.slots.memories,
            self.slots.knowledge,
            self.slots.relations,
        ]
        .into_iter()
        .flatten()
        {
            if n > MAX_SLOT {
                return Err(format!("a slot holds at most {MAX_SLOT} hits"));
            }
        }
        if let Some(s) = &self.session {
            validate_session(s)?;
        }
        Ok(())
    }
}

pub fn validate_session(s: &str) -> Result<(), String> {
    if s.is_empty() || s.len() > tracker::MAX_SESSION_ID_LEN || s.chars().any(char::is_control) {
        return Err(format!(
            "session must be 1 to {} characters without control characters",
            tracker::MAX_SESSION_ID_LEN
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Coverage {
    Strong,
    Weak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchedBy {
    /// A title or alias of the page is in the query.
    Alias,
    Keyword,
}

#[derive(Debug, Clone, Serialize)]
pub struct HitLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Route {
    pub read: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub page: String,
    pub kind: Kind,
    pub title: String,
    #[serde(rename = "match")]
    pub matched_by: MatchedBy,
    pub matched_keywords: Vec<String>,
    pub summary: String,
    pub lines: Vec<HitLine>,
    pub about: Vec<String>,
    pub sources: Vec<String>,
    pub route: Route,
    pub read: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageSlot {
    pub coverage: Coverage,
    pub hits: Vec<Hit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationSlot {
    pub coverage: Coverage,
    pub relation_types: Vec<String>,
    pub edges: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    pub entities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<Intent>,
    pub memories: PageSlot,
    pub knowledge: PageSlot,
    pub relations: RelationSlot,
    pub unmatched_keywords: Vec<String>,
    /// Microseconds per step, for traces; not part of the response body.
    #[serde(skip)]
    pub timings_us: Vec<(&'static str, u64)>,
}

impl SearchResponse {
    /// What a trace keeps of a response: per slot, the pages, how each matched, and coverage.
    pub fn trace_summary(&self) -> serde_json::Value {
        let slot = |s: &PageSlot| {
            serde_json::json!({
                "coverage": s.coverage,
                "hits": s.hits.iter().map(|h| serde_json::json!({
                    "page": h.page,
                    "match": h.matched_by,
                })).collect::<Vec<_>>(),
            })
        };
        serde_json::json!({
            "entities": self.entities,
            "intent": self.intent,
            "memories": slot(&self.memories),
            "knowledge": slot(&self.knowledge),
            "relations": {
                "coverage": self.relations.coverage,
                "relation_types": self.relations.relation_types,
                "edges": self.relations.edges.len(),
            },
            "unmatched_keywords": self.unmatched_keywords,
        })
    }
}

fn micros(since: std::time::Instant) -> u64 {
    u64::try_from(since.elapsed().as_micros()).unwrap_or(u64::MAX)
}

// ---- keywords --------------------------------------------------------------------------------

/// One part of a keyword that must be found for the keyword to match.
enum Unit {
    /// A (stemmed) word or a whole identifier, looked up in a field's word tokens.
    Word(String),
    /// A Chinese run: found whole as a substring, or partly through its words.
    Cjk { run: String, pieces: Vec<String> },
}

struct Keyword {
    /// As the agent wrote it, for `matched_keywords` and `unmatched_keywords`.
    text: String,
    words: Vec<Tok>,
    bigrams: Vec<Tok>,
    units: Vec<Unit>,
    weight: f32,
    /// Counts toward coverage and feedback. Words taken from the query when the agent also
    /// passed keywords only help ranking.
    counted: bool,
}

impl Keyword {
    fn new(snap: &SearchSnapshot, text: &str, weight: f32, counted: bool) -> Option<Self> {
        let norm = normalize(text.trim());
        // (unit, is a stopword). Stopwords are dropped only when something else is left, so a
        // keyword such as "IT" or "new" still counts and can be reported as unmatched.
        let mut units: Vec<(Unit, bool)> = Vec::new();
        for run in runs(&norm) {
            match run.kind {
                RunKind::Word => {
                    let parts: Vec<&str> = word_parts(run.text).collect();
                    if parts.len() > 1 {
                        units.push((Unit::Word(run.text.to_owned()), false));
                    } else {
                        for p in parts {
                            units.push((Unit::Word(stem(p)), is_stopword(p)));
                        }
                    }
                }
                RunKind::Cjk => {
                    let pieces = snap
                        .analyzer
                        .cut(run.text)
                        .into_iter()
                        .filter(|w| !is_stopword(w))
                        .map(str::to_owned)
                        .collect();
                    units.push((
                        Unit::Cjk {
                            run: run.text.to_owned(),
                            pieces,
                        },
                        false,
                    ));
                }
            }
        }
        if units.iter().any(|(_, stop)| !stop) {
            units.retain(|(_, stop)| !stop);
        }
        let units: Vec<Unit> = units.into_iter().map(|(u, _)| u).collect();
        if units.is_empty() {
            return None;
        }
        Some(Self {
            text: text.trim().to_owned(),
            words: snap.analyzer.words(&norm),
            bigrams: snap.analyzer.bigrams(&norm),
            units,
            weight,
            counted,
        })
    }

    /// The share of this keyword found in a text, from 0 to 1.
    fn strength(&self, norm: &str, words: &HashSet<String>) -> f32 {
        let total: f32 = self
            .units
            .iter()
            .map(|u| match u {
                Unit::Word(w) => f32::from(u8::from(words.contains(w))),
                Unit::Cjk { run, pieces } => {
                    if norm.contains(run.as_str()) {
                        1.0
                    } else if pieces.is_empty() {
                        0.0
                    } else {
                        let found = pieces.iter().filter(|p| norm.contains(p.as_str())).count();
                        found as f32 / pieces.len() as f32
                    }
                }
            })
            .sum();
        total / self.units.len() as f32
    }

    fn matches(&self, field: &FieldText) -> bool {
        self.strength(&field.norm, &field.words) >= MATCH_STRENGTH
    }
}

/// Splits a query into keywords when the agent passed none: known names whole, then the
/// remaining content words.
fn keywords_from_query(
    snap: &SearchSnapshot,
    norm_query: &str,
    visible: impl Fn(&Target) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = snap
        .names
        .find(norm_query, visible)
        .into_iter()
        .map(|h| h.name.to_owned())
        .collect();
    // A word already inside an earlier keyword as a whole word (or, for Chinese, anywhere in
    // it) adds nothing: "refund" after "partial refund", but not "api" after "rapid".
    let covered = |w: &str, out: &[String]| {
        out.iter().any(|k| {
            k.match_indices(w)
                .any(|(i, _)| all_cjk(w) || on_word_boundary(k, i, i + w.len()))
        })
    };
    for run in runs(norm_query) {
        let words: Vec<String> = match run.kind {
            RunKind::Word if run.text.contains(['-', '_', '.']) => vec![run.text.to_owned()],
            RunKind::Word => word_parts(run.text)
                .filter(|p| p.chars().count() >= 2)
                .map(str::to_owned)
                .collect(),
            RunKind::Cjk => snap
                .analyzer
                .cut(run.text)
                .into_iter()
                .filter(|w| w.chars().count() >= 2)
                .map(str::to_owned)
                .collect(),
        };
        for w in words {
            if !is_stopword(&w) && !covered(&w, &out) {
                out.push(w);
            }
        }
    }
    out
}

const RELATION_CUES: &[&str] = &[
    "relationship",
    "related to",
    "depends on",
    "dependency",
    "between",
    "upstream",
    "downstream",
    "linked to",
    "关系",
    "关联",
    "依赖",
    "上下游",
    "影响",
];
const PROCEDURE_CUES: &[&str] = &[
    "how do",
    "how to",
    "how should",
    "steps",
    "what should i do",
    "怎么",
    "如何",
    "步骤",
    "流程",
    "怎样",
];
const HISTORY_CUES: &[&str] = &[
    "did we",
    "did i",
    "decided",
    "decision",
    "last time",
    "决定",
    "上次",
    "之前",
    "历史",
    "当时",
];

fn infer_intent(norm_query: &str) -> Option<Intent> {
    let has = |cues: &[&str]| cues.iter().any(|c| norm_query.contains(c));
    if has(RELATION_CUES) {
        Some(Intent::Relation)
    } else if has(HISTORY_CUES) {
        Some(Intent::History)
    } else if has(PROCEDURE_CUES) {
        Some(Intent::Procedure)
    } else {
        None
    }
}

// ---- the search --------------------------------------------------------------------------

struct Candidate {
    ord: usize,
    score: f32,
    /// Distinct names of this page found in the query or keywords.
    pins: usize,
    /// Per counted keyword: matched in the head (title, aliases, keywords, summary), the body.
    head: Vec<bool>,
    body: Vec<bool>,
}

impl Candidate {
    fn matched(&self, i: usize) -> bool {
        self.head[i] || self.body[i]
    }

    fn matched_count(&self) -> usize {
        (0..self.head.len()).filter(|&i| self.matched(i)).count()
    }

    fn head_count(&self) -> usize {
        self.head.iter().filter(|&&h| h).count()
    }

    /// Whether the page matched enough to be shown at all.
    fn above_floor(&self) -> bool {
        let n = self.head.len();
        self.pins > 0 || self.head_count() >= 1 || (n > 0 && self.matched_count() >= n.min(2))
    }

    fn strong(&self) -> bool {
        let n = self.head.len();
        self.pins > 0
            || (n > 0
                && self.head_count() >= 1
                && self.matched_count() as f32 / n as f32 >= STRONG_SHARE)
    }
}

/// Parts of keyword search that can be switched off to measure what each contributes (the
/// thesis ablations). Everything is on in normal use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ablation {
    /// Known titles and aliases in the query pin their pages to the top of the slot.
    pub pin_names: bool,
    /// Search the words version of each field (jieba, stemming).
    pub words: bool,
    /// Search the bigrams version of each field, including phrase matches.
    pub bigrams: bool,
}

impl Default for Ablation {
    fn default() -> Self {
        Self {
            pin_names: true,
            words: true,
            bigrams: true,
        }
    }
}

/// Runs one search for a caller. `reads` is what the caller's session has read so far.
pub fn search(
    snap: &SearchSnapshot,
    scope: &Scope,
    req: &SearchRequest,
    cfg: &SearchConfig,
    reads: &ReadState,
) -> anyhow::Result<SearchResponse> {
    let defaults = cfg.slots;
    let ablation = cfg.ablation;
    let sizes = SlotSizes {
        memories: req
            .slots
            .memories
            .unwrap_or(defaults.memories)
            .min(MAX_SLOT),
        knowledge: req
            .slots
            .knowledge
            .unwrap_or(defaults.knowledge)
            .min(MAX_SLOT),
        relations: req
            .slots
            .relations
            .unwrap_or(defaults.relations)
            .min(MAX_SLOT),
    };

    // Names count only when they point at something the caller may see.
    let visible = |t: &Target| match t {
        Target::Page(i) => scope.can_see(&snap.catalog.pages[*i].path),
        Target::NodeType(_) => true,
    };

    let mut timings_us = Vec::new();
    let mut step = std::time::Instant::now();

    // 1. Prepare.
    let (norm_query, keywords, intent) = tracing::info_span!("prepare").in_scope(|| {
        let norm_query = normalize(&req.query);
        let given: Vec<&String> = req.keywords.iter().chain(&req.entities).collect();
        let mut keywords: Vec<Keyword> = Vec::new();
        let push = |text: &str, weight: f32, counted: bool, into: &mut Vec<Keyword>| {
            let norm = normalize(text.trim());
            if into.iter().any(|k| normalize(&k.text) == norm) {
                return;
            }
            if let Some(k) = Keyword::new(snap, text, weight, counted) {
                into.push(k);
            }
        };
        if given.iter().any(|k| !k.trim().is_empty()) {
            for k in &given {
                push(k, 1.0, true, &mut keywords);
            }
            for w in keywords_from_query(snap, &norm_query, visible) {
                push(&w, QUERY_WEIGHT, false, &mut keywords);
            }
        } else {
            for w in keywords_from_query(snap, &norm_query, visible) {
                push(&w, 1.0, true, &mut keywords);
            }
        }
        let intent = req.intent.or_else(|| infer_intent(&norm_query));
        (norm_query, keywords, intent)
    });

    timings_us.push(("prepare", micros(step)));
    step = std::time::Instant::now();

    // 2. Name match over the query, keywords and entities.
    let (pins, entities) = tracing::info_span!("name_match").in_scope(|| {
        let mut pins: HashMap<usize, BTreeSet<&str>> = HashMap::new();
        let mut entities: BTreeSet<String> = BTreeSet::new();
        let texts = std::iter::once(norm_query.clone()).chain(
            req.keywords
                .iter()
                .chain(&req.entities)
                .map(|k| normalize(k)),
        );
        for text in texts {
            for hit in snap.names.find(&text, visible) {
                for target in hit.targets {
                    match target {
                        Target::Page(i) => {
                            if ablation.pin_names {
                                pins.entry(*i).or_default().insert(hit.name);
                            }
                        }
                        Target::NodeType(e) => {
                            entities.insert(e.clone());
                        }
                    }
                }
            }
        }
        let pins: HashMap<usize, usize> = pins.into_iter().map(|(i, n)| (i, n.len())).collect();
        (pins, entities)
    });

    timings_us.push(("name_match", micros(step)));

    // 3 and 4. Keyword search and ranking, per channel.
    let audiences = scope.audience_keys();
    let counted: Vec<&Keyword> = keywords.iter().filter(|k| k.counted).collect();
    let mut matched_any = vec![false; counted.len()];
    let mut run_channel = |slot: Slot, size: usize| -> anyhow::Result<(Vec<Candidate>, Coverage)> {
        let span = tracing::info_span!("keyword_search", slot = slot.as_str());
        let _enter = span.enter();
        let queries = keywords
            .iter()
            .map(|k| {
                let words: &[Tok] = if ablation.words { &k.words } else { &[] };
                let bigrams: &[Tok] = if ablation.bigrams { &k.bigrams } else { &[] };
                snap.index.keyword_query(words, bigrams, k.weight)
            })
            .collect();
        let found = snap.index.search(queries, &audiences, slot, CANDIDATES)?;
        let mut by_ord: HashMap<usize, f32> = found.into_iter().map(|(s, i)| (i, s)).collect();
        for &i in pins.keys() {
            if snap.catalog.pages[i].slot == slot {
                by_ord.entry(i).or_insert(0.0);
            }
        }
        let mut cands: Vec<Candidate> = by_ord
            .into_iter()
            .map(|(ord, score)| {
                let page = &snap.catalog.pages[ord];
                Candidate {
                    ord,
                    score,
                    pins: pins.get(&ord).copied().unwrap_or(0),
                    head: counted.iter().map(|k| k.matches(&page.head)).collect(),
                    body: counted.iter().map(|k| k.matches(&page.body)).collect(),
                }
            })
            .collect();
        cands.retain(Candidate::above_floor);
        // A keyword counts as matched only through a page that is good enough to show.
        for c in &cands {
            for (i, m) in matched_any.iter_mut().enumerate() {
                *m |= c.matched(i);
            }
        }
        cands.sort_by(|a, b| {
            b.pins
                .min(1)
                .cmp(&a.pins.min(1))
                .then(b.pins.cmp(&a.pins))
                .then(b.score.total_cmp(&a.score))
                .then(a.ord.cmp(&b.ord))
        });
        cands.truncate(size);
        let coverage = match cands.first() {
            Some(top) if top.strong() => Coverage::Strong,
            _ => Coverage::Weak,
        };
        Ok((cands, coverage))
    };
    step = std::time::Instant::now();
    let (mem, mem_cov) = run_channel(Slot::Memories, sizes.memories)?;
    timings_us.push(("keyword_search_memories", micros(step)));
    step = std::time::Instant::now();
    let (kno, kno_cov) = run_channel(Slot::Knowledge, sizes.knowledge)?;
    timings_us.push(("keyword_search_knowledge", micros(step)));
    step = std::time::Instant::now();

    // 5. Assemble.
    let span = tracing::info_span!("assemble");
    let _enter = span.enter();
    let make_slot = |cands: Vec<Candidate>, coverage: Coverage| PageSlot {
        coverage,
        hits: cands
            .into_iter()
            .map(|c| hit(snap, scope, &c, &counted, reads))
            .collect(),
    };
    let unmatched_keywords = counted
        .iter()
        .zip(&matched_any)
        .filter(|(_, m)| !**m)
        .map(|(k, _)| k.text.clone())
        .collect();
    let memories = make_slot(mem, mem_cov);
    let knowledge = make_slot(kno, kno_cov);
    timings_us.push(("assemble", micros(step)));
    Ok(SearchResponse {
        entities: entities.into_iter().collect(),
        intent,
        memories,
        knowledge,
        relations: RelationSlot {
            coverage: Coverage::Weak,
            relation_types: Vec::new(),
            edges: Vec::new(),
        },
        unmatched_keywords,
        timings_us,
    })
}

fn hit(
    snap: &SearchSnapshot,
    scope: &Scope,
    c: &Candidate,
    counted: &[&Keyword],
    reads: &ReadState,
) -> Hit {
    let page = &snap.catalog.pages[c.ord];
    let matched: Vec<&Keyword> = counted
        .iter()
        .enumerate()
        .filter(|(i, _)| c.matched(*i))
        .map(|(_, k)| *k)
        .collect();
    let (lines, heading) = matching_lines(page, &matched);
    let path = page.path.as_str().to_owned();
    Hit {
        read: reads.has_read(&path, heading.as_deref()),
        route: Route {
            read: path.clone(),
            heading,
        },
        page: path,
        kind: page.kind,
        title: page.title.clone(),
        matched_by: if c.pins > 0 {
            MatchedBy::Alias
        } else {
            MatchedBy::Keyword
        },
        matched_keywords: matched.iter().map(|k| k.text.clone()).collect(),
        summary: page.summary.clone(),
        lines,
        about: visible_anchors(scope, &page.about),
        sources: page.sources.clone(),
    }
}

/// Anchors that name pages the caller can't see are dropped; entities pass (graph spaces are
/// filtered in M3).
pub fn visible_anchors(scope: &Scope, about: &[String]) -> Vec<String> {
    about
        .iter()
        .filter(|a| match PagePath::parse(a) {
            Ok(p) => scope.can_see(&p),
            Err(_) => a.contains(':'),
        })
        .cloned()
        .collect()
}

/// The lines of a page that contain the most matched keywords (in page order), and the heading
/// of the section with the most matches, as the route's heading.
fn matching_lines(page: &PageEntry, matched: &[&Keyword]) -> (Vec<HitLine>, Option<String>) {
    if matched.is_empty() {
        return (Vec::new(), None);
    }
    let count = |norm: &str, words: &HashSet<String>| {
        matched
            .iter()
            .filter(|k| k.strength(norm, words) >= MATCH_STRENGTH)
            .count()
    };
    let mut scored: Vec<(usize, usize, usize)> = Vec::new(); // (count, section, line)
    let mut per_section = vec![0usize; page.sections.len()];
    let mut heading_hits = Vec::new();
    for (si, section) in page.sections.iter().enumerate() {
        if let Some(h) = &section.heading {
            let n = count(&normalize(h), &section.heading_words);
            if n > 0 {
                per_section[si] += n;
                heading_hits.push((n, si));
            }
        }
        for (li, line) in section.lines.iter().enumerate() {
            let n = count(&line.norm, &line.words);
            if n > 0 {
                scored.push((n, si, li));
                per_section[si] += n;
            }
        }
    }
    // A section whose heading matched but none of its lines: show its first line.
    if scored.is_empty() {
        heading_hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        if let Some(&(n, si)) = heading_hits
            .iter()
            .find(|&&(_, si)| !page.sections[si].lines.is_empty())
        {
            scored.push((n, si, 0));
        }
    }
    let mut best = scored.clone();
    best.sort_by(|a, b| b.0.cmp(&a.0).then((a.1, a.2).cmp(&(b.1, b.2))));
    best.truncate(LINES_PER_HIT);
    best.sort_by_key(|&(_, s, l)| (s, l));
    let lines = best
        .into_iter()
        .map(|(_, si, li)| {
            let section = &page.sections[si];
            HitLine {
                heading: section.heading.clone(),
                text: truncate(&section.lines[li].text, MAX_LINE_CHARS),
            }
        })
        .collect();
    let heading = per_section
        .iter()
        .enumerate()
        .filter(|&(si, &n)| n > 0 && page.sections[si].level >= 2)
        .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(&a.0)))
        .and_then(|(si, _)| page.sections[si].heading.clone());
    (lines, heading)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_from_cue_words() {
        assert_eq!(
            infer_intent("which supplier does this part depend on? 依赖"),
            Some(Intent::Relation)
        );
        assert_eq!(infer_intent("上次我们决定了什么"), Some(Intent::History));
        assert_eq!(infer_intent("how do i reset sso"), Some(Intent::Procedure));
        assert_eq!(infer_intent("hello"), None);
    }

    #[test]
    fn query_keywords_skip_stopwords() {
        let snap = SearchSnapshot::empty().unwrap();
        let kws = keywords_from_query(
            &snap,
            &normalize("What's the weather in Hangzhou tomorrow?"),
            |_| true,
        );
        assert_eq!(kws, vec!["weather", "hangzhou"]);
        // A word inside another word is still its own keyword.
        let kws = keywords_from_query(&snap, &normalize("rapid api setup"), |_| true);
        assert_eq!(kws, vec!["rapid", "api", "setup"]);
    }

    #[test]
    fn keyword_strength() {
        let snap = SearchSnapshot::empty().unwrap();
        let kw = Keyword::new(&snap, "Change shipping address", 1.0, true).unwrap();
        let text = normalize("The address change form; shipping fees apply");
        let words = snap
            .analyzer
            .words(&text)
            .into_iter()
            .map(|t| t.text)
            .collect();
        assert_eq!(kw.strength(&text, &words), 1.0);
        let zh = Keyword::new(&snap, "部分退款", 1.0, true).unwrap();
        assert_eq!(zh.strength("已发货的部分退款", &HashSet::new()), 1.0);
        // Stopwords go only when something else is left.
        assert_eq!(
            Keyword::new(&snap, "change of address", 1.0, true)
                .unwrap()
                .units
                .len(),
            2
        );
        assert_eq!(Keyword::new(&snap, "IT", 1.0, true).unwrap().units.len(), 1);
        assert!(Keyword::new(&snap, "?!", 1.0, true).is_none());
    }

    /// Keyword search alone, without name pins: which tenant pages a set of keywords reaches,
    /// best first, with the words and bigrams versions switched on or off.
    fn rank(snap: &SearchSnapshot, keywords: &[&str], words: bool, bigrams: bool) -> Vec<String> {
        let queries = keywords
            .iter()
            .filter_map(|k| Keyword::new(snap, k, 1.0, true))
            .map(|k| {
                let w: &[Tok] = if words { &k.words } else { &[] };
                let b: &[Tok] = if bigrams { &k.bigrams } else { &[] };
                snap.index.keyword_query(w, b, 1.0)
            })
            .collect();
        snap.index
            .search(queries, &["tenant".to_string()], Slot::Knowledge, 10)
            .unwrap()
            .into_iter()
            .map(|(_, i)| snap.catalog.pages[i].path.to_string())
            .collect()
    }

    fn snapshot(pages: &[(&str, &str, &str)]) -> SearchSnapshot {
        let pages = pages
            .iter()
            .map(|(path, title, body)| {
                let mut p = Page::new(PagePath::parse(path).unwrap());
                p.front.title = Some((*title).into());
                p.body = format!("# {title}\n\n{body}\n");
                p
            })
            .collect();
        SearchSnapshot::build(pages).unwrap()
    }

    /// Pages with no aliases or keywords, so only the tokenizers and BM25 can find them.
    fn plain_pages() -> SearchSnapshot {
        snapshot(&[
            (
                "expertise/returns",
                "退货规则",
                "签收后七天无理由退货，生鲜除外。",
            ),
            (
                "expertise/tracking",
                "物流查询",
                "用物流单号在快递官网查询轨迹。",
            ),
            (
                "expertise/sso",
                "SSO setup",
                "An SSO login loop (登录死循环) often means a wrong ACS URL; check the order_status webhook too.",
            ),
            (
                "expertise/refunds",
                "Refund handling",
                "Refunded orders are reconciled every night.",
            ),
            (
                "expertise/bearing",
                "轴承维护",
                "CNC-07 主轴轴承按手册设定预紧力后安装。",
            ),
        ])
    }

    #[test]
    fn chinese_keywords_reach_pages_through_segmentation() {
        let snap = plain_pages();
        assert_eq!(
            rank(&snap, &["无理由退货"], true, true)[0],
            "expertise/returns"
        );
        assert_eq!(rank(&snap, &["预紧力"], true, true)[0], "expertise/bearing");
        assert_eq!(rank(&snap, &["登录死循环"], true, true)[0], "expertise/sso");
    }

    /// jieba splits 物流单号 into 物流/单/号 on the page, so the word 单号 is only found through
    /// bigrams: the reason every field is indexed twice.
    #[test]
    fn bigrams_find_what_segmentation_splits() {
        let snap = plain_pages();
        let tracking = "expertise/tracking".to_string();
        assert!(!rank(&snap, &["单号"], true, false).contains(&tracking));
        assert_eq!(rank(&snap, &["单号"], false, true)[0], tracking);
        assert_eq!(rank(&snap, &["单号"], true, true)[0], tracking);
    }

    #[test]
    fn english_keywords_reach_pages_through_stems_and_identifiers() {
        let snap = plain_pages();
        assert_eq!(
            rank(&snap, &["refunding"], true, false)[0],
            "expertise/refunds"
        );
        assert_eq!(
            rank(&snap, &["order status"], true, false)[0],
            "expertise/sso"
        );
        assert_eq!(
            rank(&snap, &["ＣＮＣ－０７"], true, true)[0],
            "expertise/bearing"
        );
        assert_eq!(
            rank(&snap, &["SSO 登录循环"], true, true)[0],
            "expertise/sso"
        );
        assert!(rank(&snap, &["weather"], true, true).is_empty());
    }

    /// The same text declared as a page alias becomes a tenant term: it is kept whole on every
    /// page and in queries, so a keyword equal to it matches as one word.
    #[test]
    fn declared_terms_are_matched_whole() {
        let mut alias = Page::new(PagePath::parse("expertise/terms").unwrap());
        alias.front.title = Some("术语".into());
        alias.front.aliases = vec!["物流单号".into()];
        let mut tracking = Page::new(PagePath::parse("expertise/tracking").unwrap());
        tracking.front.title = Some("物流查询".into());
        tracking.body = "用物流单号查询轨迹。\n".into();
        let snap = SearchSnapshot::build(vec![alias, tracking]).unwrap();
        let words: Vec<String> = snap
            .analyzer
            .words(&normalize("用物流单号查询轨迹"))
            .into_iter()
            .map(|t| t.text)
            .collect();
        assert!(words.contains(&"物流单号".to_string()), "{words:?}");
        let tracking = "expertise/tracking".to_string();
        assert!(rank(&snap, &["物流单号"], true, false).contains(&tracking));
    }
}
