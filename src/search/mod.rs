//! The one search entry (design, "One way in: the wiki as the only search entry").
//!
//! A search reads only the tenant's in-memory copies: the [`SearchSnapshot`] of the wiki (page
//! catalog, name matcher, tantivy index, page vectors) and the [`GraphSnapshot`]. It never
//! touches SQLite, Neo4j or an LLM; meaning-based modes run the local embedding model on the
//! query. Steps:
//!
//! 1. prepare: normalize the query and keywords, infer the intent;
//! 2. name match: known titles and aliases pin their pages; node and relation type names,
//!    node names, identifier keys and space names become the starting entities, only those the
//!    caller may see;
//! 3. search per channel (memories, knowledge), filtered to the caller's audiences: BM25 over
//!    keywords, similarity of the query to the page vectors, or both (see [`Mode`]);
//! 4. rank: pinned pages first, then by score; pages that match too little are dropped;
//! 5. hop: the knowledge pages the top memories are anchored to join the knowledge slot,
//!    below its strong hits (`via: memory`);
//! 6. relations: edges around the starting entities and the entities the hits are anchored to
//!    (see [`relations`]);
//! 7. assemble: summaries, matching lines, routes, coverage and unmatched keywords.

pub mod catalog;
pub mod index;
pub mod names;
pub mod relations;
pub mod text;
pub mod tokenize;
pub mod tracker;
pub mod vectors;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use petgraph::graph::NodeIndex;

use serde::{Deserialize, Serialize};

use catalog::{Catalog, FieldText, PageEntry, Slot};
use index::PageIndex;
use names::{NameMatcher, Target, find_in};
use text::{RunKind, all_cjk, is_stopword, normalize, on_word_boundary, runs, word_parts};
use tokenize::{Analyzer, Tok, stem};
use tracker::ReadState;

use crate::config::SearchConfig;
use crate::embed::Embedder;
use crate::graph::{EntityRef, GraphSnapshot, Visible};
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
/// A page found by meaning is shown only when its best part is at least this similar to the
/// query, and then makes its slot strong (cosine similarity, jina-embeddings-v2-base-zh).
/// Measured on the test set: off-topic questions reach at most 0.20, the pages that answer
/// cross-language questions at least 0.30 (tests/meaning_testset.rs).
const MEANING_FLOOR: f32 = 0.25;

/// Everything a search reads, built from a tenant's wiki. Replaced whole when pages change, so
/// running searches keep the snapshot they started with.
pub struct SearchSnapshot {
    /// A hash of the pages it was built from, so a trace says which wiki a search ran on.
    pub version: String,
    pub catalog: Catalog,
    analyzer: Analyzer,
    names: NameMatcher,
    index: PageIndex,
    /// The pages' vectors, when an embedding model is configured.
    pub vectors: Option<vectors::Vectors>,
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
            vectors: None,
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

/// How the memories and knowledge channels rank pages. Names are matched in every mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// BM25 over the keywords.
    #[default]
    Keyword,
    /// Similarity of the query to the page vectors.
    Semantic,
    /// Both: pages the keywords match strongly keep their keyword rank, meaning orders the
    /// rest.
    Both,
    /// Keywords first; a slot that comes back weak is searched again with both.
    Auto,
}

impl Mode {
    /// Whether this mode needs page vectors and the embedding model.
    pub fn needs_meaning(self) -> bool {
        matches!(self, Mode::Semantic | Mode::Both)
    }
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
    /// `[search] default_mode` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default)]
    pub entities: Vec<String>,
    #[serde(default)]
    pub intent: Option<Intent>,
    #[serde(default)]
    pub slots: SlotRequest,
    /// Follow the top memories to the knowledge pages they are anchored to.
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
    /// Keyword search found it.
    Keyword,
    /// Only its meaning is close to the query's.
    Meaning,
    /// Not matched itself: a top memory is anchored to it (`via: memory`).
    Anchor,
}

/// How a hit got into its slot other than by matching the request itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// A top memory is anchored to it (the memory-to-knowledge hop).
    Memory,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
    pub lines: Vec<HitLine>,
    pub about: Vec<String>,
    /// The raw records the page cites, as `/read` ids (`sources/<name>`).
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
    /// Relation types the request names, as `schema:relation`.
    pub relation_types: Vec<String>,
    pub edges: Vec<relations::Edge>,
    /// Where to go next: a walk from the first named node, and the path between the first two
    /// named nodes of one space.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<GraphRoute>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphRoute {
    pub graph: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathRoute>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathRoute {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchResponse {
    /// The mode the search ran in: the request's, or the default.
    pub mode: Mode,
    pub entities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<Intent>,
    pub memories: PageSlot,
    pub knowledge: PageSlot,
    pub relations: RelationSlot,
    pub unmatched_keywords: Vec<String>,
    /// Keywords that named nodes only as part of a node's name, with those nodes. For traces,
    /// so lint (M8) can ask the host for the missing aliases; not part of the response body.
    #[serde(skip)]
    pub partial_names: Vec<(String, Vec<String>)>,
    /// Microseconds per step, for traces; not part of the response body.
    #[serde(skip)]
    pub timings_us: Vec<(&'static str, u64)>,
}

impl SearchResponse {
    /// What a trace keeps of a response: per slot, the pages, how each matched, and coverage;
    /// and the keywords that named nodes only by part of their names.
    pub fn trace_summary(&self) -> serde_json::Value {
        let slot = |s: &PageSlot| {
            serde_json::json!({
                "coverage": s.coverage,
                "hits": s.hits.iter().map(|h| {
                    let mut hit = serde_json::json!({ "page": h.page, "match": h.matched_by });
                    if let Some(via) = h.via {
                        hit["via"] = serde_json::json!(via);
                    }
                    hit
                }).collect::<Vec<_>>(),
            })
        };
        let mut out = serde_json::json!({
            "mode": self.mode,
            "entities": self.entities,
            "memories": slot(&self.memories),
            "knowledge": slot(&self.knowledge),
            "relations": {
                "coverage": self.relations.coverage,
                "relation_types": self.relations.relation_types,
                "edges": self.relations.edges.iter()
                    .map(|e| serde_json::json!({ "from": e.from, "type": e.ty, "to": e.to }))
                    .collect::<Vec<_>>(),
            },
            "unmatched_keywords": self.unmatched_keywords,
            "partial_names": self.partial_names.iter()
                .map(|(k, nodes)| serde_json::json!({ "keyword": k, "nodes": nodes }))
                .collect::<Vec<_>>(),
        });
        if let Some(intent) = self.intent {
            out["intent"] = serde_json::json!(intent);
        }
        out
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
    graph: &GraphSnapshot,
    norm_query: &str,
    visible: impl Fn(&Target) -> bool,
) -> Vec<String> {
    let mut out: Vec<String> = find_in(&[&snap.names, &graph.names], norm_query, visible)
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
    /// What it is ranked by after pins: BM25, similarity or the fused rank, by mode.
    score: f32,
    /// Its BM25 score, when keyword search found it.
    bm25: Option<f32>,
    /// Its similarity to the query, when meaning search found it.
    similarity: Option<f32>,
    /// Distinct names of this page found in the query or keywords.
    pins: usize,
    /// Per counted keyword: matched in the head (title, aliases, keywords, summary), the body.
    head: Vec<bool>,
    body: Vec<bool>,
    /// Brought in by the memory-to-knowledge hop.
    via: Option<Via>,
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

    /// Whether the page matched enough to be shown at all: named, matched by keywords enough
    /// (when keyword search found it), or close enough in meaning.
    fn above_floor(&self) -> bool {
        let n = self.head.len();
        self.pins > 0
            || (self.bm25.is_some()
                && (self.head_count() >= 1 || (n > 0 && self.matched_count() >= n.min(2))))
            || self.meaning_strong()
    }

    /// Close enough in meaning to show, and then to make its slot strong.
    fn meaning_strong(&self) -> bool {
        self.similarity.is_some_and(|s| s >= MEANING_FLOOR)
    }

    /// Whether the keywords match it well: at least one in its head, and enough of them.
    fn keyword_strong(&self) -> bool {
        let n = self.head.len();
        n > 0 && self.head_count() >= 1 && self.matched_count() as f32 / n as f32 >= STRONG_SHARE
    }

    fn strong(&self) -> bool {
        self.pins > 0 || self.keyword_strong() || self.meaning_strong()
    }

    fn matched_by(&self) -> MatchedBy {
        if self.pins > 0 {
            MatchedBy::Alias
        } else if self.bm25.is_some() {
            MatchedBy::Keyword
        } else if self.similarity.is_some() {
            MatchedBy::Meaning
        } else {
            MatchedBy::Anchor
        }
    }
}

fn candidate(
    snap: &SearchSnapshot,
    counted: &[&Keyword],
    ord: usize,
    bm25: Option<f32>,
    similarity: Option<f32>,
    pins: &HashMap<usize, usize>,
) -> Candidate {
    let page = &snap.catalog.pages[ord];
    Candidate {
        ord,
        score: 0.0,
        bm25,
        similarity,
        pins: pins.get(&ord).copied().unwrap_or(0),
        head: counted.iter().map(|k| k.matches(&page.head)).collect(),
        body: counted.iter().map(|k| k.matches(&page.body)).collect(),
        via: None,
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

/// Why a search in a meaning-based mode can't run.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct NoMeaning(pub &'static str);

/// Whether meaning-based search can run on this snapshot: there is an embedding model, and
/// the pages have vectors made by it.
pub fn has_meaning(snap: &SearchSnapshot, embedder: Option<&dyn Embedder>) -> bool {
    meaning(snap, embedder).is_some()
}

/// Whether a search in this mode can run on this snapshot: `semantic` and `both` need
/// [`has_meaning`]. `auto` runs without it, by keywords alone.
pub fn check_meaning(
    mode: Mode,
    snap: &SearchSnapshot,
    embedder: Option<&dyn Embedder>,
) -> Result<(), NoMeaning> {
    if mode.needs_meaning() && !has_meaning(snap, embedder) {
        return Err(NoMeaning(
            "semantic search is off: no embedding model is loaded, or this tenant's page vectors could not be built ([embedding] model; see the service log)",
        ));
    }
    Ok(())
}

/// The page vectors and the model that made them, when both are there.
fn meaning<'a>(
    snap: &'a SearchSnapshot,
    embedder: Option<&'a dyn Embedder>,
) -> Option<(&'a vectors::Vectors, &'a dyn Embedder)> {
    match (&snap.vectors, embedder) {
        (Some(v), Some(e)) if v.model == e.model() => Some((v, e)),
        _ => None,
    }
}

/// Runs one search for a caller over the wiki and graph snapshots. `reads` is what the caller's
/// session has read so far. `embedder` embeds the query in the meaning-based modes; call
/// [`check_meaning`] first. Blocking when it embeds: run it off the async threads.
pub fn search(
    snap: &SearchSnapshot,
    graph: &GraphSnapshot,
    scope: &Scope,
    req: &SearchRequest,
    cfg: &SearchConfig,
    reads: &ReadState,
    embedder: Option<&dyn Embedder>,
) -> anyhow::Result<SearchResponse> {
    let mode = req.mode.unwrap_or(cfg.default_mode);
    check_meaning(mode, snap, embedder)?;
    let meaning = meaning(snap, embedder);
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

    // Which graph spaces the caller may see, once per request.
    let vis = graph.visible(scope);
    // Names count only when they point at something the caller may see.
    let visible = |t: &Target| match t {
        Target::Page(i) => scope.can_see(&snap.catalog.pages[*i].path),
        Target::NodeType(_) | Target::RelationType(_) => true,
        Target::Node(n) => vis.node(graph, NodeIndex::new(*n as usize)),
        Target::Space(i) => vis.space(*i),
    };
    // Relation type names only steer the relations channel; they don't become keywords.
    let keyword_names = |t: &Target| !matches!(t, Target::RelationType(_)) && visible(t);

    let mut timings_us = Vec::new();
    let mut step = std::time::Instant::now();

    // 1. Prepare.
    // Ids in the query are looked up whole, once; their pieces are not keywords.
    let (query_ids, query_words) = resolve_ids(&req.query, snap, graph, &vis);
    let (keywords, intent) = tracing::info_span!("prepare").in_scope(|| {
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
            for w in keywords_from_query(snap, graph, &query_words, keyword_names) {
                push(&w, QUERY_WEIGHT, false, &mut keywords);
            }
        } else {
            for w in keywords_from_query(snap, graph, &query_words, keyword_names) {
                push(&w, 1.0, true, &mut keywords);
            }
        }
        let intent = req.intent.or_else(|| infer_intent(&norm_query));
        (keywords, intent)
    });

    timings_us.push(("prepare", micros(step)));
    step = std::time::Instant::now();
    let counted: Vec<&Keyword> = keywords.iter().filter(|k| k.counted).collect();

    // 2. Name match over the query, keywords and entities.
    let found = tracing::info_span!("name_match").in_scope(|| {
        let mut f = Found::default();
        // Entity ids the agent already holds are looked up exactly and taken out of the text
        // before names are matched (see `resolve_ids`).
        let mut take_ids = |raw: &str, (ids, rest): (Vec<IdHit>, String)| -> String {
            let mut resolved = false;
            for hit in ids {
                match hit {
                    IdHit::Node(Some(n)) => {
                        push_unique(&mut f.nodes, n);
                        push_unique(&mut f.explicit, n);
                        resolved = true;
                    }
                    IdHit::Node(None) => {}
                    IdHit::Type(t) => {
                        push_unique(&mut f.node_types, t);
                        resolved = true;
                    }
                }
            }
            if resolved {
                f.id_texts.insert(normalize(raw.trim()));
            }
            rest
        };
        let query_text = take_ids(&req.query, (query_ids, query_words.clone()));
        let agent_texts: Vec<String> = req
            .keywords
            .iter()
            .chain(&req.entities)
            .map(|k| take_ids(k, resolve_ids(k, snap, graph, &vis)))
            .filter(|t| !t.is_empty())
            .collect();

        // Each distinct text is matched once: whether it named something, and what.
        let mut seen: HashMap<String, (bool, bool)> = HashMap::new();
        let mut match_text = |f: &mut Found, text: &str| -> (bool, bool) {
            if let Some(&flags) = seen.get(text) {
                return flags;
            }
            let hits = find_in(&[&snap.names, &graph.names], text, visible);
            let numeric = graph.numeric_key_hits(text, &vis);
            let graph_hit = !numeric.is_empty()
                || hits
                    .iter()
                    .any(|h| h.targets.iter().any(|t| !matches!(t, Target::Page(_))));
            let any_hit = !hits.is_empty() || !numeric.is_empty();
            for hit in &hits {
                for target in &hit.targets {
                    f.target(target, hit.name, ablation.pin_names);
                }
            }
            for n in numeric {
                push_unique(&mut f.nodes, n);
            }
            seen.insert(text.to_owned(), (graph_hit, any_hit));
            (graph_hit, any_hit)
        };
        let (query_hit, _) = match_text(&mut f, &query_text);
        f.named.insert(query_text.clone(), query_hit);
        for text in &agent_texts {
            let (graph_hit, any_hit) = match_text(&mut f, text);
            // An agent keyword that names nothing whole may be part of a node's name.
            let mut partial = f.partial.contains_key(text);
            if !any_hit && !partial {
                let nodes = graph.partial_name_hits(text, &vis);
                for &n in &nodes {
                    push_unique(&mut f.nodes, n);
                }
                if !nodes.is_empty() {
                    partial = true;
                    f.partial.insert(text.clone(), nodes);
                }
            }
            f.named.insert(text.clone(), graph_hit || partial);
        }
        // Relation types: their names found above anywhere in the request, plus those whose
        // names or aliases a counted keyword matches word by word (stems, split identifiers),
        // which exact names on word boundaries miss.
        for (i, k) in counted.iter().enumerate() {
            for rt in snap.catalog.relation_types() {
                if k.matches(&rt.text) {
                    push_unique(&mut f.relation_types, rt.entity.clone());
                    f.relation_keywords.insert(i);
                }
            }
        }
        // Words taken from the query only need to know whether they named a graph entity;
        // their targets were already found in the query as a whole.
        for k in &counted {
            let text = normalize(&k.text);
            if f.named.contains_key(&text) {
                continue;
            }
            let hit = !graph.numeric_key_hits(&text, &vis).is_empty()
                || find_in(&[&snap.names, &graph.names], &text, visible)
                    .iter()
                    .any(|h| h.targets.iter().any(|t| !matches!(t, Target::Page(_))));
            f.named.insert(text, hit);
        }
        // A space named in the request narrows the named nodes to it, when some are in it; ids
        // the agent passed are always kept.
        let keep =
            |n: &NodeIndex| f.spaces.contains(&graph.graph[*n].space) || f.explicit.contains(n);
        if f.nodes
            .iter()
            .any(|n| f.spaces.contains(&graph.graph[*n].space))
        {
            let kept: Vec<NodeIndex> = f.nodes.iter().copied().filter(keep).collect();
            f.nodes = kept;
        }
        f
    });
    let pins = &found.pins;
    // A keyword that names a graph entity or a relation type matched something, even when no
    // page shows it. An id matched only if it was found (and is visible).
    let mut matched_any: Vec<bool> = counted
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let text = normalize(&k.text);
            // A keyword holding an id matched when the id was found (and is visible); a keyword
            // that is only an id and wasn't found matched nothing.
            if found.id_texts.contains(&text) {
                return true;
            }
            if let Some(EntityRef::Node { .. }) = EntityRef::parse(k.text.trim()) {
                return false;
            }
            found.relation_keywords.contains(&i) || found.named.get(&text).copied().unwrap_or(false)
        })
        .collect();

    timings_us.push(("name_match", micros(step)));

    // 3 and 4. Search and ranking, per channel.
    let audiences = scope.audience_keys();
    let mut query_vector: Option<Vec<f32>> = None;
    let mut run_channel = |slot: Slot,
                           mode: Mode,
                           timings: &mut Vec<(&'static str, u64)>|
     -> anyhow::Result<Vec<Candidate>> {
        let mut by_ord: HashMap<usize, (Option<f32>, Option<f32>)> = HashMap::new();
        if matches!(mode, Mode::Keyword | Mode::Both) {
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
            for (score, ord) in snap.index.search(queries, &audiences, slot, CANDIDATES)? {
                by_ord.entry(ord).or_default().0 = Some(score);
            }
        }
        if matches!(mode, Mode::Semantic | Mode::Both)
            && let Some((vectors, embedder)) = meaning
        {
            if query_vector.is_none() {
                let started = std::time::Instant::now();
                let v = tracing::info_span!("embed_query")
                    .in_scope(|| embedder.embed(&[req.query.trim().to_owned()]))?;
                query_vector = v.into_iter().next();
                timings.push(("embed_query", micros(started)));
            }
            let span = tracing::info_span!("meaning_search", slot = slot.as_str());
            let _enter = span.enter();
            let q = query_vector.as_deref().unwrap_or_default();
            let found = vectors.best(q, |ord| {
                let page = &snap.catalog.pages[ord];
                page.slot == slot && audiences.contains(&page.audience)
            });
            // In `both` mode every keyword candidate gets its similarity too.
            for (rank, (ord, sim)) in found.into_iter().enumerate() {
                match by_ord.get_mut(&ord) {
                    Some(e) => e.1 = Some(sim),
                    None if rank < CANDIDATES => {
                        by_ord.insert(ord, (None, Some(sim)));
                    }
                    None => {}
                }
            }
        }
        for &i in pins.keys() {
            if snap.catalog.pages[i].slot == slot {
                by_ord.entry(i).or_default();
            }
        }
        let mut cands: Vec<Candidate> = by_ord
            .into_iter()
            .map(|(ord, (bm25, similarity))| candidate(snap, &counted, ord, bm25, similarity, pins))
            .collect();
        cands.retain(Candidate::above_floor);
        // In `both` mode a page keywords match strongly keeps its keyword rank; meaning orders
        // the rest. Scores from different rankings are never compared.
        for c in &mut cands {
            c.score = match mode {
                Mode::Semantic => c.similarity.unwrap_or(0.0),
                Mode::Keyword | Mode::Auto => c.bm25.unwrap_or(0.0),
                Mode::Both if c.bm25.is_some() && c.keyword_strong() => c.bm25.unwrap_or(0.0),
                Mode::Both => c.similarity.unwrap_or(0.0),
            };
        }
        let tier = |c: &Candidate| match mode {
            Mode::Both => u8::from(c.bm25.is_some() && c.keyword_strong()),
            _ => 0,
        };
        cands.sort_by(|a, b| {
            b.pins
                .min(1)
                .cmp(&a.pins.min(1))
                .then(b.pins.cmp(&a.pins))
                .then(tier(b).cmp(&tier(a)))
                .then(b.score.total_cmp(&a.score))
                .then(a.ord.cmp(&b.ord))
        });
        Ok(cands)
    };
    // `auto` searches a slot that comes back weak again with both rankings, when meaning is
    // available.
    let mut run_slot = |slot: Slot,
                        timings: &mut Vec<(&'static str, u64)>|
     -> anyhow::Result<(Vec<Candidate>, Coverage)> {
        let first = if mode == Mode::Auto {
            Mode::Keyword
        } else {
            mode
        };
        let mut cands = run_channel(slot, first, timings)?;
        if mode == Mode::Auto && meaning.is_some() && !cands.first().is_some_and(Candidate::strong)
        {
            cands = run_channel(slot, Mode::Both, timings)?;
        }
        // A keyword counts as matched only through a page that is good enough to show.
        for c in &cands {
            for (i, m) in matched_any.iter_mut().enumerate() {
                *m |= c.matched(i);
            }
        }
        let coverage = match cands.first() {
            Some(top) if top.strong() => Coverage::Strong,
            _ => Coverage::Weak,
        };
        Ok((cands, coverage))
    };
    // A slot's time leaves out embedding the query, which has its own entry.
    let embedded = |t: &[(&'static str, u64)]| -> u64 {
        t.iter()
            .filter(|(k, _)| *k == "embed_query")
            .map(|(_, v)| v)
            .sum()
    };
    step = std::time::Instant::now();
    let before = timings_us.len();
    let (mut mem, mem_cov) = run_slot(Slot::Memories, &mut timings_us)?;
    let spent = micros(step).saturating_sub(embedded(&timings_us[before..]));
    timings_us.push(("search_memories", spent));
    step = std::time::Instant::now();
    let before = timings_us.len();
    let (mut kno, kno_cov) = run_slot(Slot::Knowledge, &mut timings_us)?;
    let spent = micros(step).saturating_sub(embedded(&timings_us[before..]));
    timings_us.push(("search_knowledge", spent));
    step = std::time::Instant::now();

    // 5. Hop: the knowledge pages the top memories are anchored to, when those memories match
    // well on their own, fill the knowledge slot's free places after its own hits ("these
    // score lower, so the hop can't drift far"). A page already among the hits stays as it is.
    mem.truncate(sizes.memories);
    kno.truncate(sizes.knowledge);
    if req.hop && kno.len() < sizes.knowledge {
        tracing::info_span!("hop").in_scope(|| {
            for c in mem.iter().take(cfg.hop.top_memories).filter(|c| c.strong()) {
                for anchor in &snap.catalog.pages[c.ord].about {
                    if kno.len() >= sizes.knowledge {
                        return;
                    }
                    if let Some((ord, page)) = snap.catalog.get(anchor)
                        && page.slot == Slot::Knowledge
                        && scope.can_see(&page.path)
                        && !kno.iter().any(|k| k.ord == ord)
                    {
                        let mut hop = candidate(snap, &counted, ord, None, None, pins);
                        hop.via = Some(Via::Memory);
                        kno.push(hop);
                    }
                }
            }
        });
        timings_us.push(("hop", micros(step)));
        step = std::time::Instant::now();
    }
    // Pages the hop brought in are shown too.
    for c in kno.iter().filter(|c| c.via.is_some()) {
        for (i, m) in matched_any.iter_mut().enumerate() {
            *m |= c.matched(i);
        }
    }

    // 6. Relations: edges around the named entities, then the entities the hits are anchored
    // to. A relationship question, or two named nodes, gets more room.
    let relation_slot = tracing::info_span!("relations").in_scope(|| {
        let mut lifted: Vec<NodeIndex> = Vec::new();
        for anchor in mem
            .iter()
            .chain(&kno)
            .flat_map(|c| &snap.catalog.pages[c.ord].about)
        {
            if let Some(n) = graph.node(anchor)
                && vis.node(graph, n)
                && !found.nodes.contains(&n)
            {
                push_unique(&mut lifted, n);
            }
        }
        let wide = intent == Some(Intent::Relation) || found.nodes.len() >= 2;
        let size = if wide && req.slots.relations.is_none() {
            (sizes.relations * 2).min(MAX_SLOT)
        } else {
            sizes.relations
        };
        let start = relations::Start {
            named: &found.nodes,
            lifted: &lifted,
            node_types: &found.node_types,
            relation_types: &found.relation_types,
        };
        let got = relations::find(graph, &vis, &start, size);
        let route = found.nodes.first().map(|&first| {
            let second = found
                .nodes
                .iter()
                .skip(1)
                .find(|&&n| graph.graph[n].space == graph.graph[first].space);
            GraphRoute {
                graph: graph.graph[first].id.clone(),
                path: second.map(|&n| PathRoute {
                    from: graph.graph[first].id.clone(),
                    to: graph.graph[n].id.clone(),
                }),
            }
        });
        RelationSlot {
            coverage: if got.strong {
                Coverage::Strong
            } else {
                Coverage::Weak
            },
            relation_types: found.relation_types.clone(),
            edges: got.edges,
            route,
        }
    });
    timings_us.push(("relations", micros(step)));
    step = std::time::Instant::now();

    // 7. Assemble.
    let span = tracing::info_span!("assemble");
    let _enter = span.enter();
    let make_slot = |cands: Vec<Candidate>, coverage: Coverage| PageSlot {
        coverage,
        hits: cands
            .into_iter()
            .map(|c| hit(snap, graph, &vis, scope, &c, &counted, reads))
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
    let mut entities: Vec<String> = found
        .nodes
        .iter()
        .map(|&n| graph.graph[n].id.clone())
        .collect();
    let types: BTreeSet<&String> = found.node_types.iter().collect();
    entities.extend(types.into_iter().cloned());
    let partial_names = found
        .partial
        .iter()
        .map(|(k, nodes)| {
            let ids = nodes.iter().map(|&n| graph.graph[n].id.clone()).collect();
            (k.clone(), ids)
        })
        .collect();
    Ok(SearchResponse {
        mode,
        entities,
        intent,
        memories,
        knowledge,
        relations: relation_slot,
        unmatched_keywords,
        partial_names,
        timings_us,
    })
}

/// What the name match found: pinned pages, and the graph entities a search starts from.
#[derive(Default)]
struct Found {
    /// Per pinned page, the distinct names of it that were found.
    pin_names: HashMap<usize, BTreeSet<String>>,
    /// Per pinned page, how many of its names were found.
    pins: HashMap<usize, usize>,
    nodes: Vec<NodeIndex>,
    spaces: Vec<usize>,
    node_types: Vec<String>,
    relation_types: Vec<String>,
    /// Keywords (normalized) that named nodes only as part of their names, with those nodes.
    partial: BTreeMap<String, Vec<NodeIndex>>,
    /// Nodes the agent passed by id; a space named in the query never drops them.
    explicit: Vec<NodeIndex>,
    /// Texts (normalized, as the agent wrote them) holding an id that resolved to a visible
    /// node or an existing node type.
    id_texts: HashSet<String>,
    /// Per matched text (normalized, ids taken out): whether it named a graph entity.
    named: HashMap<String, bool>,
    /// Counted keywords (by index) that matched a relation type.
    relation_keywords: HashSet<usize>,
}

impl Found {
    fn target(&mut self, target: &Target, name: &str, pin_names: bool) {
        match target {
            Target::Page(i) => {
                if pin_names {
                    let names = self.pin_names.entry(*i).or_default();
                    names.insert(name.to_owned());
                    self.pins.insert(*i, names.len());
                }
            }
            Target::NodeType(e) => push_unique(&mut self.node_types, e.clone()),
            Target::RelationType(e) => push_unique(&mut self.relation_types, e.clone()),
            Target::Node(n) => push_unique(&mut self.nodes, NodeIndex::new(*n as usize)),
            Target::Space(i) => push_unique(&mut self.spaces, *i),
        }
    }
}

/// An entity id found in a text.
enum IdHit {
    /// A node id: the node, when it exists and the caller may see it.
    Node(Option<NodeIndex>),
    /// An existing node type, `schema:type`.
    Type(String),
}

/// The entity ids in a text, and the text without them (normalized). A node id is taken out
/// whether or not it is found, so a hidden id and an unknown one behave the same; a
/// `schema:type` is an id only when that node type exists, so words such as "status:active"
/// stay text. Chinese text right after an id ("mainland:order/1042的客户") runs into the key:
/// unless the whole span names a visible node, the id ends before the first Chinese character
/// of its key, a rule that never depends on hidden nodes.
fn resolve_ids(
    raw: &str,
    snap: &SearchSnapshot,
    graph: &GraphSnapshot,
    vis: &Visible,
) -> (Vec<IdHit>, String) {
    let mut ids = Vec::new();
    let mut rest = String::new();
    let mut at = 0;
    for (start, mut end) in id_spans(raw) {
        let whole = graph
            .node(&raw[start..end])
            .is_some_and(|n| vis.node(graph, n));
        if !whole
            && let Some(slash) = raw[start..end].find('/')
            && let Some((i, _)) = raw[start + slash..end]
                .char_indices()
                .find(|&(_, c)| text::is_cjk(c))
        {
            end = start + slash + i;
        }
        let id = &raw[start..end];
        let hit = match EntityRef::parse(id) {
            Some(EntityRef::Node { .. }) => {
                Some(IdHit::Node(graph.node(id).filter(|&n| vis.node(graph, n))))
            }
            Some(EntityRef::Type { schema, ty })
                if graph.has_node_type(schema, ty) || snap.catalog.has_node_type(id) =>
            {
                Some(IdHit::Type(id.to_owned()))
            }
            _ => None,
        };
        if let Some(hit) = hit {
            ids.push(hit);
            rest.push_str(&raw[at..start]);
            rest.push(' ');
            at = end;
        }
    }
    rest.push_str(&raw[at..]);
    (ids, normalize(rest.trim()))
}

/// Where entity ids might be in a text: `name:name` or `name:name/key`, where a name is a
/// lowercase letter then lowercase letters, digits and `_`, not preceded by a letter, digit or
/// `_`. A key runs to whitespace or punctuation that closes a phrase (`,;!?)` and their
/// Chinese forms, quotes, brackets); a trailing `.` is left out. Chinese text needs no spaces
/// around an id: 谁下的（mainland:order/1042）？
fn id_spans(text: &str) -> Vec<(usize, usize)> {
    let is_name = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_';
    let ends_key = |c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | ';'
                    | '!'
                    | '?'
                    | ')'
                    | ']'
                    | '}'
                    | '"'
                    | '\''
                    | '，'
                    | '；'
                    | '！'
                    | '？'
                    | '）'
                    | '】'
                    | '」'
                    | '』'
                    | '。'
                    | '、'
                    | '“'
                    | '”'
            )
    };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let byte_at = |i: usize| chars.get(i).map_or(text.len(), |&(b, _)| b);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let starts = chars[i].1.is_ascii_lowercase()
            && (i == 0 || !(chars[i - 1].1.is_ascii_alphanumeric() || chars[i - 1].1 == '_'));
        if !starts {
            i += 1;
            continue;
        }
        // name ':' name
        let mut j = i;
        while j < chars.len() && is_name(chars[j].1) {
            j += 1;
        }
        let second = j + 1;
        if chars.get(j).map(|c| c.1) != Some(':')
            || !chars.get(second).is_some_and(|c| c.1.is_ascii_lowercase())
        {
            i = j.max(i + 1);
            continue;
        }
        let mut k = second;
        while k < chars.len() && is_name(chars[k].1) {
            k += 1;
        }
        let mut end = k;
        if chars.get(k).map(|c| c.1) == Some('/') {
            let mut m = k + 1;
            while m < chars.len() && !ends_key(chars[m].1) {
                m += 1;
            }
            while m > k + 1 && chars[m - 1].1 == '.' {
                m -= 1;
            }
            if m > k + 1 {
                end = m;
            }
        } else if chars.get(k).is_some_and(|c| c.1.is_ascii_alphanumeric()) {
            // An uppercase letter continues the word: not an id.
            i = k;
            continue;
        }
        out.push((byte_at(i), byte_at(end)));
        i = end;
    }
    out
}

fn push_unique<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

fn hit(
    snap: &SearchSnapshot,
    graph: &GraphSnapshot,
    vis: &Visible,
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
        matched_by: c.matched_by(),
        via: c.via,
        matched_keywords: matched.iter().map(|k| k.text.clone()).collect(),
        summary: page.summary.clone(),
        lines,
        about: visible_anchors(scope, &snap.catalog, graph, vis, &page.about),
        // As `/read` takes them.
        sources: page
            .sources
            .iter()
            .map(|s| format!("{}{s}", crate::api::read::SOURCE_PREFIX))
            .collect(),
    }
}

/// The anchors a caller may see: pages they can see; node types that exist (in a stored
/// schema or a schema page) unless the prefix is really a space's name; and nodes of spaces
/// they can see. Anything else, including nodes of unknown spaces, is dropped.
pub fn visible_anchors(
    scope: &Scope,
    catalog: &Catalog,
    graph: &GraphSnapshot,
    vis: &Visible,
    about: &[String],
) -> Vec<String> {
    about
        .iter()
        .filter(|a| match PagePath::parse(a) {
            Ok(p) => scope.can_see(&p),
            Err(_) => match EntityRef::parse(a) {
                Some(EntityRef::Type { schema, ty }) => {
                    graph.space_index(schema).is_none()
                        && (graph.has_node_type(schema, ty) || catalog.has_node_type(a))
                }
                Some(EntityRef::Node { .. }) => vis.node_anchor(graph, a),
                None => false,
            },
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
    fn ids_are_found_in_running_text() {
        let ids = |t: &str| -> Vec<String> {
            id_spans(t)
                .into_iter()
                .map(|(s, e)| t[s..e].to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids("who placed mainland:order/1042."),
            vec!["mainland:order/1042"]
        );
        assert_eq!(
            ids("谁下的（mainland:order/1042）？"),
            vec!["mainland:order/1042"]
        );
        assert_eq!(
            ids("see commerce:order, then a:b/c/d and x:y/v2.1!"),
            vec!["commerce:order", "a:b/c/d", "x:y/v2.1"]
        );
        for none in [
            "Mainland:order/1",
            "re:Refunds",
            "10:30",
            "http://x",
            "a: b",
            "Xa:b",
        ] {
            assert!(
                ids(none).iter().all(|i| *i != none),
                "{none}: {:?}",
                ids(none)
            );
        }
        assert!(
            ids("status:active").len() == 1,
            "shaped like a type; kept as text unless it is one"
        );
    }

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
        let graph = GraphSnapshot::empty();
        let kws = keywords_from_query(
            &snap,
            &graph,
            &normalize("What's the weather in Hangzhou tomorrow?"),
            |_| true,
        );
        assert_eq!(kws, vec!["weather", "hangzhou"]);
        // A word inside another word is still its own keyword.
        let kws = keywords_from_query(&snap, &graph, &normalize("rapid api setup"), |_| true);
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
