//! The per-tenant tantivy index over wiki pages. Each text field is indexed twice, as words and
//! as bigrams (see `tokenize`).
//!
//! A tenant's index lives in `index/tantivy/`. Each document carries its page's path and hash,
//! so when the tenant opens only pages changed since the index was written are indexed again,
//! and a write deletes and adds only the pages it changed. Documents also keep the page's text
//! and the stamp of the file it came from, so opening a tenant reads one index instead of
//! every page file: only files whose stamp changed are read (see [`PageIndex::cached_pages`]).
//! The commit payload records the format and the tenant's term list: tokens depend on that
//! list, so an index written with another one is rebuilt whole. Snapshots of a past wiki
//! (replays) get an index in memory.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tantivy::collector::TopDocs;
use tantivy::columnar::{Column, StrColumn};
use tantivy::directory::MmapDirectory;
use tantivy::query::{
    BooleanQuery, BoostQuery, ConstScoreQuery, Occur, PhraseQuery, Query, TermQuery, TermSetQuery,
};
use tantivy::schema::{
    FAST, Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{PreTokenizedString, Token};
use tantivy::{
    DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, Term,
};

use super::catalog::{Catalog, PageEntry, Slot};
use super::tokenize::{Analyzer, Tok};
use crate::wiki::Stamp;

/// A text field in its two versions.
#[derive(Debug, Clone, Copy)]
pub struct TextField {
    pub words: Field,
    pub bigrams: Field,
    /// How much a match in this field counts (design: title and aliases most, then summary
    /// and keywords, then body).
    pub weight: f32,
}

#[derive(Debug, Clone, Copy)]
struct Fields {
    path: Field,
    /// The page's key (see `PageEntry::key`), read for every hit.
    key: Field,
    hash: Field,
    /// The page as written, and its file's stamp (time in nanoseconds since 1970, length).
    text: Field,
    modified: Field,
    len: Field,
    audience: Field,
    slot: Field,
    title: TextField,
    aliases: TextField,
    meta: TextField,
    body: TextField,
}

const PATH_FIELD: &str = "path";
const KEY_FIELD: &str = "key";
const HASH_FIELD: &str = "hash";
/// Bumped whenever the schema or the tokens change, so an index on disk from an older build is
/// made again.
const FORMAT: u32 = 3;
/// Bigram matches count half as much as whole-word matches.
const BIGRAM_WEIGHT: f32 = 0.5;
/// A keyword found as a whole phrase gets this much on top of its term matches.
const PHRASE_WEIGHT: f32 = 1.0;
/// Pages tokenized at once when many are indexed, so their tokens never all sit in memory.
const BATCH: usize = 2048;
/// Indexing memory per writer thread (tantivy's minimum is 15 MB).
const THREAD_BUDGET: usize = 40_000_000;

/// What a commit records besides the documents.
#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct Payload {
    format: u32,
    /// A hash of the term list the tokens were made with.
    terms: String,
}

impl Payload {
    fn of(analyzer: &Analyzer) -> Self {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        for t in analyzer.terms() {
            h.update(t.as_bytes());
            h.update([0u8]);
        }
        Self {
            format: FORMAT,
            terms: hex::encode(&h.finalize()[..8]),
        }
    }
}

/// The index, its reader and its fields, shared by the snapshots built on it.
struct Shared {
    index: Index,
    reader: IndexReader,
    fields: Fields,
}

/// One snapshot's view of the index: what it holds stays fixed while later writes go on.
pub struct PageIndex {
    shared: Arc<Shared>,
    searcher: Searcher,
}

fn schema() -> (Schema, Fields) {
    let mut sb = Schema::builder();
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default().set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let mut text_field = |name: &str, weight: f32| TextField {
        words: sb.add_text_field(&format!("{name}_w"), text.clone()),
        bigrams: sb.add_text_field(&format!("{name}_b"), text.clone()),
        weight,
    };
    let title = text_field("title", 3.0);
    let aliases = text_field("aliases", 3.0);
    let meta = text_field("meta", 1.5);
    let body = text_field("body", 1.0);
    let fields = Fields {
        path: sb.add_text_field(PATH_FIELD, STRING | FAST | STORED),
        key: sb.add_u64_field(KEY_FIELD, FAST),
        hash: sb.add_text_field(HASH_FIELD, STRING | FAST),
        text: sb.add_text_field("text", STORED),
        modified: sb.add_u64_field("modified", STORED),
        len: sb.add_u64_field("len", STORED),
        audience: sb.add_text_field("audience", STRING),
        slot: sb.add_text_field("slot", STRING),
        title,
        aliases,
        meta,
        body,
    };
    (sb.build(), fields)
}

impl PageIndex {
    /// An index in memory holding a catalog's pages (without their text).
    pub fn build(catalog: &Catalog, analyzer: &Analyzer) -> anyhow::Result<Self> {
        let (schema, fields) = schema();
        let shared = Arc::new(Shared::new(Index::create_in_ram(schema), fields)?);
        let pages: Vec<(&PageEntry, Option<&str>)> =
            catalog.pages.iter().map(|p| (&**p, None)).collect();
        shared.write(&[], &pages, analyzer)?;
        shared.view()
    }

    /// The pages the index in `dir` keeps, path → (stamp of the file, text), for opening the
    /// tenant without reading the files that haven't changed since. Empty when there is no
    /// index there, or one of another format.
    pub fn cached_pages(dir: &Path) -> anyhow::Result<HashMap<String, (Stamp, String)>> {
        use tantivy::schema::Value;
        let (schema, f) = schema();
        let Some(index) = Index::open_in_dir(dir)
            .ok()
            .filter(|index| index.schema() == schema && stored_format(index) == Some(FORMAT))
        else {
            return Ok(HashMap::new());
        };
        let reader = index.reader()?;
        let searcher = reader.searcher();
        let mut out = HashMap::new();
        for segment in searcher.segment_readers() {
            let store = segment.get_store_reader(64)?;
            for doc in store.iter::<TantivyDocument>(segment.alive_bitset()) {
                let doc = doc?;
                let text = |field| {
                    doc.get_first(field)
                        .and_then(|v| v.as_str().map(str::to_owned))
                };
                let num = |field| doc.get_first(field).and_then(|v| v.as_u64());
                let (Some(path), Some(text), Some(modified), Some(len)) =
                    (text(f.path), text(f.text), num(f.modified), num(f.len))
                else {
                    continue;
                };
                let stamp = Stamp {
                    modified: std::time::UNIX_EPOCH + std::time::Duration::from_nanos(modified),
                    len,
                };
                out.insert(path, (stamp, text));
            }
        }
        Ok(out)
    }

    /// The index in `dir`, brought in line with a catalog's pages (each with its text as
    /// written): pages missing from it or with another hash are indexed, pages it holds that
    /// the catalog lacks are deleted. An index of another format, or tokenized with another
    /// term list, is made again whole.
    pub fn open(
        dir: &Path,
        pages: &[(Arc<PageEntry>, String)],
        analyzer: &Analyzer,
    ) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating the index folder {}", dir.display()))?;
        let (schema, fields) = schema();
        let payload = Payload::of(analyzer);
        let existing = Index::open_in_dir(dir)
            .ok()
            .filter(|index| index.schema() == schema && stored_format(index) == Some(FORMAT));
        let index = match existing {
            Some(index) => index,
            None => {
                // Another format or a damaged index: start over.
                std::fs::remove_dir_all(dir).ok();
                std::fs::create_dir_all(dir)?;
                Index::create(
                    MmapDirectory::open(dir)?,
                    schema,
                    tantivy::IndexSettings::default(),
                )?
            }
        };
        let shared = Arc::new(Shared::new(index, fields)?);
        let stored = shared.stored_pages()?;
        let same_terms = shared
            .index
            .load_metas()?
            .payload
            .and_then(|p| serde_json::from_str::<Payload>(&p).ok())
            .is_some_and(|p| p == payload);
        let all = || pages.iter().map(|(p, t)| (&**p, Some(t.as_str())));
        let (remove, add): (Vec<String>, Vec<(&PageEntry, Option<&str>)>) = if same_terms {
            let wanted: HashMap<&str, &str> = pages
                .iter()
                .map(|(p, _)| (p.path.as_str(), p.hash.as_str()))
                .collect();
            let remove = stored
                .iter()
                .filter(|(path, hash)| wanted.get(path.as_str()) != Some(&hash.as_str()))
                .map(|(path, _)| path.clone())
                .collect();
            let add = all()
                .filter(|(p, _)| stored.get(p.path.as_str()) != Some(&p.hash))
                .collect();
            (remove, add)
        } else {
            (stored.into_keys().collect(), all().collect())
        };
        if !remove.is_empty() || !add.is_empty() || !same_terms {
            tracing::info!(
                removed = remove.len(),
                indexed = add.len(),
                "index brought up to date"
            );
            shared.write(&remove, &add, analyzer)?;
        }
        shared.view()
    }

    /// Removes the pages under `prefix` (a user's or group's folder) from the index in `dir`
    /// for good: their documents are deleted, then every segment holding a deleted document is
    /// merged, so neither these pages' text nor older copies of any page stay in the files,
    /// and the old segment files are removed. An index of another format is removed whole
    /// (it is made again when the tenant opens). Callers make sure no snapshot holds the index.
    pub fn purge(dir: &Path, prefix: &str) -> anyhow::Result<()> {
        let (schema, fields) = schema();
        let index = match Index::open_in_dir(dir) {
            Ok(index) if index.schema() == schema && stored_format(&index) == Some(FORMAT) => index,
            _ => {
                return match std::fs::remove_dir_all(dir) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
                    _ => Ok(()),
                };
            }
        };
        let payload = index.load_metas()?.payload;
        let shared = Shared::new(index, fields)?;
        let gone: Vec<String> = shared
            .stored_pages()?
            .into_keys()
            .filter(|p| p.starts_with(prefix))
            .collect();
        // The reader keeps the old segments open, which would keep their files.
        let Shared { index, reader, .. } = shared;
        drop(reader);
        let mut writer: IndexWriter = index.writer_with_num_threads(1, THREAD_BUDGET)?;
        for path in &gone {
            writer.delete_term(Term::from_field_text(fields.path, path));
        }
        let mut commit = writer.prepare_commit()?;
        if let Some(p) = &payload {
            commit.set_payload(p);
        }
        commit.commit()?;
        let with_deletes: Vec<_> = index
            .searchable_segment_metas()?
            .iter()
            .filter(|m| m.has_deletes())
            .map(|m| m.id())
            .collect();
        if !with_deletes.is_empty() {
            writer.merge(&with_deletes).wait()?;
        }
        writer.garbage_collect_files().wait()?;
        writer.wait_merging_threads()?;
        tracing::info!(
            pages = gone.len(),
            segments = with_deletes.len(),
            "pages purged from the index"
        );
        Ok(())
    }

    /// The index after a write: `remove` paths deleted, `add` pages (with their text as
    /// written) indexed, a page already there replaced. This snapshot's view stays as it was.
    pub fn update(
        &self,
        remove: &[String],
        add: &[(Arc<PageEntry>, String)],
        analyzer: &Analyzer,
    ) -> anyhow::Result<Self> {
        let add: Vec<(&PageEntry, Option<&str>)> =
            add.iter().map(|(p, t)| (&**p, Some(t.as_str()))).collect();
        self.shared.write(remove, &add, analyzer)?;
        self.shared.clone().view()
    }

    fn text_fields(&self) -> [TextField; 4] {
        let f = &self.shared.fields;
        [f.title, f.aliases, f.meta, f.body]
    }

    /// The query for one keyword: each of its tokens in every field, weighted by field, plus a
    /// phrase match on the bigram fields when it has several tokens. `words` and `bigrams` are
    /// the keyword's tokens from the tenant's analyzer.
    pub fn keyword_query(&self, words: &[Tok], bigrams: &[Tok], weight: f32) -> Box<dyn Query> {
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        let unique_w = unique(words);
        let unique_b = unique(bigrams);
        for field in self.text_fields() {
            for t in &unique_w {
                clauses.push((Occur::Should, term(field.words, t, field.weight * weight)));
            }
            for t in &unique_b {
                clauses.push((
                    Occur::Should,
                    term(field.bigrams, t, field.weight * BIGRAM_WEIGHT * weight),
                ));
            }
            // One token per position makes a phrase; sub-words sharing a position are skipped.
            let mut phrase: Vec<(usize, Term)> = Vec::new();
            for t in bigrams {
                if phrase.last().is_none_or(|(p, _)| *p < t.pos) {
                    phrase.push((t.pos, Term::from_field_text(field.bigrams, &t.text)));
                }
            }
            if phrase.len() >= 2 {
                let first = phrase[0].0;
                let phrase = phrase.into_iter().map(|(p, t)| (p - first, t)).collect();
                clauses.push((
                    Occur::Should,
                    Box::new(BoostQuery::new(
                        Box::new(PhraseQuery::new_with_offset(phrase)),
                        field.weight * PHRASE_WEIGHT * weight,
                    )),
                ));
            }
        }
        Box::new(BooleanQuery::new(clauses))
    }

    /// Runs keyword queries inside the caller's audiences and one slot, and returns up to
    /// `limit` pages as (score, catalog index), best first.
    pub fn search(
        &self,
        keyword_queries: Vec<Box<dyn Query>>,
        audiences: &[String],
        slot: Slot,
        limit: usize,
        catalog: &Catalog,
    ) -> anyhow::Result<Vec<(f32, usize)>> {
        if keyword_queries.is_empty() || audiences.is_empty() {
            return Ok(Vec::new());
        }
        let f = &self.shared.fields;
        let audience_filter = TermSetQuery::new(
            audiences
                .iter()
                .map(|a| Term::from_field_text(f.audience, a)),
        );
        let slot_filter = TermQuery::new(
            Term::from_field_text(f.slot, slot.as_str()),
            IndexRecordOption::Basic,
        );
        let any_keyword = BooleanQuery::new(
            keyword_queries
                .into_iter()
                .map(|q| (Occur::Should, q))
                .collect(),
        );
        let query = BooleanQuery::new(vec![
            (
                Occur::Must,
                Box::new(ConstScoreQuery::new(Box::new(audience_filter), 0.0)),
            ),
            (
                Occur::Must,
                Box::new(ConstScoreQuery::new(Box::new(slot_filter), 0.0)),
            ),
            (Occur::Must, Box::new(any_keyword)),
        ]);
        let top = self
            .searcher
            .search(&query, &TopDocs::with_limit(limit).order_by_score())?;
        let mut columns: HashMap<u32, Column<u64>> = HashMap::new();
        let mut out = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let key = self.key(addr, &mut columns)?;
            // The index and the catalog of one snapshot hold the same pages.
            let ord = catalog
                .by_key(key)
                .with_context(|| format!("indexed page {key:x} is not in the catalog"))?;
            out.push((score, ord));
        }
        Ok(out)
    }

    fn key(
        &self,
        addr: DocAddress,
        columns: &mut HashMap<u32, Column<u64>>,
    ) -> anyhow::Result<u64> {
        let column = match columns.entry(addr.segment_ord) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(
                self.searcher
                    .segment_reader(addr.segment_ord)
                    .fast_fields()
                    .u64(KEY_FIELD)?,
            ),
        };
        column
            .first(addr.doc_id)
            .context("indexed page without a key")
    }
}

impl Shared {
    fn new(index: Index, fields: Fields) -> anyhow::Result<Self> {
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(Self {
            index,
            reader,
            fields,
        })
    }

    /// A view of what is committed now.
    fn view(self: Arc<Self>) -> anyhow::Result<PageIndex> {
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        Ok(PageIndex {
            shared: self,
            searcher,
        })
    }

    /// Every page the committed index holds, path → hash.
    fn stored_pages(&self) -> anyhow::Result<HashMap<String, String>> {
        self.reader.reload()?;
        let searcher = self.reader.searcher();
        let mut out = HashMap::new();
        for segment in searcher.segment_readers() {
            let fast = segment.fast_fields();
            let (Some(paths), Some(hashes)) = (fast.str(PATH_FIELD)?, fast.str(HASH_FIELD)?) else {
                continue;
            };
            // Each column's terms read in one pass, in order: looking every document's term up
            // on its own took tens of seconds for 100,000 pages.
            let (paths_by_ord, hashes_by_ord) = (terms(&paths)?, terms(&hashes)?);
            for doc in segment.doc_ids_alive() {
                let (Some(p), Some(h)) =
                    (paths.term_ords(doc).next(), hashes.term_ords(doc).next())
                else {
                    continue;
                };
                if let (Some(path), Some(hash)) =
                    (paths_by_ord.get(p as usize), hashes_by_ord.get(h as usize))
                {
                    out.insert(path.clone(), hash.clone());
                }
            }
        }
        Ok(out)
    }

    /// Deletes `remove` and the pages of `add`, indexes `add`, and commits. Many pages are
    /// tokenized on all cores, a batch at a time.
    fn write(
        &self,
        remove: &[String],
        add: &[(&PageEntry, Option<&str>)],
        analyzer: &Analyzer,
    ) -> anyhow::Result<()> {
        let threads = if add.len() > BATCH {
            std::thread::available_parallelism().map_or(1, |n| n.get().min(4))
        } else {
            1
        };
        let mut writer: IndexWriter = self
            .index
            .writer_with_num_threads(threads, threads * THREAD_BUDGET)?;
        let f = self.fields;
        for path in remove
            .iter()
            .map(String::as_str)
            .chain(add.iter().map(|(p, _)| p.path.as_str()))
        {
            writer.delete_term(Term::from_field_text(f.path, path));
        }
        for batch in add.chunks(BATCH) {
            for doc in crate::par::map(batch.to_vec(), |(page, text)| {
                document(&f, page, text, analyzer)
            }) {
                writer.add_document(doc)?;
            }
        }
        let mut commit = writer.prepare_commit()?;
        let payload = serde_json::to_string(&Payload::of(analyzer))?;
        commit.set_payload(&payload);
        commit.commit()?;
        // Let merges finish, so the segments a write leaves behind don't pile up.
        writer.wait_merging_threads()?;
        Ok(())
    }
}

/// A page as an index document; with its text, the text and its file's stamp are kept.
fn document(
    f: &Fields,
    page: &PageEntry,
    text: Option<&str>,
    analyzer: &Analyzer,
) -> TantivyDocument {
    let mut doc = TantivyDocument::default();
    doc.add_text(f.path, page.path.as_str());
    doc.add_u64(f.key, page.key);
    doc.add_text(f.hash, &page.hash);
    if let (Some(text), Some(stamp)) = (text, page.stamp) {
        let nanos = stamp
            .modified
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));
        doc.add_text(f.text, text);
        doc.add_u64(f.modified, nanos);
        doc.add_u64(f.len, stamp.len);
    }
    doc.add_text(f.audience, &page.audience);
    doc.add_text(f.slot, page.slot.as_str());
    let aliases = page.aliases.join("\n");
    let meta = [
        page.summary.as_str(),
        &page.keywords.join("\n"),
        &page.schema_text,
    ]
    .join("\n");
    let body: String = page
        .sections
        .iter()
        .flat_map(|s| s.heading.iter().chain(s.lines.iter().map(|l| &l.text)))
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    for (field, value) in [
        (f.title, page.title.as_str()),
        (f.aliases, &aliases),
        (f.meta, &meta),
        (f.body, &body),
    ] {
        let norm = super::text::normalize(value);
        doc.add_pre_tokenized_text(field.words, pre_tokenized(&norm, analyzer.words(&norm)));
        doc.add_pre_tokenized_text(field.bigrams, pre_tokenized(&norm, analyzer.bigrams(&norm)));
    }
    doc
}

/// Every term of a column, by its ordinal.
fn terms(column: &StrColumn) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut stream = column.dictionary().stream()?;
    while stream.advance() {
        out.push(String::from_utf8_lossy(stream.key()).into_owned());
    }
    Ok(out)
}

/// The format an index was committed with, if it says.
fn stored_format(index: &Index) -> Option<u32> {
    let payload = index.load_metas().ok()?.payload?;
    serde_json::from_str::<Payload>(&payload)
        .ok()
        .map(|p| p.format)
}

fn unique(toks: &[Tok]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for t in toks {
        if !out.contains(&t.text.as_str()) {
            out.push(&t.text);
        }
    }
    out
}

fn term(field: Field, text: &str, boost: f32) -> Box<dyn Query> {
    Box::new(BoostQuery::new(
        Box::new(TermQuery::new(
            Term::from_field_text(field, text),
            IndexRecordOption::WithFreqs,
        )),
        boost,
    ))
}

fn pre_tokenized(text: &str, toks: Vec<Tok>) -> PreTokenizedString {
    PreTokenizedString {
        text: text.to_owned(),
        tokens: toks
            .into_iter()
            .map(|t| Token {
                offset_from: 0,
                offset_to: 0,
                position: t.pos,
                text: t.text,
                position_length: 1,
            })
            .collect(),
    }
}
