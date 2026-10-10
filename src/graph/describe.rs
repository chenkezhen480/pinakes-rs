//! The wiki description of a schema, `graph/<schema>.md`.
//!
//! Code owns the structural fields (node types with their keys, relation types with their ends
//! and `via`), copied from the stored definition, so an edit can never change what data is
//! accepted. The descriptive fields (title, summary, aliases, what each type means in English
//! and Chinese) come from the definition when it has them, otherwise from the page as it
//! stands. Code writes no prose of its own: the librarian fills in what is missing and writes
//! the body (`librarian::described_page`), and a description it can't write is an error.

use serde::Serialize;
use serde_norway::{Mapping, Value};
use std::collections::BTreeMap;

use super::def::SchemaDef;
use crate::kinds::Kind;
use crate::wiki::{Page, PagePath, WikiError};

/// The path of a schema's description.
pub fn page_path(schema: &str) -> Result<PagePath, WikiError> {
    PagePath::parse(&format!("graph/{schema}"))
}

#[derive(Serialize)]
struct NodeFront<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    en: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    zh: Option<&'a str>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    aliases: &'a [String],
    key: &'a str,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    properties: &'a BTreeMap<String, crate::graph::def::PropertyDef>,
}

#[derive(Serialize)]
struct RelationFront<'a> {
    from: &'a str,
    to: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    en: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    zh: Option<&'a str>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    aliases: &'a [String],
}

/// A descriptive field of one type on the existing page, when the definition lacks it.
fn existing<'a>(
    page: Option<&'a Page>,
    section: &str,
    name: &str,
    field: &str,
) -> Option<&'a Value> {
    page?.front.extra.get(section)?.get(name)?.get(field)
}

fn existing_str<'a>(
    page: Option<&'a Page>,
    section: &str,
    name: &str,
    field: &str,
) -> Option<&'a str> {
    existing(page, section, name, field).and_then(Value::as_str)
}

fn existing_list(page: Option<&Page>, section: &str, name: &str) -> Vec<String> {
    existing(page, section, name, "aliases")
        .and_then(Value::as_sequence)
        .map(|s| {
            s.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The description page for a schema definition, keeping what the existing page says where
/// the definition is silent. `today` is the `updated` date.
pub fn describe(def: &SchemaDef, current: Option<&Page>, today: &str) -> Result<Page, WikiError> {
    let path = page_path(&def.schema)?;
    let mut page = current.cloned().unwrap_or_else(|| Page::new(path.clone()));
    page.path = path;
    let f = &mut page.front;
    f.kind = Some(Kind::Graph);
    if def.title.is_some() {
        f.title.clone_from(&def.title);
    }
    if !def.aliases.is_empty() || f.aliases.is_empty() {
        let mut aliases = def.aliases.clone();
        if !aliases.iter().any(|a| a == &def.schema) {
            aliases.insert(0, def.schema.clone());
        }
        f.aliases = aliases;
    }
    if def.summary.is_some() {
        f.summary.clone_from(&def.summary);
    }
    f.updated = Some(today.to_owned());

    let mut nodes = Mapping::new();
    for (name, n) in &def.nodes {
        let aliases = if n.aliases.is_empty() {
            existing_list(current, "nodes", name)
        } else {
            n.aliases.clone()
        };
        let front = NodeFront {
            en: n
                .en
                .as_deref()
                .or_else(|| existing_str(current, "nodes", name, "en")),
            zh: n
                .zh
                .as_deref()
                .or_else(|| existing_str(current, "nodes", name, "zh")),
            aliases: &aliases,
            key: &n.key,
            properties: &n.properties,
        };
        nodes.insert(Value::from(name.as_str()), to_value(&front));
    }
    let mut relations = Mapping::new();
    for (name, r) in &def.relations {
        let aliases = if r.aliases.is_empty() {
            existing_list(current, "relations", name)
        } else {
            r.aliases.clone()
        };
        let front = RelationFront {
            from: &r.from,
            to: &r.to,
            via: r.via.as_deref(),
            en: r
                .en
                .as_deref()
                .or_else(|| existing_str(current, "relations", name, "en")),
            zh: r
                .zh
                .as_deref()
                .or_else(|| existing_str(current, "relations", name, "zh")),
            aliases: &aliases,
        };
        relations.insert(Value::from(name.as_str()), to_value(&front));
    }
    f.extra
        .insert("schema".into(), Value::from(def.schema.as_str()));
    f.extra.insert("nodes".into(), Value::Mapping(nodes));
    f.extra
        .insert("relations".into(), Value::Mapping(relations));
    Ok(page)
}

fn to_value(v: &impl Serialize) -> Value {
    serde_norway::to_value(v).expect("front matter serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(json: serde_json::Value) -> SchemaDef {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn a_new_schema_gets_its_structure_and_no_invented_prose() {
        let d = def(serde_json::json!({
            "schema": "work_orders",
            "nodes": { "ticket": { "key": "no", "en": "A ticket" } },
            "relations": { "blocks": { "from": "ticket", "to": "ticket" } }
        }));
        let page = describe(&d, None, "2026-09-30").unwrap();
        assert_eq!(page.path.as_str(), "graph/work_orders");
        assert_eq!(page.front.title, None, "prose is the librarian's");
        assert_eq!(page.front.summary, None);
        assert!(page.body.is_empty());
        assert_eq!(page.front.aliases, vec!["work_orders"]);
        // It parses back as a schema page.
        let again = Page::parse(page.path.clone(), &page.render()).unwrap();
        assert_eq!(
            again.front.extra["nodes"]["ticket"]["key"].as_str(),
            Some("no")
        );
        assert_eq!(
            again.front.extra["relations"]["blocks"]["to"].as_str(),
            Some("ticket")
        );
    }

    /// Structure comes from the definition, prose from the page where the definition is silent.
    #[test]
    fn an_existing_page_keeps_its_prose() {
        let text = "---\nkind: graph\ntitle: Commerce graph\naliases: [commerce, 订单关系]\nsummary: Orders and buyers.\nschema: commerce\nnodes:\n  order: { en: \"A customer order\", zh: \"客户订单\", aliases: [订单], key: wrong_key }\nrelations: {}\nupdated: 2026-09-20\n---\n\n# Commerce graph\n\nHand-written prose.\n";
        let current = Page::parse(PagePath::parse("graph/commerce").unwrap(), text).unwrap();
        let d = def(serde_json::json!({
            "schema": "commerce",
            "nodes": { "order": { "key": "order_id" }, "coupon": { "key": "code", "zh": "优惠券" } }
        }));
        let page = describe(&d, Some(&current), "2026-09-30").unwrap();
        let f = &page.front;
        assert_eq!(f.title.as_deref(), Some("Commerce graph"));
        assert_eq!(f.summary.as_deref(), Some("Orders and buyers."));
        assert_eq!(f.aliases, vec!["commerce", "订单关系"]);
        assert_eq!(f.extra["nodes"]["order"]["key"].as_str(), Some("order_id"));
        assert_eq!(f.extra["nodes"]["order"]["zh"].as_str(), Some("客户订单"));
        assert_eq!(f.extra["nodes"]["coupon"]["zh"].as_str(), Some("优惠券"));
        assert_eq!(f.updated.as_deref(), Some("2026-09-30"));
        assert!(page.body.contains("Hand-written prose."));
    }
}
