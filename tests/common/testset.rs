//! Loading `testset/` into tenants: shared by the search tests and `examples/load_testset.rs`.
//! `tests/testset.rs` validates the same files in depth; this module only reads what is needed
//! to install the pages and graph data, ask the questions and judge the answers.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use kb::tenants::{group_dir_name, user_dir_name};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn testset_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testset")
}

#[derive(Debug, Deserialize)]
pub struct TenantFile {
    pub id: String,
    pub users: Vec<UserEntry>,
    #[serde(default)]
    pub graph: GraphFile,
}

#[derive(Debug, Default, Deserialize)]
pub struct GraphFile {
    #[serde(default)]
    pub schemas: Vec<String>,
    #[serde(default)]
    pub spaces: Vec<SpaceEntry>,
}

#[derive(Debug, Deserialize)]
pub struct SpaceEntry {
    pub id: String,
    pub schema: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub members: Members,
}

#[derive(Debug, Default, Deserialize)]
pub struct Members {
    #[serde(default)]
    pub users: Vec<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct UserEntry {
    pub id: String,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct QuestionFile {
    pub tenant: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Deserialize)]
pub struct Question {
    pub id: String,
    pub category: String,
    pub milestone: String,
    pub lang: String,
    #[serde(default)]
    pub user: Option<String>,
    pub query: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub expect: Expect,
    #[serde(default)]
    pub expect_empty: Vec<String>,
    #[serde(default)]
    pub must_not: Vec<String>,
    #[serde(default)]
    pub must_not_spaces: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Expect {
    #[serde(default)]
    pub memories: Vec<String>,
    #[serde(default)]
    pub knowledge: Vec<String>,
    #[serde(default)]
    pub relations: Vec<ExpectedEdge>,
}

#[derive(Debug, Deserialize)]
pub struct ExpectedEdge {
    pub from: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub to: String,
}

/// The scenario tenants, in name order.
pub fn tenants() -> Vec<TenantFile> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(testset_dir().join("tenants"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.iter()
        .map(|d| {
            serde_norway::from_str(&fs::read_to_string(d.join("tenant.yaml")).unwrap()).unwrap()
        })
        .collect()
}

pub fn questions(tenant: &str) -> Vec<Question> {
    let path = testset_dir()
        .join("questions")
        .join(format!("{tenant}.yaml"));
    let file: QuestionFile = serde_norway::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(file.tenant, tenant);
    file.questions
}

impl TenantFile {
    /// The groups the host sends for a user.
    pub fn groups_of(&self, user: &str) -> Vec<String> {
        self.users
            .iter()
            .find(|u| u.id == user)
            .map(|u| u.groups.clone())
            .unwrap_or_default()
    }
}

/// Maps a fixture path to a real page path: `users/<raw>/…` → `users/u_<hash>/…`, and
/// `groups/<raw>/…` → `groups/g_<hash>/…`.
pub fn page_path(fixture: &str) -> String {
    let mut segs: Vec<String> = fixture.split('/').map(str::to_owned).collect();
    match segs.first().map(String::as_str) {
        Some("users") if segs.len() > 1 => segs[1] = user_dir_name(&segs[1]),
        Some("groups") if segs.len() > 1 => segs[1] = group_dir_name(&segs[1]),
        _ => {}
    }
    segs.join("/")
}

/// Copies a scenario's wiki pages into a tenant's `wiki/` folder, mapping user and group
/// folders to their hashed names.
pub fn install_wiki(tenant: &str, wiki_dir: &Path) {
    let src = testset_dir().join("tenants").join(tenant).join("wiki");
    let mut files = Vec::new();
    walk(&src, &src, &mut files);
    for (rel, file) in files {
        let dest = wiki_dir.join(format!("{}.md", page_path(&rel)));
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(&file, &dest).unwrap();
    }
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(base, &p, out);
        } else if p.extension().is_some_and(|e| e == "md") {
            let rel = p.strip_prefix(base).unwrap().with_extension("");
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, p));
        }
    }
}

/// One call that loads graph data: who makes it (user and groups headers), where, and what.
#[derive(Debug)]
pub struct GraphCall {
    pub user: Option<String>,
    pub groups: Vec<String>,
    pub uri: &'static str,
    pub body: Value,
}

/// The schema definition a host would send for one of a scenario's schema pages: structural
/// fields and descriptions, read back from `wiki/graph/<schema>.md`.
pub fn schema_request(tenant: &str, schema: &str) -> Value {
    let path = testset_dir()
        .join("tenants")
        .join(tenant)
        .join("wiki")
        .join("graph")
        .join(format!("{schema}.md"));
    let text = fs::read_to_string(&path).unwrap();
    let yaml = text.split("---").nth(1).expect("front matter");
    let front: Value = serde_norway::from_str(yaml).unwrap();
    let pick = |v: &Value, keys: &[&str]| -> Value {
        let mut out = serde_json::Map::new();
        for k in keys {
            if !v[k].is_null() {
                out.insert((*k).to_owned(), v[k].clone());
            }
        }
        Value::Object(out)
    };
    let map = |section: &str, keys: &[&str]| -> Value {
        let mut out = serde_json::Map::new();
        for (name, def) in front[section].as_object().into_iter().flatten() {
            out.insert(name.clone(), pick(def, keys));
        }
        Value::Object(out)
    };
    json!({
        "schema": schema,
        "title": front["title"],
        "summary": front["summary"],
        "aliases": front["aliases"],
        "nodes": map("nodes", &["key", "en", "zh", "aliases"]),
        "relations": map("relations", &["from", "to", "via", "en", "zh", "aliases"]),
    })
}

/// The calls that load a scenario's graph: its schemas, its spaces (a private space by its
/// owner, a restricted one by its first member), and each space's nodes and edges by the same
/// caller.
pub fn graph_calls(t: &TenantFile) -> Vec<GraphCall> {
    let mut out: Vec<GraphCall> = t
        .graph
        .schemas
        .iter()
        .map(|schema| GraphCall {
            user: None,
            groups: Vec::new(),
            uri: "/ingest/graph/schema",
            body: schema_request(&t.id, schema),
        })
        .collect();
    for s in &t.graph.spaces {
        let visibility = s.visibility.clone().unwrap_or_else(|| "shared".into());
        let (user, groups) = match visibility.as_str() {
            "private" => (s.owner.clone(), Vec::new()),
            "restricted" => match s.members.users.first() {
                Some(u) => (Some(u.clone()), t.groups_of(u)),
                None => (None, s.members.groups.clone()),
            },
            _ => (None, Vec::new()),
        };
        let mut body = json!({
            "space": s.id,
            "schema": s.schema,
            "name": s.name,
            "aliases": s.aliases,
            "visibility": visibility,
        });
        if visibility == "restricted" {
            body["members"] = json!({ "users": s.members.users, "groups": s.members.groups });
        }
        out.push(GraphCall {
            user: user.clone(),
            groups: groups.clone(),
            uri: "/ingest/graph/space",
            body,
        });
        let data_file = testset_dir()
            .join("tenants")
            .join(&t.id)
            .join("graph")
            .join(format!("{}.json", s.id));
        let data: Value = serde_json::from_str(&fs::read_to_string(data_file).unwrap()).unwrap();
        out.push(GraphCall {
            user,
            groups,
            uri: "/ingest/graph/data",
            body: data,
        });
    }
    out
}

pub fn slot_pages(resp: &Value, slot: &str) -> Vec<String> {
    resp[slot]["hits"]
        .as_array()
        .map(|hits| {
            hits.iter()
                .map(|h| h["page"].as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// The relations slot's edges as `(from, type, to)`.
pub fn slot_edges(resp: &Value) -> Vec<(String, String, String)> {
    resp["relations"]["edges"]
        .as_array()
        .map(|edges| {
            edges
                .iter()
                .map(|e| {
                    let s = |k: &str| e[k].as_str().unwrap_or("").to_owned();
                    (s("from"), s("type"), s("to"))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn slot_is_empty_or_weak(resp: &Value, slot: &str) -> bool {
    let s = &resp[slot];
    let empty = match slot {
        "relations" => s["edges"].as_array().is_none_or(|e| e.is_empty()),
        _ => s["hits"].as_array().is_none_or(|h| h.is_empty()),
    };
    empty || s["coverage"] == "weak"
}

/// Why a question failed, or `None` when it passed (testset/README.md, "Questions"): every
/// expected page and edge is in its slot, nothing in `must_not` appears, nothing from a
/// `must_not_spaces` space appears (no entity of it anywhere; the caller's own words echoed in
/// `unmatched_keywords` don't count), and every `expect_empty` slot is empty or weak.
pub fn judge(q: &Question, resp: &Value) -> Option<String> {
    let mut problems = Vec::new();
    for (slot, expected) in [
        ("memories", &q.expect.memories),
        ("knowledge", &q.expect.knowledge),
    ] {
        let got = slot_pages(resp, slot);
        for page in expected {
            if !got.contains(&page_path(page)) {
                problems.push(format!("{slot} lacks {page}"));
            }
        }
    }
    let edges = slot_edges(resp);
    for e in &q.expect.relations {
        let want = (e.from.clone(), e.ty.clone(), e.to.clone());
        if !edges.contains(&want) {
            problems.push(format!("relations lack {} {} {}", e.from, e.ty, e.to));
        }
    }
    let text = resp.to_string();
    for page in &q.must_not {
        if text.contains(&page_path(page)) {
            problems.push(format!("must_not {page} appears"));
        }
    }
    let mut shown = resp.clone();
    if let Some(o) = shown.as_object_mut() {
        o.remove("unmatched_keywords");
    }
    let shown = shown.to_string();
    for space in &q.must_not_spaces {
        if shown.contains(&format!("\"{space}:"))
            || shown.contains(&format!("\"space\":\"{space}\""))
        {
            problems.push(format!("something of space {space} appears"));
        }
    }
    for slot in &q.expect_empty {
        if !slot_is_empty_or_weak(resp, slot) {
            problems.push(format!("{slot} should be empty or weak"));
        }
    }
    (!problems.is_empty()).then(|| problems.join("; "))
}

/// The slots of a response in one line each, for failure reports.
pub fn describe(resp: &Value) -> String {
    let mut out = String::new();
    for slot in ["memories", "knowledge"] {
        let hits: Vec<String> = resp[slot]["hits"]
            .as_array()
            .map(|hs| {
                hs.iter()
                    .map(|h| {
                        format!(
                            "{} [{}; {}]",
                            h["page"].as_str().unwrap_or(""),
                            h["match"].as_str().unwrap_or(""),
                            h["matched_keywords"]
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push_str(&format!(
            "\n      {slot} ({}): {}",
            resp[slot]["coverage"].as_str().unwrap_or("?"),
            hits.join(" | ")
        ));
    }
    let edges: Vec<String> = resp["relations"]["edges"]
        .as_array()
        .map(|es| {
            es.iter()
                .map(|e| {
                    format!(
                        "{} {} {} [{}]",
                        e["from"].as_str().unwrap_or(""),
                        e["type"].as_str().unwrap_or(""),
                        e["to"].as_str().unwrap_or(""),
                        e["match"].as_str().unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    out.push_str(&format!(
        "\n      relations ({}; types {}): {}",
        resp["relations"]["coverage"].as_str().unwrap_or("?"),
        resp["relations"]["relation_types"],
        edges.join(" | ")
    ));
    out.push_str(&format!(
        "\n      entities: {}  unmatched: {}",
        resp["entities"], resp["unmatched_keywords"]
    ));
    out
}

/// Counts of passes per key, for the report.
#[derive(Default)]
pub struct Tally(pub HashMap<String, (usize, usize)>);

impl Tally {
    pub fn add(&mut self, key: &str, passed: bool) {
        let e = self.0.entry(key.to_owned()).or_default();
        e.1 += 1;
        if passed {
            e.0 += 1;
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut keys: Vec<&String> = self.0.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|k| {
                let (p, n) = self.0[k];
                format!("  {k:<14} {p:>3}/{n:<3}")
            })
            .collect()
    }
}
