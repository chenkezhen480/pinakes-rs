//! The in-memory copy of a tenant's pages that searches read: front matter, the body split into
//! sections and lines, and normalized text for matching keywords. Built from the wiki files.
//!
//! Entries are shared between snapshots: after a write, the next snapshot keeps every unchanged
//! page's entry and builds only the changed ones.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_norway::Value;

use super::text::normalize;
use super::tokenize::Analyzer;
use crate::kinds::Kind;
use crate::wiki::{Audience, Page, PagePath, Stamp, object_name};

/// The slot a page comes back in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slot {
    Memories,
    Knowledge,
}

impl Slot {
    pub fn of(kind: Kind) -> Slot {
        match kind {
            Kind::Experience | Kind::Decision | Kind::Preference => Slot::Memories,
            Kind::Expertise | Kind::Graph => Slot::Knowledge,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Slot::Memories => "memories",
            Slot::Knowledge => "knowledge",
        }
    }
}

/// The audience value indexed for a page: `tenant`, or the group or user folder name.
pub fn audience_key(path: &PagePath) -> String {
    match path.audience() {
        Audience::Tenant => "tenant".to_owned(),
        Audience::Group(dir) | Audience::User(dir) => dir.to_owned(),
    }
}

#[derive(Debug, Clone)]
pub struct Line {
    pub text: String,
    pub norm: String,
    /// Word tokens, for finding matching lines without segmenting at search time.
    pub words: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct Section {
    /// `None` for text before the first heading.
    pub heading: Option<String>,
    pub heading_words: HashSet<String>,
    pub level: usize,
    pub lines: Vec<Line>,
}

/// Normalized text of a group of fields, with its word tokens, for checking which keywords a
/// page matched.
#[derive(Debug, Clone, Default)]
pub struct FieldText {
    pub norm: String,
    pub words: HashSet<String>,
}

impl FieldText {
    fn new(analyzer: &Analyzer, parts: &[&str]) -> Self {
        let norm = normalize(&parts.join("\n"));
        let words = analyzer.words(&norm).into_iter().map(|t| t.text).collect();
        Self { norm, words }
    }
}

/// A node type described by a schema page: `schema:type` and the names it goes by.
#[derive(Debug, Clone)]
pub struct NodeType {
    pub entity: String,
    pub names: Vec<String>,
}

/// A relation type described by a schema page, one entry of the relation-type index.
#[derive(Debug, Clone)]
pub struct RelationTypeEntry {
    /// `schema:relation`.
    pub entity: String,
    pub schema: String,
    pub name: String,
    /// The relation's name (with `_` as spaces) and aliases, for the name matcher.
    pub names: Vec<String>,
    /// Names and aliases as matchable text, for keywords.
    pub text: FieldText,
}

#[derive(Debug, Clone)]
pub struct PageEntry {
    pub path: PagePath,
    pub kind: Kind,
    pub slot: Slot,
    pub audience: String,
    pub title: String,
    pub summary: String,
    pub aliases: Vec<String>,
    pub keywords: Vec<String>,
    pub about: Vec<String>,
    pub sources: Vec<String>,
    pub sections: Vec<Section>,
    /// `[[link]]` targets in the body, as written.
    pub links: Vec<String>,
    /// For schema pages: the node types it defines.
    pub node_types: Vec<NodeType>,
    /// For schema pages: the relation types it defines.
    pub relation_types: Vec<RelationTypeEntry>,
    /// Descriptions of the schema's node and relation types (schema pages only).
    pub schema_text: String,
    /// Title, aliases, keywords and summary.
    pub head: FieldText,
    pub body: FieldText,
    /// The page's object name in the wiki history (a hash of the page as written), which tells
    /// a later build whether it changed.
    pub hash: String,
    /// 64 bits of a SHA-256 over the path: how the search index names the page.
    pub key: u64,
    /// The stamp of the file it was read from; none for pages from the history.
    pub stamp: Option<Stamp>,
    /// The terms it gives the tenant's tokenizer (see [`page_terms`]).
    pub terms: Vec<String>,
    /// Its title and aliases as the name matcher keys them (normalized, too short ones left
    /// out), made once with the entry rather than at every build of the matcher.
    pub names: Vec<String>,
}

/// All pages of one tenant, in path order.
#[derive(Debug, Default)]
pub struct Catalog {
    pub pages: Vec<Arc<PageEntry>>,
    by_path: HashMap<String, usize>,
    by_key: HashMap<u64, usize>,
    /// Entity anchors (`schema:type`, `space:type/key`) to the pages anchored to them.
    anchored: HashMap<String, Vec<usize>>,
    /// Every `schema:type` the schema pages define.
    node_types: HashSet<String>,
    /// Every relation type of every schema page: the relation-type index.
    relations: Vec<RelationTypeEntry>,
}

/// Catalog entries for pages, each with the page's text as written, built on all cores, in the
/// order given. Pages without a kind (`index`, `log`) are skipped.
pub fn entries(
    pages: Vec<(Page, Option<Stamp>)>,
    analyzer: &Analyzer,
) -> Vec<(Arc<PageEntry>, String)> {
    crate::par::map(pages, |(page, stamp)| {
        let kind = page.path.kind()?;
        let text = page.render();
        Some((
            Arc::new(entry_for(page, kind, &text, stamp, analyzer)),
            text,
        ))
    })
    .into_iter()
    .flatten()
    .collect()
}

/// The terms a page gives the tenant's tokenizer: titles, aliases, keywords and node type names
/// of tenant-level pages. Group and user pages give none, so one user's private terms never
/// change how anyone else's text is split; bigrams still cover them.
pub fn page_terms(page: &Page) -> Vec<String> {
    if !matches!(page.path.audience(), Audience::Tenant) {
        return Vec::new();
    }
    let f = &page.front;
    let mut out: Vec<String> = f.title.iter().map(|t| normalize(t)).collect();
    out.extend(f.aliases.iter().map(|t| normalize(t)));
    out.extend(f.keywords.iter().map(|t| normalize(t)));
    for nt in node_types(&f.extra) {
        out.extend(nt.names.iter().map(|t| normalize(t)));
    }
    out
}

impl Catalog {
    /// Builds entries from pages. Pages without a kind (`index`, `log`) are skipped.
    pub fn build(pages: Vec<Page>, analyzer: &Analyzer) -> Self {
        let pages = pages.into_iter().map(|p| (p, None)).collect();
        Self::from_entries(
            entries(pages, analyzer)
                .into_iter()
                .map(|(e, _)| e)
                .collect(),
        )
    }

    /// A catalog of entries, put in path order.
    pub fn from_entries(mut entries: Vec<Arc<PageEntry>>) -> Self {
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let mut out = Catalog::default();
        for entry in entries {
            let ord = out.pages.len();
            out.by_path.insert(entry.path.as_str().to_owned(), ord);
            out.by_key.insert(entry.key, ord);
            for anchor in entry.about.iter().filter(|a| a.contains(':')) {
                // Ords only grow, so a page's repeated anchor is the list's last entry.
                let list = out.anchored.entry(anchor.clone()).or_default();
                if list.last() != Some(&ord) {
                    list.push(ord);
                }
            }
            out.node_types
                .extend(entry.node_types.iter().map(|nt| nt.entity.clone()));
            out.relations.extend(entry.relation_types.iter().cloned());
            out.pages.push(entry);
        }
        out
    }

    /// Whether a schema page defines this `schema:type`.
    pub fn has_node_type(&self, entity: &str) -> bool {
        self.node_types.contains(entity)
    }

    /// Every relation type of every schema page: the relation-type index.
    pub fn relation_types(&self) -> impl Iterator<Item = &RelationTypeEntry> {
        self.relations.iter()
    }

    /// The pages anchored to an entity, in path order. Callers filter them by audience.
    pub fn anchored_to(&self, entity: &str) -> &[usize] {
        self.anchored.get(entity).map_or(&[], Vec::as_slice)
    }

    /// A page's position by its key (see [`PageEntry::key`]).
    pub fn by_key(&self, key: u64) -> Option<usize> {
        self.by_key.get(&key).copied()
    }

    pub fn get(&self, path: &str) -> Option<(usize, &PageEntry)> {
        self.by_path.get(path).map(|&i| (i, &*self.pages[i]))
    }

    /// Every term the pages give the tenant's tokenizer (see [`page_terms`]).
    pub fn terms_of(pages: &[Page]) -> Vec<String> {
        pages.iter().flat_map(page_terms).collect()
    }

    /// The version of the pages: 16 hex digits of a SHA-256 over every page's path and hash. A
    /// search snapshot built from them has this version, and the wiki history stores them under
    /// it.
    pub fn version(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for page in &self.pages {
            h.update(page.path.as_str().as_bytes());
            h.update([0u8]);
            h.update(page.hash.as_bytes());
            h.update([0u8]);
        }
        hex::encode(&h.finalize()[..8])
    }
}

fn entry_for(
    page: Page,
    kind: Kind,
    text: &str,
    stamp: Option<Stamp>,
    analyzer: &Analyzer,
) -> PageEntry {
    let hash = object_name(text);
    let key = {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(page.path.as_str().as_bytes());
        u64::from_le_bytes(digest[..8].try_into().expect("8 bytes"))
    };
    let terms = page_terms(&page);
    let f = page.front;
    let title = f
        .title
        .clone()
        .unwrap_or_else(|| page.path.segments().last().unwrap_or("").to_owned());
    let summary = f.summary.clone().unwrap_or_default();
    let node_types = if kind == Kind::Graph {
        node_types(&f.extra)
    } else {
        Vec::new()
    };
    let schema_text = if kind == Kind::Graph {
        schema_text(&f.extra)
    } else {
        String::new()
    };
    let relation_types = if kind == Kind::Graph {
        relation_types(&f.extra, analyzer)
    } else {
        Vec::new()
    };
    let sections = sections(&page.body, analyzer);
    let links = links(&page.body);
    let mut names: Vec<String> = std::iter::once(&title)
        .chain(&f.aliases)
        .map(|n| normalize(n.trim()))
        .filter(|n| super::names::long_enough(n))
        .collect();
    names.dedup();

    let mut head_parts: Vec<&str> = vec![&title, &summary];
    head_parts.extend(f.aliases.iter().map(String::as_str));
    head_parts.extend(f.keywords.iter().map(String::as_str));
    head_parts.push(&schema_text);
    let head = FieldText::new(analyzer, &head_parts);
    let body = FieldText::new(analyzer, &[&page.body]);

    PageEntry {
        audience: audience_key(&page.path),
        slot: Slot::of(kind),
        path: page.path,
        kind,
        title,
        summary,
        aliases: f.aliases,
        keywords: f.keywords,
        about: f.about,
        sources: f.sources,
        sections,
        links,
        node_types,
        relation_types,
        schema_text,
        head,
        body,
        hash,
        key,
        stamp,
        terms,
        names,
    }
}

fn words_of(analyzer: &Analyzer, norm: &str) -> HashSet<String> {
    analyzer.words(norm).into_iter().map(|t| t.text).collect()
}

/// Splits a Markdown body at ATX headings; `#` lines inside fenced code blocks are text, not
/// headings. Blank lines are dropped.
pub fn sections(body: &str, analyzer: &Analyzer) -> Vec<Section> {
    let mut out = vec![Section {
        heading: None,
        heading_words: HashSet::new(),
        level: 0,
        lines: Vec::new(),
    }];
    let mut fence = Fence::default();
    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if !fence.step(line)
            && let Some((level, heading)) = heading_of(line)
        {
            out.push(Section {
                heading: Some(heading.to_owned()),
                heading_words: words_of(analyzer, &normalize(heading)),
                level,
                lines: Vec::new(),
            });
            continue;
        }
        let norm = normalize(line);
        out.last_mut().expect("never empty").lines.push(Line {
            text: line.to_owned(),
            words: words_of(analyzer, &norm),
            norm,
        });
    }
    out.retain(|s| s.heading.is_some() || !s.lines.is_empty());
    out
}

/// Tracks fenced code blocks (``` or ~~~) line by line.
#[derive(Debug, Default)]
pub struct Fence {
    open: Option<char>,
}

impl Fence {
    /// Feeds one trimmed line; returns whether it is inside a code block or is a fence line
    /// itself, in which case it can't be a heading.
    pub fn step(&mut self, line: &str) -> bool {
        let marker = ['`', '~']
            .into_iter()
            .find(|&c| line.starts_with(&c.to_string().repeat(3)));
        match (self.open, marker) {
            (None, Some(c)) => {
                self.open = Some(c);
                true
            }
            (Some(open), Some(c)) if open == c => {
                self.open = None;
                true
            }
            (open, _) => open.is_some(),
        }
    }
}

/// `## Pitfalls` → `(2, "Pitfalls")`.
pub fn heading_of(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|&b| b == b'#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &line[level..];
    if !rest.starts_with(' ') {
        return None;
    }
    Some((level, rest.trim().trim_end_matches('#').trim()))
}

/// `[[link]]` targets in a Markdown body (`[[page]]`, `[[page|text]]`, `[[page#heading]]`), in
/// order, without duplicates; links in code blocks don't count.
pub fn links(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut fence = Fence::default();
    for line in body.lines() {
        if fence.step(line.trim()) {
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find("[[") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("]]") else {
                break;
            };
            let target = after[..end].split(['|', '#']).next().unwrap_or("").trim();
            if !target.is_empty() && !out.iter().any(|t| t == target) {
                out.push(target.to_owned());
            }
            rest = &after[end + 2..];
        }
    }
    out
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn str_list(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_sequence)
        .map(|seq| {
            seq.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn node_types(extra: &std::collections::BTreeMap<String, Value>) -> Vec<NodeType> {
    let Some(schema) = extra.get("schema").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(nodes) = extra.get("nodes").and_then(Value::as_mapping) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, def) in nodes {
        let Some(name) = name.as_str() else {
            continue;
        };
        let mut names = vec![name.to_owned()];
        names.extend(str_list(def, "aliases"));
        out.push(NodeType {
            entity: format!("{schema}:{name}"),
            names,
        });
    }
    out
}

fn relation_types(
    extra: &std::collections::BTreeMap<String, Value>,
    analyzer: &Analyzer,
) -> Vec<RelationTypeEntry> {
    let Some(schema) = extra.get("schema").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(relations) = extra.get("relations").and_then(Value::as_mapping) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, def) in relations {
        let Some(name) = name.as_str() else {
            continue;
        };
        let mut names = vec![name.replace('_', " ")];
        if name.contains('_') {
            names.push(name.to_owned());
        }
        names.extend(str_list(def, "aliases"));
        let parts: Vec<&str> = names.iter().map(String::as_str).collect();
        out.push(RelationTypeEntry {
            entity: format!("{schema}:{name}"),
            schema: schema.to_owned(),
            name: name.to_owned(),
            text: FieldText::new(analyzer, &parts),
            names,
        });
    }
    out
}

/// The schema's type descriptions as searchable text: names, English and Chinese meanings and
/// aliases of every node and relation type.
fn schema_text(extra: &std::collections::BTreeMap<String, Value>) -> String {
    let mut parts = Vec::new();
    for section in ["nodes", "relations"] {
        let Some(map) = extra.get(section).and_then(Value::as_mapping) else {
            continue;
        };
        for (name, def) in map {
            if let Some(name) = name.as_str() {
                parts.push(name.replace('_', " "));
            }
            for key in ["en", "zh"] {
                if let Some(s) = str_field(def, key) {
                    parts.push(s.to_owned());
                }
            }
            parts.extend(str_list(def, "aliases"));
        }
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sections() {
        let s = sections(
            "# Title\n\nintro\n\n## Steps\n1. one\n2. two\n```bash\n# install deps\n```\n### Deep ###\nx\n",
            &Analyzer::new([]),
        );
        let heads: Vec<_> = s
            .iter()
            .map(|s| (s.heading.as_deref(), s.level, s.lines.len()))
            .collect();
        assert_eq!(
            heads,
            vec![
                (Some("Title"), 1, 1),
                (Some("Steps"), 2, 5),
                (Some("Deep"), 3, 1)
            ]
        );
        assert_eq!(heading_of("#hashtag"), None);
    }

    #[test]
    fn finds_links() {
        assert_eq!(
            links("see [[expertise/a]] and [[b|B]], [[expertise/a]] [[broken"),
            vec!["expertise/a", "b"]
        );
    }

    #[test]
    fn schema_pages_yield_node_types() {
        let text = "---\nkind: graph\ntitle: Commerce\nschema: commerce\nnodes:\n  order: { en: \"An order\", zh: \"订单\", aliases: [orders, 订单], key: order_id }\nrelations:\n  placed_by: { from: order, to: customer, en: \"Who placed it\", zh: \"下单的客户\", aliases: [下单人] }\n---\n\n# Commerce\n";
        let page = Page::parse(PagePath::parse("graph/commerce").unwrap(), text).unwrap();
        let cat = Catalog::build(vec![page], &Analyzer::new([]));
        let e = &cat.pages[0];
        assert_eq!(e.slot, Slot::Knowledge);
        assert_eq!(e.node_types[0].entity, "commerce:order");
        assert_eq!(e.node_types[0].names, vec!["order", "orders", "订单"]);
        assert!(e.schema_text.contains("placed by"));
        assert_eq!(e.relation_types[0].entity, "commerce:placed_by");
        assert_eq!(
            e.relation_types[0].names,
            vec!["placed by", "placed_by", "下单人"]
        );
        assert!(e.head.norm.contains("下单人"));
    }
}
