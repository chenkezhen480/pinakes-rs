//! Lint (design, "The core idea": the librarian checks the wiki for contradictions, stale
//! claims, duplicate pages, broken links and memories that aren't anchored, and "Misses improve
//! the wiki"). `POST /lint` queues one run on the tenant's write queue; its findings replace
//! the last run's and `GET /lint` lists those the caller may see.
//!
//! Code checks every page each run, without a model: links and anchors that lead nowhere (or
//! somewhere not every reader of the page can see), memory pages without an anchor, raw records
//! no page cites, citations of records that are gone or of a document reading never finished,
//! and from the last days' search traces the keywords that keep coming back weak and the
//! keywords that named graph nodes only by part of their names (missing aliases, for the
//! host: the librarian never writes graph data).
//!
//! The model reviews pages that changed since the last run (or whose dates have passed since):
//! each with the pages most related to it, for contradictions, duplicates and stale claims;
//! and proposes fixes where they are clear: anchors for an unanchored memory, a weak keyword
//! as an alias of the page that covers it, a stale claim superseded, a duplicate's names on
//! the page to keep. Fixes are never written by lint itself: each is held as a proposal for a
//! person to approve (`/proposals`), or approved at once when the admin turned on
//! `[librarian] auto_approve`. Contradictions are only reported.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::{PageAudience, Target};
use super::{
    ATTEMPTS, Edit, HeldEdit, Librarian, MAX_PAGE_CHARS, Related, anchor_options, check, cut, find,
    object, parse_json, plan_schema, prompts, record_log, string, strings,
};
use crate::graph::{EntityRef, GraphSnapshot, Visibility};
use crate::identity::Scope;
use crate::jobs::JobResult;
use crate::kinds::Kind;
use crate::search::catalog::{Catalog, PageEntry};
use crate::search::text::normalize;
use crate::tenants::Tenant;
use crate::wiki::{Page, PagePath};

/// Most pages one run reviews with the model; the rest wait for the next run.
const REVIEW_PAGES: usize = 30;
/// Related pages reviewed with each page.
const RELATED: usize = 4;
/// Most unanchored memories and weak keywords a run asks the model to fix.
const FIX_TASKS: usize = 10;
/// Tasks per model call: the work is divided by tasks, never the pages by size.
const TASKS_PER_CALL: usize = 4;
/// Days of search traces read for weak keywords and partial names.
const TRACE_DAYS: i64 = 7;
/// Searches a keyword must have come back weak in (or named nodes by part of their names in)
/// to be reported.
const MIN_SEARCHES: u32 = 3;
/// Pages shown for a weak keyword.
const MISS_PAGES: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// A `[[link]]` to a page or entity that doesn't exist or not every reader can see.
    BrokenLink,
    /// An `about` anchor that doesn't exist or not every reader can see.
    BadAnchor,
    /// A memory page without any anchor.
    Unanchored,
    /// A page cites a raw record that isn't in `sources/`.
    MissingSource,
    /// A raw record no page cites (its job failed, or a resumed reading's own copy).
    OrphanRecord,
    /// A page cites lines of a document no version holds: its reading never finished.
    UnfinishedReading,
    /// A keyword searches keep coming back weak for.
    FrequentMiss,
    /// A keyword that named graph nodes only by part of their names: a missing alias.
    PartialName,
    Contradiction,
    Duplicate,
    Stale,
}

/// Text on a page that a finding is about, as the page has it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Claim {
    pub page: String,
    pub text: String,
}

/// One finding, as `GET /lint` lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Finding {
    pub kind: FindingKind,
    /// The pages it is about, as `/read` takes them.
    pub pages: Vec<String>,
    pub detail: String,
    /// What a broken link or anchor names, the record, or the keyword.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub claims: Vec<Claim>,
    /// For a partial name: the nodes it named.
    pub nodes: Vec<String>,
    /// For a weak keyword or a partial name: in how many searches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// The held proposal with a fix, if one was proposed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
}

/// A finding as stored: who may see it (`tenant`, or a group or user folder), and for a
/// model's finding the pages' hashes when it was made, so it is kept until they change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub audience: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hashes: BTreeMap<String, String>,
    #[serde(flatten)]
    pub finding: Finding,
}

/// What a lint run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct LintRun {
    pub at: String,
    pub findings: usize,
    /// Pages the model reviewed this run.
    pub pages_reviewed: usize,
    /// Changed pages left for the next run.
    pub pages_waiting: usize,
    /// How many proposals this run held with fixes (each finding names its own).
    pub proposals: usize,
}

/// One piece of work for the model.
struct Task {
    id: String,
    what: TaskKind,
    pages: Vec<Page>,
}

enum TaskKind {
    Review,
    /// The unanchored memory (the code finding's index) and the anchors it may use.
    Anchor(usize, Vec<String>),
    /// The weak keyword's finding, and the keyword.
    Miss(usize, String),
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    findings: Vec<RawFinding>,
}

#[derive(Deserialize)]
struct RawFinding {
    task: String,
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    pages: Vec<String>,
    #[serde(default)]
    claims: Vec<Claim>,
    #[serde(default)]
    reason: String,
    /// The model's second thought, after writing the reason: is something really wrong?
    #[serde(default = "yes")]
    problem: bool,
    #[serde(default)]
    edits: Vec<Edit>,
}

fn yes() -> bool {
    true
}

impl Librarian {
    /// One lint run over the tenant. Runs on its write queue.
    #[tracing::instrument(name = "lint", skip_all, fields(tenant = %tenant.dir_name))]
    pub async fn lint(&self, tenant: Arc<Tenant>) -> anyhow::Result<JobResult> {
        let started = SystemTime::now();
        let now = crate::trace::now_rfc3339();
        let today = now[..10].to_owned();
        let snap = tenant.snapshot().await?;
        let graph = tenant
            .graph()
            .await
            .context("lint checks anchors against the graph")?;
        let store = &tenant.store;
        let last = store.lint_run().await?;
        let reviewed: HashMap<String, String> = store.lint_reviewed().await?.into_iter().collect();
        let held: Vec<super::Proposal> = store.held_proposals().await?;
        let held_ids: HashSet<String> = held.iter().map(|p| p.id.clone()).collect();
        let versions: HashSet<String> = store
            .documents_by_source()
            .await?
            .into_iter()
            .map(|(s, _)| s)
            .collect();
        let mut keep_records: HashSet<String> = versions.clone();
        keep_records.extend(held.iter().filter_map(|p| p.source.clone()));
        let record_audiences: HashMap<String, String> =
            store.records().await?.into_iter().collect();
        let deleted: HashMap<String, String> = store
            .meta(crate::deletion::DELETED_OWNERS)
            .await?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default();

        // Code checks and traces, off the async threads.
        let (catalog_snap, graph2, dir) = (snap.clone(), graph.clone(), tenant.dir.clone());
        let mut findings: Vec<Stored> = tokio::task::spawn_blocking(move || {
            let mut out = code_findings(
                &catalog_snap.catalog,
                &graph2,
                &dir,
                &versions,
                &keep_records,
                &record_audiences,
                started,
            );
            out.extend(trace_findings(
                &dir.join("logs"),
                &catalog_snap.catalog,
                &graph2,
                &deleted,
            ));
            out
        })
        .await
        .context("lint checks panicked")?;

        // A fix proposed by an earlier run and still held stays with its finding.
        let earlier_fix: HashMap<(FindingKind, Vec<String>, Option<String>), String> = last
            .as_ref()
            .map(|(_, f)| f.as_slice())
            .unwrap_or_default()
            .iter()
            .filter_map(|s| {
                let f = &s.finding;
                let p = f.proposal.clone().filter(|p| held_ids.contains(p))?;
                Some(((f.kind, f.pages.clone(), f.target.clone()), p))
            })
            .collect();
        for s in &mut findings {
            let f = &mut s.finding;
            f.proposal = earlier_fix
                .get(&(f.kind, f.pages.clone(), f.target.clone()))
                .cloned();
        }

        // What the model reviews: pages changed since the last review, then pages whose dates
        // have passed since the last run.
        let since = last
            .as_ref()
            .and_then(|(r, _)| crate::trace::parse_date(&r.at[..10]));
        let today_n = crate::trace::today();
        let mut changed = Vec::new();
        let mut dated = Vec::new();
        for p in snap.catalog.pages.iter().filter(|p| p.kind != Kind::Graph) {
            if reviewed.get(p.path.as_str()) != Some(&p.hash) {
                changed.push(p.clone());
            } else if passed_date(p, since, today_n) {
                dated.push(p.clone());
            }
        }
        let seeds_total = changed.len() + dated.len();
        let seeds: Vec<Arc<PageEntry>> = changed
            .into_iter()
            .chain(dated)
            .take(REVIEW_PAGES)
            .collect();

        let meaning = self.meaning_for(&snap);
        let (cfg, snap2, graph2) = (self.search.clone(), snap.clone(), graph.clone());
        let tenant2 = tenant.clone();
        let fix_wanted: Vec<(usize, Stored)> = findings
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                // A keyword as an alias of a tenant page is for everyone to see: only the
                // tenant's own weak keywords (searches without a user) are fixed that way.
                s.finding.proposal.is_none()
                    && match s.finding.kind {
                        FindingKind::Unanchored => true,
                        FindingKind::FrequentMiss => s.audience == "tenant",
                        _ => false,
                    }
            })
            .take(FIX_TASKS)
            .map(|(i, s)| (i, s.clone()))
            .collect();
        let seeds2 = seeds.clone();
        let tasks: Vec<Task> = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            build_tasks(
                &tenant2,
                &snap2,
                &graph2,
                &cfg,
                meaning.as_deref(),
                &seeds2,
                &fix_wanted,
            )
        })
        .await
        .context("gathering lint tasks panicked")??;

        // The model's findings, in calls of a few tasks each.
        // A model's finding from an earlier run stays while its pages are as they were and
        // weren't reviewed again now; one found again now is not added twice.
        let now_reviewed: HashSet<&str> = seeds.iter().map(|p| p.path.as_str()).collect();
        let mut fresh: Vec<Stored> = Vec::new();
        if let Some((_, earlier)) = &last {
            for s in earlier.iter().filter(|s| !s.hashes.is_empty()) {
                let same = s.hashes.iter().all(|(page, hash)| {
                    snap.catalog.get(page).is_some_and(|(_, e)| &e.hash == hash)
                        && !now_reviewed.contains(page.as_str())
                });
                if same {
                    let mut s = s.clone();
                    // A fix approved or dropped since is no longer pending.
                    s.finding.proposal = s.finding.proposal.filter(|p| held_ids.contains(p));
                    fresh.push(s);
                }
            }
        }
        let mut proposals = 0;
        for chunk in tasks.chunks(TASKS_PER_CALL) {
            let raw = self.ask(chunk, &today).await?;
            for r in raw {
                // Pages shared by two tasks, or a finding kept from the last run, can bring the
                // same finding twice; the first counts.
                let Some(task) = chunk.iter().find(|t| t.id == r.task) else {
                    continue;
                };
                let Some(mut stored) = accept(task, &r, &snap.catalog) else {
                    continue;
                };
                // The audience of what the fix is for: an edit may change only its pages, so
                // nothing read on a narrower page reaches a wider one through a proposal.
                let audience = match task.what {
                    TaskKind::Review => stored.audience.clone(),
                    TaskKind::Anchor(i, _) | TaskKind::Miss(i, _) => {
                        if findings[i].finding.proposal.is_some() {
                            continue; // One fix per task; the first counts.
                        }
                        findings[i].audience.clone()
                    }
                };
                let edits: Vec<Edit> = if stored.finding.kind == FindingKind::Contradiction {
                    Vec::new()
                } else {
                    r.edits
                        .into_iter()
                        .filter(|e| matches!(e, Edit::Update { .. } | Edit::Supersede { .. }))
                        .filter(|e| task.pages.iter().any(|p| p.path.as_str() == e.page()))
                        .filter(|e| {
                            PagePath::parse(&e.page())
                                .is_ok_and(|p| PageAudience::of(&p).key() == audience)
                        })
                        .collect()
                };
                let repeated = matches!(task.what, TaskKind::Review)
                    && fresh.iter().any(|s| {
                        s.finding.kind == stored.finding.kind
                            && s.hashes.keys().eq(stored.hashes.keys())
                    });
                if repeated {
                    continue;
                }
                let why = format!("lint: {}", stored.finding.detail);
                let proposal = self.propose(&tenant, &today, edits, &why).await?;
                proposals += usize::from(proposal.is_some());
                match task.what {
                    TaskKind::Review => {
                        stored.finding.proposal = proposal;
                        fresh.push(stored);
                    }
                    TaskKind::Anchor(i, _) | TaskKind::Miss(i, _) => {
                        findings[i].finding.proposal = proposal;
                    }
                }
            }
        }

        findings.extend(fresh);

        let reviewed_now: Vec<(String, String)> = seeds
            .iter()
            .map(|p| (p.path.to_string(), p.hash.clone()))
            .collect();
        let run = LintRun {
            at: now,
            findings: findings.len(),
            pages_reviewed: reviewed_now.len(),
            pages_waiting: seeds_total - reviewed_now.len(),
            proposals,
        };
        store.put_lint_run(&run, &findings, &reviewed_now).await?;
        {
            let _writer = tenant.lock_wiki().await?;
            let line = format!(
                "lint: {} findings, {} pages reviewed, {} proposals",
                run.findings, run.pages_reviewed, run.proposals
            );
            record_log(&tenant, &line).await?;
            store.audit("lint", "lint.run", &line).await?;
        }
        tracing::info!(
            findings = run.findings,
            reviewed = run.pages_reviewed,
            "lint done"
        );
        Ok(JobResult {
            lint: Some(run),
            ..JobResult::default()
        })
    }

    /// Asks the model about a few tasks; a reply that can't be read is asked for again.
    async fn ask(&self, tasks: &[Task], today: &str) -> anyhow::Result<Vec<RawFinding>> {
        let mut msg = format!("Today is {today}.\n\n## Tasks\n\n");
        for t in tasks {
            match &t.what {
                TaskKind::Review => {
                    let _ = write!(
                        msg,
                        "### Task {}: review\n\nPages: {}\n\n",
                        t.id,
                        ids(&t.pages)
                    );
                    // Code finds the dates; whether a claim depends on one is the model's call.
                    let passed = passed_dates(&t.pages, crate::trace::today());
                    if !passed.is_empty() {
                        let _ = write!(msg, "Dates before today on these pages: {passed}\n\n");
                    }
                }
                TaskKind::Anchor(_, anchors) => {
                    let _ = write!(
                        msg,
                        "### Task {}: anchor\n\nThe memory page without anchors: {}\n\nAnchors \
                         allowed for it:\n{}\n\n",
                        t.id,
                        t.pages[0].path,
                        anchors.join("\n")
                    );
                }
                TaskKind::Miss(_, keyword) => {
                    let _ = write!(
                        msg,
                        "### Task {}: miss\n\nKeyword: {keyword}\n\nPages: {}\n\n",
                        t.id,
                        ids(&t.pages)
                    );
                }
            }
            for p in &t.pages {
                let _ = write!(
                    msg,
                    "#### {}\n\n```markdown\n{}\n```\n\n",
                    p.path,
                    cut(&p.render(), MAX_PAGE_CHARS)
                );
            }
        }
        let ids: Vec<String> = tasks.iter().map(|t| t.id.clone()).collect();
        let prompt = self.prompt(prompts::LINT, msg, lint_schema(&ids));
        let mut why = String::new();
        for _ in 0..ATTEMPTS {
            let reply = self
                .llm
                .complete(&prompt)
                .await
                .context("asking the librarian to lint")?;
            match parse_json::<Reply>(&reply) {
                Ok(r) => return Ok(r.findings),
                Err(e) => why = e,
            }
        }
        anyhow::bail!("the librarian's lint answer could not be read: {why}")
    }

    /// Holds a finding's edits as proposals after the code checks, one per page audience and
    /// kind (approved at once with `auto_approve`); answers the first. Edits the checks refuse
    /// are dropped.
    async fn propose(
        &self,
        tenant: &Arc<Tenant>,
        today: &str,
        edits: Vec<Edit>,
        why: &str,
    ) -> anyhow::Result<Option<String>> {
        let mut by_target: Vec<(Target, Vec<Edit>)> = Vec::new();
        for e in edits {
            let Ok(path) = PagePath::parse(&e.page()) else {
                continue;
            };
            let Some(target) = path
                .kind()
                .and_then(|k| Target::new(k, PageAudience::of(&path)).ok())
            else {
                continue;
            };
            match by_target.iter_mut().find(|(t, _)| *t == target) {
                Some((_, list)) => list.push(e),
                None => by_target.push((target, vec![e])),
            }
        }
        let mut first = None;
        for (target, edits) in by_target {
            let writer = reader_of(tenant, &target.audience);
            let checked = check(tenant, &target, &writer, &[], today, &edits, true).await?;
            let held: Vec<HeldEdit> = edits
                .into_iter()
                .zip(checked.results)
                .filter(|(_, r)| r.is_ok())
                .map(|(edit, _)| HeldEdit {
                    edit,
                    reason: why.to_owned(),
                })
                .collect();
            if held.is_empty() {
                continue;
            }
            let _writer = tenant.lock_wiki().await?;
            let (id, _) = self
                .hold_or_approve(tenant, &target, &writer, None, held)
                .await?;
            first.get_or_insert(id);
        }
        Ok(first)
    }
}

fn ids(pages: &[Page]) -> String {
    pages
        .iter()
        .map(|p| p.path.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Who sees what a page's audience sees, without knowing a user's groups: the tenant alone, the
/// tenant and the group, or the user by their folder.
fn reader_of(tenant: &Arc<Tenant>, audience: &PageAudience) -> Scope {
    let user = match audience {
        PageAudience::User(u) => Some(u.clone()),
        _ => None,
    };
    super::reader_scope(
        tenant,
        audience,
        &Scope::offline(tenant.clone(), user, Vec::new()),
    )
}

/// The reply: findings for the tasks given.
fn lint_schema(task_ids: &[String]) -> serde_json::Value {
    let edit_kinds = plan_schema()["properties"]["edits"]["items"]["anyOf"].clone();
    let (update, supersede) = (edit_kinds[1].clone(), edit_kinds[2].clone());
    let claim = object(&[("page", string()), ("text", string())]);
    let finding = object(&[
        ("task", json!({ "type": "string", "enum": task_ids })),
        (
            "type",
            json!({ "type": "string", "enum": ["contradiction", "duplicate", "stale", "fix"] }),
        ),
        ("pages", strings()),
        ("claims", json!({ "type": "array", "items": claim })),
        ("reason", string()),
        ("problem", json!({ "type": "boolean" })),
        (
            "edits",
            json!({ "type": "array", "items": { "anyOf": [update, supersede] } }),
        ),
    ]);
    object(&[("findings", json!({ "type": "array", "items": finding }))])
}

/// A model's finding, checked: its pages are the task's, its claims are on those pages, and a
/// review finding has what it needs (a claim, or two pages for a duplicate). A fix only
/// carries edits. `None` when nothing of it holds.
fn accept(task: &Task, r: &RawFinding, catalog: &Catalog) -> Option<Stored> {
    if !r.problem {
        return None;
    }
    let shown: HashMap<&str, &Page> = task.pages.iter().map(|p| (p.path.as_str(), p)).collect();
    let pages: Vec<String> = r
        .pages
        .iter()
        .chain(r.claims.iter().map(|c| &c.page))
        .filter(|p| shown.contains_key(p.as_str()))
        .fold(Vec::new(), |mut v, p| {
            if !v.contains(p) {
                v.push(p.clone());
            }
            v
        });
    let claims: Vec<Claim> = r
        .claims
        .iter()
        .filter(|c| {
            shown
                .get(c.page.as_str())
                .is_some_and(|p| has_text(&p.render(), &c.text))
        })
        .cloned()
        .collect();
    let kind = match (r.ty.as_str(), &task.what) {
        ("contradiction", TaskKind::Review) if !claims.is_empty() => FindingKind::Contradiction,
        ("stale", TaskKind::Review) if !claims.is_empty() => FindingKind::Stale,
        // A playbook and a concept page on one topic are meant to stand side by side.
        ("duplicate", TaskKind::Review) if pages.len() >= 2 && same_kind(&pages) => {
            FindingKind::Duplicate
        }
        // A fix belongs to the code finding its task is for.
        ("fix", TaskKind::Anchor(..) | TaskKind::Miss(..)) => FindingKind::Unanchored,
        _ => return None,
    };
    let hashes = pages
        .iter()
        .filter_map(|p| catalog.get(p).map(|(_, e)| (p.clone(), e.hash.clone())))
        .collect();
    Some(Stored {
        audience: narrowest(&pages),
        hashes,
        finding: Finding {
            kind,
            pages,
            detail: r.reason.trim().to_owned(),
            target: None,
            claims,
            nodes: Vec::new(),
            count: None,
            proposal: None,
        },
    })
}

/// Whether the pages are all of one kind.
fn same_kind(pages: &[String]) -> bool {
    let kinds: Vec<Option<Kind>> = pages
        .iter()
        .map(|p| PagePath::parse(p).ok().and_then(|p| p.kind()))
        .collect();
    kinds.windows(2).all(|w| w[0] == w[1])
}

/// Whether a page holds a text, ignoring differences in white space and a list marker.
fn has_text(page: &str, text: &str) -> bool {
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let t = squash(text.trim().trim_start_matches(['-', '*']).trim());
    !t.is_empty() && squash(page).contains(&t)
}

/// Who may see a finding about these pages: the narrowest of their audiences.
fn narrowest(pages: &[String]) -> String {
    let audiences: Vec<PageAudience> = pages
        .iter()
        .filter_map(|p| PagePath::parse(p).ok())
        .map(|p| PageAudience::of(&p))
        .collect();
    audiences
        .iter()
        .find(|a| matches!(a, PageAudience::User(_)))
        .or_else(|| {
            audiences
                .iter()
                .find(|a| matches!(a, PageAudience::Group(_)))
        })
        .map_or_else(|| "tenant".to_owned(), |a| a.key().to_owned())
}

/// The model's tasks: each page to review with its most related pages, then the fixes asked
/// for (an unanchored memory with the anchors it may use, a weak keyword with the pages it
/// finds).
fn build_tasks(
    tenant: &Arc<Tenant>,
    snap: &crate::search::SearchSnapshot,
    graph: &GraphSnapshot,
    cfg: &crate::config::SearchConfig,
    meaning: Option<&dyn crate::embed::Embedder>,
    seeds: &[Arc<PageEntry>],
    fixes: &[(usize, Stored)],
) -> anyhow::Result<Vec<Task>> {
    let wiki = tenant.wiki();
    let read = |paths: &[String]| -> Vec<Page> {
        paths
            .iter()
            .filter_map(|p| PagePath::parse(p).ok())
            .filter_map(|p| wiki.read(&p).ok())
            .collect()
    };
    let related =
        |entry: &PageEntry, reader: &Scope| -> anyhow::Result<(Vec<String>, Vec<String>)> {
            let mut terms = vec![entry.title.clone()];
            terms.extend(entry.aliases.iter().chain(&entry.keywords).cloned());
            let keywords = with_words(&terms);
            let query = format!("{} {}", entry.title, entry.summary);
            let found = find(snap, graph, reader, cfg, meaning, &[query], &keywords, &[])?;
            let pages = found
                .pages
                .into_iter()
                .filter(|(p, k, _)| *p != entry.path.as_str() && *k != Kind::Graph)
                .map(|(p, _, _)| p)
                .collect();
            Ok((pages, found.entities))
        };
    let mut tasks = Vec::new();
    let mut groups: Vec<HashSet<String>> = Vec::new();
    for seed in seeds {
        let reader = reader_of(tenant, &PageAudience::of(&seed.path));
        let (mut pages, _) = related(seed, &reader)?;
        pages.truncate(RELATED);
        pages.insert(0, seed.path.to_string());
        let set: HashSet<String> = pages.iter().cloned().collect();
        // The same pages again add nothing. A group inside another is still asked about, with
        // its own page first: shown only as another's related page, a page is easily passed by.
        if groups.contains(&set) {
            continue;
        }
        groups.push(set);
        tasks.push(Task {
            id: format!("t{}", tasks.len() + 1),
            what: TaskKind::Review,
            pages: read(&pages),
        });
    }
    for (i, stored) in fixes {
        let f = &stored.finding;
        match f.kind {
            FindingKind::Unanchored => {
                let Some(path) = f.pages.first().and_then(|p| PagePath::parse(p).ok()) else {
                    continue;
                };
                let Some((_, entry)) = snap.catalog.get(path.as_str()) else {
                    continue;
                };
                let audience = PageAudience::of(&path);
                let Ok(target) = Target::new(entry.kind, audience.clone()) else {
                    continue;
                };
                let reader = reader_of(tenant, &audience);
                let (pages, entities) = related(entry, &reader)?;
                let rel = Related {
                    pages: read(&pages[..pages.len().min(RELATED * 2)]),
                    entities,
                };
                let anchors = anchor_options(&target, &reader, &rel, &[], &snap.catalog, graph);
                tasks.push(Task {
                    id: format!("t{}", tasks.len() + 1),
                    what: TaskKind::Anchor(*i, anchors),
                    pages: read(&[path.to_string()]),
                });
            }
            FindingKind::FrequentMiss => {
                let Some(keyword) = f.target.clone() else {
                    continue;
                };
                // Only tenant pages: the keyword came from the tenant's searches as a whole.
                let reader = Scope::offline(tenant.clone(), None, Vec::new());
                let found = find(
                    snap,
                    graph,
                    &reader,
                    cfg,
                    meaning,
                    std::slice::from_ref(&keyword),
                    &with_words(std::slice::from_ref(&keyword)),
                    &[],
                )?;
                let pages: Vec<String> = found
                    .pages
                    .into_iter()
                    .filter(|(_, k, _)| *k != Kind::Graph)
                    .take(MISS_PAGES)
                    .map(|(p, _, _)| p)
                    .collect();
                if pages.is_empty() {
                    continue;
                }
                tasks.push(Task {
                    id: format!("t{}", tasks.len() + 1),
                    what: TaskKind::Miss(*i, keyword),
                    pages: read(&pages),
                });
            }
            _ => {}
        }
    }
    // A page that can't be read (changed on disk since the snapshot) leaves nothing to ask.
    tasks.retain(|t| !t.pages.is_empty());
    Ok(tasks)
}

/// Terms and, for those of several words, each word on its own: a page sharing part of a
/// term is related too.
fn with_words(terms: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in terms {
        for w in std::iter::once(t.as_str()).chain(t.split_whitespace()) {
            if !w.is_empty() && !out.iter().any(|o| o == w) {
                out.push(w.to_owned());
            }
        }
    }
    out
}

/// The dates before `today` the pages name, as `page: date, date; page: ...`.
fn passed_dates(pages: &[Page], today: i64) -> String {
    pages
        .iter()
        .filter_map(|p| {
            let mut found: Vec<String> = Vec::new();
            for (d, text) in dates(&p.body) {
                if d < today && !found.contains(&text) {
                    found.push(text);
                }
            }
            (!found.is_empty()).then(|| format!("{}: {}", p.path, found.join(", ")))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Whether a page names a date that has passed since the last run (after `since`, up to
/// today), so a claim depending on it may have gone stale.
fn passed_date(entry: &PageEntry, since: Option<i64>, today: i64) -> bool {
    let Some(since) = since else {
        return false;
    };
    entry
        .sections
        .iter()
        .flat_map(|s| &s.lines)
        .flat_map(|l| dates(&l.text))
        .any(|(d, _)| d > since && d <= today)
}

/// The dates a text names as `2026-10-01`, `2026/10/01`, `2026.10.01` or `2026年10月1日`: as
/// days since 1970, and as written.
fn dates(text: &str) -> Vec<(i64, String)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let number = |i: &mut usize, max: usize| -> Option<u32> {
        let start = *i;
        while *i < chars.len() && *i - start < max && chars[*i].is_ascii_digit() {
            *i += 1;
        }
        (*i > start).then(|| chars[start..*i].iter().collect::<String>().parse().ok())?
    };
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() || (i > 0 && chars[i - 1].is_ascii_digit()) {
            i += 1;
            continue;
        }
        let mut j = i;
        let parsed = (|| {
            let y = number(&mut j, 4).filter(|_| j - i == 4)?;
            let sep = *chars.get(j)?;
            if !matches!(sep, '-' | '/' | '.' | '年') {
                return None;
            }
            j += 1;
            let m = number(&mut j, 2)?;
            let sep2 = *chars.get(j)?;
            let ok = match sep {
                '年' => sep2 == '月',
                s => sep2 == s,
            };
            if !ok {
                return None;
            }
            j += 1;
            let d = number(&mut j, 2)?;
            crate::trace::parse_date(&format!("{y:04}-{m:02}-{d:02}"))
        })();
        if let Some(d) = parsed {
            out.push((d, chars[i..j].iter().collect()));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Every check code makes on its own, over the catalog, the graph and `sources/`.
fn code_findings(
    catalog: &Catalog,
    graph: &GraphSnapshot,
    tenant_dir: &Path,
    versions: &HashSet<String>,
    keep_records: &HashSet<String>,
    record_audiences: &HashMap<String, String>,
    started: SystemTime,
) -> Vec<Stored> {
    let mut out = Vec::new();
    let mut push = |kind, page: &PageEntry, detail: String, target: Option<String>| {
        out.push(Stored {
            audience: page.audience.clone(),
            hashes: BTreeMap::new(),
            finding: Finding {
                kind,
                pages: vec![page.path.to_string()],
                detail,
                target,
                claims: Vec::new(),
                nodes: Vec::new(),
                count: None,
                proposal: None,
            },
        });
    };
    // The records in sources/, with when each was written.
    let records: HashMap<String, SystemTime> = std::fs::read_dir(tenant_dir.join("sources"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let written = e.metadata().ok()?.modified().ok()?;
            super::sources::valid_name(&name).then_some((name, written))
        })
        .collect();
    let mut cited: HashSet<&str> = HashSet::new();
    for page in &catalog.pages {
        for link in &page.links {
            let target = link.trim_end_matches(".md");
            if let Some(why) = anchor_problem(target, &page.path, catalog, graph) {
                let shown = (!why.hidden).then(|| target.to_owned());
                push(
                    FindingKind::BrokenLink,
                    page,
                    format!("a link {}", why.text),
                    shown,
                );
            }
        }
        for anchor in &page.about {
            if let Some(why) = anchor_problem(anchor, &page.path, catalog, graph) {
                let shown = (!why.hidden).then(|| anchor.clone());
                push(
                    FindingKind::BadAnchor,
                    page,
                    format!("an anchor {}", why.text),
                    shown,
                );
            }
        }
        let memory = matches!(
            page.kind,
            Kind::Experience | Kind::Decision | Kind::Preference
        );
        if memory && page.about.is_empty() {
            push(
                FindingKind::Unanchored,
                page,
                "a memory page with no anchor: hops and lifts can't reach it".into(),
                None,
            );
        }
        for source in &page.sources {
            let (name, lines) = match source.split_once('#') {
                Some((n, l)) => (n, Some(l)),
                None => (source.as_str(), None),
            };
            cited.insert(name);
            if !records.contains_key(name) {
                push(
                    FindingKind::MissingSource,
                    page,
                    "cites a raw record that is not in sources/".into(),
                    Some(format!("sources/{name}")),
                );
            } else if lines.is_some() && !versions.contains(name) {
                push(
                    FindingKind::UnfinishedReading,
                    page,
                    "cites lines of a document whose reading never finished: no version holds them; upload the document again to finish it".into(),
                    Some(format!("sources/{source}")),
                );
            }
        }
    }
    let mut orphans: Vec<&String> = records
        .iter()
        // Only records older than this run: a newer one's job may still be queued.
        .filter(|(name, written)| {
            **written < started && !cited.contains(name.as_str()) && !keep_records.contains(*name)
        })
        .map(|(n, _)| n)
        .collect();
    orphans.sort();
    for name in orphans {
        out.push(Stored {
            audience: record_audiences
                .get(name)
                .cloned()
                .unwrap_or_else(|| "tenant".into()),
            hashes: BTreeMap::new(),
            finding: Finding {
                kind: FindingKind::OrphanRecord,
                pages: Vec::new(),
                detail: "a raw record no page cites: its job failed or was overtaken".into(),
                target: Some(format!("sources/{name}")),
                claims: Vec::new(),
                nodes: Vec::new(),
                count: None,
                proposal: None,
            },
        });
    }
    out
}

/// Why a link or anchor fails.
struct Problem {
    text: &'static str,
    /// It names a node not every reader of the page may know of; the finding leaves it out.
    hidden: bool,
}

/// What is wrong with a page linking or anchoring to `anchor`, if anything: it must exist and
/// every reader of the page must be able to see it (the librarian's anchor rule). For a user's
/// page, spaces and group pages that user may see through a group are taken as visible: lint
/// doesn't know a user's groups.
fn anchor_problem(
    anchor: &str,
    page: &PagePath,
    catalog: &Catalog,
    graph: &GraphSnapshot,
) -> Option<Problem> {
    let problem = |text| {
        Some(Problem {
            text,
            hidden: false,
        })
    };
    let readers = PageAudience::of(page);
    if let Ok(target) = PagePath::parse(anchor) {
        if target.kind().is_none() || catalog.get(target.as_str()).is_none() {
            return problem("leads to a page that doesn't exist");
        }
        let ok = match PageAudience::of(&target) {
            PageAudience::Tenant => true,
            PageAudience::Group(g) => match &readers {
                PageAudience::Group(own) => *own == g,
                PageAudience::User(_) => true,
                PageAudience::Tenant => false,
            },
            PageAudience::User(u) => readers == PageAudience::User(u),
        };
        return if ok {
            None
        } else {
            problem("leads to a page not every reader of this page can see")
        };
    }
    match EntityRef::parse(anchor) {
        Some(EntityRef::Type { schema, ty }) => {
            if graph.space_index(schema).is_none() && graph.has_node_type(schema, ty) {
                None
            } else {
                problem("names a node type that doesn't exist")
            }
        }
        Some(EntityRef::Node { space, .. }) => {
            let Some(entry) = graph.node(anchor).and_then(|_| graph.space(space)) else {
                return problem("names a node that doesn't exist");
            };
            let ok = match &readers {
                PageAudience::User(u) => {
                    entry.def.visibility == Visibility::Shared
                        || (entry.def.visibility == Visibility::Restricted
                            && !entry.def.members.groups.is_empty())
                        || entry.visible_to_user(u)
                }
                _ => entry.def.visibility == Visibility::Shared,
            };
            (!ok).then_some(Problem {
                text: "names a node in a space not every reader of this page can see",
                hidden: true,
            })
        }
        None if page.kind().is_some() && anchor.contains(':') => {
            problem("names an entity that doesn't exist")
        }
        None => problem("leads to a page that doesn't exist"),
    }
}

/// From the last days' search traces: keywords searches keep coming back weak for, unless a
/// page has since taken one as its title, an alias or a keyword; and keywords that named graph
/// nodes only by part of their names (only nodes in shared spaces: the report is the tenant's).
fn trace_findings(
    logs: &Path,
    catalog: &Catalog,
    graph: &GraphSnapshot,
    deleted: &HashMap<String, String>,
) -> Vec<Stored> {
    use std::io::BufRead;
    let today = crate::trace::today();
    // Counted per searcher (a user folder, or `tenant` for searches without a user), so one
    // user's search terms are never shown to another and leave with the user.
    type Counts<K> = HashMap<(String, K), (String, u32)>;
    let mut weak: Counts<String> = HashMap::new();
    let mut partial: Counts<(String, Vec<String>)> = HashMap::new();
    let files = std::fs::read_dir(logs).into_iter().flatten().flatten();
    for file in files {
        let name = file.file_name();
        let Some(day) = crate::trace::file_day(name.to_str().unwrap_or("")) else {
            continue;
        };
        if today - day >= TRACE_DAYS {
            continue;
        }
        let Ok(f) = std::fs::File::open(file.path()) else {
            continue;
        };
        // Line by line: a week of a busy tenant's traces needn't be in memory at once.
        for line in std::io::BufReader::new(f).lines() {
            let Ok(line) = line else {
                break;
            };
            if !line.contains("\"endpoint\":\"search\"") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            // Whose the search was: its user; else its one group; with several groups and no
            // user it can't be told, so it isn't counted; with neither, the tenant's.
            let groups: Vec<&str> = v["groups"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|g| g.as_str())
                .collect();
            let who = match (v["user"].as_str(), groups.as_slice()) {
                (Some(u), _) => u.to_owned(),
                (None, []) => "tenant".to_owned(),
                (None, [g]) => (*g).to_owned(),
                (None, _) => continue,
            };
            // A deleted owner's searches from before the deletion stay out.
            let at = v["at"].as_str().unwrap_or("");
            if deleted.get(&who).is_some_and(|when| at <= when.as_str()) {
                continue;
            }
            let r = &v["result"];
            let is_weak =
                r["memories"]["coverage"] == "weak" && r["knowledge"]["coverage"] == "weak";
            if is_weak {
                let words = v["request"]["keywords"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(r["unmatched_keywords"].as_array().into_iter().flatten())
                    .filter_map(|k| k.as_str());
                let mut seen = HashSet::new();
                for k in words {
                    let key = normalize(k.trim());
                    if !key.is_empty() && seen.insert(key.clone()) {
                        weak.entry((who.clone(), key))
                            .or_insert((k.trim().to_owned(), 0))
                            .1 += 1;
                    }
                }
            }
            for p in r["partial_names"].as_array().into_iter().flatten() {
                let Some(k) = p["keyword"].as_str() else {
                    continue;
                };
                let mut nodes: Vec<String> = p["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|n| n.as_str())
                    .filter(|n| shared_node(graph, n))
                    .map(str::to_owned)
                    .collect();
                if nodes.is_empty() {
                    continue;
                }
                nodes.sort();
                partial
                    .entry((who.clone(), (normalize(k.trim()), nodes)))
                    .or_insert((k.trim().to_owned(), 0))
                    .1 += 1;
            }
        }
    }
    let finding = |audience: String, kind, detail: String, target: String, nodes, count| Stored {
        audience,
        hashes: BTreeMap::new(),
        finding: Finding {
            kind,
            pages: Vec::new(),
            detail,
            target: Some(target),
            claims: Vec::new(),
            nodes,
            count: Some(count),
            proposal: None,
        },
    };
    let named: HashSet<String> = catalog
        .pages
        .iter()
        .flat_map(|p| {
            std::iter::once(&p.title)
                .chain(&p.aliases)
                .chain(&p.keywords)
        })
        .map(|t| normalize(t.trim()))
        .collect();
    let mut weak: Vec<(String, String, u32)> = weak
        .into_iter()
        .filter(|((_, key), (_, n))| *n >= MIN_SEARCHES && !named.contains(key))
        .map(|((who, _), (k, n))| (who, k, n))
        .collect();
    weak.sort_by(|a, b| b.2.cmp(&a.2).then(a.1.cmp(&b.1)).then(a.0.cmp(&b.0)));
    let mut partial: Vec<(String, Vec<String>, String, u32)> = partial
        .into_iter()
        .filter(|(_, (_, n))| *n >= MIN_SEARCHES)
        .map(|((who, (_, nodes)), (k, n))| (who, nodes, k, n))
        .collect();
    partial.sort_by(|a, b| b.3.cmp(&a.3).then(a.2.cmp(&b.2)).then(a.0.cmp(&b.0)));
    let mut out: Vec<Stored> = weak
        .into_iter()
        .map(|(who, k, n)| {
            finding(
                who,
                FindingKind::FrequentMiss,
                format!("{n} searches with this keyword in the last {TRACE_DAYS} days found no strong memory or knowledge page"),
                k,
                Vec::new(),
                n,
            )
        })
        .collect();
    out.extend(partial.into_iter().map(|(who, nodes, k, n)| {
        finding(
            who,
            FindingKind::PartialName,
            format!("{n} searches named these nodes only by part of their names; add the keyword to their aliases in the graph data"),
            k,
            nodes,
            n,
        )
    }));
    out
}

fn shared_node(graph: &GraphSnapshot, id: &str) -> bool {
    match EntityRef::parse(id) {
        Some(EntityRef::Node { space, .. }) => {
            graph.node(id).is_some()
                && graph
                    .space(space)
                    .is_some_and(|s| s.def.visibility == Visibility::Shared)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_in_text() {
        let d = |s| crate::trace::parse_date(s).unwrap();
        assert_eq!(
            dates("valid until 2026-06-30, then 2026/7/1 and 2026年8月5日; not 12026-01-01x"),
            vec![
                (d("2026-06-30"), "2026-06-30".into()),
                (d("2026-07-01"), "2026/7/1".into()),
                (d("2026-08-05"), "2026年8月5".into())
            ]
        );
        assert!(dates("version 1.2.3 and 2026-13-01 and 2026-10").is_empty());
    }

    #[test]
    fn claims_are_found_despite_spacing() {
        let page = "# Refunds\n\n- Refunds take   7 days.\n";
        assert!(has_text(page, "- Refunds take 7 days."));
        assert!(has_text(page, "Refunds take 7\ndays."));
        assert!(!has_text(page, "Refunds take 5 days."));
        assert!(!has_text(page, "  "));
    }
}
