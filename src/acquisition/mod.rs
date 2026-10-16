//! Filling gaps from outside, with approval (design, "Filling gaps from outside, with approval"
//! and its "Plan for M9"). Off unless the deployment turns it on (`[acquisition]`) and the
//! tenant does too (`PUT /admin/tenants/{id}/acquisition`, with its allowlist of trusted
//! sites). Only those sites are ever read: no search engine or other third party is asked
//! anything, so what a tenant is missing never leaves the service.
//!
//! After each lint run the trusted sites are read again when they are due (`crawl`, in the
//! background: a crawl takes minutes and writes no page) into a local index of their pages
//! (title, headings, terms and a vector, no text). Then a run on the tenant's write queue takes
//! that lint run's weak keywords no page covers and its stale claims, matches each against the
//! index by its words and meaning, has the librarian pick at most two of the best pages from
//! their titles and headings, fetches each again (allowlisted public hosts only), has the
//! converter turn it into Markdown and the librarian screen its text: a page holding
//! instructions aimed at an AI, or not filling the gap, is turned back and noted. What passes is
//! saved as a raw record (`sources/<date>-external-<hex>.json`, with the original) and held as a
//! proposal of a source. Compared with uploading a document, that review is the one step more:
//! approving it files the record as expertise through the same reading, planning and
//! verifying; rejecting it removes the record, and the address is never proposed again.

pub mod crawl;
pub mod web;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::AppState;
use crate::error::ApiError;
use crate::identity::Scope;
use crate::jobs::{JobInfo, JobResult};
use crate::kinds::Kind;
use crate::librarian::acquire::GapText;
use crate::librarian::lint::{FindingKind, Stored};
use crate::librarian::{
    DocumentIn, Ingest, PageAudience, Proposal, ProposalStatus, Target, sources,
};
use crate::tenants::Tenant;
use crawl::{CrawlReport, CrawledPage};

/// Days a gap rests after it was searched for.
const GAP_REST_DAYS: i64 = 30;
/// Most gaps one run searches for.
const MAX_GAPS: usize = 5;
/// Most pages fetched (and proposed) per gap.
const PICKS: usize = 2;
/// The most of a fetched page's Markdown that is screened, and so the most that is filed.
pub const MAX_TEXT: usize = 40_000;
/// Refusals and rejections kept to show.
const KEEP_NOTES: usize = 50;
/// Rejection reasons shown to the librarian when it chooses among results.
const SHOW_REJECTIONS: usize = 10;
/// Most entries on an allowlist, and start addresses per entry.
const MAX_SOURCES: usize = 50;
const MAX_URLS: usize = 50;
/// Pages of the index a gap's best matches are chosen from.
const CANDIDATES: usize = 8;
/// A page matches a gap when it holds at least this share of the gap's terms, or is at least
/// this similar to it in meaning (jina-embeddings-v2-base-zh; search's floor is 0.25).
const MATCH_SHARE: f32 = 0.5;
const MATCH_MEANING: f32 = 0.3;

const SETTINGS_KEY: &str = "acquisition";
const STATE_KEY: &str = "acquisition.state";
/// Kept apart from the state: the crawl runs off the write queue, the rest on it.
const CRAWL_KEY: &str = "acquisition.crawl";

/// A tenant's own switch and the sites it trusts.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(rename = "AcquisitionSettings"))]
#[serde(deny_unknown_fields)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub sources: Vec<AllowedSource>,
}

/// One trusted site: a domain (its subdomains included), the license its content comes under,
/// whether the tenant may keep copies of it, and addresses to start reading it from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct AllowedSource {
    pub domain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Without it, a fetched page's text is kept only until it is filed; then only its address
    /// and what the librarian wrote from it stay.
    #[serde(default)]
    pub copy: bool,
    /// Pages of the site to start from, besides what its sitemaps list; their origins are
    /// where its `robots.txt` and sitemaps are read. Without them, `https://<domain>/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
}

impl Settings {
    /// Checks and tidies what a caller sent: domains lowercase, without scheme or path.
    pub fn checked(mut self) -> Result<Self, String> {
        if self.sources.len() > MAX_SOURCES {
            return Err(format!("at most {MAX_SOURCES} sources"));
        }
        let mut seen = BTreeSet::new();
        for s in &mut self.sources {
            let d = s.domain.trim().trim_end_matches('.').to_ascii_lowercase();
            let ok = !d.is_empty()
                && d.len() <= 253
                && d.contains('.')
                && !d.starts_with(['.', '-'])
                && d.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'));
            if !ok {
                return Err(format!(
                    "{:?} is not a domain (such as docs.example.com, without https:// or a path)",
                    s.domain
                ));
            }
            if !seen.insert(d.clone()) {
                return Err(format!("{d} is listed twice"));
            }
            s.domain = d;
            s.license = s
                .license
                .take()
                .map(|l| l.trim().to_owned())
                .filter(|l| !l.is_empty());
            if s.license.as_ref().is_some_and(|l| l.chars().count() > 200) {
                return Err("a license is at most 200 characters".into());
            }
            if s.urls.len() > MAX_URLS {
                return Err(format!("at most {MAX_URLS} start addresses per site"));
            }
            for u in &mut s.urls {
                *u = u.trim().to_owned();
                let ok = reqwest::Url::parse(u).is_ok_and(|url| {
                    matches!(url.scheme(), "http" | "https")
                        && url
                            .host_str()
                            .is_some_and(|h| web::allowed(h, std::slice::from_ref(&s.domain)))
                });
                if !ok {
                    return Err(format!("{u:?} is not an address on {}", s.domain));
                }
            }
        }
        Ok(self)
    }

    /// What a crawl read: a changed allowlist is read again at once.
    fn crawl_key(&self) -> Vec<String> {
        self.sources
            .iter()
            .map(|s| {
                format!("{} {}", s.domain, s.urls.join(" "))
                    .trim()
                    .to_owned()
            })
            .collect()
    }

    fn domains(&self) -> Vec<String> {
        self.sources.iter().map(|s| s.domain.clone()).collect()
    }

    /// The allowlist entry a host falls under.
    fn source_of(&self, host: &str) -> Option<&AllowedSource> {
        self.sources
            .iter()
            .find(|s| web::allowed(host, std::slice::from_ref(&s.domain)))
    }
}

/// What a tenant's runs remember.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// The UTC day the token count is for, and the count.
    #[serde(default)]
    pub day: String,
    #[serde(default)]
    pub tokens: u64,
    /// When each gap (`audience|kind|target`) was last searched for.
    #[serde(default)]
    pub gaps: BTreeMap<String, String>,
    /// Addresses proposed, turned back or rejected: never proposed again.
    #[serde(default)]
    pub seen: BTreeSet<String>,
    /// Pages turned back by a run, newest last.
    #[serde(default)]
    pub refused: Vec<Note>,
    /// Proposals a person rejected, newest last.
    #[serde(default)]
    pub rejected: Vec<Note>,
    #[serde(default)]
    pub last_run: Option<RunReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Note {
    pub at: String,
    pub url: String,
    pub reason: String,
    /// Whose gap it was for (`tenant`, or a group or user folder): who may see it, and what
    /// deleting that owner removes.
    #[serde(default = "tenant")]
    pub owner: String,
}

fn tenant() -> String {
    "tenant".to_owned()
}

/// What a run did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct RunReport {
    pub at: String,
    /// Gaps found in the last lint run, and how many were taken this run.
    pub gaps: usize,
    pub gaps_taken: usize,
    /// Each gap matched against the trusted sites' pages, with how many pages matched.
    pub matched: Vec<Matched>,
    /// Gaps nothing was proposed for before choosing, with why.
    pub skipped: Vec<Skipped>,
    pub proposals: Vec<String>,
    /// Of those, the ones approved at once (`[librarian] auto_approve`): their filing jobs are
    /// queued after this run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved: Vec<String>,
    /// Pages turned back this run (the reasons are at `GET /acquisition`).
    pub refused: usize,
    pub tokens: u64,
    pub budget_left: u64,
    /// Why the run stopped early, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Matched {
    pub gap: String,
    /// Pages of the index that matched it (at most the few shown to the librarian).
    pub pages: usize,
    /// Whose gap it was.
    #[serde(default = "tenant")]
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Skipped {
    pub gap: String,
    pub reason: String,
    #[serde(default = "tenant")]
    pub owner: String,
}

/// A gap: a weak keyword or a stale claim from lint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Gap {
    /// Whose searches or pages it came from (the finding's audience): usually the audience
    /// its answer is filed for, but a personal deployment's user's gaps are filed for the
    /// tenant. Deleting that owner removes what acquisition kept of the gap.
    #[serde(default = "tenant")]
    pub owner: String,
    /// `frequent_miss` or `stale`.
    pub kind: FindingKind,
    /// The keyword, or the stale claim's page.
    pub target: String,
    pub detail: String,
    /// For a stale claim: the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
}

impl Gap {
    /// What a run remembers the gap by; it starts with the owner's folder.
    fn key(&self) -> String {
        let kind = match self.kind {
            FindingKind::Stale => "stale",
            _ => "miss",
        };
        format!(
            "{}|{kind}|{}|{}",
            self.owner,
            self.target,
            self.claim.as_deref().unwrap_or("")
        )
    }

    /// How it is shown in proposals and reports.
    fn label(&self) -> String {
        match (&self.kind, &self.claim) {
            (FindingKind::Stale, Some(c)) => format!("stale claim on {}: {c}", self.target),
            _ => format!("weak keyword: {}", self.target),
        }
    }
}

/// A page of a trusted site, matched to a gap and shown to the librarian to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub title: String,
    pub url: String,
    /// Its headings, as what it covers.
    pub snippet: String,
}

/// A page found outside and proposed as a source, held in the review queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Candidate {
    pub gap: Gap,
    pub url: String,
    pub title: String,
    /// Its headings, as the librarian saw them when choosing.
    pub snippet: String,
    /// Why the librarian chose it.
    pub why: String,
    /// What the screening says the page holds.
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub copy: bool,
    pub fetched_at: String,
}

pub async fn settings(tenant: &Tenant) -> anyhow::Result<Settings> {
    Ok(match tenant.store.meta(SETTINGS_KEY).await? {
        Some(j) => serde_json::from_str(&j).context("stored acquisition settings")?,
        None => Settings::default(),
    })
}

pub async fn put_settings(tenant: &Tenant, s: &Settings) -> anyhow::Result<()> {
    tenant
        .store
        .set_meta(SETTINGS_KEY, &serde_json::to_string(s)?)
        .await
}

pub async fn state(tenant: &Tenant) -> anyhow::Result<State> {
    Ok(match tenant.store.meta(STATE_KEY).await? {
        Some(j) => serde_json::from_str(&j).context("stored acquisition state")?,
        None => State::default(),
    })
}

async fn put_state(tenant: &Tenant, s: &State) -> anyhow::Result<()> {
    tenant
        .store
        .set_meta(STATE_KEY, &serde_json::to_string(s)?)
        .await
}

fn push_note(list: &mut Vec<Note>, note: Note) {
    list.push(note);
    if list.len() > KEEP_NOTES {
        list.drain(..list.len() - KEEP_NOTES);
    }
}

/// Tenants whose sites are being read, and then run on: one at a time per tenant.
static UNDER_WAY: std::sync::LazyLock<std::sync::Mutex<BTreeSet<String>>> =
    std::sync::LazyLock::new(Default::default);

/// A tenant's place on [`UNDER_WAY`], given up when dropped.
struct UnderWay(String);

impl Drop for UnderWay {
    fn drop(&mut self) {
        UNDER_WAY
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.0);
    }
}

/// The last crawl, if the sites were read.
pub async fn last_crawl(tenant: &Tenant) -> anyhow::Result<Option<CrawlReport>> {
    Ok(match tenant.store.meta(CRAWL_KEY).await? {
        Some(j) => Some(serde_json::from_str(&j).context("the stored crawl report")?),
        None => None,
    })
}

/// After a lint run, when the deployment and the tenant have acquisition on: reads the trusted
/// sites again when they are due (`[acquisition] crawl_every_days`, or the allowlist changed),
/// in the background and off the write queue, then queues a run on it. A tenant whose sites are
/// still being read gets no second crawl.
pub async fn after_lint(app: &AppState, tenant: &Arc<Tenant>) {
    if !app.config.acquisition.enabled {
        return;
    }
    let settings = match settings(tenant).await {
        Ok(s) if s.enabled && !s.sources.is_empty() => s,
        Ok(_) => return,
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "acquisition settings can't be read");
            return;
        }
    };
    // By the tenant's folder on disk: unique, also when two services share a process (tests).
    let busy_key = tenant.dir.display().to_string();
    if !UNDER_WAY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(busy_key.clone())
    {
        return;
    }
    let (app, tenant) = (app.clone(), tenant.clone());
    tokio::spawn(async move {
        // Taken off the list however the crawl ends, a panic included.
        let under_way = UnderWay(busy_key);
        if let Err(e) = crawl_if_due(&app, &tenant, &settings).await {
            tracing::warn!(error = %format!("{e:#}"), "the trusted sites could not be read");
        }
        drop(under_way);
        let (a, t) = (app.clone(), tenant.clone());
        let info = app.jobs.submit_once(
            &tenant.dir_name,
            "acquisition",
            Box::pin(async move { run(a, t).await }),
        );
        tracing::info!(job = %info.job, "acquisition queued after lint");
    });
}

async fn crawl_if_due(
    app: &AppState,
    tenant: &Arc<Tenant>,
    settings: &Settings,
) -> anyhow::Result<()> {
    let last = last_crawl(tenant).await?;
    let due = last.as_ref().is_none_or(|c| {
        c.sites != settings.crawl_key()
            || days_since(&c.at, crate::trace::today())
                .is_none_or(|d| d >= i64::from(app.config.acquisition.crawl_every_days))
    });
    if !due {
        return Ok(());
    }
    let mut report = crawl::crawl(app, tenant, settings).await?;
    report.sites = settings.crawl_key();
    tenant
        .store
        .set_meta(CRAWL_KEY, &serde_json::to_string(&report)?)
        .await
}

/// The gaps of a lint run that may be filled from outside, with the audience the answer is
/// filed for: the tenant's own and its groups' (a personal deployment's one user's are the
/// tenant's), never another user's, whose searches a tenant page would reveal.
fn gaps_of(findings: &[Stored], personal_user: Option<&str>) -> Vec<(String, Gap)> {
    let mut out = Vec::new();
    for s in findings {
        let f = &s.finding;
        let audience = if s.audience == "tenant" || s.audience.starts_with("g_") {
            s.audience.clone()
        } else if Some(s.audience.as_str()) == personal_user {
            "tenant".to_owned()
        } else {
            continue;
        };
        let gap = match f.kind {
            // A weak keyword a page covers got its fix as a proposal: not a gap.
            FindingKind::FrequentMiss if f.proposal.is_none() => match &f.target {
                Some(k) => Gap {
                    owner: s.audience.clone(),
                    kind: f.kind,
                    target: k.clone(),
                    detail: f.detail.clone(),
                    claim: None,
                },
                None => continue,
            },
            FindingKind::Stale => match (f.pages.first(), f.claims.first()) {
                (Some(page), claim) => Gap {
                    owner: s.audience.clone(),
                    kind: f.kind,
                    target: page.clone(),
                    detail: f.detail.clone(),
                    claim: claim.map(|c| c.text.clone()),
                },
                _ => continue,
            },
            _ => continue,
        };
        if !out.iter().any(|(a, g)| *a == audience && *g == gap) {
            out.push((audience, gap));
        }
    }
    out
}

fn days_since(date: &str, today: i64) -> Option<i64> {
    crate::trace::parse_date(date.get(..10)?).map(|d| today - d)
}

/// One run over a tenant. Runs on its write queue.
#[tracing::instrument(name = "acquisition", skip_all, fields(tenant = %tenant.dir_name))]
pub async fn run(app: AppState, tenant: Arc<Tenant>) -> anyhow::Result<JobResult> {
    let mut state = state(&tenant).await?;
    let now = crate::trace::now_rfc3339();
    let today = now[..10].to_owned();
    if state.day != today {
        state.day = today.clone();
        state.tokens = 0;
    }
    let budget = app.config.acquisition.daily_token_budget;
    let mut report = RunReport {
        at: now,
        ..RunReport::default()
    };
    let outcome = run_inner(&app, &tenant, &mut state, &mut report, budget).await;
    report.budget_left = budget.saturating_sub(state.tokens);
    state.last_run = Some(report.clone());
    put_state(&tenant, &state).await?;
    outcome?;
    let line = format!(
        "acquisition: {} gaps, {} matched, {} proposals, {} turned back",
        report.gaps_taken,
        report.matched.len(),
        report.proposals.len(),
        report.refused
    );
    tenant
        .store
        .audit("acquisition", "acquisition.run", &line)
        .await?;
    tracing::info!(
        proposals = report.proposals.len(),
        refused = report.refused,
        tokens = report.tokens,
        "acquisition done"
    );
    Ok(JobResult {
        acquisition: Some(report),
        ..JobResult::default()
    })
}

async fn run_inner(
    app: &AppState,
    tenant: &Arc<Tenant>,
    state: &mut State,
    report: &mut RunReport,
    budget: u64,
) -> anyhow::Result<()> {
    let settings = settings(tenant).await?;
    if !app.config.acquisition.enabled {
        report.stopped = Some("acquisition is off for this deployment".into());
        return Ok(());
    }
    if !settings.enabled || settings.sources.is_empty() {
        report.stopped = Some("acquisition is off for this tenant, or it trusts no site".into());
        return Ok(());
    }
    let librarian = app.librarian().map_err(|e| anyhow::anyhow!("{e}"))?;
    let Some((_, findings)) = tenant.store.lint_run().await? else {
        report.stopped = Some("lint hasn't run yet".into());
        return Ok(());
    };
    let domains = settings.domains();
    let index: Vec<CrawledPage> = tenant
        .store
        .crawled_pages()
        .await?
        .into_iter()
        .filter(|p| domains.contains(&p.site))
        .collect();
    if index.is_empty() {
        report.stopped = Some("nothing has been read from the trusted sites yet".into());
        return Ok(());
    }
    let personal = (app.config.identity.deployment == crate::config::Deployment::Personal)
        .then(|| crate::tenants::user_dir_name(&app.config.identity.default_user));
    let all = gaps_of(&findings, personal.as_deref());
    report.gaps = all.len();
    let today_n = crate::trace::today();
    let gaps: Vec<(String, Gap)> = all
        .into_iter()
        .filter(|(_, g)| {
            state
                .gaps
                .get(&g.key())
                .and_then(|d| days_since(d, today_n))
                .is_none_or(|d| d >= GAP_REST_DAYS)
        })
        .take(MAX_GAPS)
        .collect();
    report.gaps_taken = gaps.len();
    if gaps.is_empty() {
        return Ok(());
    }
    let snap = tenant.snapshot().await?;
    let texts: Vec<GapText> = gaps
        .iter()
        .enumerate()
        .map(|(i, (_, g))| GapText {
            id: format!("g{}", i + 1),
            kind: if g.kind == FindingKind::Stale {
                "stale"
            } else {
                "miss"
            },
            text: g.claim.clone().unwrap_or_else(|| g.target.clone()),
            page: (g.kind == FindingKind::Stale)
                .then(|| snap.catalog.get(&g.target).map(|(_, e)| e.title.clone()))
                .flatten(),
        })
        .collect();
    let spend = |state: &mut State, report: &mut RunReport, n: u64| {
        state.tokens += n;
        report.tokens += n;
    };
    let fetcher = web::Fetcher::new(&domains, app.config.acquisition.allow_private_hosts)
        .map_err(anyhow::Error::msg)?;
    let embedder = app.embedder.clone().filter(|e| e.ready());
    // Each page's terms, and its title's and headings', looked up once for every gap.
    let page_terms: Vec<BTreeSet<String>> = index
        .iter()
        .map(|p| p.terms.iter().cloned().collect())
        .collect();
    let head_terms: Vec<BTreeSet<String>> = index
        .iter()
        .map(|p| {
            crawl::terms(&format!("{}\n{}", p.title, p.headings.join("\n")))
                .into_iter()
                .collect()
        })
        .collect();
    let today = state.day.clone();
    for (i, (audience, gap)) in gaps.iter().enumerate() {
        let text = &texts[i];
        let wanted = match &text.page {
            Some(title) => format!("{title}\n{}", text.text),
            None => text.text.clone(),
        };
        let want_terms = crawl::terms(&wanted);
        let want_vector = match &embedder {
            Some(e) => {
                let (e, w) = (e.clone(), wanted.clone());
                tokio::task::spawn_blocking(move || e.embed(&[w]))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .and_then(|mut v| v.pop())
            }
            None => None,
        };
        let share = |set: &BTreeSet<String>| {
            if want_terms.is_empty() {
                0.0
            } else {
                want_terms.iter().filter(|t| set.contains(*t)).count() as f32
                    / want_terms.len() as f32
            }
        };
        let mut scored: Vec<(f32, usize)> = index
            .iter()
            .enumerate()
            .filter(|(_, p)| !state.seen.contains(&p.url))
            .filter_map(|(j, p)| {
                let words = share(&page_terms[j]);
                let meaning = match (&want_vector, &p.vector) {
                    (Some(a), Some(b)) if a.len() == b.len() => {
                        a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
                    }
                    _ => 0.0,
                };
                (words >= MATCH_SHARE || meaning >= MATCH_MEANING)
                    .then(|| (words + 0.5 * share(&head_terms[j]) + meaning, j))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.truncate(CANDIDATES);
        if !scored.is_empty() && state.tokens >= budget {
            // Left for a run with budget: the gap isn't marked searched.
            report.stopped = Some("today's token budget is spent".into());
            return Ok(());
        }
        state.gaps.insert(gap.key(), today.clone());
        report.matched.push(Matched {
            gap: gap.label(),
            pages: scored.len(),
            owner: gap.owner.clone(),
        });
        if scored.is_empty() {
            report.skipped.push(Skipped {
                gap: gap.label(),
                reason: "no page of the trusted sites matches it".into(),
                owner: gap.owner.clone(),
            });
            continue;
        }
        let results: Vec<Found> = scored
            .iter()
            .map(|&(_, j)| Found {
                title: index[j].title.clone(),
                url: index[j].url.clone(),
                snippet: index[j].headings.join("; "),
            })
            .collect();
        let page_audience = PageAudience::from_key(audience);
        let mut terms = vec![gap.target.clone()];
        if let Some(c) = &gap.claim {
            terms.push(c.clone());
        }
        let related = librarian.gap_pages(tenant, &page_audience, &terms).await?;
        let rejections: Vec<String> = state
            .rejected
            .iter()
            .rev()
            .take(SHOW_REJECTIONS)
            .map(|n| n.reason.clone())
            .collect();
        let (picks, used) = librarian
            .acquire_rank(text, &related, &results, &rejections)
            .await?;
        spend(state, report, used);
        for (idx, pick) in picks.into_iter().take(PICKS) {
            let r = &results[idx];
            if state.tokens >= budget {
                report.stopped = Some("today's token budget is spent".into());
                return Ok(());
            }
            // Fetched again: the index keeps no text, and the page may have changed since.
            let fetched = match fetcher.page(&r.url).await {
                Ok(f) => f,
                // Fetching and converting may fail for a while (a site or the converter down):
                // noted, but the address may come back with the gap after its rest.
                Err(why) => {
                    refuse(
                        state,
                        report,
                        &gap.owner,
                        &r.url,
                        &format!("not fetched: {why}"),
                        false,
                    );
                    continue;
                }
            };
            let converted = match app
                .converter
                .convert(&fetched.ext, fetched.bytes.clone())
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    refuse(
                        state,
                        report,
                        &gap.owner,
                        &r.url,
                        &format!("not converted: {e}"),
                        false,
                    );
                    continue;
                }
            };
            let text_md: String = converted.markdown.trim().chars().take(MAX_TEXT).collect();
            if text_md.trim().is_empty() {
                refuse(
                    state,
                    report,
                    &gap.owner,
                    &r.url,
                    "the page has no text",
                    true,
                );
                continue;
            }
            let (screen, used) = librarian
                .acquire_screen(text, &r.title, &fetched.url, &text_md)
                .await?;
            spend(state, report, used);
            if let Some(why) = screen.refusal(&text_md) {
                refuse(state, report, &gap.owner, &r.url, &why, true);
                continue;
            }
            let host = reqwest::Url::parse(&fetched.url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_default();
            let allowed = settings.source_of(&host);
            let candidate = Candidate {
                gap: gap.clone(),
                url: fetched.url.clone(),
                title: if r.title.trim().is_empty() {
                    fetched.url.clone()
                } else {
                    r.title.trim().chars().take(200).collect()
                },
                snippet: r.snippet.clone(),
                why: pick.reason.clone(),
                summary: screen.summary.clone(),
                license: allowed.and_then(|s| s.license.clone()),
                copy: allowed.is_some_and(|s| s.copy),
                fetched_at: crate::trace::now_rfc3339(),
            };
            let id = propose(tenant, audience, candidate, &text_md, &fetched, &converted).await?;
            state.seen.insert(r.url.clone());
            state.seen.insert(fetched.url.clone());
            report.proposals.push(id.clone());
            if app.config.librarian.auto_approve {
                // Approved at once, as the admin would: its filing is queued after this run.
                let p = tenant
                    .store
                    .proposal(&id)
                    .await?
                    .with_context(|| format!("proposal {id} was just held"))?;
                approve(app, tenant.clone(), p, "auto-approve")
                    .await
                    .map_err(|e| anyhow::anyhow!("approving {id} at once: {e:?}"))?;
                report.approved.push(id);
            }
        }
    }
    Ok(())
}

/// Notes a page turned back; with `for_good` its address is never fetched again (what it
/// holds was judged), otherwise it may be again once its gap has rested (it couldn't be read).
fn refuse(
    state: &mut State,
    report: &mut RunReport,
    owner: &str,
    url: &str,
    reason: &str,
    for_good: bool,
) {
    tracing::info!(url, reason, "a fetched page was turned back");
    if for_good {
        state.seen.insert(url.to_owned());
    }
    report.refused += 1;
    push_note(
        &mut state.refused,
        Note {
            at: crate::trace::now_rfc3339(),
            url: url.to_owned(),
            reason: reason.to_owned(),
            owner: owner.to_owned(),
        },
    );
}

/// Forgets what acquisition kept of a deleted user's or group's gaps, so none of their search
/// terms stay behind (design, "Lint and deletion"): the gaps a run remembers, the entries of
/// the last run and the notes for them, and every proposed source made for one of their gaps
/// whose audience isn't theirs (their own audience's proposals and records go with it), with
/// its record if it wasn't filed. Callers hold the tenant's wiki writer, on its write queue.
pub async fn forget_owner(tenant: &Arc<Tenant>, owner: &str) -> anyhow::Result<()> {
    let mut s = state(tenant).await?;
    let prefix = format!("{owner}|");
    s.gaps.retain(|k, _| !k.starts_with(&prefix));
    s.refused.retain(|n| n.owner != owner);
    s.rejected.retain(|n| n.owner != owner);
    if let Some(run) = &mut s.last_run {
        run.matched.retain(|e| e.owner != owner);
        run.skipped.retain(|e| e.owner != owner);
    }
    put_state(tenant, &s).await?;
    for p in tenant.store.proposals().await? {
        let Some(c) = &p.candidate else {
            continue;
        };
        if c.gap.owner != owner {
            continue;
        }
        if p.status == ProposalStatus::Held
            && let Some(name) = &p.source
        {
            let (dir, name) = (tenant.dir.clone(), name.clone());
            tokio::task::spawn_blocking(move || sources::remove(&dir, &name))
                .await
                .context("removing a proposed source panicked")??;
        }
        tenant.store.delete_proposal(&p.id).await?;
    }
    Ok(())
}

/// Saves the fetched page as a raw record (with its original) and holds it as a proposal.
async fn propose(
    tenant: &Arc<Tenant>,
    audience: &str,
    c: Candidate,
    text: &str,
    fetched: &web::Fetched,
    converted: &crate::convert::Converted,
) -> anyhow::Result<String> {
    let today = crate::trace::now_rfc3339()[..10].to_owned();
    let name = sources::new_external_name(&today);
    let original = format!("{}.{}", name.trim_end_matches(".json"), fetched.ext);
    let record = json!({
        "title": c.title,
        "content": text,
        "source_url": c.url,
        "date": today,
        "entities": [],
        "external": {
            "url": c.url,
            "fetched_at": c.fetched_at,
            "license": c.license,
            "copy": c.copy,
            "kept": true,
        },
        "file": {
            "name": c.url,
            "original": original,
            "converter": converted.converter,
            "version": converted.version,
        },
    });
    // Noted before it is written, as every record is, so deleting its group finds it.
    tenant.store.add_record(&name, audience).await?;
    let (dir, n, rec, bytes) = (
        tenant.dir.clone(),
        name.clone(),
        record,
        fetched.bytes.clone(),
    );
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        sources::save_file(&dir, &original, &bytes)?;
        sources::save(&dir, &n, &rec)
    })
    .await
    .context("saving a fetched page panicked")??;
    let p = Proposal {
        id: crate::librarian::new_proposal_id(&tenant.dir_name),
        status: ProposalStatus::Held,
        created_at: crate::trace::now_rfc3339(),
        kind: Kind::Expertise,
        audience: audience.to_owned(),
        source: Some(name),
        edits: Vec::new(),
        user: None,
        groups: Vec::new(),
        candidate: Some(Box::new(c)),
        reason: None,
        graph: None,
    };
    tenant.store.put_proposal(&p).await?;
    tracing::info!(proposal = %p.id, "a page from outside is proposed");
    Ok(p.id)
}

/// Approves a proposed source: queues its filing as an expertise document for the gap's
/// audience (the normal reading, planning and verifying), which marks it approved once it is
/// filed; a filing that fails (the model unreachable) leaves it held, to approve again.
/// Answers the filing job, queued.
pub async fn approve(
    app: &AppState,
    tenant: Arc<Tenant>,
    proposal: Proposal,
    actor: &'static str,
) -> Result<JobInfo, ApiError> {
    let librarian = app.librarian()?;
    let candidate = proposal
        .candidate
        .clone()
        .ok_or_else(|| ApiError::BadRequest("not a proposed source".into()))?;
    let record_name = proposal
        .source
        .clone()
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("a proposed source without a record")))?;
    // Read before the proposal is marked, so one whose record is gone stays held.
    let dir = tenant.dir.clone();
    let name = record_name.clone();
    let record: serde_json::Value = tokio::task::spawn_blocking(move || sources::read(&dir, &name))
        .await
        .map_err(anyhow::Error::from)?
        .map_err(anyhow::Error::from)?
        .and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| ApiError::Internal(anyhow::anyhow!("the proposed record is gone")))?;
    let audience = PageAudience::from_key(&proposal.audience);
    let target = Target::new(Kind::Expertise, audience.clone()).map_err(ApiError::BadRequest)?;
    let writer = match &audience {
        PageAudience::Group(g) => Scope::offline(tenant.clone(), None, vec![g.clone()]),
        _ => Scope::offline(tenant.clone(), None, Vec::new()),
    };
    let title = record["title"].as_str().unwrap_or_default().to_owned();
    let content = record["content"].as_str().unwrap_or_default().to_owned();
    let job = Ingest {
        target,
        writer,
        read: Vec::new(),
        record,
        source: record_name.clone(),
        query: format!("{title}\n{content}"),
        keywords: vec![title.clone(), candidate.gap.target.clone()],
        entities: Vec::new(),
        document: Some(DocumentIn {
            given: None,
            title,
            content,
        }),
    };
    let copy = candidate.copy;
    let (t3, id) = (tenant.clone(), proposal.id.clone());
    let work: crate::jobs::Work = Box::pin(async move {
        // On the write queue, like a rejection: decided once, whichever comes first.
        let mut p = t3
            .store
            .proposal(&id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("{GONE}"))?;
        anyhow::ensure!(p.status == ProposalStatus::Held, "{ALREADY}");
        let result = librarian.ingest(t3.clone(), job).await?;
        p.status = ProposalStatus::Approved;
        t3.store.put_proposal(&p).await?;
        t3.store.audit(actor, "proposal.approve", &p.id).await?;
        if !copy {
            // The license doesn't say copies may be kept: only the address and what the
            // librarian wrote from it stay.
            drop_text(&t3, &record_name).await?;
        }
        Ok(result)
    });
    Ok(app
        .jobs
        .submit(&tenant.dir_name, None, Kind::Expertise.as_str(), work))
}

const GONE: &str = "the proposal is gone";
const ALREADY: &str = "the proposal is already decided";

/// Removes a fetched page's text and original once it is filed, keeping the record's address
/// and metadata.
async fn drop_text(tenant: &Arc<Tenant>, name: &str) -> anyhow::Result<()> {
    let _writer = tenant.lock_wiki().await?;
    let (dir, name) = (tenant.dir.clone(), name.to_owned());
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let Some(text) = sources::read(&dir, &name)? else {
            return Ok(());
        };
        let mut record: serde_json::Value = serde_json::from_str(&text)?;
        if let Some(original) = record["file"]["original"].as_str() {
            sources::remove_file(&dir, original)?;
        }
        if let Some(o) = record.as_object_mut() {
            o.remove("content");
            o.remove("file");
        }
        record["external"]["kept"] = json!(false);
        sources::replace(&dir, &name, &record)?;
        Ok(())
    })
    .await
    .context("dropping a fetched page's text panicked")?
}

/// Rejects a held proposal with a reason, on the write queue. A proposed source's record is
/// removed and its address never proposed again; the reason is shown to later runs.
pub async fn reject(
    app: &AppState,
    tenant: Arc<Tenant>,
    id: String,
    reason: String,
) -> Result<(), ApiError> {
    let t2 = tenant.clone();
    let done = app
        .jobs
        .run(
            &tenant.dir_name,
            None,
            "reject",
            Box::pin(async move {
                let mut p = t2
                    .store
                    .proposal(&id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("{GONE}"))?;
                anyhow::ensure!(p.status == ProposalStatus::Held, "{ALREADY}");
                p.status = ProposalStatus::Rejected;
                p.reason = Some(reason.clone());
                t2.store.put_proposal(&p).await?;
                if let Some(c) = &p.candidate {
                    if let Some(name) = &p.source {
                        let (dir, name) = (t2.dir.clone(), name.clone());
                        tokio::task::spawn_blocking(move || sources::remove(&dir, &name)).await??;
                    }
                    let mut s = state(&t2).await?;
                    s.seen.insert(c.url.clone());
                    push_note(
                        &mut s.rejected,
                        Note {
                            at: crate::trace::now_rfc3339(),
                            url: c.url.clone(),
                            reason: reason.clone(),
                            owner: c.gap.owner.clone(),
                        },
                    );
                    put_state(&t2, &s).await?;
                }
                t2.store.audit("admin", "proposal.reject", &p.id).await?;
                Ok(JobResult::default())
            }),
        )
        .await;
    match done.error.as_deref() {
        None => Ok(()),
        Some(GONE) => Err(ApiError::NotFound),
        Some(ALREADY) => Err(ApiError::Conflict("the proposal is already decided".into())),
        Some(other) => Err(ApiError::Internal(anyhow::anyhow!(
            "rejecting failed: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::librarian::lint::{Claim, Finding};

    fn stored(audience: &str, kind: FindingKind, target: Option<&str>, proposal: bool) -> Stored {
        Stored {
            audience: audience.into(),
            hashes: Default::default(),
            finding: Finding {
                kind,
                pages: vec!["expertise/returns".into()],
                detail: "d".into(),
                target: target.map(str::to_owned),
                claims: vec![Claim {
                    page: "expertise/returns".into(),
                    text: "valid until 2026-06-30".into(),
                }],
                nodes: Vec::new(),
                count: Some(3),
                proposal: proposal.then(|| "p_x".into()),
            },
        }
    }

    #[test]
    fn gaps_are_the_tenants_and_groups_unfixed_misses_and_stale_claims() {
        let findings = vec![
            stored("tenant", FindingKind::FrequentMiss, Some("退货"), false),
            stored("tenant", FindingKind::FrequentMiss, Some("covered"), true),
            stored("g_1", FindingKind::Stale, None, false),
            stored(
                "u_someone",
                FindingKind::FrequentMiss,
                Some("private"),
                false,
            ),
            stored("u_me", FindingKind::FrequentMiss, Some("mine"), false),
            stored("tenant", FindingKind::BrokenLink, Some("x"), false),
        ];
        let got = gaps_of(&findings, Some("u_me"));
        assert_eq!(
            got[2].1.owner, "u_me",
            "filed for the tenant, owned by the user"
        );
        assert!(got[2].1.key().starts_with("u_me|"));
        let summary: Vec<(String, String)> =
            got.iter().map(|(a, g)| (a.clone(), g.label())).collect();
        assert_eq!(
            summary,
            vec![
                ("tenant".into(), "weak keyword: 退货".into()),
                (
                    "g_1".into(),
                    "stale claim on expertise/returns: valid until 2026-06-30".into()
                ),
                ("tenant".into(), "weak keyword: mine".into()),
            ]
        );
    }

    #[test]
    fn settings_are_tidied_and_checked() {
        let s = Settings {
            enabled: true,
            sources: vec![AllowedSource {
                domain: " Docs.Example.com. ".into(),
                license: Some("  ".into()),
                copy: false,
                urls: Vec::new(),
            }],
        };
        let s = s.checked().unwrap();
        assert_eq!(s.sources[0].domain, "docs.example.com");
        assert_eq!(s.sources[0].license, None);
        for bad in ["https://a.com", "a.com/path", "localhost", ""] {
            let s = Settings {
                enabled: true,
                sources: vec![AllowedSource {
                    domain: bad.into(),
                    license: None,
                    copy: false,
                    urls: Vec::new(),
                }],
            };
            assert!(s.checked().is_err(), "{bad}");
        }
    }
}
