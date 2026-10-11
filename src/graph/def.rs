//! Graph definitions: schemas (node and relation types), graph spaces with their visibility,
//! the nodes and edges written to a space, and entity references.
//!
//! Structural fields (type names, keys, relation ends, `via`) decide what data is accepted;
//! descriptive fields (`en`, `zh`, `aliases`, `title`, `summary`) only feed the schema's wiki
//! description. A schema change may only add types, relations and aliases.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::tenants::validate_id;

/// Longest schema, space, type or relation name.
pub const MAX_NAME_LEN: usize = 64;
/// Longest node key, node name, alias or description.
pub const MAX_TEXT_LEN: usize = 200;
/// Most aliases on one type, relation, space or node.
pub const MAX_ALIASES: usize = 20;
/// Most node types, and most relation types, in one schema.
pub const MAX_TYPES: usize = 200;
/// Most member users plus groups of a restricted space.
pub const MAX_MEMBERS: usize = 1_000;

/// Schema, space, type and relation names: a lowercase letter, then lowercase letters, digits
/// and `_`. They appear in entity references and wiki paths, so nothing else is allowed.
pub fn validate_name(what: &str, name: &str) -> Result<(), String> {
    let ok = name.len() <= MAX_NAME_LEN
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{what} {name:?} must be 1 to {MAX_NAME_LEN} characters: a lowercase letter, then lowercase letters, digits or _"
        ))
    }
}

/// Keys, names, aliases and descriptions: not empty, at most [`MAX_TEXT_LEN`] characters, no
/// control characters.
fn validate_text(what: &str, text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if text.chars().count() > MAX_TEXT_LEN {
        return Err(format!("{what} is longer than {MAX_TEXT_LEN} characters"));
    }
    if text.chars().any(char::is_control) {
        return Err(format!("{what} contains control characters"));
    }
    Ok(())
}

/// A node key: text, and no whitespace at either end, since ids are looked up trimmed.
fn validate_key(what: &str, key: &str) -> Result<(), String> {
    validate_text(what, key)?;
    if key.trim() != key {
        return Err(format!("{what} starts or ends with whitespace"));
    }
    Ok(())
}

fn validate_aliases(what: &str, aliases: &[String]) -> Result<(), String> {
    if aliases.len() > MAX_ALIASES {
        return Err(format!("{what} has more than {MAX_ALIASES} aliases"));
    }
    aliases
        .iter()
        .try_for_each(|a| validate_text(&format!("an alias of {what}"), a))
}

fn validate_opt(what: &str, text: &Option<String>) -> Result<(), String> {
    text.as_deref().map_or(Ok(()), |t| validate_text(what, t))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct NodeTypeDef {
    /// The field that identifies a node of this type, such as `order_id`.
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// What a node of this type may hold besides its name and aliases, as the schema's author
    /// declares it (decided with the user 2026-10-10).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, PropertyDef>,
}

/// A property a node type declares: the kind of value it holds, and what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PropertyDef {
    #[serde(rename = "type")]
    pub ty: PropertyType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh: Option<String>,
}

/// The kinds of property values: text, a number, a date (`YYYY-MM-DD`) or true/false.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum PropertyType {
    Text,
    Number,
    Date,
    Bool,
}

impl PropertyType {
    fn as_str(self) -> &'static str {
        match self {
            PropertyType::Text => "text",
            PropertyType::Number => "number",
            PropertyType::Date => "date",
            PropertyType::Bool => "bool",
        }
    }

    /// Whether a value is of this kind.
    fn holds(self, v: &serde_json::Value) -> bool {
        match self {
            PropertyType::Text => v
                .as_str()
                .is_some_and(|s| validate_text("a value", s).is_ok()),
            PropertyType::Number => v.as_f64().is_some_and(f64::is_finite),
            PropertyType::Bool => v.is_boolean(),
            PropertyType::Date => v.as_str().is_some_and(crate::trace::is_date),
        }
    }
}

/// Most properties a node type may declare.
pub const MAX_PROPERTIES: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RelationDef {
    pub from: String,
    pub to: String,
    /// The key field the relation joins on, such as `customer_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// A graph schema: node types and relation types. Its stored copy is authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct SchemaDef {
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub nodes: BTreeMap<String, NodeTypeDef>,
    #[serde(default)]
    pub relations: BTreeMap<String, RelationDef>,
}

/// What a space's data already uses, for checking that a schema change removes nothing in use.
pub trait Usage {
    fn uses_node_type(&self, schema: &str, ty: &str) -> bool;
    fn uses_relation(&self, schema: &str, rel: &str) -> bool;
}

impl SchemaDef {
    /// Checks names, limits and that every relation connects node types of this schema.
    pub fn validate(&self) -> Result<(), String> {
        validate_name("schema", &self.schema)?;
        validate_opt("the title", &self.title)?;
        validate_opt("the summary", &self.summary)?;
        validate_aliases("the schema", &self.aliases)?;
        if self.nodes.is_empty() {
            return Err("a schema needs at least one node type".into());
        }
        if self.nodes.len() > MAX_TYPES || self.relations.len() > MAX_TYPES {
            return Err(format!(
                "a schema has at most {MAX_TYPES} node types and {MAX_TYPES} relation types"
            ));
        }
        for (name, def) in &self.nodes {
            validate_name("node type", name)?;
            validate_text(&format!("the key of node type {name}"), &def.key)?;
            validate_opt(&format!("en of node type {name}"), &def.en)?;
            validate_opt(&format!("zh of node type {name}"), &def.zh)?;
            validate_aliases(&format!("node type {name}"), &def.aliases)?;
            if def.properties.len() > MAX_PROPERTIES {
                return Err(format!(
                    "node type {name} declares more than {MAX_PROPERTIES} properties"
                ));
            }
            for (prop, d) in &def.properties {
                validate_name("property", prop)?;
                if matches!(prop.as_str(), "name" | "aliases" | "key" | "type" | "space") {
                    return Err(format!(
                        "property {prop} of node type {name} has a name every node has already"
                    ));
                }
                validate_opt(&format!("en of property {prop}"), &d.en)?;
                validate_opt(&format!("zh of property {prop}"), &d.zh)?;
            }
        }
        for (name, def) in &self.relations {
            validate_name("relation type", name)?;
            if self.nodes.contains_key(name) {
                return Err(format!("{name:?} is both a node type and a relation type"));
            }
            for end in [&def.from, &def.to] {
                if !self.nodes.contains_key(end) {
                    return Err(format!(
                        "relation {name} refers to unknown node type {end:?}"
                    ));
                }
            }
            validate_opt(&format!("via of relation {name}"), &def.via)?;
            validate_opt(&format!("en of relation {name}"), &def.en)?;
            validate_opt(&format!("zh of relation {name}"), &def.zh)?;
            validate_aliases(&format!("relation {name}"), &def.aliases)?;
        }
        Ok(())
    }

    /// Checks that `self` only adds to `old`: keys and relation ends stay the same, and a node
    /// or relation type is removed only when no space uses it. Returns the definition to
    /// store: `self`, with descriptive fields it leaves out taken from `old`.
    pub fn apply_change(&self, old: &SchemaDef, usage: &dyn Usage) -> Result<SchemaDef, String> {
        let mut merged = self.clone();
        for (name, before) in &old.nodes {
            match merged.nodes.get_mut(name) {
                Some(now) => {
                    if now.key != before.key {
                        return Err(format!(
                            "node type {name} changes its key from {:?} to {:?}; keys can't change",
                            before.key, now.key
                        ));
                    }
                    fill(&mut now.en, &before.en);
                    fill(&mut now.zh, &before.zh);
                    if now.aliases.is_empty() {
                        now.aliases.clone_from(&before.aliases);
                    }
                    // Properties are only added: one stays, with its kind of value.
                    for (prop, was) in &before.properties {
                        match now.properties.get_mut(prop) {
                            Some(p) if p.ty == was.ty => {
                                fill(&mut p.en, &was.en);
                                fill(&mut p.zh, &was.zh);
                            }
                            Some(p) => {
                                return Err(format!(
                                    "property {prop} of node type {name} changes from {} to {}; only additions are allowed",
                                    was.ty.as_str(),
                                    p.ty.as_str()
                                ));
                            }
                            None => {
                                return Err(format!(
                                    "property {prop} of node type {name} is removed; only additions are allowed"
                                ));
                            }
                        }
                    }
                }
                None if usage.uses_node_type(&old.schema, name) => {
                    return Err(format!(
                        "node type {name} is removed but a space still has nodes of it"
                    ));
                }
                None => {}
            }
        }
        for (name, before) in &old.relations {
            match merged.relations.get_mut(name) {
                Some(now) => {
                    if (&now.from, &now.to, &now.via) != (&before.from, &before.to, &before.via) {
                        return Err(format!(
                            "relation {name} changes its ends or via; only additions are allowed"
                        ));
                    }
                    fill(&mut now.en, &before.en);
                    fill(&mut now.zh, &before.zh);
                    if now.aliases.is_empty() {
                        now.aliases.clone_from(&before.aliases);
                    }
                }
                None if usage.uses_relation(&old.schema, name) => {
                    return Err(format!(
                        "relation {name} is removed but a space still has edges of it"
                    ));
                }
                None => {}
            }
        }
        fill(&mut merged.title, &old.title);
        fill(&mut merged.summary, &old.summary);
        if merged.aliases.is_empty() {
            merged.aliases.clone_from(&old.aliases);
        }
        Ok(merged)
    }
}

fn fill(now: &mut Option<String>, before: &Option<String>) {
    if now.is_none() {
        now.clone_from(before);
    }
}

/// Who can see a graph space.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    /// Everyone in the tenant.
    #[default]
    Shared,
    /// Only its owner, the user who created it.
    Private,
    /// Its owner (the user who created it, when there was one) and the users and groups on
    /// its member list.
    Restricted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct Members {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub users: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
}

impl Members {
    fn is_empty(&self) -> bool {
        self.users.is_empty() && self.groups.is_empty()
    }
}

/// What a caller sends to create a space. The owner is never in the body: it is the caller.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct SpaceRequest {
    pub space: String,
    pub schema: String,
    /// Display name, such as 跨境站.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default)]
    pub members: Members,
}

/// A registered graph space. Its owner and members are raw ids, kept only in the tenant's
/// store; everywhere else they are compared as folder names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SpaceDef {
    pub space: String,
    pub schema: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    pub visibility: Visibility,
    /// The user who created it, when the call had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default)]
    pub members: Members,
}

impl SpaceRequest {
    /// Checks the request and turns it into a space owned by `owner` (the caller's user id).
    pub fn into_def(self, owner: Option<&str>) -> Result<SpaceDef, String> {
        validate_name("space", &self.space)?;
        validate_name("schema", &self.schema)?;
        validate_opt("the space name", &self.name)?;
        validate_aliases("the space", &self.aliases)?;
        let m = &self.members;
        if m.users.len() + m.groups.len() > MAX_MEMBERS {
            return Err(format!("a space has at most {MAX_MEMBERS} members"));
        }
        for u in &m.users {
            validate_id("member user", u)?;
        }
        for g in &m.groups {
            validate_id("member group", g)?;
        }
        match self.visibility {
            Visibility::Shared if !m.is_empty() => {
                return Err("a shared space has no member list".into());
            }
            Visibility::Private if owner.is_none() => {
                return Err("a private space needs a user: send the user id header".into());
            }
            Visibility::Private if !m.is_empty() => {
                return Err("a private space has no member list; only its owner sees it".into());
            }
            Visibility::Restricted if m.is_empty() => {
                return Err("a restricted space needs member users or groups".into());
            }
            _ => {}
        }
        Ok(SpaceDef {
            space: self.space,
            schema: self.schema,
            name: self.name,
            aliases: self.aliases,
            visibility: self.visibility,
            owner: owner.map(str::to_owned),
            members: self.members,
        })
    }
}

/// One node of a space, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NodeRecord {
    pub space: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Values of the properties its type declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, serde_json::Value>,
}

/// One edge of a space, as stored. Both ends are in the same space.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct EdgeRecord {
    pub space: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub from_type: String,
    pub from_key: String,
    pub to_type: String,
    pub to_key: String,
}

/// Which node: its space, type and key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NodeKey {
    pub space: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub key: String,
}

/// Every node and edge of one tenant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphData {
    pub nodes: Vec<NodeRecord>,
    pub edges: Vec<EdgeRecord>,
}

/// A node as a caller writes it; its space comes from the request.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct NodeIn {
    #[serde(rename = "type")]
    pub ty: String,
    pub key: String,
    /// Defaults to the key.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Values of properties its type declares; a node written again replaces them all.
    #[serde(default)]
    pub properties: BTreeMap<String, serde_json::Value>,
}

/// An edge as a caller writes it: ends as `type/key` inside the request's space.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct EdgeIn {
    pub from: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub to: String,
}

/// Most nodes, and most edges, in one data request.
pub const MAX_BATCH: usize = 10_000;

/// `POST /ingest/graph/data`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DataRequest {
    pub space: String,
    /// When given, must be the space's schema.
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub nodes: Vec<NodeIn>,
    #[serde(default)]
    pub edges: Vec<EdgeIn>,
}

/// Checks nodes against a schema and turns them into records of `space`. Later nodes with the
/// same type and key replace earlier ones.
pub fn check_nodes(
    schema: &SchemaDef,
    space: &str,
    nodes: Vec<NodeIn>,
) -> Result<Vec<NodeRecord>, String> {
    let mut out: BTreeMap<(String, String), NodeRecord> = BTreeMap::new();
    for n in nodes {
        if !schema.nodes.contains_key(&n.ty) {
            return Err(format!(
                "node type {:?} is not in schema {}",
                n.ty, schema.schema
            ));
        }
        validate_key(&format!("the key of a {} node", n.ty), &n.key)?;
        validate_opt(&format!("the name of {}/{}", n.ty, n.key), &n.name)?;
        let declared = &schema.nodes[&n.ty].properties;
        for (prop, value) in &n.properties {
            let Some(d) = declared.get(prop) else {
                return Err(format!(
                    "node type {} declares no property {prop:?} (schema {})",
                    n.ty, schema.schema
                ));
            };
            if !d.ty.holds(value) {
                return Err(format!(
                    "property {prop} of {}/{} must be {}",
                    n.ty,
                    n.key,
                    match d.ty {
                        PropertyType::Text => "text of at most 200 characters",
                        PropertyType::Number => "a number",
                        PropertyType::Date => "a date such as 2026-10-10",
                        PropertyType::Bool => "true or false",
                    }
                ));
            }
        }
        validate_aliases(&format!("{}/{}", n.ty, n.key), &n.aliases)?;
        let name = n.name.unwrap_or_else(|| n.key.clone());
        out.insert(
            (n.ty.clone(), n.key.clone()),
            NodeRecord {
                space: space.to_owned(),
                ty: n.ty,
                key: n.key,
                name,
                aliases: n.aliases,
                properties: n.properties,
            },
        );
    }
    Ok(out.into_values().collect())
}

/// Checks edges against a schema: the relation exists and its ends have the right types.
/// Whether the end nodes exist is checked by the caller, which knows the stored nodes.
pub fn check_edges(
    schema: &SchemaDef,
    space: &str,
    edges: Vec<EdgeIn>,
) -> Result<Vec<EdgeRecord>, String> {
    let mut out = std::collections::BTreeSet::new();
    for e in edges {
        let Some(rel) = schema.relations.get(&e.ty) else {
            return Err(format!(
                "relation {:?} is not in schema {}",
                e.ty, schema.schema
            ));
        };
        let end = |s: &str, want: &str| -> Result<(String, String), String> {
            let (ty, key) = s
                .split_once('/')
                .ok_or_else(|| format!("edge end {s:?} is not type/key"))?;
            if ty != want {
                return Err(format!(
                    "relation {} goes from {} to {}, but an edge has end {s:?}",
                    e.ty, rel.from, rel.to
                ));
            }
            validate_key("an edge end's key", key)?;
            Ok((ty.to_owned(), key.to_owned()))
        };
        let (from_type, from_key) = end(&e.from, &rel.from)?;
        let (to_type, to_key) = end(&e.to, &rel.to)?;
        out.insert(EdgeRecord {
            space: space.to_owned(),
            ty: e.ty,
            from_type,
            from_key,
            to_type,
            to_key,
        });
    }
    Ok(out.into_iter().collect())
}

/// An entity reference: `schema:type` names a node type, `space:type/key` one node. Space and
/// schema names never collide, so the two can't be confused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityRef<'a> {
    Type {
        schema: &'a str,
        ty: &'a str,
    },
    Node {
        space: &'a str,
        ty: &'a str,
        key: &'a str,
    },
}

impl<'a> EntityRef<'a> {
    /// Parses a reference; the key is everything after the first `/`, so it may hold `/`.
    pub fn parse(s: &'a str) -> Option<Self> {
        let (prefix, rest) = s.split_once(':')?;
        validate_name("", prefix).ok()?;
        match rest.split_once('/') {
            Some((ty, key)) => {
                validate_name("", ty).ok()?;
                (!key.is_empty() && !key.chars().any(char::is_control)).then_some(EntityRef::Node {
                    space: prefix,
                    ty,
                    key,
                })
            }
            None => {
                validate_name("", rest).ok()?;
                Some(EntityRef::Type {
                    schema: prefix,
                    ty: rest,
                })
            }
        }
    }
}

/// The reference of one node: `space:type/key`.
pub fn node_ref(space: &str, ty: &str, key: &str) -> String {
    format!("{space}:{ty}/{key}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commerce() -> SchemaDef {
        serde_json::from_value(serde_json::json!({
            "schema": "commerce",
            "nodes": {
                "order": { "key": "order_id", "aliases": ["订单"] },
                "customer": { "key": "customer_id" }
            },
            "relations": {
                "placed_by": { "from": "order", "to": "customer", "via": "customer_id" }
            }
        }))
        .unwrap()
    }

    struct Uses(&'static [&'static str]);
    impl Usage for Uses {
        fn uses_node_type(&self, _: &str, ty: &str) -> bool {
            self.0.contains(&ty)
        }
        fn uses_relation(&self, _: &str, rel: &str) -> bool {
            self.0.contains(&rel)
        }
    }

    #[test]
    fn names_and_schemas_validate() {
        assert!(validate_name("x", "order_2").is_ok());
        for bad in ["", "Order", "2order", "or-der", "订单", &"a".repeat(65)] {
            assert!(validate_name("x", bad).is_err(), "{bad}");
        }
        assert!(commerce().validate().is_ok());
        let mut s = commerce();
        s.relations.get_mut("placed_by").unwrap().to = "buyer".into();
        assert!(s.validate().is_err());
        let mut s = commerce();
        s.nodes.clear();
        assert!(s.validate().is_err());
    }

    #[test]
    fn schema_changes_are_additive() {
        let old = commerce();
        let mut added = commerce();
        added.nodes.get_mut("order").unwrap().aliases.clear();
        added.nodes.insert(
            "coupon".into(),
            NodeTypeDef {
                key: "code".into(),
                en: None,
                zh: None,
                aliases: vec![],
                properties: BTreeMap::new(),
            },
        );
        let merged = added.apply_change(&old, &Uses(&[])).unwrap();
        assert!(merged.nodes.contains_key("coupon"));
        // Descriptive fields left out are kept.
        assert_eq!(merged.nodes["order"].aliases, vec!["订单"]);

        let mut rekeyed = commerce();
        rekeyed.nodes.get_mut("order").unwrap().key = "id".into();
        assert!(rekeyed.apply_change(&old, &Uses(&[])).is_err());

        let mut rewired = commerce();
        rewired.relations.get_mut("placed_by").unwrap().via = None;
        assert!(rewired.apply_change(&old, &Uses(&[])).is_err());

        let mut removed = commerce();
        removed.relations.clear();
        assert!(removed.apply_change(&old, &Uses(&[])).is_ok());
        assert!(removed.apply_change(&old, &Uses(&["placed_by"])).is_err());
    }

    #[test]
    fn space_requests() {
        let req = |vis: Visibility, users: &[&str]| SpaceRequest {
            space: "mainland".into(),
            schema: "commerce".into(),
            name: Some("国内站".into()),
            aliases: vec![],
            visibility: vis,
            members: Members {
                users: users.iter().map(|u| u.to_string()).collect(),
                groups: vec![],
            },
        };
        assert!(req(Visibility::Shared, &[]).into_def(None).is_ok());
        assert!(req(Visibility::Shared, &["a"]).into_def(None).is_err());
        assert!(req(Visibility::Private, &[]).into_def(None).is_err());
        let private = req(Visibility::Private, &[]).into_def(Some("jin")).unwrap();
        assert_eq!(private.owner.as_deref(), Some("jin"));
        assert!(
            req(Visibility::Restricted, &[])
                .into_def(Some("x"))
                .is_err()
        );
        assert!(
            req(Visibility::Restricted, &["marco"])
                .into_def(None)
                .is_ok()
        );
    }

    #[test]
    fn data_is_checked_against_the_schema() {
        let s = commerce();
        let nodes = check_nodes(
            &s,
            "mainland",
            vec![NodeIn {
                ty: "order".into(),
                key: "1042".into(),
                name: None,
                aliases: vec![],
                properties: BTreeMap::new(),
            }],
        )
        .unwrap();
        assert_eq!(nodes[0].name, "1042");
        assert!(
            check_nodes(
                &s,
                "mainland",
                vec![NodeIn {
                    ty: "coupon".into(),
                    key: "x".into(),
                    name: None,
                    aliases: vec![],
                    properties: BTreeMap::new(),
                }]
            )
            .is_err()
        );
        let edge = |from: &str, ty: &str, to: &str| EdgeIn {
            from: from.into(),
            ty: ty.into(),
            to: to.into(),
        };
        let edges = check_edges(
            &s,
            "mainland",
            vec![edge("order/1042", "placed_by", "customer/c/1")],
        )
        .unwrap();
        assert_eq!(edges[0].to_key, "c/1");
        assert!(check_edges(&s, "m", vec![edge("customer/1", "placed_by", "order/2")]).is_err());
        assert!(check_edges(&s, "m", vec![edge("order/1", "ships", "customer/2")]).is_err());
        assert!(check_edges(&s, "m", vec![edge("order", "placed_by", "customer/2")]).is_err());
    }

    #[test]
    fn entity_refs() {
        assert_eq!(
            EntityRef::parse("commerce:order"),
            Some(EntityRef::Type {
                schema: "commerce",
                ty: "order"
            })
        );
        assert_eq!(
            EntityRef::parse("mainland:order/10/42"),
            Some(EntityRef::Node {
                space: "mainland",
                ty: "order",
                key: "10/42"
            })
        );
        for bad in [
            "expertise/refunds",
            "order/1042",
            "mainland:order/",
            "Main:order/1",
            ":x",
        ] {
            assert_eq!(EntityRef::parse(bad), None, "{bad}");
        }
    }

    /// Node types declare their properties; values are checked against them, and a schema
    /// change may add properties but not drop one or change its kind.
    #[test]
    fn properties_are_declared_checked_and_only_added() {
        let with = |props: serde_json::Value| -> SchemaDef {
            serde_json::from_value(serde_json::json!({
                "schema": "novel",
                "nodes": { "person": { "key": "id", "properties": props } }
            }))
            .unwrap()
        };
        let s = with(serde_json::json!({
            "age": { "type": "number", "zh": "年龄" },
            "born": { "type": "date" },
            "alive": { "type": "bool" },
            "motto": { "type": "text" }
        }));
        s.validate().unwrap();
        let node = |props: serde_json::Value| NodeIn {
            ty: "person".into(),
            key: "anna".into(),
            name: None,
            aliases: vec![],
            properties: serde_json::from_value(props).unwrap(),
        };
        let ok = check_nodes(
            &s,
            "book1",
            vec![node(serde_json::json!({ "age": 31, "born": "1995-04-02", "alive": true, "motto": "carpe diem" }))],
        )
        .unwrap();
        assert_eq!(ok[0].properties["age"], 31);
        for bad in [
            serde_json::json!({ "height": 170 }),
            serde_json::json!({ "age": "thirty" }),
            serde_json::json!({ "born": "April 2nd" }),
            serde_json::json!({ "born": "2026-02-30" }),
            serde_json::json!({ "born": "2026-13-01" }),
            serde_json::json!({ "alive": "yes" }),
            serde_json::json!({ "motto": "" }),
        ] {
            assert!(
                check_nodes(&s, "book1", vec![node(bad.clone())]).is_err(),
                "{bad}"
            );
        }
        assert!(
            with(serde_json::json!({ "name": { "type": "text" } }))
                .validate()
                .is_err(),
            "a property can't shadow a node's own fields"
        );
        // Adding is fine; dropping one or changing its kind isn't.
        let more = with(serde_json::json!({
            "age": { "type": "number" }, "born": { "type": "date" }, "alive": { "type": "bool" },
            "motto": { "type": "text" }, "rank": { "type": "text" }
        }));
        let merged = more.apply_change(&s, &Uses(&[])).unwrap();
        assert_eq!(
            merged.nodes["person"].properties["age"].zh.as_deref(),
            Some("年龄")
        );
        let fewer = with(serde_json::json!({ "age": { "type": "number" } }));
        assert!(fewer.apply_change(&s, &Uses(&[])).is_err());
        let changed = with(serde_json::json!({
            "age": { "type": "text" }, "born": { "type": "date" }, "alive": { "type": "bool" },
            "motto": { "type": "text" }
        }));
        assert!(changed.apply_change(&s, &Uses(&[])).is_err());
    }
}
