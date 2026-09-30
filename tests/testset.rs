//! Checks that `testset/` is internally consistent: pages parse and follow the wiki rules,
//! anchors and graph data resolve, and every question points at things that exist.
//! Run `cargo test --test testset -- --nocapture` to see the per-category counts.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use kb::kinds::Kind;
use kb::tenants::{group_dir_name, user_dir_name};
use kb::wiki::{Page, PagePath};
use serde::Deserialize;

fn testset_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testset")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TenantFile {
    id: String,
    business: String,
    #[allow(dead_code)]
    description: String,
    users: Vec<UserEntry>,
    graph: GraphConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphConfig {
    schemas: Vec<String>,
    spaces: Vec<SpaceEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpaceEntry {
    id: String,
    schema: String,
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    aliases: Vec<String>,
    #[serde(default = "shared")]
    visibility: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    members: Option<Members>,
}

fn shared() -> String {
    "shared".into()
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Members {
    #[serde(default)]
    users: Vec<String>,
    #[serde(default)]
    groups: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UserEntry {
    id: String,
    #[allow(dead_code)]
    role: String,
    #[allow(dead_code)]
    lang: String,
    /// The groups the host sends for this user (X-User-Groups).
    #[serde(default)]
    groups: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct SchemaNode {
    #[allow(dead_code)]
    en: String,
    #[allow(dead_code)]
    zh: String,
    #[serde(default)]
    #[allow(dead_code)]
    aliases: Vec<String>,
    #[allow(dead_code)]
    key: String,
}

#[derive(Debug, Deserialize)]
struct SchemaRelation {
    from: String,
    to: String,
    #[allow(dead_code)]
    en: String,
    #[allow(dead_code)]
    zh: String,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphData {
    space: String,
    schema: String,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphNode {
    #[serde(rename = "type")]
    node_type: String,
    key: String,
    name: String,
    #[serde(default)]
    #[allow(dead_code)]
    aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphEdge {
    from: String,
    #[serde(rename = "type")]
    rel: String,
    to: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuestionFile {
    tenant: String,
    questions: Vec<Question>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Question {
    id: String,
    category: String,
    milestone: String,
    lang: String,
    #[serde(default)]
    user: Option<String>,
    query: String,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    expect: Expect,
    #[serde(default)]
    expect_empty: Vec<String>,
    #[serde(default)]
    must_not: Vec<String>,
    #[serde(default)]
    must_not_spaces: Vec<String>,
    #[serde(default)]
    trap: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    notes: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expect {
    #[serde(default)]
    memories: Vec<String>,
    #[serde(default)]
    knowledge: Vec<String>,
    #[serde(default)]
    relations: Vec<ExpectedEdge>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedEdge {
    from: String,
    #[serde(rename = "type")]
    rel: String,
    to: String,
}

/// Who a fixture page is for, with raw group and user ids.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Aud {
    Tenant,
    Group(String),
    User(String),
}

/// A page as the fixtures name it: group and user pages use raw ids (`users/lily/preferences`).
struct FixturePage {
    kind: Option<Kind>,
    audience: Aud,
    page: Page,
}

struct Schema {
    nodes: HashMap<String, SchemaNode>,
    relations: HashMap<String, SchemaRelation>,
}

struct Space {
    schema: String,
    visibility: String,
    owner: Option<String>,
    members: Members,
    /// `type/key` of every node in the data.
    instances: HashSet<String>,
    /// `from|rel|to`, all as `type/key`.
    edges: HashSet<String>,
}

struct Tenant {
    file: TenantFile,
    /// user id -> the groups the host sends for them.
    user_groups: HashMap<String, Vec<String>>,
    pages: BTreeMap<String, FixturePage>,
    schemas: HashMap<String, Schema>,
    spaces: HashMap<String, Space>,
}

struct Report {
    errors: Vec<String>,
}

impl Space {
    /// Whether a caller with this user id and these groups may see the space.
    fn visible_to(&self, user: Option<&str>, groups: &[String]) -> bool {
        match self.visibility.as_str() {
            "shared" => true,
            "private" => user.is_some() && user == self.owner.as_deref(),
            "restricted" => {
                user.is_some_and(|u| self.members.users.iter().any(|m| m == u))
                    || groups.iter().any(|g| self.members.groups.contains(g))
            }
            _ => false,
        }
    }
}

impl Report {
    fn err(&mut self, msg: impl Into<String>) {
        self.errors.push(msg.into());
    }
}

fn read_dir_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries.map(|e| e.unwrap().path()).collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    out
}

/// Walks `wiki/` and returns fixture paths (raw user ids) with their files.
fn wiki_files(wiki: &Path) -> Vec<(String, PathBuf)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
        for p in read_dir_sorted(dir) {
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
    let mut out = Vec::new();
    walk(wiki, wiki, &mut out);
    out
}

/// Maps a fixture path to a real page path: `users/<raw>/…` → `users/u_<hash>/…`, and
/// `groups/<raw>/…` → `groups/g_<hash>/…`.
fn to_page_path(fixture: &str) -> (String, Aud) {
    let segs: Vec<&str> = fixture.split('/').collect();
    let (dir, aud) = match segs.as_slice() {
        ["users", raw, ..] => (user_dir_name(raw), Aud::User(raw.to_string())),
        ["groups", raw, ..] => (group_dir_name(raw), Aud::Group(raw.to_string())),
        _ => return (fixture.to_owned(), Aud::Tenant),
    };
    let mut real = segs.clone();
    real[1] = &dir;
    (real.join("/"), aud)
}

/// Whether a caller with this user id and these groups may read a page.
fn page_visible(aud: &Aud, user: Option<&str>, groups: &[String]) -> bool {
    match aud {
        Aud::Tenant => true,
        Aud::Group(g) => groups.contains(g),
        Aud::User(u) => user == Some(u.as_str()),
    }
}

/// The groups a question's caller brings: those of its user, none without a user.
fn caller_groups(t: &Tenant, user: Option<&str>) -> Vec<String> {
    user.and_then(|u| t.user_groups.get(u))
        .cloned()
        .unwrap_or_default()
}

fn load_tenant(dir: &Path, report: &mut Report) -> Tenant {
    let name = dir.file_name().unwrap().to_str().unwrap().to_owned();
    let file: TenantFile =
        serde_norway::from_str(&fs::read_to_string(dir.join("tenant.yaml")).unwrap())
            .unwrap_or_else(|e| panic!("{name}/tenant.yaml: {e}"));
    if file.id != name {
        report.err(format!("{name}: tenant.yaml id is {:?}", file.id));
    }
    let users: HashSet<&str> = file.users.iter().map(|u| u.id.as_str()).collect();
    let known_groups: HashSet<&str> = file
        .users
        .iter()
        .flat_map(|u| u.groups.iter())
        .chain(
            file.graph
                .spaces
                .iter()
                .filter_map(|s| s.members.as_ref())
                .flat_map(|m| m.groups.iter()),
        )
        .map(String::as_str)
        .collect();

    let mut pages = BTreeMap::new();
    for (fixture, path) in wiki_files(&dir.join("wiki")) {
        let (real, audience) = to_page_path(&fixture);
        let where_ = format!("{name}/wiki/{fixture}.md");
        match &audience {
            Aud::User(u) if !users.contains(u.as_str()) => {
                report.err(format!("{where_}: user {u:?} is not in tenant.yaml"));
            }
            Aud::Group(g) if !known_groups.contains(g.as_str()) => {
                report.err(format!("{where_}: group {g:?} is not used in tenant.yaml"));
            }
            _ => {}
        }
        let page_path = match PagePath::parse(&real) {
            Ok(p) => p,
            Err(e) => {
                report.err(format!("{where_}: {e}"));
                continue;
            }
        };
        let text = fs::read_to_string(&path).unwrap();
        let page = match Page::parse(page_path.clone(), &text) {
            Ok(p) => p,
            Err(e) => {
                report.err(format!("{where_}: {e}"));
                continue;
            }
        };
        let kind = page_path.kind();
        if page.front.kind != kind {
            report.err(format!(
                "{where_}: front matter kind {:?} but folder says {kind:?}",
                page.front.kind
            ));
        }
        for (field, missing) in [
            ("title", page.front.title.is_none()),
            ("summary", page.front.summary.is_none()),
            ("aliases", page.front.aliases.is_empty()),
            ("updated", page.front.updated.is_none()),
        ] {
            if missing {
                report.err(format!("{where_}: missing {field}"));
            }
        }
        if page.body.trim().is_empty() {
            report.err(format!("{where_}: empty body"));
        }
        let is_memory = matches!(
            kind,
            Some(Kind::Experience | Kind::Decision | Kind::Preference)
        );
        if is_memory && page.front.about.is_empty() {
            report.err(format!("{where_}: memory page without an anchor"));
        }
        pages.insert(
            fixture,
            FixturePage {
                kind,
                audience,
                page,
            },
        );
    }

    let mut schemas = HashMap::new();
    for schema_name in &file.graph.schemas {
        let swhere = format!("{name}/wiki/graph/{schema_name}.md");
        let Some(page) = pages.get(&format!("graph/{schema_name}")) else {
            report.err(format!("{swhere}: missing schema description"));
            continue;
        };
        let extra = &page.page.front.extra;
        match extra.get("schema").and_then(|v| v.as_str()) {
            Some(n) if n == schema_name => {}
            other => report.err(format!("{swhere}: schema field is {other:?}")),
        }
        let nodes: HashMap<String, SchemaNode> = extra
            .get("nodes")
            .cloned()
            .map(serde_norway::from_value)
            .transpose()
            .unwrap_or_else(|e| panic!("{swhere} nodes: {e}"))
            .unwrap_or_default();
        let relations: HashMap<String, SchemaRelation> = extra
            .get("relations")
            .cloned()
            .map(serde_norway::from_value)
            .transpose()
            .unwrap_or_else(|e| panic!("{swhere} relations: {e}"))
            .unwrap_or_default();
        if nodes.is_empty() || relations.is_empty() {
            report.err(format!("{swhere}: no nodes or relations"));
        }
        for (rel, r) in &relations {
            for end in [&r.from, &r.to] {
                if !nodes.contains_key(end) {
                    report.err(format!("{swhere}: {rel} uses unknown type {end}"));
                }
            }
            if r.aliases.is_empty() {
                report.err(format!("{swhere}: {rel} has no aliases"));
            }
        }
        schemas.insert(schema_name.clone(), Schema { nodes, relations });
    }
    // Only schemas get wiki pages; spaces and their data never do.
    for fixture in pages.keys() {
        if let Some(n) = fixture.strip_prefix("graph/")
            && !file.graph.schemas.iter().any(|s| s == n)
        {
            report.err(format!(
                "{name}/wiki/{fixture}.md: not a declared schema (spaces and data get no wiki pages)"
            ));
        }
    }

    let mut spaces = HashMap::new();
    for sp in &file.graph.spaces {
        let dwhere = format!("{name}/graph/{}.json", sp.id);
        if file.graph.schemas.contains(&sp.id) {
            report.err(format!(
                "{name}: space {} has the same name as a schema",
                sp.id
            ));
        }
        if sp.name.trim().is_empty() {
            report.err(format!("{name}: space {} has no display name", sp.id));
        }
        let users: HashSet<&str> = file.users.iter().map(|u| u.id.as_str()).collect();
        let members = sp.members.clone().unwrap_or_default();
        match sp.visibility.as_str() {
            "shared" if sp.owner.is_none() && sp.members.is_none() => {}
            "private" if sp.members.is_none() => match &sp.owner {
                Some(o) if users.contains(o.as_str()) => {}
                other => report.err(format!(
                    "{name}: private space {} has owner {other:?}",
                    sp.id
                )),
            },
            "restricted" if sp.owner.is_none() => {
                if members.users.is_empty() && members.groups.is_empty() {
                    report.err(format!("{name}: restricted space {} has no members", sp.id));
                }
                for u in &members.users {
                    if !users.contains(u.as_str()) {
                        report.err(format!("{name}: space {} lists unknown user {u}", sp.id));
                    }
                }
            }
            v => report.err(format!(
                "{name}: space {} has visibility {v:?} with the wrong owner/members fields",
                sp.id
            )),
        }
        let Some(schema) = schemas.get(&sp.schema) else {
            report.err(format!(
                "{name}: space {} uses unknown schema {}",
                sp.id, sp.schema
            ));
            continue;
        };
        let data_path = dir.join("graph").join(format!("{}.json", sp.id));
        let data: GraphData = serde_json::from_str(
            &fs::read_to_string(&data_path)
                .unwrap_or_else(|_| panic!("missing {}", data_path.display())),
        )
        .unwrap_or_else(|e| panic!("{}: {e}", data_path.display()));
        if data.space != sp.id || data.schema != sp.schema {
            report.err(format!(
                "{dwhere}: says space {:?} of schema {:?}",
                data.space, data.schema
            ));
        }
        let mut instances = HashSet::new();
        let mut types_of = HashMap::new();
        for n in &data.nodes {
            if !schema.nodes.contains_key(&n.node_type) {
                report.err(format!(
                    "{dwhere}: node {} has unknown type {}",
                    n.key, n.node_type
                ));
            }
            if n.name.trim().is_empty() {
                report.err(format!("{dwhere}: node {} has no name", n.key));
            }
            let id = format!("{}/{}", n.node_type, n.key);
            if !instances.insert(id.clone()) {
                report.err(format!("{dwhere}: duplicate node {id}"));
            }
            types_of.insert(id, n.node_type.clone());
        }
        let mut edges = HashSet::new();
        for e in &data.edges {
            let Some(r) = schema.relations.get(&e.rel) else {
                report.err(format!("{dwhere}: edge uses unknown relation {}", e.rel));
                continue;
            };
            for (end, want) in [(&e.from, &r.from), (&e.to, &r.to)] {
                match types_of.get(end) {
                    None => report.err(format!("{dwhere}: edge end {end} does not exist")),
                    Some(t) if t != want => report.err(format!(
                        "{dwhere}: {} expects {want} but {end} is a {t}",
                        e.rel
                    )),
                    _ => {}
                }
            }
            if !edges.insert(format!("{}|{}|{}", e.from, e.rel, e.to)) {
                report.err(format!(
                    "{dwhere}: duplicate edge {} {} {}",
                    e.from, e.rel, e.to
                ));
            }
        }
        if spaces
            .insert(
                sp.id.clone(),
                Space {
                    schema: sp.schema.clone(),
                    visibility: sp.visibility.clone(),
                    owner: sp.owner.clone(),
                    members,
                    instances,
                    edges,
                },
            )
            .is_some()
        {
            report.err(format!("{name}: duplicate space {}", sp.id));
        }
    }
    for path in read_dir_sorted(&dir.join("graph")) {
        let stem = path.file_stem().unwrap().to_str().unwrap();
        if !spaces.contains_key(stem) {
            report.err(format!("{name}/graph/{stem}.json: not a declared space"));
        }
    }

    let user_groups = file
        .users
        .iter()
        .map(|u| (u.id.clone(), u.groups.clone()))
        .collect();
    Tenant {
        file,
        user_groups,
        pages,
        schemas,
        spaces,
    }
}

/// Resolves `schema:type` (a node type) or `space:type/key` (one node).
fn check_entity(t: &Tenant, entity: &str) -> Result<(), String> {
    let (scope, rest) = entity
        .split_once(':')
        .ok_or_else(|| format!("{entity:?} is not an entity"))?;
    match rest.split_once('/') {
        None => {
            let schema = t.schemas.get(scope).ok_or_else(|| {
                format!("{entity:?}: {scope} is not a schema (node types are schema:type)")
            })?;
            schema
                .nodes
                .contains_key(rest)
                .then_some(())
                .ok_or_else(|| format!("{entity:?}: unknown node type {rest}"))
        }
        Some((ty, _)) => {
            let space = t.spaces.get(scope).ok_or_else(|| {
                format!("{entity:?}: {scope} is not a graph space (nodes are space:type/key)")
            })?;
            if !t.schemas[&space.schema].nodes.contains_key(ty) {
                return Err(format!("{entity:?}: unknown node type {ty}"));
            }
            space
                .instances
                .contains(rest)
                .then_some(())
                .ok_or_else(|| format!("{entity:?}: no such node in the graph data"))
        }
    }
}

fn check_anchors(t: &Tenant, report: &mut Report) {
    for (fixture, fp) in &t.pages {
        for anchor in &fp.page.front.about {
            let res = if anchor.contains(':') {
                check_entity(t, anchor).and_then(|()| {
                    let space = anchor.split_once(':').and_then(|(sp, rest)| {
                        rest.contains('/').then(|| &t.spaces[sp])
                    });
                    let visible = |space: &Space| match &fp.audience {
                        Aud::Tenant => space.visible_to(None, &[]),
                        Aud::Group(g) => space.visible_to(None, std::slice::from_ref(g)),
                        Aud::User(u) => space.visible_to(Some(u), &caller_groups(t, Some(u))),
                    };
                    match space {
                        Some(space) if !visible(space) => Err(format!(
                            "{anchor:?}: its space is not visible to everyone who can read this page"
                        )),
                        _ => Ok(()),
                    }
                })
            } else {
                match t.pages.get(anchor) {
                    None => Err(format!("{anchor:?}: no such page")),
                    Some(target) if !covers(t, &fp.audience, &target.audience) => Err(format!(
                        "{anchor:?}: not visible to everyone who can read this page"
                    )),
                    Some(_) => Ok(()),
                }
            };
            if let Err(e) = res {
                report.err(format!("{}/wiki/{fixture}.md: anchor {e}", t.file.id));
            }
        }
    }
}

/// Whether every reader of a page with audience `source` can also read one with `target`.
fn covers(t: &Tenant, source: &Aud, target: &Aud) -> bool {
    match (target, source) {
        (Aud::Tenant, _) => true,
        (Aud::Group(g), Aud::Group(sg)) => g == sg,
        (Aud::Group(g), Aud::User(u)) => caller_groups(t, Some(u)).contains(g),
        (Aud::User(u), Aud::User(su)) => u == su,
        _ => false,
    }
}

/// Lowercased title, aliases and keywords of a page: what keyword search can match on.
fn page_terms(page: &Page) -> Vec<String> {
    let mut terms: Vec<String> = page.front.aliases.clone();
    terms.extend(page.front.title.clone());
    if let Some(serde_norway::Value::Sequence(kw)) = page.front.extra.get("keywords") {
        terms.extend(kw.iter().filter_map(|v| v.as_str().map(str::to_owned)));
    }
    terms.into_iter().map(|t| t.to_lowercase()).collect()
}

const CATEGORIES: &[(&str, &str)] = &[
    ("experience", "M2"),
    ("expertise", "M2"),
    ("decision", "M2"),
    ("preference", "M2"),
    ("graph", "M3"),
    ("hop", "M5"),
    ("cross_lang", "M5"),
    ("negative", "M2"),
    ("isolation", "M2"),
    ("space_isolation", "M3"),
];
const SLOTS: &[&str] = &["memories", "knowledge", "relations"];

fn check_questions(
    tenants: &HashMap<String, Tenant>,
    report: &mut Report,
) -> BTreeMap<String, BTreeMap<String, usize>> {
    let categories: HashMap<&str, &str> = CATEGORIES.iter().copied().collect();
    let mut ids = HashSet::new();
    let mut referenced: HashSet<(String, String)> = HashSet::new();
    // category -> tenant -> count
    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut langs: BTreeMap<String, usize> = BTreeMap::new();

    for path in read_dir_sorted(&testset_dir().join("questions")) {
        let fname = path.file_name().unwrap().to_str().unwrap().to_owned();
        let qf: QuestionFile = serde_norway::from_str(&fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("questions/{fname}: {e}"));
        let Some(t) = tenants.get(&qf.tenant) else {
            report.err(format!("questions/{fname}: unknown tenant {}", qf.tenant));
            continue;
        };
        let users: HashSet<&str> = t.file.users.iter().map(|u| u.id.as_str()).collect();

        for q in &qf.questions {
            let w = format!("{} ({})", q.id, fname);
            let groups = caller_groups(t, q.user.as_deref());
            if !ids.insert(q.id.clone()) {
                report.err(format!("{w}: duplicate id"));
            }
            match categories.get(q.category.as_str()) {
                None => report.err(format!("{w}: unknown category {}", q.category)),
                Some(m) if *m != q.milestone => report.err(format!(
                    "{w}: category {} belongs to {m}, not {}",
                    q.category, q.milestone
                )),
                _ => {}
            }
            if !["en", "zh", "mixed"].contains(&q.lang.as_str()) {
                report.err(format!("{w}: unknown lang {}", q.lang));
            }
            if q.query.trim().is_empty() {
                report.err(format!("{w}: empty query"));
            }
            if q.keywords.iter().any(|k| k.trim().is_empty()) {
                report.err(format!("{w}: empty keyword"));
            }
            if let Some(u) = &q.user
                && !users.contains(u.as_str())
            {
                report.err(format!("{w}: unknown user {u}"));
            }
            *counts
                .entry(q.category.clone())
                .or_default()
                .entry(qf.tenant.clone())
                .or_default() += 1;
            *langs.entry(q.lang.clone()).or_default() += 1;

            let has_expectation = !q.expect.memories.is_empty()
                || !q.expect.knowledge.is_empty()
                || !q.expect.relations.is_empty();
            match q.category.as_str() {
                "negative" => {
                    if has_expectation || q.expect_empty.len() != SLOTS.len() {
                        report.err(format!("{w}: negative questions expect every slot empty"));
                    }
                }
                "isolation" => {
                    if q.must_not.is_empty() || q.user.is_none() {
                        report.err(format!("{w}: isolation needs a user and must_not"));
                    }
                }
                "space_isolation" => {
                    if q.must_not_spaces.is_empty() || !q.expect.relations.is_empty() {
                        report.err(format!(
                            "{w}: space_isolation needs must_not_spaces and expects no edges"
                        ));
                    }
                }
                _ if !has_expectation => report.err(format!("{w}: expects nothing")),
                _ => {}
            }
            for s in &q.expect_empty {
                if !SLOTS.contains(&s.as_str()) {
                    report.err(format!("{w}: unknown slot {s} in expect_empty"));
                }
            }

            let slot_pages = [
                ("memories", &q.expect.memories),
                ("knowledge", &q.expect.knowledge),
            ];
            for (slot, list) in slot_pages {
                for p in list {
                    let Some(fp) = t.pages.get(p) else {
                        report.err(format!("{w}: expected page {p} does not exist"));
                        continue;
                    };
                    referenced.insert((t.file.id.clone(), p.clone()));
                    let right_slot = match fp.kind {
                        Some(Kind::Experience | Kind::Decision | Kind::Preference) => "memories",
                        Some(Kind::Expertise | Kind::Graph) => "knowledge",
                        None => "none",
                    };
                    if right_slot != slot {
                        report.err(format!("{w}: {p} belongs in {right_slot}, not {slot}"));
                    }
                    if !page_visible(&fp.audience, q.user.as_deref(), &groups) {
                        report.err(format!("{w}: the caller can't see {p}"));
                    }
                }
            }
            // A cross-language question must not be answerable by plain keyword matching: its
            // query may not contain any alias, keyword or title of the page it expects.
            if q.category == "cross_lang" {
                let query = q.query.to_lowercase();
                for p in q.expect.memories.iter().chain(&q.expect.knowledge) {
                    let Some(fp) = t.pages.get(p) else { continue };
                    for term in page_terms(&fp.page) {
                        if term.chars().count() >= 2 && query.contains(&term) {
                            report.err(format!("{w}: query contains {term:?} from {p}"));
                        }
                    }
                }
            }
            for sp in &q.must_not_spaces {
                match t.spaces.get(sp) {
                    None => report.err(format!("{w}: unknown space {sp} in must_not_spaces")),
                    Some(space) if space.visible_to(q.user.as_deref(), &groups) => report.err(
                        format!("{w}: the caller can see {sp}, so it can't be hidden"),
                    ),
                    _ => {}
                }
            }
            for p in q.must_not.iter().chain(&q.trap) {
                if !t.pages.contains_key(p) {
                    report.err(format!("{w}: page {p} does not exist"));
                }
            }
            for p in &q.must_not {
                if let Some(fp) = t.pages.get(p)
                    && page_visible(&fp.audience, q.user.as_deref(), &groups)
                {
                    report.err(format!(
                        "{w}: the caller can see {p}, so it can't be hidden"
                    ));
                }
            }
            if !q.expect.relations.is_empty() && q.category != "graph" && q.category != "hop" {
                report.err(format!("{w}: only graph and hop questions expect edges"));
            }
            for e in &q.expect.relations {
                let parse = |end: &str| -> Option<(String, String)> {
                    let (space, rest) = end.split_once(':')?;
                    Some((space.to_owned(), rest.to_owned()))
                };
                let (Some((fs_, from)), Some((ts, to))) = (parse(&e.from), parse(&e.to)) else {
                    report.err(format!("{w}: edge ends must be space:type/key"));
                    continue;
                };
                if !from.contains('/') || !to.contains('/') {
                    report.err(format!("{w}: edge ends must be nodes, space:type/key"));
                    continue;
                }
                if fs_ != ts {
                    report.err(format!("{w}: edge crosses graph spaces"));
                    continue;
                }
                let Some(space) = t.spaces.get(&fs_) else {
                    report.err(format!("{w}: unknown graph space {fs_}"));
                    continue;
                };
                if !t.schemas[&space.schema].relations.contains_key(&e.rel) {
                    report.err(format!("{w}: unknown relation {}", e.rel));
                }
                if !space.visible_to(q.user.as_deref(), &groups) {
                    report.err(format!("{w}: the caller can't see space {fs_}"));
                }
                if !space.edges.contains(&format!("{from}|{}|{to}", e.rel)) {
                    report.err(format!("{w}: edge {from} {} {to} not in graph data", e.rel));
                }
            }
        }
    }

    // Every tenant-level page and every user page should be the answer to something.
    for t in tenants.values() {
        for (fixture, fp) in &t.pages {
            if fp.kind.is_some()
                && fp.kind != Some(Kind::Graph)
                && !referenced.contains(&(t.file.id.clone(), fixture.clone()))
            {
                report.err(format!(
                    "{}/wiki/{fixture}.md: no question expects it",
                    t.file.id
                ));
            }
        }
    }

    eprintln!("\nquestions by language: {langs:?}");
    counts
}

#[test]
fn testset_is_consistent() {
    let mut report = Report { errors: Vec::new() };
    let mut tenants = HashMap::new();
    for dir in read_dir_sorted(&testset_dir().join("tenants")) {
        if dir.is_dir() {
            let t = load_tenant(&dir, &mut report);
            check_anchors(&t, &mut report);
            tenants.insert(t.file.id.clone(), t);
        }
    }
    if tenants.len() < 6 {
        report.err(format!(
            "expected six scenario tenants, found {}",
            tenants.len()
        ));
    }
    let counts = check_questions(&tenants, &mut report);

    let tenant_names: BTreeSet<&String> = tenants.keys().collect();
    eprintln!(
        "\n{:<12}{}",
        "category",
        tenant_names
            .iter()
            .map(|t| format!("{t:>12}"))
            .collect::<String>()
            + "       total"
    );
    let mut grand = 0;
    for (cat, per) in &counts {
        let total: usize = per.values().sum();
        grand += total;
        let row: String = tenant_names
            .iter()
            .map(|t| format!("{:>12}", per.get(*t).copied().unwrap_or(0)))
            .collect();
        eprintln!("{cat:<12}{row}{total:>12}");
    }
    eprintln!(
        "{:<12}{grand:>width$}",
        "all",
        width = 12 * tenant_names.len() + 12
    );
    for (t, tenant) in &tenants {
        eprintln!(
            "{t}: {} ({} pages)",
            tenant.file.business,
            tenant.pages.len()
        );
    }

    for kind in ["experience", "expertise", "decision", "preference", "graph"] {
        let n: usize = counts.get(kind).map(|m| m.values().sum()).unwrap_or(0);
        if n < 30 {
            report.err(format!(
                "only {n} {kind} questions; the design asks for at least 30"
            ));
        }
    }

    if !report.errors.is_empty() {
        panic!(
            "{} problem(s) in testset/:\n  {}",
            report.errors.len(),
            report.errors.join("\n  ")
        );
    }
}
