//! The librarian: files new content into the wiki (design, "The librarian: how new content is
//! filed"). Every run is a job on the tenant's write queue:
//!
//! 1. receive: the raw record is already saved in `sources/` by the insert endpoint;
//! 2. find related pages, without an LLM: the one search entry, run as the new page's audience
//!    (so the librarian only ever sees what that audience may see);
//! 3. plan the edits (LLM): create, update a section, add anchors, supersede a claim;
//! 4. verify each edit (LLM, possibly a cheaper model); failed edits go back to step 3 once
//!    with the reasons, and are held as a proposal if they fail again;
//! 5. apply: code checks each edit (kind, folder, fields, anchors, merge rules) and writes ids,
//!    dates and `sources` itself;
//! 6. record: re-index, rewrite `index.md`, add one line to `log.md`.
//!
//! The librarian never writes graph data. Schema descriptions go through [`Librarian::describe`];
//! structural fields come from the stored definition, never from the LLM.

pub mod documents;
pub mod edit;
pub mod prompts;
mod reading;
pub mod sources;

pub use documents::{Document, DocumentMatch};
pub use edit::{Edit, PageAudience, Target};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::{LibrarianConfig, SearchConfig};
use crate::graph::{EntityRef, GraphSnapshot, SchemaDef, Visibility, describe};
use crate::identity::Scope;
use crate::jobs::{JobResult, Rejected};
use crate::kinds::Kind;
use crate::llm::{Llm, Prompt};
use crate::search::catalog::Catalog;
use crate::search::{SearchRequest, SearchSnapshot};
use crate::tenants::Tenant;
use crate::wiki::{Page, PagePath, WikiError, WikiFs};
use edit::{Draft, Rules};

/// Most characters of one related page shown to the librarian.
const MAX_PAGE_CHARS: usize = 12_000;
/// Most pages of a document's versions shown with each concept.
const MAX_DOC_PAGES: usize = 20;
/// Tries for one step whose answer can't be used (a reply that can't be read, a plan refused,
/// a read out of bounds): the first and three retries, each told why; then the step fails.
const ATTEMPTS: usize = 4;
/// Documents shown to the model when recognizing an upload, and what is shown of each.
const MATCH_CANDIDATES: usize = 3;
const MATCH_PAGES: usize = 5;
const MATCH_PAGE_CHARS: usize = 3_000;

/// What the librarian writes about one node or relation type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeText {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub en: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zh: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// The descriptive fields of a schema description, as the librarian writes them. Structure
/// is never taken from here: types the definition doesn't have are ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Description {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub nodes: BTreeMap<String, TypeText>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub relations: BTreeMap<String, TypeText>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalStatus {
    Held,
    Approved,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Held => "held",
            ProposalStatus::Approved => "approved",
        }
    }
}

/// An edit held for review, with why the verifier objected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeldEdit {
    pub edit: Edit,
    pub reason: String,
}

/// Edits held for review (design, "Filling gaps from outside, with approval": one queue for
/// held edits and, from M9, external candidates). Approving one applies its edits through the
/// same code checks; the verifier is not asked again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    #[serde(rename = "proposal")]
    pub id: String,
    pub status: ProposalStatus,
    pub created_at: String,
    pub kind: Kind,
    /// `tenant`, or the group or user folder the pages are for.
    pub audience: String,
    /// The raw record the edits were planned from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub edits: Vec<HeldEdit>,
    /// The writer's user and group folders, for checking anchors on approval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

/// A new proposal id: `p_`, the tenant folder's hash (so the admin route finds the tenant, as
/// API keys do), and a random part.
pub fn new_proposal_id(tenant_dir: &str) -> String {
    let hash = tenant_dir.strip_prefix("t_").unwrap_or(tenant_dir);
    format!("p_{hash}_{}", hex::encode(rand::random::<[u8; 8]>()))
}

/// The tenant folder a proposal id points to, if it is well-formed.
pub fn tenant_dir_of_proposal(id: &str) -> Option<String> {
    let (hash, rand) = id.strip_prefix("p_")?.split_once('_')?;
    let hex = |s: &str, n: usize| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    (hex(hash, 16) && hex(rand, 16)).then(|| format!("t_{hash}"))
}

/// One record to file.
pub struct Ingest {
    pub target: Target,
    /// Who sent it: whose spaces a user page may anchor to.
    pub writer: Scope,
    /// The record as received.
    pub record: serde_json::Value,
    /// Its file name in `sources/`.
    pub source: String,
    /// Text and terms for finding related pages.
    pub query: String,
    pub keywords: Vec<String>,
    pub entities: Vec<String>,
    /// For a document (expertise): its title and text, and its id when the caller sent one.
    pub document: Option<DocumentIn>,
}

/// A document being filed.
pub struct DocumentIn {
    /// The document the caller named by its id, already checked.
    pub given: Option<Document>,
    pub title: String,
    pub content: String,
}

/// What the first read of an upload says it is, to recognize it by: the main thread, the first
/// concept's title and terms, and the lines that read took.
struct Opening {
    thread: String,
    terms: Vec<String>,
    text: String,
}

/// One plan's worth of content: a whole record, or one concept of a document with its lines.
struct Unit {
    /// The record as the librarian is shown it; for a concept, its lines.
    record: String,
    /// What to search related pages with.
    queries: Vec<String>,
    keywords: Vec<String>,
    /// What the librarian should know about the document: its main thread, the ledger, and
    /// for a new version what changed.
    note: Option<String>,
    /// What this one call is for: one concept of the document.
    focus: Option<String>,
    /// Pages shown first: the concept's page, and the pages an earlier version filed its
    /// lines into.
    first_pages: Vec<String>,
}

/// Pages a search found, best first, with their kind and score, and the entities it named.
struct Found {
    pages: Vec<(String, Kind, f32)>,
    entities: Vec<String>,
}

/// Why a schema description couldn't be written.
#[derive(Debug, thiserror::Error)]
pub enum DescribeError {
    #[error("the librarian's model could not be reached: {0:#}")]
    Llm(anyhow::Error),
    #[error("the librarian's description was rejected twice: {0}")]
    Rejected(String),
}

pub struct Librarian {
    llm: Llm,
    cfg: LibrarianConfig,
    search: SearchConfig,
}

/// The pages related to new content, and the entities its search named.
struct Related {
    pages: Vec<Page>,
    entities: Vec<String>,
}

/// One plan or edit checked by code: per edit, why it was refused, if it was; the pages as
/// they would be written; and, for section updates, the section as it stands.
struct Checked {
    results: Vec<Result<(), String>>,
    pages: Vec<(Page, bool)>,
    before: Vec<Option<String>>,
}

impl Librarian {
    pub fn new(llm: Llm, cfg: LibrarianConfig, search: SearchConfig) -> Self {
        Self { llm, cfg, search }
    }

    fn prompt(&self, system: &str, user: String, schema: serde_json::Value) -> Prompt {
        Prompt {
            system: system.to_owned(),
            user,
            max_tokens: self.cfg.max_tokens,
            schema,
        }
    }

    /// Files one record: steps 2 to 6. The record was saved before the job was queued.
    ///
    /// A document (expertise) is read like a person reads it, one concept at a time, each
    /// concept filed as it is read (see `reading`); a record is the one-step case.
    #[tracing::instrument(name = "librarian_ingest", skip_all, fields(tenant = %tenant.dir_name, kind = job.target.kind.as_str()))]
    pub async fn ingest(&self, tenant: Arc<Tenant>, job: Ingest) -> anyhow::Result<JobResult> {
        let target = &job.target;
        let reader = reader_scope(&tenant, &target.audience, &job.writer);
        let rules = prompts::rules(&tenant.dir, target.kind);
        let today = today();
        let mut result = JobResult {
            source: Some(format!("sources/{}", job.source)),
            ..JobResult::default()
        };
        if let Some(d) = &job.document {
            self.read_document(&tenant, &job, d, &reader, &rules, &today, &mut result)
                .await?;
            dedup_pages(&mut result);
            return Ok(result);
        }

        let unit = Unit {
            record: serde_json::to_string_pretty(&job.record)?,
            queries: vec![job.query.clone()],
            keywords: job.keywords.clone(),
            note: None,
            focus: None,
            first_pages: Vec::new(),
        };
        let (pass, held) = self
            .plan_unit(
                &tenant,
                &job,
                &reader,
                &rules,
                &today,
                &unit,
                std::slice::from_ref(&job.source),
                &job.source,
                &mut result,
            )
            .await?;
        let _writer = tenant.wiki_writer.lock().await;
        apply_and_record(
            &tenant,
            target,
            &job.writer,
            Some(&job.source),
            &today,
            &pass,
            &mut result,
            &format!("ingest {}", target.kind.as_str()),
        )
        .await?;
        if !held.is_empty() {
            let id = hold(&tenant, target, &job.writer, Some(&job.source), held).await?;
            record_log(
                &tenant,
                &format!("ingest {}: held as {id}", target.kind.as_str()),
            )
            .await?;
            result.proposal = Some(id);
        }
        Ok(result)
    }

    /// Steps 2 to 4 for one unit (the whole record, or one concept of a document): related pages,
    /// the plan, the code checks and the verdicts, with one retry. `cite` is what the pages
    /// cite for it (the record, or the concept's lines in it). Returns the edits to apply and
    /// the edits to hold; code-refused edits go into `result.rejected`.
    #[allow(clippy::too_many_arguments)]
    async fn plan_unit(
        &self,
        tenant: &Arc<Tenant>,
        job: &Ingest,
        reader: &Scope,
        rules: &str,
        today: &str,
        unit: &Unit,
        doc_sources: &[String],
        cite: &str,
        result: &mut JobResult,
    ) -> anyhow::Result<(Vec<Edit>, Vec<HeldEdit>)> {
        let target = &job.target;
        let related = self
            .related(
                tenant,
                target,
                reader,
                &unit.queries,
                &unit.keywords,
                &job.entities,
                doc_sources,
                &unit.first_pages,
            )
            .await?;
        let snap = tenant.snapshot().await?;
        // Node anchors are checked against the graph: without it the job fails rather than
        // having every node anchor refused.
        let graph = tenant.graph().await.context("anchors need the graph")?;
        let anchors = anchor_options(
            target,
            &job.writer,
            &related,
            &job.entities,
            &snap.catalog,
            &graph,
        );
        let mut feedback: Vec<String> = Vec::new();
        let mut last = None;
        for attempt in 1..=ATTEMPTS {
            let user = plan_message(
                rules,
                target,
                today,
                &unit.record,
                unit.note.as_deref(),
                unit.focus.as_deref(),
                &related.pages,
                &anchors,
                &feedback,
            );
            let reply = self
                .llm
                .complete(&self.prompt(prompts::INGEST, user, plan_schema()))
                .await
                .context("planning the edits")?;
            let edits = match parse_plan(&reply) {
                Ok(e) => e,
                Err(e) if attempt < ATTEMPTS => {
                    feedback = vec![format!(
                        "Your reply was not one JSON object of the required shape: {e}"
                    )];
                    continue;
                }
                Err(e) => {
                    anyhow::bail!("the librarian's plan could not be read in {ATTEMPTS} tries: {e}")
                }
            };
            let checked =
                check(tenant, target, &job.writer, Some(cite), today, &edits, true).await?;
            let verdicts = self
                .verify(
                    rules,
                    target,
                    &unit.record,
                    unit.note.as_deref(),
                    &related.pages,
                    &edits,
                    &checked,
                )
                .await?;
            let failures: Vec<String> = edits
                .iter()
                .enumerate()
                .filter_map(|(i, e)| {
                    let reason = match (&checked.results[i], verdicts.get(&i)) {
                        (Err(r), _) | (Ok(()), Some(Err(r))) => r,
                        _ => return None,
                    };
                    Some(format!(
                        "- edit {i} ({} {}): {reason}",
                        op_name(e),
                        e.page()
                    ))
                })
                .collect();
            let done = failures.is_empty() || attempt == ATTEMPTS;
            last = Some((edits, checked, verdicts));
            if done {
                break;
            }
            tracing::info!(reasons = ?failures, "plan rejected; asking again");
            feedback = failures;
        }
        let (edits, checked, verdicts) = last.context("no plan")?;
        let mut pass = Vec::new();
        let mut held = Vec::new();
        let hold_it = |edit: Edit, reason: String| HeldEdit { edit, reason };
        for (i, e) in edits.into_iter().enumerate() {
            match (&checked.results[i], verdicts.get(&i)) {
                (Err(r), _) => result.rejected.push(Rejected {
                    page: e.page(),
                    reason: r.clone(),
                }),
                (Ok(()), Some(Err(r))) => held.push(hold_it(e, r.clone())),
                (Ok(()), _) => pass.push(e),
            }
        }
        // An edit of, linking to or anchored to a page whose creation is held would fail
        // without it: it is held with that page, and so on for edits that depend on it.
        loop {
            let held_pages: HashSet<String> = held
                .iter()
                .filter(|h| matches!(h.edit, Edit::Create { .. }))
                .map(|h| page_id(&h.edit.page()))
                .collect();
            let Some(i) = pass.iter().position(|e| {
                held_pages.contains(&page_id(&e.page()))
                    || added_links(e)
                        .iter()
                        .any(|l| held_pages.contains(&page_id(l)))
            }) else {
                break;
            };
            let e = pass.remove(i);
            let reason = "it depends on a page whose creation is held for review".to_owned();
            held.push(hold_it(e, reason));
        }
        Ok((pass, held))
    }

    /// Which document a new upload is, when the caller didn't say, from what its first read
    /// says it is (`opening`): the documents whose pages its title, main thread and first terms
    /// find (scores of a document's pages added up), the best three shown to the model as the
    /// wiki pages filed from them, and the model says which one this is a new version of, or
    /// none (`new`). Only documents of the same audience are candidates.
    async fn identify(
        &self,
        tenant: &Arc<Tenant>,
        reader: &Scope,
        title: &str,
        new: Document,
        opening: &Opening,
    ) -> anyhow::Result<(Document, DocumentMatch)> {
        let filed: Vec<(String, Document)> = tenant
            .store
            .documents_by_source()
            .await?
            .into_iter()
            .filter(|(_, doc)| doc.audience == new.audience)
            .collect();
        if filed.is_empty() {
            return Ok((new, DocumentMatch::New));
        }
        let doc_of: HashMap<&str, &Document> = filed.iter().map(|(s, d)| (s.as_str(), d)).collect();
        let snap = tenant.snapshot().await?;
        let graph = tenant.graph_or_empty().await;
        let queries: Vec<String> = [
            title.to_owned(),
            opening.thread.clone(),
            opening.terms.join(" "),
        ]
        .into_iter()
        .filter(|q| !q.trim().is_empty())
        .collect();
        let mut keywords = vec![title.to_owned()];
        keywords.extend(opening.terms.iter().cloned());
        let found = self.find(&snap, &graph, reader, &queries, &keywords, &[])?;
        let mut scores: HashMap<&str, f32> = HashMap::new();
        for (page, _, score) in &found.pages {
            let Some((_, entry)) = snap.catalog.get(page) else {
                continue;
            };
            let docs: HashSet<&str> = entry
                .sources
                .iter()
                .filter_map(|s| doc_of.get(documents::record_name(s)).map(|d| d.id.as_str()))
                .collect();
            for id in docs {
                *scores.entry(id).or_default() += score;
            }
        }
        let mut ranked: Vec<(&str, f32)> = scores.into_iter().collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(b.0)));
        ranked.truncate(MATCH_CANDIDATES);
        if ranked.is_empty() {
            return Ok((new, DocumentMatch::New));
        }

        // The candidates as the wiki shows them: the pages filed from each.
        let mut msg = format!(
            "## New document\n\nTitle: {title}\n\nMain thread: {}\n\nIts opening, as first \
             read:\n\n```text\n{}\n```\n\n## Candidates\n\n",
            opening.thread, opening.text
        );
        let wiki = tenant.wiki();
        let mut ids = Vec::new();
        for (id, _) in &ranked {
            let sources: HashSet<&str> = filed
                .iter()
                .filter(|(_, doc)| doc.id == *id)
                .map(|(s, _)| s.as_str())
                .collect();
            let title = filed
                .iter()
                .find(|(_, doc)| doc.id == *id)
                .map(|(_, doc)| doc.title.clone())
                .unwrap_or_default();
            let paths: Vec<PagePath> = snap
                .catalog
                .pages
                .iter()
                .filter(|p| reader.can_see(&p.path))
                .filter(|p| {
                    p.sources
                        .iter()
                        .any(|s| sources.contains(documents::record_name(s)))
                })
                .map(|p| p.path.clone())
                .take(MATCH_PAGES)
                .collect();
            let wiki = wiki.clone();
            let pages = tokio::task::spawn_blocking(move || {
                paths
                    .iter()
                    .filter_map(|p| wiki.read(p).ok())
                    .collect::<Vec<_>>()
            })
            .await?;
            let _ = write!(
                msg,
                "### Document {id}: {title}\n\nWiki pages filed from it:\n\n"
            );
            for page in pages {
                let _ = write!(
                    msg,
                    "#### {}\n\n```markdown\n{}\n```\n\n",
                    page.path,
                    cut(&page.render(), MATCH_PAGE_CHARS)
                );
            }
            ids.push((*id).to_owned());
        }
        let prompt = self.prompt(prompts::MATCH, msg, match_schema(&ids));
        let mut answer = Err(String::new());
        for _ in 0..ATTEMPTS {
            let reply = self
                .llm
                .complete(&prompt)
                .await
                .context("recognizing the document")?;
            answer = parse_json::<MatchReply>(&reply);
            if answer.is_ok() {
                break;
            }
        }
        let answer =
            answer.map_err(|e| anyhow::anyhow!("the librarian's answer could not be read: {e}"))?;
        match answer.document.filter(|id| ids.contains(id)) {
            Some(id) => {
                let mut doc = filed
                    .iter()
                    .find(|(_, d)| d.id == id)
                    .map(|(_, d)| d.clone())
                    .context("a candidate")?;
                doc.title = title.to_owned();
                Ok((doc, DocumentMatch::Matched))
            }
            None => Ok((new, DocumentMatch::New)),
        }
    }

    /// Searches with each query and adds up how each page ranks (1 for first, ½ for second,
    /// …), so pages found for many parts of a text come first. Also the entities named.
    fn find(
        &self,
        snap: &SearchSnapshot,
        graph: &GraphSnapshot,
        reader: &Scope,
        queries: &[String],
        keywords: &[String],
        entities: &[String],
    ) -> anyhow::Result<Found> {
        let short = |list: &[String]| -> Vec<String> {
            list.iter()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty() && s.chars().count() <= 100)
                .take(20)
                .collect()
        };
        let mut scores: HashMap<String, (Kind, f32)> = HashMap::new();
        let mut named: Vec<String> = Vec::new();
        for q in queries.iter().filter(|q| !q.trim().is_empty()) {
            let req: SearchRequest = serde_json::from_value(json!({
                "query": q,
                "keywords": short(keywords),
                "entities": short(entities),
                "slots": { "memories": 20, "knowledge": 20, "relations": 0 },
                "hop": false,
            }))?;
            let resp = crate::search::search(
                snap,
                graph,
                reader,
                &req,
                &self.search,
                &Default::default(),
            )?;
            for slot in [&resp.memories.hits, &resp.knowledge.hits] {
                for (rank, h) in slot.iter().enumerate() {
                    let e = scores.entry(h.page.clone()).or_insert((h.kind, 0.0));
                    e.1 += 1.0 / (rank as f32 + 1.0);
                }
            }
            for e in resp.entities {
                if !named.contains(&e) {
                    named.push(e);
                }
            }
        }
        let mut pages: Vec<(String, Kind, f32)> =
            scores.into_iter().map(|(p, (k, s))| (p, k, s)).collect();
        pages.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        Ok(Found {
            pages,
            entities: named,
        })
    }

    /// Step 2: the pages the librarian is shown. First `first_pages` (a concept's page, and
    /// where an earlier version filed its lines) and the pages that cite `first_sources` (a
    /// document's earlier versions and this one's earlier concepts), up to a cap; then the
    /// best related pages the audience may see, pages of the job's kind first.
    #[allow(clippy::too_many_arguments)]
    async fn related(
        &self,
        tenant: &Arc<Tenant>,
        target: &Target,
        reader: &Scope,
        queries: &[String],
        keywords: &[String],
        entities: &[String],
        first_sources: &[String],
        first_pages: &[String],
    ) -> anyhow::Result<Related> {
        let snap = tenant.snapshot().await?;
        let graph = tenant.graph_or_empty().await;
        let queries = if queries.iter().all(|q| q.trim().is_empty()) {
            vec![target.kind.as_str().to_owned()]
        } else {
            queries.to_vec()
        };
        let found = self.find(&snap, &graph, reader, &queries, keywords, entities)?;
        let mut first: Vec<String> = Vec::new();
        // The one page a preference job writes always comes first when it exists.
        if target.kind == Kind::Preference && snap.catalog.get(&target.folder()).is_some() {
            first.push(target.folder());
        }
        first.extend(
            first_pages
                .iter()
                .filter(|p| {
                    snap.catalog
                        .get(p)
                        .is_some_and(|(_, e)| reader.can_see(&e.path))
                })
                .cloned(),
        );
        first.extend(
            snap.catalog
                .pages
                .iter()
                .filter(|p| reader.can_see(&p.path))
                .filter(|p| {
                    p.sources
                        .iter()
                        .any(|s| first_sources.iter().any(|f| f == documents::record_name(s)))
                })
                .map(|p| p.path.to_string())
                .filter(|p| !first_pages.contains(p))
                .take(MAX_DOC_PAGES),
        );
        let mut others: Vec<(bool, &String)> = found
            .pages
            .iter()
            .map(|(p, k, _)| (*k != target.kind, p))
            .collect();
        others.sort_by_key(|(other_kind, _)| *other_kind);
        let mut seen: HashSet<String> = first.iter().cloned().collect();
        let mut ids = first;
        ids.extend(
            others
                .into_iter()
                .map(|(_, p)| p.clone())
                .filter(|p| seen.insert(p.clone()))
                .take(self.cfg.related_pages),
        );
        let paths: Vec<PagePath> = ids.iter().filter_map(|p| PagePath::parse(p).ok()).collect();
        let wiki = tenant.wiki();
        let pages = tokio::task::spawn_blocking(move || {
            paths
                .iter()
                .filter_map(|p| wiki.read(p).ok())
                .collect::<Vec<_>>()
        })
        .await?;
        tracing::debug!(related = pages.len(), "related pages found");
        Ok(Related {
            pages,
            entities: found.entities,
        })
    }

    /// Step 4: a verdict for each edit that passed the code checks. Edits that failed them get
    /// no verdict. A reply that can't be read is asked for once more.
    #[allow(clippy::too_many_arguments)]
    async fn verify(
        &self,
        rules: &str,
        target: &Target,
        record: &str,
        note: Option<&str>,
        related: &[Page],
        edits: &[Edit],
        checked: &Checked,
    ) -> anyhow::Result<HashMap<usize, Result<(), String>>> {
        let todo: Vec<usize> = (0..edits.len())
            .filter(|&i| checked.results[i].is_ok())
            .collect();
        if todo.is_empty() {
            return Ok(HashMap::new());
        }
        let mut msg = format!(
            "## Rules for {} pages\n\n{rules}\n\n## New content\n\n```json\n{record}\n```\n\n",
            target.kind.as_str()
        );
        if let Some(note) = note {
            // The document's main thread and ledger: links to the ledger's pages are expected.
            let _ = write!(msg, "## This document\n\n{note}\n\n");
        }
        push_pages(&mut msg, "Related pages, as they stand", related);
        msg.push_str("## Edits to check\n\n");
        for &i in &todo {
            let e = &edits[i];
            let _ = write!(
                msg,
                "### Edit {i}\n\n```json\n{}\n```\n\n",
                serde_json::to_string_pretty(e)?
            );
            if let Some(before) = &checked.before[i] {
                let _ = write!(
                    msg,
                    "The section this edit replaces:\n\n```markdown\n{before}\n```\n\n"
                );
            }
        }
        let _ = writeln!(
            msg,
            "Give one verdict for each edit above, numbered as shown ({}).",
            todo.iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let verdicts = self.ask_verdicts(msg).await?;
        Ok(todo
            .into_iter()
            .map(|i| {
                let v = match verdicts.iter().find(|v| v.edit == i) {
                    Some(v) if v.ok => Ok(()),
                    Some(v) => Err(v
                        .reason
                        .clone()
                        .filter(|r| !r.trim().is_empty())
                        .unwrap_or_else(|| "the verifier objected".into())),
                    None => Err("the verifier gave no verdict".into()),
                };
                (i, v)
            })
            .collect())
    }

    async fn ask_verdicts(&self, msg: String) -> anyhow::Result<Vec<Verdict>> {
        let prompt = self.prompt(prompts::VERIFY, msg, verdicts_schema());
        let mut last_err = None;
        for _ in 0..ATTEMPTS {
            let reply = self
                .llm
                .complete(&prompt)
                .await
                .context("verifying the edits")?;
            match parse_json::<Verdicts>(&reply) {
                Ok(v) => return Ok(v.verdicts),
                Err(e) => last_err = Some(e),
            }
        }
        anyhow::bail!(
            "the verifier's reply could not be read: {}",
            last_err.unwrap_or_default()
        )
    }

    /// Writes a schema's description (design, "The business graph"): the librarian writes the
    /// prose, code the structure from the definition. There is no fallback: when the model
    /// can't be reached, or the description is rejected in every try, this fails and the
    /// caller stores nothing.
    #[tracing::instrument(name = "librarian_describe", skip_all, fields(schema = %def.schema))]
    pub async fn describe(
        &self,
        tenant_dir: &std::path::Path,
        def: &SchemaDef,
        current: Option<&Page>,
    ) -> Result<Page, DescribeError> {
        let rules = prompts::rules(tenant_dir, Kind::Graph);
        let today = today();
        let def_json =
            serde_json::to_string_pretty(def).map_err(|e| DescribeError::Llm(e.into()))?;
        let mut msg = format!(
            "## Rules for schema descriptions\n\n{rules}\n\n## Schema definition\n\n```json\n{def_json}\n```\n\n"
        );
        match current {
            Some(page) => {
                let _ = write!(
                    msg,
                    "## Current description page\n\n```markdown\n{}\n```\n\n",
                    page.render()
                );
            }
            None => msg.push_str("## Current description page\n\nNone yet.\n\n"),
        }
        let schema = description_schema(def);
        let mut feedback = String::new();
        let mut reason = String::new();
        for _ in 0..ATTEMPTS {
            let prompt = self.prompt(
                prompts::DESCRIBE,
                format!("{msg}{feedback}"),
                schema.clone(),
            );
            let reply = self
                .llm
                .complete(&prompt)
                .await
                .map_err(DescribeError::Llm)?;
            reason = match parse_json::<Description>(&reply) {
                Err(e) => format!("the reply was not a description: {e}"),
                Ok(desc) => match described_page(def, &desc, current, &today) {
                    Err(r) => r,
                    Ok(page) => {
                        let check = format!(
                            "## Schema definition\n\n```json\n{def_json}\n```\n\n## Description written\n\n```markdown\n{}\n```\n",
                            page.render()
                        );
                        match self
                            .ask_ok(prompts::VERIFY_DESCRIBE, check)
                            .await
                            .map_err(DescribeError::Llm)?
                        {
                            Ok(()) => return Ok(page),
                            Err(r) => r,
                        }
                    }
                },
            };
            tracing::info!(%reason, "description rejected");
            feedback = format!("## Why the last description was rejected\n\n{reason}\n");
        }
        Err(DescribeError::Rejected(reason))
    }

    async fn ask_ok(&self, system: &str, msg: String) -> anyhow::Result<Result<(), String>> {
        let prompt = self.prompt(system, msg, verdict_schema());
        let mut last_err = String::new();
        for _ in 0..ATTEMPTS {
            let reply = self.llm.complete(&prompt).await?;
            match parse_json::<Verdict>(&reply) {
                Ok(v) if v.ok => return Ok(Ok(())),
                Ok(v) => {
                    return Ok(Err(v
                        .reason
                        .unwrap_or_else(|| "the verifier objected".into())));
                }
                Err(e) => last_err = e,
            }
        }
        anyhow::bail!("the verifier's reply could not be read: {last_err}")
    }

    /// A hand edit through `PUT /pages` (design, "Manual edits get the same check"): verified,
    /// then saved, or held as a proposal with the verifier's reasons.
    #[tracing::instrument(name = "librarian_review", skip_all, fields(tenant = %tenant.dir_name))]
    pub async fn review(
        &self,
        tenant: Arc<Tenant>,
        writer: Scope,
        target: Target,
        edit: Edit,
    ) -> anyhow::Result<JobResult> {
        let today = today();
        let checked = check(
            &tenant,
            &target,
            &writer,
            None,
            &today,
            std::slice::from_ref(&edit),
            false,
        )
        .await?;
        let mut result = JobResult::default();
        if let Err(reason) = &checked.results[0] {
            result.rejected.push(Rejected {
                page: edit.page(),
                reason: reason.clone(),
            });
            return Ok(result);
        }
        let (title, summary) = match &edit {
            Edit::Replace { front_matter, .. } => (
                front_matter.title.clone().unwrap_or_default(),
                front_matter.summary.clone().unwrap_or_default(),
            ),
            _ => Default::default(),
        };
        let reader = reader_scope(&tenant, &target.audience, &writer);
        let queries = vec![format!("{title}\n{summary}")];
        let mut related = self
            .related(&tenant, &target, &reader, &queries, &[], &[], &[], &[])
            .await?;
        // The page itself shows as it stands, first.
        let own = edit.page();
        related.pages.retain(|p| p.path.as_str() != own);
        let wiki = tenant.wiki();
        if let Ok(path) = PagePath::parse(&own)
            && let Ok(Ok(page)) = tokio::task::spawn_blocking(move || wiki.read(&path)).await
        {
            related.pages.insert(0, page);
        }
        let rules = prompts::rules(&tenant.dir, target.kind);
        let record = serde_json::to_string_pretty(&json!({
            "hand_edit": "A person edited this page by hand; the edit below is the whole new page. Check it against the pages shown."
        }))?;
        let verdicts = self
            .verify(
                &rules,
                &target,
                &record,
                None,
                &related.pages,
                std::slice::from_ref(&edit),
                &checked,
            )
            .await?;
        let _writer = tenant.wiki_writer.lock().await;
        match verdicts.get(&0) {
            Some(Err(reason)) => {
                result.proposal = Some(
                    hold(
                        &tenant,
                        &target,
                        &writer,
                        None,
                        vec![HeldEdit {
                            edit,
                            reason: reason.clone(),
                        }],
                    )
                    .await?,
                );
                record_log(
                    &tenant,
                    &format!("hand edit of {}: held", log_page(&target, &own)),
                )
                .await?;
            }
            _ => {
                apply_and_record(
                    &tenant,
                    &target,
                    &writer,
                    None,
                    &today,
                    &[edit],
                    &mut result,
                    "hand edit",
                )
                .await?;
            }
        }
        Ok(result)
    }
}

/// The code checks for one edit on its own, as a handler runs them before queueing a hand
/// edit: `Err` says why the edit can't be saved.
pub async fn check_edit(
    tenant: &Arc<Tenant>,
    target: &Target,
    writer: &Scope,
    edit: &Edit,
) -> anyhow::Result<Result<(), String>> {
    let checked = check(
        tenant,
        target,
        writer,
        None,
        &today(),
        std::slice::from_ref(edit),
        false,
    )
    .await?;
    Ok(checked.results.into_iter().next().unwrap_or(Ok(())))
}

/// How an approval that would write nothing fails; the proposal stays held.
pub const NO_LONGER_APPLIES: &str = "none of the proposal's edits applies to the wiki as it is now";

/// Applies an approved proposal's edits through the code checks, then marks it approved. No
/// LLM is involved, so approving works without a librarian. When none of them applies any
/// more, it fails with `NO_LONGER_APPLIES` and the proposal stays held.
#[tracing::instrument(name = "approve_proposal", skip_all, fields(tenant = %tenant.dir_name))]
pub async fn approve(tenant: Arc<Tenant>, mut proposal: Proposal) -> anyhow::Result<JobResult> {
    let today = today();
    let mut result = JobResult {
        source: proposal.source.as_ref().map(|s| format!("sources/{s}")),
        ..JobResult::default()
    };
    let _writer = tenant.wiki_writer.lock().await;
    // Approvals of one proposal can race; the status is checked again under the writer lock.
    let stored = tenant.store.proposal(&proposal.id).await?;
    anyhow::ensure!(
        stored.is_some_and(|p| p.status == ProposalStatus::Held),
        "proposal {} is no longer held",
        proposal.id
    );
    let target = Target::new(proposal.kind, PageAudience::from_key(&proposal.audience))
        .map_err(anyhow::Error::msg)?;
    let writer = Scope::offline(
        tenant.clone(),
        proposal.user.clone(),
        proposal.groups.clone(),
    );
    let edits: Vec<Edit> = proposal.edits.iter().map(|h| h.edit.clone()).collect();
    let wrote = apply_and_record(
        &tenant,
        &target,
        &writer,
        proposal.source.as_deref(),
        &today,
        &edits,
        &mut result,
        &format!("approve {}", proposal.id),
    )
    .await?;
    // Nothing applies any more (the pages changed or went away): the proposal stays held, so
    // it can be rejected or redone, and the caller learns why.
    if !wrote {
        let reasons: Vec<String> = result
            .rejected
            .iter()
            .map(|r| format!("{}: {}", r.page, r.reason))
            .collect();
        anyhow::bail!("{NO_LONGER_APPLIES}: {}", reasons.join("; "));
    }
    proposal.status = ProposalStatus::Approved;
    tenant.store.put_proposal(&proposal).await?;
    tenant
        .store
        .audit("admin", "proposal.approve", &proposal.id)
        .await?;
    Ok(result)
}

/// The scope that sees what a page's audience may see: the tenant's pages, plus the group's
/// for a group page, plus the writer's own (user and groups) for a user page.
fn reader_scope(tenant: &Arc<Tenant>, audience: &PageAudience, writer: &Scope) -> Scope {
    match audience {
        PageAudience::Tenant => Scope::offline(tenant.clone(), None, Vec::new()),
        PageAudience::Group(g) => Scope::offline(tenant.clone(), None, vec![g.clone()]),
        PageAudience::User(_) => writer.clone(),
    }
}

fn today() -> String {
    crate::trace::now_rfc3339()[..10].to_owned()
}

fn op_name(e: &Edit) -> &'static str {
    match e {
        Edit::Create { .. } => "create",
        Edit::Update { .. } => "update",
        Edit::Supersede { .. } => "supersede",
        Edit::Replace { .. } => "replace",
    }
}

/// A path a new page of the target would have, for checking anchors before a page exists.
fn sample_path(target: &Target) -> Option<PagePath> {
    let folder = target.folder();
    PagePath::parse(&folder)
        .or_else(|_| PagePath::parse(&format!("{folder}/new-page")))
        .ok()
}

/// Whether `page` may anchor to `anchor`: a page that exists (or this plan creates) and that
/// everyone who can read `page` can read too; a node type that exists; or a node in a space
/// everyone who can read `page` can see (shared spaces, or for a user's own page, the spaces
/// that user sees). Unknown and hidden are reported alike.
fn check_anchor(
    anchor: &str,
    page: &PagePath,
    created: &HashSet<PagePath>,
    catalog: &Catalog,
    graph: &GraphSnapshot,
    writer: &Scope,
) -> Result<(), String> {
    let readers = PageAudience::of(page);
    if let Ok(target) = PagePath::parse(anchor) {
        let exists = target.kind().is_some()
            && (catalog.get(target.as_str()).is_some() || created.contains(&target));
        if &target == page {
            return Err("a page can't anchor to itself".into());
        }
        if !exists {
            return Err("no such page".into());
        }
        let ok = match PageAudience::of(&target) {
            PageAudience::Tenant => true,
            PageAudience::Group(g) => match &readers {
                PageAudience::Group(own) => *own == g,
                PageAudience::User(_) => writer.groups().iter().any(|x| x.dir == g),
                PageAudience::Tenant => false,
            },
            PageAudience::User(u) => readers == PageAudience::User(u),
        };
        return if ok {
            Ok(())
        } else {
            Err("not every reader of this page can see that page".into())
        };
    }
    match EntityRef::parse(anchor) {
        Some(EntityRef::Type { schema, ty }) => {
            if graph.space_index(schema).is_none() && graph.has_node_type(schema, ty) {
                Ok(())
            } else {
                Err("no such node type".into())
            }
        }
        Some(EntityRef::Node { space, .. }) => {
            let entry = graph
                .node(anchor)
                .and_then(|_| graph.space(space))
                .ok_or_else(|| "no such node".to_owned())?;
            let ok = match readers {
                PageAudience::User(_) => entry.visible_to(writer),
                _ => entry.def.visibility == Visibility::Shared,
            };
            if ok {
                Ok(())
            } else {
                Err("no such node".into())
            }
        }
        None => Err("an anchor is a page id, schema:type or space:type/key".into()),
    }
}

/// The anchors the librarian may use, as prompt lines: related pages, node types, and the
/// nodes the content names, each only when the job's pages may anchor to it.
fn anchor_options(
    target: &Target,
    writer: &Scope,
    related: &Related,
    entities: &[String],
    catalog: &Catalog,
    graph: &GraphSnapshot,
) -> Vec<String> {
    let Some(sample) = sample_path(target) else {
        return Vec::new();
    };
    let none = HashSet::new();
    let ok = |a: &str| check_anchor(a, &sample, &none, catalog, graph, writer).is_ok();
    let pages: Vec<&str> = related
        .pages
        .iter()
        .map(|p| p.path.as_str())
        .filter(|p| ok(p))
        .collect();
    let types: Vec<String> = graph
        .schemas
        .values()
        .flat_map(|s| s.nodes.keys().map(move |t| format!("{}:{t}", s.schema)))
        .collect();
    let mut nodes: Vec<&str> = Vec::new();
    for e in related.entities.iter().chain(entities) {
        if matches!(EntityRef::parse(e), Some(EntityRef::Node { .. }))
            && ok(e)
            && !nodes.contains(&e.as_str())
        {
            nodes.push(e);
        }
    }
    let list = |v: Vec<&str>| {
        if v.is_empty() {
            "none".to_owned()
        } else {
            v.join(", ")
        }
    };
    vec![
        format!("- Pages: {}", list(pages)),
        format!(
            "- Node types: {}",
            list(types.iter().map(String::as_str).collect())
        ),
        format!("- Nodes: {}", list(nodes)),
    ]
}

fn push_pages(msg: &mut String, title: &str, pages: &[Page]) {
    let _ = write!(msg, "## {title}\n\n");
    if pages.is_empty() {
        msg.push_str("None: no page in the wiki is related yet.\n\n");
    }
    for page in pages {
        let mut text = page.render();
        if text.chars().count() > MAX_PAGE_CHARS {
            text = text.chars().take(MAX_PAGE_CHARS).collect();
            text.push_str("\n[… the rest of the page is left out]");
        }
        let _ = write!(msg, "### {}\n\n```markdown\n{text}\n```\n\n", page.path);
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_message(
    rules: &str,
    target: &Target,
    today: &str,
    record: &str,
    note: Option<&str>,
    focus: Option<&str>,
    related: &[Page],
    anchors: &[String],
    feedback: &[String],
) -> String {
    let mut msg = format!(
        "## Rules for {} pages\n\n{rules}\n\n## Where to write\n\n",
        target.kind.as_str()
    );
    if target.kind == Kind::Preference {
        let _ = writeln!(
            msg,
            "The only page this job may write is `{}` (create it if it doesn't exist yet).",
            target.folder()
        );
    } else {
        let _ = writeln!(
            msg,
            "Pages of this job are in `{0}/`: a new page's id is `{0}/<name>`. Only pages in \
             that folder may be created or changed.",
            target.folder()
        );
    }
    let _ = write!(
        msg,
        "Today is {today}.\n\n## New content\n\n```json\n{record}\n```\n\n"
    );
    if let Some(note) = note {
        let _ = write!(msg, "## This document\n\n{note}\n\n");
    }
    push_pages(&mut msg, "Related pages", related);
    let _ = write!(msg, "## Allowed anchors\n\n{}\n\n", anchors.join("\n"));
    if let Some(focus) = focus {
        let _ = write!(msg, "## This call\n\n{focus}\n\n");
    }
    if !feedback.is_empty() {
        let _ = write!(
            msg,
            "## Why the last plan was rejected\n\nSend a whole new plan that fixes these:\n\n{}\n",
            feedback.join("\n")
        );
    }
    msg
}

/// A page id as code writes it (`expertise/x.md` is `expertise/x`); anything else as it is.
fn page_id(page: &str) -> String {
    PagePath::parse(page).map_or_else(|_| page.to_owned(), |p| p.to_string())
}

/// The pages and anchors an edit's new text links to and its new `about` names.
fn added_links(e: &Edit) -> Vec<String> {
    let (text, about): (Option<&str>, &[String]) = match e {
        Edit::Create { body, about, .. } => (Some(body), about),
        Edit::Replace { body, .. } => (Some(body), &[]),
        Edit::Update {
            text, add_about, ..
        } => (text.as_deref(), add_about),
        Edit::Supersede { replacement, .. } => (replacement.as_deref(), &[]),
    };
    let mut out = text.map(crate::search::catalog::links).unwrap_or_default();
    out.extend(about.iter().cloned());
    out
}

/// A page written for several concepts is listed once: as created if any call created it.
fn dedup_pages(result: &mut JobResult) {
    let mut seen = HashSet::new();
    result.pages_created.retain(|p| seen.insert(p.clone()));
    result.pages_updated.retain(|p| seen.insert(p.clone()));
}

/// At most `max` characters of a text, marking a cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("\n[… the rest is left out]");
    out
}

#[derive(Deserialize)]
struct MatchReply {
    document: Option<String>,
}

// ---- reply shapes, enforced by the API's structured output parameter -----------------------
// Every object lists all its properties as required and allows no others, and optional values
// are nullable, so the same schemas work with Anthropic's `output_config.format` and OpenAI's
// strict `response_format`.

fn object(props: &[(&str, serde_json::Value)]) -> serde_json::Value {
    let properties: serde_json::Map<String, serde_json::Value> = props
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    let required: Vec<&str> = props.iter().map(|(k, _)| *k).collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn string() -> serde_json::Value {
    json!({ "type": "string" })
}

fn nullable_string() -> serde_json::Value {
    json!({ "anyOf": [{ "type": "string" }, { "type": "null" }] })
}

fn strings() -> serde_json::Value {
    json!({ "type": "array", "items": { "type": "string" } })
}

fn op(name: &str) -> serde_json::Value {
    json!({ "type": "string", "enum": [name] })
}

/// An edit plan: `{"edits": [...]}` with create, update and supersede edits.
pub fn plan_schema() -> serde_json::Value {
    let create = object(&[
        ("op", op("create")),
        ("page", string()),
        ("title", string()),
        ("summary", string()),
        ("aliases", strings()),
        ("keywords", strings()),
        ("about", strings()),
        ("body", string()),
    ]);
    let update = object(&[
        ("op", op("update")),
        ("page", string()),
        ("heading", nullable_string()),
        ("text", nullable_string()),
        ("summary", nullable_string()),
        ("add_aliases", strings()),
        ("add_keywords", strings()),
        ("add_about", strings()),
    ]);
    let supersede = object(&[
        ("op", op("supersede")),
        ("page", string()),
        ("claim", string()),
        ("replacement", nullable_string()),
        ("reason", string()),
    ]);
    object(&[(
        "edits",
        json!({ "type": "array", "items": { "anyOf": [create, update, supersede] } }),
    )])
}

/// One verdict per edit.
pub fn verdicts_schema() -> serde_json::Value {
    let verdict = object(&[
        ("edit", json!({ "type": "integer" })),
        ("ok", json!({ "type": "boolean" })),
        ("reason", nullable_string()),
    ]);
    object(&[("verdicts", json!({ "type": "array", "items": verdict }))])
}

/// Which candidate document an upload is a new version of: one of their ids, or null.
pub fn match_schema(ids: &[String]) -> serde_json::Value {
    object(&[
        (
            "document",
            json!({ "anyOf": [{ "type": "string", "enum": ids }, { "type": "null" }] }),
        ),
        ("reason", string()),
    ])
}

/// One verdict.
pub fn verdict_schema() -> serde_json::Value {
    object(&[
        ("ok", json!({ "type": "boolean" })),
        ("reason", nullable_string()),
    ])
}

/// A description of exactly this definition's node and relation types: the schema names each
/// one, so the model can neither leave one out nor add one.
pub fn description_schema(def: &SchemaDef) -> serde_json::Value {
    let text = || object(&[("en", string()), ("zh", string()), ("aliases", strings())]);
    let nodes: Vec<(&str, serde_json::Value)> =
        def.nodes.keys().map(|n| (n.as_str(), text())).collect();
    let relations: Vec<(&str, serde_json::Value)> =
        def.relations.keys().map(|r| (r.as_str(), text())).collect();
    object(&[
        ("title", string()),
        ("summary", string()),
        ("aliases", strings()),
        ("nodes", object(&nodes)),
        ("relations", object(&relations)),
        ("body", string()),
    ])
}

#[derive(Deserialize)]
struct Plan {
    #[serde(default)]
    edits: Vec<Edit>,
}

#[derive(Deserialize)]
struct Verdicts {
    #[serde(default)]
    verdicts: Vec<Verdict>,
}

#[derive(Deserialize)]
struct Verdict {
    #[serde(default)]
    edit: usize,
    ok: bool,
    #[serde(default)]
    reason: Option<String>,
}

/// The JSON object in a reply: models sometimes wrap it in a code fence or a sentence.
fn parse_json<T: serde::de::DeserializeOwned>(reply: &str) -> Result<T, String> {
    let start = reply.find('{').ok_or("no JSON object")?;
    let end = reply.rfind('}').ok_or("no JSON object")?;
    if end < start {
        return Err("no JSON object".into());
    }
    serde_json::from_str(&reply[start..=end]).map_err(|e| e.to_string())
}

fn parse_plan(reply: &str) -> Result<Vec<Edit>, String> {
    parse_json::<Plan>(reply).map(|p| p.edits)
}

/// Runs the code checks over a list of edits, off the async threads. `from_plan` refuses the
/// edit only a person may make (`replace`).
async fn check(
    tenant: &Arc<Tenant>,
    target: &Target,
    writer: &Scope,
    source: Option<&str>,
    today: &str,
    edits: &[Edit],
    from_plan: bool,
) -> anyhow::Result<Checked> {
    let snap = tenant.snapshot().await?;
    // Without the graph every node anchor would be refused as unknown; the job fails instead.
    let graph = tenant.graph().await.context("anchors need the graph")?;
    let (wiki, target, writer, source, today, edits) = (
        tenant.wiki(),
        target.clone(),
        writer.clone(),
        source.map(str::to_owned),
        today.to_owned(),
        edits.to_vec(),
    );
    tokio::task::spawn_blocking(move || {
        check_blocking(
            &wiki,
            &snap,
            &graph,
            &target,
            &writer,
            source.as_deref(),
            &today,
            &edits,
            from_plan,
        )
    })
    .await
    .context("checking edits panicked")?
}

#[allow(clippy::too_many_arguments)]
fn check_blocking(
    wiki: &WikiFs,
    snap: &SearchSnapshot,
    graph: &GraphSnapshot,
    target: &Target,
    writer: &Scope,
    source: Option<&str>,
    today: &str,
    edits: &[Edit],
    from_plan: bool,
) -> anyhow::Result<Checked> {
    let anchor = |a: &str, page: &PagePath, created: &HashSet<PagePath>| {
        check_anchor(a, page, created, &snap.catalog, graph, writer)
    };
    let rules = Rules {
        target,
        today,
        source,
        anchor: &anchor,
        links: from_plan,
    };
    let mut draft = Draft::new(wiki);
    let mut results = Vec::new();
    let mut before = Vec::new();
    for e in edits {
        before.push(section_before(wiki, e));
        if from_plan && matches!(e, Edit::Replace { .. }) {
            results.push(Err("replace is not an edit the librarian may make".into()));
            continue;
        }
        results.push(draft.apply(e, &rules)?.map(|_| ()));
    }
    Ok(Checked {
        results,
        pages: draft.into_pages(),
        before,
    })
}

/// For a section update, the section as it stands, so the verifier sees what is replaced.
fn section_before(wiki: &WikiFs, e: &Edit) -> Option<String> {
    let Edit::Update {
        page,
        heading: Some(h),
        ..
    } = e
    else {
        return None;
    };
    let page = wiki.read(&PagePath::parse(page).ok()?).ok()?;
    let (start, end) = edit::find_section(&page.body, h.trim().trim_start_matches('#').trim())?;
    Some(
        page.body
            .lines()
            .skip(start)
            .take(end - start)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Holds edits as a new proposal. Callers hold the tenant's wiki writer lock.
async fn hold(
    tenant: &Arc<Tenant>,
    target: &Target,
    writer: &Scope,
    source: Option<&str>,
    edits: Vec<HeldEdit>,
) -> anyhow::Result<String> {
    let p = Proposal {
        id: new_proposal_id(&tenant.dir_name),
        status: ProposalStatus::Held,
        created_at: crate::trace::now_rfc3339(),
        kind: target.kind,
        audience: target.audience.key().to_owned(),
        source: source.map(str::to_owned),
        edits,
        user: writer.user().map(|u| u.dir.clone()),
        groups: writer.groups().iter().map(|g| g.dir.clone()).collect(),
    };
    tenant.store.put_proposal(&p).await?;
    tracing::info!(
        proposal = %p.id,
        reasons = ?p.edits.iter().map(|e| e.reason.as_str()).collect::<Vec<_>>(),
        "edits held for review"
    );
    Ok(p.id)
}

/// Step 5 and 6: checks the edits again on the wiki as it is now, writes what passes and
/// records the job; says whether any page was written. Callers hold the tenant's wiki writer
/// lock.
#[allow(clippy::too_many_arguments)]
async fn apply_and_record(
    tenant: &Arc<Tenant>,
    target: &Target,
    writer: &Scope,
    source: Option<&str>,
    today: &str,
    edits: &[Edit],
    result: &mut JobResult,
    what: &str,
) -> anyhow::Result<bool> {
    let checked = check(tenant, target, writer, source, today, edits, false).await?;
    let mut rejected = 0;
    for (e, r) in edits.iter().zip(&checked.results) {
        if let Err(reason) = r {
            rejected += 1;
            result.rejected.push(Rejected {
                page: e.page(),
                reason: reason.clone(),
            });
        }
    }
    let pages = checked.pages;
    // This call's pages; `result` gathers a whole job's, which may run several calls.
    let (mut created, mut updated) = (Vec::new(), Vec::new());
    for (page, new) in &pages {
        if *new { &mut created } else { &mut updated }.push(page.path.to_string());
    }
    let wiki = tenant.wiki();
    tokio::task::spawn_blocking(move || pages.iter().try_for_each(|(p, _)| wiki.write(p)))
        .await
        .context("writing pages panicked")??;
    let mut line = format!("{what}: ");
    if target.audience == PageAudience::Tenant {
        let _ = write!(
            line,
            "created [{}]; updated [{}]",
            created.join(", "),
            updated.join(", ")
        );
    } else {
        // Group and user pages stay out of the tenant-wide log by name.
        let _ = write!(
            line,
            "{} created, {} updated ({} pages)",
            created.len(),
            updated.len(),
            match target.audience {
                PageAudience::Group(_) => "group",
                _ => "user",
            }
        );
    }
    if let Some(p) = &result.proposal {
        let _ = write!(line, "; held as {p}");
    }
    if rejected > 0 {
        let _ = write!(line, "; {rejected} edits rejected");
    }
    let changed = !created.is_empty() || !updated.is_empty();
    result.pages_created.extend(created);
    result.pages_updated.extend(updated);
    if changed {
        tenant.reindex().await?;
        write_index(tenant).await?;
    }
    record_log(tenant, &line).await?;
    tenant.store.audit(&actor(writer), what, &line).await?;
    Ok(changed)
}

/// The description page for a definition with the librarian's text: the definition's own
/// descriptive fields first, then the librarian's, then the page as it stands. Structure
/// always comes from the definition; types the definition lacks are ignored.
pub fn described_page(
    def: &SchemaDef,
    desc: &Description,
    current: Option<&Page>,
    today: &str,
) -> Result<Page, String> {
    let mut d = def.clone();
    let fill = |slot: &mut Option<String>, from: &Option<String>| {
        if slot.is_none() {
            slot.clone_from(
                &from
                    .as_ref()
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty()),
            );
        }
    };
    fill(&mut d.title, &desc.title);
    fill(&mut d.summary, &desc.summary);
    if d.aliases.is_empty() {
        d.aliases.clone_from(&desc.aliases);
    }
    for (name, n) in &mut d.nodes {
        if let Some(t) = desc.nodes.get(name) {
            fill(&mut n.en, &t.en);
            fill(&mut n.zh, &t.zh);
            if n.aliases.is_empty() {
                n.aliases.clone_from(&t.aliases);
            }
        }
    }
    for (name, r) in &mut d.relations {
        if let Some(t) = desc.relations.get(name) {
            fill(&mut r.en, &t.en);
            fill(&mut r.zh, &t.zh);
            if r.aliases.is_empty() {
                r.aliases.clone_from(&t.aliases);
            }
        }
    }
    d.validate()?;
    let mut page = describe::describe(&d, current, today).map_err(|e| e.to_string())?;
    if page.front.title.is_none() || page.front.summary.is_none() {
        return Err("the description needs a title and a summary".into());
    }
    let body = desc.body.as_deref().map(str::trim).unwrap_or_default();
    if body.is_empty() {
        return Err("the description needs a body".into());
    }
    page.body = format!("{body}\n");
    Ok(page)
}

/// Rewrites `index.md` from the search catalog: every tenant-level page with its summary.
/// Group and user pages are left out, since every caller can read the index.
pub(crate) async fn write_index(tenant: &Arc<Tenant>) -> anyhow::Result<()> {
    let snap = tenant.snapshot().await?;
    let mut body = String::from("# Index\n");
    for kind in Kind::ALL {
        let pages: Vec<_> = snap
            .catalog
            .pages
            .iter()
            .filter(|p| p.kind == kind && p.audience == "tenant")
            .collect();
        if pages.is_empty() {
            continue;
        }
        let _ = write!(body, "\n## {}\n\n", kind.as_str());
        for p in pages {
            let _ = writeln!(body, "- [[{}]] {}: {}", p.path, p.title, p.summary);
        }
    }
    let wiki = tenant.wiki();
    tokio::task::spawn_blocking(move || {
        let mut page = Page::new(PagePath::parse("index")?);
        page.body = body;
        wiki.write(&page)
    })
    .await??;
    Ok(())
}

/// Adds one line to `log.md`.
pub(crate) async fn record_log(tenant: &Arc<Tenant>, what: &str) -> anyhow::Result<()> {
    let wiki = tenant.wiki();
    let line = format!("- {} {what}\n", crate::trace::now_rfc3339());
    tokio::task::spawn_blocking(move || -> Result<(), WikiError> {
        let path = PagePath::parse("log")?;
        let mut page = match wiki.read(&path) {
            Ok(p) => p,
            Err(WikiError::NotFound) => {
                let mut p = Page::new(path.clone());
                p.body = "# Log\n".into();
                p
            }
            Err(e) => return Err(e),
        };
        if !page.body.ends_with('\n') {
            page.body.push('\n');
        }
        page.body.push_str(&line);
        wiki.write(&page)
    })
    .await??;
    Ok(())
}

/// A page id for the tenant-wide log: group and user pages stay unnamed.
fn log_page(target: &Target, page: &str) -> String {
    match target.audience {
        PageAudience::Tenant => page.to_owned(),
        PageAudience::Group(_) => "a group page".into(),
        PageAudience::User(_) => "a user page".into(),
    }
}

/// Who did it, for the audit log: the user folder, never the raw id.
fn actor(scope: &Scope) -> String {
    scope
        .user()
        .map_or_else(|| "api_key".to_owned(), |u| u.dir.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_ids_route_to_their_tenant() {
        let dir = crate::tenants::tenant_dir_name("acme");
        let id = new_proposal_id(&dir);
        assert_eq!(tenant_dir_of_proposal(&id), Some(dir));
        assert_eq!(tenant_dir_of_proposal("p_x_y"), None);
        assert_eq!(tenant_dir_of_proposal("p_../../x_0011223344556677"), None);
    }

    #[test]
    fn replies_are_read_leniently() {
        let plan = parse_plan(
            "Here is the plan:\n```json\n{\"edits\": [{\"op\": \"update\", \"page\": \"experience/x\", \"add_about\": [\"commerce:order\"], \"note\": \"extra fields are ignored\"}]}\n```",
        )
        .unwrap();
        assert_eq!(plan.len(), 1);
        assert!(parse_plan("no json").is_err());
        assert!(parse_plan("{\"edits\": [{\"op\": \"delete\", \"page\": \"x\"}]}").is_err());
        let v: Verdicts =
            parse_json("{\"verdicts\": [{\"edit\": 0, \"ok\": false, \"reason\": \"r\"}]}")
                .unwrap();
        assert!(!v.verdicts[0].ok);
    }

    /// Both APIs' strict structured outputs need every object closed, with every property
    /// required (optional values are nullable instead).
    #[test]
    fn reply_schemas_are_strict() {
        fn check(v: &serde_json::Value, at: &str) {
            match v {
                serde_json::Value::Object(o) => {
                    if o.get("type").and_then(|t| t.as_str()) == Some("object") {
                        assert_eq!(o["additionalProperties"], json!(false), "{at}");
                        let mut props: Vec<&String> =
                            o["properties"].as_object().unwrap().keys().collect();
                        let mut required: Vec<&str> = o["required"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|r| r.as_str().unwrap())
                            .collect();
                        props.sort();
                        required.sort();
                        assert_eq!(props, required, "{at}");
                    }
                    for (k, sub) in o {
                        check(sub, &format!("{at}.{k}"));
                    }
                }
                serde_json::Value::Array(a) => a.iter().for_each(|x| check(x, at)),
                _ => {}
            }
        }
        let def: SchemaDef = serde_json::from_value(json!({
            "schema": "commerce",
            "nodes": { "order": { "key": "order_id" }, "customer": { "key": "id" } },
            "relations": { "placed_by": { "from": "order", "to": "customer" } }
        }))
        .unwrap();
        let described = description_schema(&def);
        for (name, schema) in [
            ("plan", plan_schema()),
            ("verdicts", verdicts_schema()),
            ("verdict", verdict_schema()),
            ("match", match_schema(&["d_0011223344556677".to_owned()])),
            ("read", reading::read_schema()),
            ("description", described.clone()),
        ] {
            check(&schema, name);
        }
        // The description lists exactly the definition's types.
        let nodes = &described["properties"]["nodes"]["required"];
        assert_eq!(nodes, &json!(["customer", "order"]));
        assert_eq!(
            described["properties"]["relations"]["required"],
            json!(["placed_by"])
        );
    }

    #[test]
    fn descriptions_never_change_structure() {
        let def: SchemaDef = serde_json::from_value(json!({
            "schema": "commerce",
            "nodes": { "order": { "key": "order_id", "en": "A customer order" } },
            "relations": { "placed_by": { "from": "order", "to": "order" } }
        }))
        .unwrap();
        let desc: Description = serde_json::from_value(json!({
            "title": "Commerce graph",
            "summary": "Orders.",
            "nodes": {
                "order": { "en": "An order (librarian)", "zh": "订单", "aliases": ["orders"], "key": "id" },
                "ghost": { "en": "Not in the definition" }
            },
            "relations": { "placed_by": { "zh": "下单", "from": "x" } },
            "body": "# Commerce graph\n\nProse."
        }))
        .unwrap();
        let page = described_page(&def, &desc, None, "2026-10-01").unwrap();
        let nodes = &page.front.extra["nodes"];
        assert_eq!(nodes["order"]["key"].as_str(), Some("order_id"));
        assert_eq!(
            nodes["order"]["en"].as_str(),
            Some("A customer order"),
            "the host's text wins"
        );
        assert_eq!(nodes["order"]["zh"].as_str(), Some("订单"));
        assert!(nodes.get("ghost").is_none());
        let rel = &page.front.extra["relations"]["placed_by"];
        assert_eq!(rel["from"].as_str(), Some("order"));
        assert_eq!(rel["zh"].as_str(), Some("下单"));
        assert_eq!(page.front.title.as_deref(), Some("Commerce graph"));
        assert_eq!(page.body, "# Commerce graph\n\nProse.\n");
        // Text over the definition's limits is refused, not cut.
        let long = Description {
            summary: Some("x".repeat(1_000)),
            ..Description::default()
        };
        assert!(described_page(&def, &long, None, "2026-10-01").is_err());
    }
}
