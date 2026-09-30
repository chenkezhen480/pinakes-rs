//! The name matcher: every title and alias of the tenant's pages, and every node type name of
//! its schemas, in one Aho-Corasick automaton. It finds all known names in a query in one pass.
//! Graph instance names join it in M3.

use std::collections::HashMap;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use super::catalog::Catalog;
use super::text::{has_cjk, normalize, on_word_boundary};

/// What a name points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A page, by its index in the catalog.
    Page(usize),
    /// A node type, as `schema:type`.
    NodeType(String),
}

/// One name found in a text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameHit<'a> {
    pub name: &'a str,
    /// The targets the caller may see; never empty.
    pub targets: Vec<&'a Target>,
}

#[derive(Default)]
pub struct NameMatcher {
    ac: Option<AhoCorasick>,
    names: Vec<String>,
    targets: Vec<Vec<Target>>,
}

/// Names too short to route on: under two Chinese characters or three other characters. A
/// single character such as 券 would pin its page on almost any question.
fn long_enough(name: &str) -> bool {
    let n = name.chars().count();
    if has_cjk(name) { n >= 2 } else { n >= 3 }
}

impl NameMatcher {
    pub fn build(catalog: &Catalog) -> Self {
        let mut by_name: HashMap<String, Vec<Target>> = HashMap::new();
        let mut add = |name: &str, target: Target| {
            let norm = normalize(name.trim());
            if !long_enough(&norm) {
                return;
            }
            let list = by_name.entry(norm).or_default();
            if !list.contains(&target) {
                list.push(target);
            }
        };
        for (i, page) in catalog.pages.iter().enumerate() {
            add(&page.title, Target::Page(i));
            for alias in &page.aliases {
                add(alias, Target::Page(i));
            }
            for nt in &page.node_types {
                for name in &nt.names {
                    add(name, Target::NodeType(nt.entity.clone()));
                }
            }
        }
        let mut entries: Vec<(String, Vec<Target>)> = by_name.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let (names, targets): (Vec<String>, Vec<Vec<Target>>) = entries.into_iter().unzip();
        let ac = (!names.is_empty()).then(|| {
            AhoCorasickBuilder::new()
                .match_kind(MatchKind::Standard)
                .build(&names)
                .expect("name automaton builds")
        });
        Self { ac, names, targets }
    }

    /// Every known name in a normalized text, on word boundaries, that points at something the
    /// caller may see (`visible`). Names of hidden pages are dropped first, so they can neither
    /// be reported nor shadow a visible name. A name inside a longer name that also matched is
    /// dropped, so "partial refund" doesn't also report "refund".
    pub fn find(&self, text: &str, visible: impl Fn(&Target) -> bool) -> Vec<NameHit<'_>> {
        let Some(ac) = &self.ac else {
            return Vec::new();
        };
        let found: Vec<(usize, usize, NameHit<'_>)> = ac
            .find_overlapping_iter(text)
            .filter(|m| on_word_boundary(text, m.start(), m.end()))
            .filter_map(|m| {
                let p = m.pattern().as_usize();
                let targets: Vec<&Target> = self.targets[p].iter().filter(|t| visible(t)).collect();
                (!targets.is_empty()).then(|| {
                    let hit = NameHit {
                        name: &self.names[p],
                        targets,
                    };
                    (m.start(), m.end(), hit)
                })
            })
            .collect();
        let mut out: Vec<NameHit<'_>> = Vec::new();
        for (s, e, hit) in &found {
            let inside_longer = found
                .iter()
                .any(|(s2, e2, _)| s2 <= s && e <= e2 && (e2 - s2) > (e - s));
            if inside_longer || out.iter().any(|h| h.name == hit.name) {
                continue;
            }
            out.push(hit.clone());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::tokenize::Analyzer;
    use crate::wiki::{Page, PagePath};

    fn catalog() -> Catalog {
        let exp = "---\nkind: experience\ntitle: Handling partial refunds\naliases: [partial refund, 部分退款, 券]\n---\n\nbody\n";
        let sso = "---\nkind: expertise\ntitle: SSO setup\naliases: [SSO, refund]\n---\n\nbody\n";
        let graph = "---\nkind: graph\ntitle: Commerce\nschema: commerce\nnodes:\n  order: { aliases: [订单] }\n---\n";
        let pages = vec![
            Page::parse(PagePath::parse("experience/refunds").unwrap(), exp).unwrap(),
            Page::parse(PagePath::parse("expertise/sso").unwrap(), sso).unwrap(),
            Page::parse(PagePath::parse("graph/commerce").unwrap(), graph).unwrap(),
        ];
        Catalog::build(pages, &Analyzer::new([]))
    }

    fn names(m: &NameMatcher, text: &str) -> Vec<String> {
        m.find(&normalize(text), |_| true)
            .into_iter()
            .map(|h| h.name.to_owned())
            .collect()
    }

    #[test]
    fn finds_names_in_both_languages() {
        let m = NameMatcher::build(&catalog());
        assert_eq!(
            names(&m, "A Partial Refund after shipping"),
            vec!["partial refund"]
        );
        assert_eq!(names(&m, "订单1042要部分退款"), vec!["订单", "部分退款"]);
        let hits = m.find("订单", |_| true);
        assert_eq!(
            hits[0].targets,
            vec![&Target::NodeType("commerce:order".into())]
        );
    }

    /// A hidden page's longer name neither shows up nor hides a visible page's shorter name.
    #[test]
    fn hidden_names_neither_match_nor_shadow() {
        let m = NameMatcher::build(&catalog());
        let hidden = |t: &Target| *t != Target::Page(0);
        let found: Vec<String> = m
            .find(&normalize("partial refund"), hidden)
            .into_iter()
            .map(|h| h.name.to_owned())
            .collect();
        assert_eq!(found, vec!["refund"]);
    }

    #[test]
    fn respects_word_boundaries_and_length() {
        let m = NameMatcher::build(&catalog());
        assert!(names(&m, "our ssot doc").is_empty());
        assert_eq!(names(&m, "sso login"), vec!["sso"]);
        assert!(
            names(&m, "优惠券").is_empty(),
            "single-character names are skipped"
        );
    }
}
