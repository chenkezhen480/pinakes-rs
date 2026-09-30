//! The per-tenant tantivy index over wiki pages. Each text field is indexed twice, as words and
//! as bigrams (see `tokenize`). In M2 the index lives in memory and is rebuilt from the wiki when
//! the tenant opens; M6 moves it to `index/tantivy/`.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use tantivy::collector::TopDocs;
use tantivy::columnar::Column;
use tantivy::query::{
    BooleanQuery, BoostQuery, ConstScoreQuery, Occur, PhraseQuery, Query, TermQuery, TermSetQuery,
};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, STRING, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{PreTokenizedString, Token};
use tantivy::{DocAddress, Index, IndexReader, ReloadPolicy, Searcher, TantivyDocument, Term};

use super::catalog::{Catalog, Slot};
use super::tokenize::{Analyzer, Tok};

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
    ord: Field,
    audience: Field,
    slot: Field,
    title: TextField,
    aliases: TextField,
    meta: TextField,
    body: TextField,
}

/// The catalog index of each page, a fast field read straight from its column.
const ORD_FIELD: &str = "ord";
/// Bigram matches count half as much as whole-word matches.
const BIGRAM_WEIGHT: f32 = 0.5;
/// A keyword found as a whole phrase gets this much on top of its term matches.
const PHRASE_WEIGHT: f32 = 1.0;

pub struct PageIndex {
    fields: Fields,
    _reader: IndexReader,
    searcher: Searcher,
}

impl PageIndex {
    pub fn build(catalog: &Catalog, analyzer: &Analyzer) -> anyhow::Result<Self> {
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
            ord: sb.add_u64_field(ORD_FIELD, NumericOptions::default().set_fast()),
            audience: sb.add_text_field("audience", STRING),
            slot: sb.add_text_field("slot", STRING),
            title,
            aliases,
            meta,
            body,
        };
        let index = Index::create_in_ram(sb.build());
        let mut writer = index.writer_with_num_threads(1, 15_000_000)?;
        for (i, page) in catalog.pages.iter().enumerate() {
            let mut doc = TantivyDocument::default();
            doc.add_u64(fields.ord, i as u64);
            doc.add_text(fields.audience, &page.audience);
            doc.add_text(fields.slot, page.slot.as_str());
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
                (fields.title, page.title.as_str()),
                (fields.aliases, &aliases),
                (fields.meta, &meta),
                (fields.body, &body),
            ] {
                let norm = super::text::normalize(value);
                doc.add_pre_tokenized_text(
                    field.words,
                    pre_tokenized(&norm, analyzer.words(&norm)),
                );
                doc.add_pre_tokenized_text(
                    field.bigrams,
                    pre_tokenized(&norm, analyzer.bigrams(&norm)),
                );
            }
            writer.add_document(doc)?;
        }
        writer.commit()?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        Ok(Self {
            fields,
            _reader: reader,
            searcher,
        })
    }

    fn text_fields(&self) -> [TextField; 4] {
        let f = &self.fields;
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
    ) -> anyhow::Result<Vec<(f32, usize)>> {
        if keyword_queries.is_empty() || audiences.is_empty() {
            return Ok(Vec::new());
        }
        let f = &self.fields;
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
        top.into_iter()
            .map(|(score, addr)| Ok((score, self.ord(addr, &mut columns)?)))
            .collect()
    }

    fn ord(
        &self,
        addr: DocAddress,
        columns: &mut HashMap<u32, Column<u64>>,
    ) -> anyhow::Result<usize> {
        let column = match columns.entry(addr.segment_ord) {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(
                self.searcher
                    .segment_reader(addr.segment_ord)
                    .fast_fields()
                    .u64(ORD_FIELD)?,
            ),
        };
        let ord = column
            .first(addr.doc_id)
            .ok_or_else(|| anyhow::anyhow!("indexed page without an ordinal"))?;
        Ok(ord as usize)
    }
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
