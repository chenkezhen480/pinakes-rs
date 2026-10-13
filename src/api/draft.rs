//! Agents propose changes to graph data, for a person to review (design, "Agents propose graph
//! changes"): `POST /graph/draft` merges changes into the space's pending draft (or, with no
//! changes, only shows it) and `GET /graph/draft?space=` shows it. Drafts are held proposals
//! (`/proposals`), approved or rejected by the admin like every proposal; approving applies the
//! whole draft at once. Any caller who may see a space may propose changes to it. Nothing here
//! touches the wiki: graph data doesn't.

use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::ApiError;
use crate::graph::def::Visibility;
use crate::graph::draft::{Change, DraftNote, GraphDraft};
use crate::graph::{GraphSnapshot, SpaceEntry};
use crate::identity::Scope;
use crate::jobs::{JobInfo, JobResult};
use crate::kinds::Kind;
use crate::librarian::{self, Proposal, ProposalStatus};
use crate::tenants::Tenant;

/// What an approval that no longer applies fails with.
pub const NO_LONGER_APPLIES: &str = "the draft no longer applies";
/// Notes a draft keeps, the latest.
const MAX_NOTES: usize = 100;
const GONE: &str = "the proposal is gone";
const ALREADY: &str = "the proposal is already decided";

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DraftRequest {
    pub space: String,
    /// Changes to merge into the draft; none to only see it. Read as [`Change`]s, fields that
    /// are `null` left out: an agent following the tool's one schema for every kind of change
    /// may send each field, `null` where it doesn't apply.
    #[serde(default)]
    pub changes: Vec<serde_json::Value>,
    /// Why, for the person who reviews it; required with changes.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DraftQuery {
    pub space: String,
}

/// A space's pending draft: its proposal (none when nothing is pending) and its net change.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct DraftView {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal: Option<String>,
    #[serde(flatten)]
    pub draft: GraphDraft,
}

/// What approving a draft applied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct GraphApplied {
    pub space: String,
    /// The draft deleted the whole space.
    #[serde(default)]
    pub space_deleted: bool,
    pub nodes_put: usize,
    pub nodes_removed: usize,
    pub edges_added: usize,
    pub edges_removed: usize,
}

/// The audience a space's draft is listed and deleted with: a private space's owner, else the
/// tenant (a restricted space's draft is also checked against the space when listed).
fn audience_of(space: &SpaceEntry) -> String {
    match (&space.def.visibility, &space.owner_dir) {
        (Visibility::Private, Some(owner)) => owner.clone(),
        _ => "tenant".to_owned(),
    }
}

/// The held draft of a space, if any.
async fn held(tenant: &Tenant, space: &str) -> anyhow::Result<Option<Proposal>> {
    Ok(tenant
        .store
        .held_proposals()
        .await?
        .into_iter()
        .find(|p| p.graph.as_ref().is_some_and(|g| g.space == space)))
}

fn view(space: &str, p: Option<Proposal>) -> DraftView {
    match p {
        Some(p) => DraftView {
            proposal: Some(p.id),
            draft: p
                .graph
                .map(|g| g.shown())
                .unwrap_or_else(|| GraphDraft::new(space)),
        },
        None => DraftView {
            proposal: None,
            draft: GraphDraft::new(space),
        },
    }
}

#[tracing::instrument(name = "graph_draft_get", skip_all)]
pub async fn get(
    scope: Scope,
    q: Result<Query<DraftQuery>, QueryRejection>,
) -> Result<Json<DraftView>, ApiError> {
    let Query(q) = q?;
    let tenant = scope.tenant();
    let graph = tenant.graph().await?;
    if graph
        .space(&q.space)
        .filter(|s| s.visible_to(&scope))
        .is_none()
    {
        return Err(ApiError::NotFound);
    }
    Ok(Json(view(&q.space, held(tenant, &q.space).await?)))
}

#[tracing::instrument(name = "graph_draft_propose", skip_all)]
pub async fn propose(
    State(_app): State<AppState>,
    scope: Scope,
    body: Result<Json<DraftRequest>, JsonRejection>,
) -> Result<Json<DraftView>, ApiError> {
    let Json(req) = body?;
    let tenant = scope.tenant();
    // Drafts change under the graph writer, so a draft and an approval never cross.
    let _writer = tenant.lock_graph().await?;
    let graph = tenant.graph().await?;
    let Some(space) = graph.space(&req.space).filter(|s| s.visible_to(&scope)) else {
        return Err(ApiError::NotFound);
    };
    let pending = held(tenant, &req.space).await?;
    if req.changes.is_empty() {
        return Ok(Json(view(&req.space, pending)));
    }
    let reason = req.reason.as_deref().map(str::trim).unwrap_or_default();
    if reason.is_empty() || reason.chars().count() > 1000 {
        return Err(ApiError::BadRequest(
            "reason must say why, for the person who reviews it, in at most 1000 characters".into(),
        ));
    }
    let changes: Vec<Change> = req
        .changes
        .into_iter()
        .enumerate()
        .map(|(i, mut c)| {
            if let Some(o) = c.as_object_mut() {
                o.retain(|_, v| !v.is_null());
            }
            serde_json::from_value(c).map_err(|e| format!("change {}: {e}", i + 1))
        })
        .collect::<Result<_, _>>()
        .map_err(ApiError::BadRequest)?;
    let schema = graph
        .schemas
        .get(&space.def.schema)
        .ok_or_else(|| anyhow::anyhow!("space {} has no stored schema", req.space))?;
    let mut draft = pending
        .as_ref()
        .and_then(|p| p.graph.clone())
        .map(|g| *g)
        .unwrap_or_else(|| GraphDraft::new(&req.space));
    let n = changes.len();
    draft
        .amend(&graph, schema, changes)
        .map_err(ApiError::BadRequest)?;
    // The latest notes are kept; a long-lived draft doesn't grow without end.
    if draft.notes.len() >= MAX_NOTES {
        draft.notes.drain(..=draft.notes.len() - MAX_NOTES);
    }
    if let Some(u) = scope.user()
        && !draft.authors.contains(&u.dir)
    {
        draft.authors.push(u.dir.clone());
    }
    draft.notes.push(DraftNote {
        at: crate::trace::now_rfc3339(),
        by: scope.user().map(|u| u.dir.clone()),
        reason: reason.to_owned(),
        changes: n,
    });
    if draft.is_empty() {
        // Everything it held was taken back: nothing is left to review.
        if let Some(p) = pending {
            tenant.store.delete_proposal(&p.id).await?;
        }
        return Ok(Json(DraftView {
            proposal: None,
            draft: draft.shown(),
        }));
    }
    let p = match pending {
        Some(mut p) => {
            p.graph = Some(Box::new(draft));
            p
        }
        None => Proposal {
            id: librarian::new_proposal_id(&tenant.dir_name),
            status: ProposalStatus::Held,
            created_at: crate::trace::now_rfc3339(),
            kind: Kind::Graph,
            audience: audience_of(space),
            source: None,
            edits: Vec::new(),
            user: None,
            groups: Vec::new(),
            candidate: None,
            reason: None,
            graph: Some(Box::new(draft)),
        },
    };
    tenant.store.put_proposal(&p).await?;
    tenant
        .store
        .audit(
            scope.user().map_or("tenant", |u| u.dir.as_str()),
            "graph.draft",
            &format!("{}: {n} changes", p.id),
        )
        .await?;
    tracing::info!(proposal = %p.id, space = %req.space, changes = n, "graph draft changed");
    Ok(Json(view(&req.space, Some(p))))
}

/// Whether a caller may see a draft: it may see its space.
pub(crate) fn sees(graph: &GraphSnapshot, scope: &Scope, p: &Proposal) -> bool {
    match &p.graph {
        Some(g) => graph.space(&g.space).is_some_and(|s| s.visible_to(scope)),
        None => true,
    }
}

/// Approves a held draft on the tenant's write queue: checked again against the graph as it
/// is now (the host writes too), then applied whole in one transaction. Answers the finished
/// job; one that no longer applies fails with [`NO_LONGER_APPLIES`] and stays held.
pub(crate) async fn approve(
    app: &AppState,
    tenant: Arc<Tenant>,
    id: String,
) -> Result<JobInfo, ApiError> {
    let t = tenant.clone();
    let job = app
        .jobs
        .run(
            &tenant.dir_name,
            None,
            "approve",
            Box::pin(async move {
                let _writer = t.lock_graph().await?;
                let mut p = t
                    .store
                    .proposal(&id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("{GONE}"))?;
                anyhow::ensure!(p.status == ProposalStatus::Held, "{ALREADY}");
                let draft = p
                    .graph
                    .clone()
                    .map(|g| *g)
                    .ok_or_else(|| anyhow::anyhow!("not a draft"))?;
                let graph = t.graph().await?;
                if graph.space(&draft.space).is_none() {
                    anyhow::bail!("{NO_LONGER_APPLIES}: space {} is gone", draft.space);
                }
                if draft.delete_space {
                    drop(graph);
                    super::ingest::remove_space(&t, &draft.space).await?;
                    p.status = ProposalStatus::Approved;
                    t.store.put_proposal(&p).await?;
                    t.store.audit("admin", "proposal.approve", &p.id).await?;
                    tracing::info!(proposal = %p.id, space = %draft.space, "graph space deleted");
                    return Ok(JobResult {
                        graph: Some(GraphApplied {
                            space: draft.space.clone(),
                            space_deleted: true,
                            ..GraphApplied::default()
                        }),
                        ..JobResult::default()
                    });
                }
                let conflicts = draft.conflicts(&graph);
                if !conflicts.is_empty() {
                    anyhow::bail!("{NO_LONGER_APPLIES}: {}", conflicts.join("; "));
                }
                t.graph_store
                    .apply(
                        &draft.remove_nodes,
                        &draft.remove_edges,
                        &draft.put_nodes,
                        &draft.add_edges,
                    )
                    .await?;
                let (rn, re) = (draft.remove_nodes.clone(), draft.remove_edges.clone());
                let (pn, ae) = (draft.put_nodes.clone(), draft.add_edges.clone());
                let next = GraphSnapshot::build_blocking(graph, move |g| {
                    g.without(&rn, &re).with_data(pn, ae)
                })
                .await?;
                t.set_graph(next);
                p.status = ProposalStatus::Approved;
                t.store.put_proposal(&p).await?;
                t.store.audit("admin", "proposal.approve", &p.id).await?;
                tracing::info!(proposal = %p.id, space = %draft.space, "graph draft applied");
                Ok(JobResult {
                    graph: Some(GraphApplied {
                        space: draft.space.clone(),
                        space_deleted: false,
                        nodes_put: draft.put_nodes.len(),
                        nodes_removed: draft.remove_nodes.len(),
                        edges_added: draft.add_edges.len(),
                        edges_removed: draft.remove_edges.len(),
                    }),
                    ..JobResult::default()
                })
            }),
        )
        .await;
    match job.error.as_deref() {
        None => Ok(job),
        Some(GONE) => Err(ApiError::NotFound),
        Some(ALREADY) => Err(ApiError::Conflict("the proposal is already decided".into())),
        Some(e) if e.starts_with(NO_LONGER_APPLIES) => Err(ApiError::Conflict(e.to_owned())),
        Some(e) => Err(ApiError::Internal(anyhow::anyhow!("approving failed: {e}"))),
    }
}

/// Removes the drafts a deleted user wrote into (their reasons and the names they proposed are
/// their text), whatever their status. Callers delete the user on the tenant's write queue.
pub(crate) async fn forget_author(tenant: &Tenant, user_dir: &str) -> anyhow::Result<()> {
    for p in tenant.store.proposals().await? {
        let wrote = p.graph.as_ref().is_some_and(|g| {
            g.authors.iter().any(|a| a == user_dir)
                || g.notes.iter().any(|n| n.by.as_deref() == Some(user_dir))
        });
        if wrote {
            tenant.store.delete_proposal(&p.id).await?;
        }
    }
    Ok(())
}
