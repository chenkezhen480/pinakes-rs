//! The name matcher: known names in one Aho-Corasick automaton, found in a query in one pass.
//! The wiki's matcher holds every page title and alias and every node and relation type name of
//! its schemas; the graph's holds node names and aliases and graph space names. A search runs
//! both and merges their hits with [`find_in`].

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
    /// A relation type, as `schema:relation`.
    RelationType(String),
    /// A graph node, by its index in the graph snapshot.
    Node(u32),
    /// A graph space, by its index in the graph snapshot.
    Space(usize),
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
pub fn long_enough(name: &str) -> bool {
    let n = name.chars().count();
    if has_cjk(name) { n >= 2 } else { n >= 3 }
}

impl NameMatcher {
    /// A matcher over `(name, target)` pairs. Names are normalized; names too short to route
    /// on are left out.
    pub fn new(entries: impl IntoIterator<Item = (String, Target)>) -> Self {
        let mut by_name: HashMap<String, Vec<Target>> = HashMap::new();
        for (name, target) in entries {
            let norm = normalize(name.trim());
            if !long_enough(&norm) {
                continue;
            }
            let list = by_name.entry(norm).or_default();
            if !list.contains(&target) {
                list.push(target);
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

    /// The wiki's names: page titles and aliases, and the node and relation types of its
    /// schema pages.
    pub fn build(catalog: &Catalog) -> Self {
        let mut entries = Vec::new();
        for (i, page) in catalog.pages.iter().enumerate() {
            entries.push((page.title.clone(), Target::Page(i)));
            for alias in &page.aliases {
                entries.push((alias.clone(), Target::Page(i)));
            }
            for nt in &page.node_types {
                for name in &nt.names {
                    entries.push((name.clone(), Target::NodeType(nt.entity.clone())));
                }
            }
            for rt in &page.relation_types {
                for name in &rt.names {
                    entries.push((name.clone(), Target::RelationType(rt.entity.clone())));
                }
            }
        }
        Self::new(entries)
    }

    /// Every known name in a normalized text; see [`find_in`].
    pub fn find(&self, text: &str, visible: impl Fn(&Target) -> bool) -> Vec<NameHit<'_>> {
        find_in(&[self], text, visible)
    }

    /// Names on word boundaries with at least one visible target, and where they are.
    fn spans<'a>(
        &'a self,
        text: &str,
        visible: &impl Fn(&Target) -> bool,
    ) -> Vec<(usize, usize, NameHit<'a>)> {
        let Some(ac) = &self.ac else {
            return Vec::new();
        };
        ac.find_overlapping_iter(text)
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
            .collect()
    }
}

/// Every known name of several matchers in a normalized text, on word boundaries, that points
/// at something the caller may see (`visible`). Names of hidden things are dropped first, so
/// they can neither be reported nor shadow a visible name. A name inside a longer name that
/// also matched is dropped, so "partial refund" doesn't also report "refund", and a name found
/// by two matchers is one hit with the targets of both.
pub fn find_in<'a>(
    matchers: &[&'a NameMatcher],
    text: &str,
    visible: impl Fn(&Target) -> bool,
) -> Vec<NameHit<'a>> {
    let found: Vec<(usize, usize, NameHit<'a>)> = matchers
        .iter()
        .flat_map(|m| m.spans(text, &visible))
        .collect();
    let mut order: Vec<(usize, &(usize, usize, NameHit<'a>))> = found.iter().enumerate().collect();
    order.sort_by_key(|(i, (s, e, _))| (*s, std::cmp::Reverse(*e), *i));
    let mut out: Vec<NameHit<'a>> = Vec::new();
    for (_, (s, e, hit)) in order {
        let inside_longer = found
            .iter()
            .any(|(s2, e2, _)| s2 <= s && e <= e2 && (e2 - s2) > (e - s));
        if inside_longer {
            continue;
        }
        match out.iter_mut().find(|h| h.name == hit.name) {
            Some(h) => {
                for t in &hit.targets {
                    if !h.targets.contains(t) {
                        h.targets.push(t);
                    }
                }
            }
            None => out.push(hit.clone()),
        }
    }
    out
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

    #[test]
    fn two_matchers_merge() {
        let wiki = NameMatcher::build(&catalog());
        let graph = NameMatcher::new([
            ("订单1042".to_string(), Target::Node(0)),
            ("SSO".to_string(), Target::Node(1)),
        ]);
        let hits = find_in(&[&wiki, &graph], &normalize("订单1042 的 SSO"), |_| true);
        let names: Vec<&str> = hits.iter().map(|h| h.name).collect();
        // 订单 is inside the longer node name, so only the node is reported.
        assert_eq!(names, vec!["订单1042", "sso"]);
        assert_eq!(
            hits[1].targets,
            vec![&Target::Page(1), &Target::Node(1)],
            "one name, targets of both matchers"
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
