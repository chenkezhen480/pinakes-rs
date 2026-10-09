//! Deleting a user's or a group's data in one tenant (design, "Tenants, users and identity":
//! clean deletion). Hard deletion: the folder's pages, the raw records filed for it, its
//! proposals, documents and page vectors, its pages in the search index and in every kept
//! wiki version, and in the graph a user's private spaces and their places on member lists.
//! Search traces are left alone; they expire with the retention period. Tenant deletion is
//! `Tenants::delete`.

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Context;
use serde::Serialize;

use crate::graph::Visibility;
use crate::tenants::{Tenant, group_dir_name, user_dir_name};

/// Where the store keeps when each deleted user or group folder was deleted (JSON: folder →
/// time), so lint skips their searches made before then.
pub const DELETED_OWNERS: &str = "deleted_owners";

/// Whose data is deleted, by their id as the host names them.
#[derive(Debug, Clone)]
pub enum Owner {
    User(String),
    Group(String),
}

impl Owner {
    /// The folder name the id maps to (`u_…` or `g_…`).
    fn dir(&self) -> String {
        match self {
            Owner::User(id) => user_dir_name(id),
            Owner::Group(id) => group_dir_name(id),
        }
    }

    /// The wiki folder of its pages, as page paths begin.
    fn prefix(&self) -> String {
        match self {
            Owner::User(_) => format!("users/{}/", self.dir()),
            Owner::Group(_) => format!("groups/{}/", self.dir()),
        }
    }
}

/// What a deletion removed.
#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct Deleted {
    pub pages_deleted: usize,
    pub sources_deleted: usize,
    /// The user's private graph spaces, removed with their nodes and edges.
    pub spaces_deleted: Vec<String>,
    /// Spaces whose member list (or owner) no longer names them.
    pub spaces_changed: Vec<String>,
}

/// Deletes everything a user or group has in this tenant; `actor` names who asked, for the
/// audit log. Runs on the tenant's write queue, so
/// jobs queued before it have written their pages first and are deleted with the rest. The
/// graph comes first: if its store can't be reached, nothing is deleted yet.
#[tracing::instrument(skip_all, fields(tenant = %tenant.dir_name))]
pub async fn delete(tenant: &Arc<Tenant>, owner: &Owner, actor: &str) -> anyhow::Result<Deleted> {
    let mut out = Deleted::default();
    let (dir, prefix) = (owner.dir(), owner.prefix());

    {
        let _graph_writer = tenant.lock_graph().await?;
        let mut changed_graph = false;
        let spaces = graph_changes(tenant, owner, &mut out, &mut changed_graph).await;
        // The in-memory graph follows the stores. After a failure part way it is dropped (the
        // spaces done already must leave memory) and loaded again by the next request.
        if changed_graph && (spaces.is_err() || tenant.graph_after_deletion().await.is_err()) {
            tenant.forget_graph().await;
        }
        spaces?;
    }

    let _wiki_writer = tenant.lock_wiki().await?;
    // The raw records filed for the folder, and any its pages cite that weren't noted (records
    // kept before they were): a record is filed for one audience, so only its pages cite it.
    let wiki = tenant.wiki();
    let pages: Vec<_> = {
        let (wiki, folder) = (wiki.clone(), vec![dir.clone()]);
        let (groups, users) = match owner {
            Owner::User(_) => (Vec::new(), folder),
            Owner::Group(_) => (folder, Vec::new()),
        };
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let listed = wiki.folder_stamps(&groups, &users)?;
            Ok(wiki.read_stamped(listed)?)
        })
        .await
        .context("listing the pages to delete panicked")??
    };
    let mut records: BTreeSet<String> = tenant.store.records_of(&dir).await?.into_iter().collect();
    // Every version of the folder's documents, also those no page cites any more.
    for (source, doc) in tenant.store.documents_by_source().await? {
        if doc.audience == dir {
            records.insert(source);
        }
    }
    for (page, _) in &pages {
        for cited in &page.front.sources {
            let name = cited.split_once('#').map_or(cited.as_str(), |(n, _)| n);
            if crate::librarian::sources::valid_name(name) {
                records.insert(name.to_owned());
            }
        }
    }
    out.pages_deleted = pages.len();

    let (root, tenant_dir) = (wiki.root().to_path_buf(), tenant.dir.clone());
    let folder = prefix.trim_end_matches('/').to_owned();
    out.sources_deleted = tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
        match std::fs::remove_dir_all(root.join(&folder)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(anyhow::Error::from(e).context(format!("removing {folder}")));
            }
            _ => {}
        }
        let mut removed = 0;
        for name in &records {
            removed += crate::librarian::sources::remove(&tenant_dir, name)?;
        }
        Ok(removed)
    })
    .await
    .context("removing the folder panicked")??;

    tenant.store.delete_audience(&dir).await?;
    // Lint must not bring the owner's search terms back from the traces kept until they
    // expire: it skips the owner's searches made before now.
    let mut deleted: std::collections::BTreeMap<String, String> = tenant
        .store
        .meta(DELETED_OWNERS)
        .await?
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    deleted.insert(dir.clone(), crate::trace::now_rfc3339());
    tenant
        .store
        .set_meta(DELETED_OWNERS, &serde_json::to_string(&deleted)?)
        .await?;
    tenant.purge_from_search(&prefix).await?;
    if let Owner::User(_) = owner {
        tenant.reads.forget_user(&dir);
    }

    let what = match owner {
        Owner::User(_) => "user.delete",
        Owner::Group(_) => "group.delete",
    };
    let line = format!(
        "{what} {dir}: {} pages, {} raw records, {} spaces removed, {} spaces changed",
        out.pages_deleted,
        out.sources_deleted,
        out.spaces_deleted.len(),
        out.spaces_changed.len()
    );
    crate::librarian::record_log(tenant, &line).await?;
    tenant.store.audit(actor, what, &line).await?;
    tracing::info!(%dir, pages = out.pages_deleted, sources = out.sources_deleted, "deleted");
    Ok(out)
}

/// Takes the owner out of the graph: a user's private spaces with their nodes and edges, and
/// the owner from member lists and ownerships. Says in `changed` whether anything changed, also
/// when it stops on an error part way.
async fn graph_changes(
    tenant: &Arc<Tenant>,
    owner: &Owner,
    out: &mut Deleted,
    changed: &mut bool,
) -> anyhow::Result<()> {
    for mut space in tenant.store.graph_spaces().await? {
        match owner {
            Owner::User(id) if space.owner.as_deref() == Some(id.as_str()) => {
                if space.visibility == Visibility::Private {
                    tenant.graph_store.delete_space(&space.space).await?;
                    tenant.store.remove_graph_space(&space.space).await?;
                    out.spaces_deleted.push(space.space);
                    *changed = true;
                    continue;
                }
                space.owner = None;
                space.members.users.retain(|u| u != id);
            }
            Owner::User(id) if space.members.users.contains(id) => {
                space.members.users.retain(|u| u != id);
            }
            Owner::Group(id) if space.members.groups.contains(id) => {
                space.members.groups.retain(|g| g != id);
            }
            _ => continue,
        }
        tenant.store.put_graph_space(&space).await?;
        out.spaces_changed.push(space.space);
        *changed = true;
    }
    Ok(())
}
