//! The two ways every text field is indexed (design, "Tokenizing Chinese"):
//!
//! - **words**: the tenant's known Chinese terms kept whole, jieba for the rest of the Chinese
//!   text, English words lowercased and stemmed, identifiers both whole and split;
//! - **bigrams**: overlapping two-character pieces of Chinese text, English words as they are.
//!
//! All tenants share one base jieba dictionary. A tenant's own terms (aliases, keywords, titles)
//! are found first by an Aho-Corasick automaton built from that tenant's pages and emitted
//! whole, so one tenant's terms never change another tenant's tokens.
//!
//! Text passed in must already be normalized (`text::normalize`). Tokens are fed to tantivy as
//! pre-tokenized text, and queries are built from the same functions, so both sides agree.

use std::collections::HashSet;
use std::sync::LazyLock;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use jieba_rs::Jieba;
use rust_stemmers::{Algorithm, Stemmer};

use super::text::{RunKind, all_cjk, runs, word_parts};

static JIEBA: LazyLock<Jieba> = LazyLock::new(Jieba::new);
static STEMMER: LazyLock<Stemmer> = LazyLock::new(|| Stemmer::create(Algorithm::English));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tok {
    pub text: String,
    pub pos: usize,
}

/// A piece of a Chinese run: a known tenant term, or the text between terms.
enum Seg<'a> {
    Term(&'a str),
    Gap(&'a str),
}

pub struct Analyzer {
    /// The tenant's Chinese terms, leftmost-longest. `None` when it has none.
    terms: Option<AhoCorasick>,
}

impl Analyzer {
    /// `terms` are normalized strings; only all-Chinese terms of two or more characters are
    /// used, since the rest are handled by word runs anyway.
    pub fn new(terms: impl IntoIterator<Item = String>) -> Self {
        let set: HashSet<String> = terms
            .into_iter()
            .filter(|t| all_cjk(t) && t.chars().count() >= 2)
            .collect();
        let mut terms: Vec<String> = set.into_iter().collect();
        terms.sort();
        let terms = (!terms.is_empty()).then(|| {
            AhoCorasickBuilder::new()
                .match_kind(MatchKind::LeftmostLongest)
                .build(&terms)
                .expect("term automaton builds")
        });
        Self { terms }
    }

    fn segments<'a>(&self, run: &'a str) -> Vec<Seg<'a>> {
        let Some(ac) = &self.terms else {
            return vec![Seg::Gap(run)];
        };
        let mut out = Vec::new();
        let mut at = 0;
        for m in ac.find_iter(run) {
            if m.start() > at {
                out.push(Seg::Gap(&run[at..m.start()]));
            }
            out.push(Seg::Term(&run[m.start()..m.end()]));
            at = m.end();
        }
        if at < run.len() {
            out.push(Seg::Gap(&run[at..]));
        }
        out
    }

    /// The words version of a field. Sub-words of a known term share its position.
    pub fn words(&self, text: &str) -> Vec<Tok> {
        let mut out = Vec::new();
        let mut pos = 0;
        for run in runs(text) {
            match run.kind {
                RunKind::Word => push_word_run(run.text, true, &mut pos, &mut out),
                RunKind::Cjk => {
                    for seg in self.segments(run.text) {
                        match seg {
                            Seg::Term(t) => {
                                out.push(tok(t, pos));
                                for sub in JIEBA.cut_for_search(t, true) {
                                    if sub.word != t && sub.word.chars().count() >= 2 {
                                        out.push(tok(sub.word, pos));
                                    }
                                }
                                pos += 1;
                            }
                            Seg::Gap(g) => {
                                for sub in JIEBA.cut_for_search(g, true) {
                                    out.push(tok(sub.word, pos));
                                    pos += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// The bigrams version of a field.
    pub fn bigrams(&self, text: &str) -> Vec<Tok> {
        let mut out = Vec::new();
        let mut pos = 0;
        for run in runs(text) {
            match run.kind {
                RunKind::Word => push_word_run(run.text, false, &mut pos, &mut out),
                RunKind::Cjk => {
                    let chars: Vec<(usize, char)> = run.text.char_indices().collect();
                    if chars.len() == 1 {
                        out.push(tok(run.text, pos));
                        pos += 1;
                        continue;
                    }
                    for w in chars.windows(2) {
                        let end = w[1].0 + w[1].1.len_utf8();
                        out.push(tok(&run.text[w[0].0..end], pos));
                        pos += 1;
                    }
                }
            }
        }
        out
    }

    /// Plain segmentation of a Chinese run into words (known terms whole, jieba for the rest),
    /// without the overlapping sub-words of search mode. Used to split queries.
    pub fn cut<'a>(&self, run: &'a str) -> Vec<&'a str> {
        let mut out = Vec::new();
        for seg in self.segments(run) {
            match seg {
                Seg::Term(t) => out.push(t),
                Seg::Gap(g) => out.extend(JIEBA.cut(g, true).into_iter().map(|t| t.word)),
            }
        }
        out
    }
}

fn tok(text: &str, pos: usize) -> Tok {
    Tok {
        text: text.to_owned(),
        pos,
    }
}

/// A word run: the whole identifier (when it has joiners) plus each part.
fn push_word_run(run: &str, stem_parts: bool, pos: &mut usize, out: &mut Vec<Tok>) {
    let parts: Vec<&str> = word_parts(run).collect();
    if parts.len() > 1 {
        out.push(tok(run, *pos));
    }
    for (i, p) in parts.iter().enumerate() {
        let text = if stem_parts { stem(p) } else { (*p).to_owned() };
        out.push(Tok {
            text,
            pos: *pos + i,
        });
    }
    *pos += parts.len().max(1);
}

/// English stemming for purely alphabetic ASCII words; anything else is kept as is.
pub fn stem(word: &str) -> String {
    if word.bytes().all(|b| b.is_ascii_alphabetic()) {
        STEMMER.stem(word).into_owned()
    } else {
        word.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::text::normalize;

    fn texts(toks: &[Tok]) -> Vec<&str> {
        toks.iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn english_words_are_stemmed_and_identifiers_kept() {
        let a = Analyzer::new([]);
        let toks = a.words(&normalize("Shipped orders, order_status CNC-07"));
        assert_eq!(
            texts(&toks),
            vec![
                "ship",
                "order",
                "order_status",
                "order",
                "status",
                "cnc-07",
                "cnc",
                "07"
            ]
        );
        // Positions never go backwards.
        assert!(toks.windows(2).all(|w| w[0].pos <= w[1].pos));
    }

    #[test]
    fn tenant_terms_stay_whole() {
        let plain = Analyzer::new([]);
        let tenant = Analyzer::new(["部分退款".to_string(), "x".to_string()]);
        let text = normalize("客户要求部分退款");
        assert!(!texts(&plain.words(&text)).contains(&"部分退款"));
        let words = tenant.words(&text);
        assert!(texts(&words).contains(&"部分退款"));
        assert!(texts(&words).contains(&"退款"));
        assert_eq!(
            tenant.cut("客户要求部分退款"),
            vec!["客户", "要求", "部分退款"]
        );
    }

    #[test]
    fn bigrams_overlap() {
        let a = Analyzer::new([]);
        assert_eq!(
            texts(&a.bigrams(&normalize("物流单号 SKU-1"))),
            vec!["物流", "流单", "单号", "sku-1", "sku", "1"]
        );
        assert_eq!(texts(&a.bigrams("券")), vec!["券"]);
    }

    fn words_of(a: &Analyzer, s: &str) -> Vec<String> {
        a.words(&normalize(s)).into_iter().map(|t| t.text).collect()
    }

    /// Domain terms the base jieba dictionary splits apart. Declared as a tenant's aliases or
    /// keywords they come out whole, and their pieces are still emitted for recall.
    #[test]
    fn tenant_terms_repair_what_jieba_splits() {
        let terms = ["七天无理由", "物流单号", "预紧力", "跨境站"];
        let plain = Analyzer::new([]);
        let tenant = Analyzer::new(terms.map(String::from));
        for term in terms {
            let text = format!("关于{term}的说明");
            assert!(
                !words_of(&plain, &text).contains(&term.to_string()),
                "{term}"
            );
            let words = words_of(&tenant, &text);
            assert!(words.contains(&term.to_string()), "{term}: {words:?}");
        }
        let words = words_of(&tenant, "七天无理由退货");
        assert!(words.contains(&"理由".to_string()) && words.contains(&"退货".to_string()));
    }

    /// Where jieba splits a term and no tenant term covers it, the bigrams still hold it.
    #[test]
    fn bigrams_hold_what_jieba_splits() {
        let a = Analyzer::new([]);
        assert_eq!(words_of(&a, "物流单号"), vec!["物流", "单", "号"]);
        assert!(texts(&a.bigrams("物流单号")).contains(&"单号"));
        assert_eq!(words_of(&a, "预紧力"), vec!["预紧", "力"]);
        assert!(texts(&a.bigrams("预紧力")).contains(&"紧力"));
    }

    /// One tenant's terms never change another tenant's tokens.
    #[test]
    fn tenant_terms_stay_in_their_tenant() {
        let a = Analyzer::new(["跨境站".to_string()]);
        let b = Analyzer::new(["部分退款".to_string()]);
        assert!(words_of(&a, "跨境站订单").contains(&"跨境站".to_string()));
        assert!(!words_of(&b, "跨境站订单").contains(&"跨境站".to_string()));
    }

    #[test]
    fn mixed_scripts_and_full_width() {
        let a = Analyzer::new([]);
        assert_eq!(words_of(&a, "ＳＳＯ登录循环"), vec!["sso", "登录", "循环"]);
        assert_eq!(words_of(&a, "6206轴承更换"), vec!["6206", "轴承", "更换"]);
        assert_eq!(words_of(&a, "满300减50"), vec!["满", "300", "减", "50"]);
        // Width and case never change tokens, so queries and pages agree.
        assert_eq!(words_of(&a, "ＣＮＣ－０７"), words_of(&a, "cnc-07"));
        assert_eq!(words_of(&a, "Okta"), words_of(&a, "OKTA"));
    }

    #[test]
    fn english_inflections_share_a_stem() {
        let a = Analyzer::new([]);
        assert_eq!(
            words_of(&a, "refunds refunded refunding"),
            vec!["refund", "refund", "refund"]
        );
        assert_eq!(words_of(&a, "shipped shipping"), vec!["ship", "ship"]);
        // A known limit of stemming: derived nouns keep their own stem.
        assert_eq!(words_of(&a, "shipment"), vec!["shipment"]);
    }

    #[test]
    fn identifiers_carry_their_parts() {
        let a = Analyzer::new([]);
        let doc = words_of(&a, "check order_status first");
        for w in ["order_status", "order", "status"] {
            assert!(doc.contains(&w.to_string()), "{w}: {doc:?}");
        }
        assert!(words_of(&a, "order status").iter().all(|w| doc.contains(w)));
    }
}
