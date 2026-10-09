//! Which store each tenant's graph data is in, and moving it from one store to the other.
//!
//! Graph data is kept in SQLite (`graph.sqlite` in the tenant's folder) or Neo4j, as `[graph]
//! backend` says. Each tenant records which one holds its data, so a service configured for
//! the other store refuses to open the tenant instead of serving an empty graph; moving the
//! data is `pinakes-migrate-graph` (this module's [`migrate`]).

use std::path::{Path, PathBuf};

use anyhow::Context;

use super::GraphBackend;
use crate::store::{SqliteStore, Store};

/// The key in the tenant's store that names its graph data's store.
const META_KEY: &str = "graph_backend";

/// Why the old copy is still there, when it is.
const OLD_COPY: &str = "the tenant reads the new store now, but the old copy could not be removed; run the command again to remove it";

/// Nodes or edges written per call when moving data.
const CHUNK: usize = super::def::MAX_BATCH;

/// Where a tenant's graph data is (`sqlite` or `neo4j`), and whether that was recorded rather
/// than worked out. `None`: it has no graph data yet, so any store will do. A tenant from before
/// the record: its `graph.sqlite` file means SQLite; spaces registered without one mean Neo4j
/// (the shipped store until 2026-10-09); neither means no data.
pub async fn location(
    store: &dyn Store,
    tenant_dir: &Path,
) -> anyhow::Result<Option<(String, bool)>> {
    if let Some(at) = store.meta(META_KEY).await? {
        return Ok(Some((at, true)));
    }
    if tokio::fs::try_exists(tenant_dir.join("graph.sqlite")).await? {
        return Ok(Some(("sqlite".into(), false)));
    }
    if !store.graph_spaces().await?.is_empty() {
        return Ok(Some(("neo4j".into(), false)));
    }
    Ok(None)
}

/// Checks, when a tenant opens, that its graph data is in the configured store, and records
/// the store when it wasn't recorded yet (only when `record`: a read-only tool writes nothing).
pub async fn check(
    store: &dyn Store,
    tenant_dir: &Path,
    backend: &GraphBackend,
    record: bool,
) -> anyhow::Result<()> {
    let here = backend.name();
    match location(store, tenant_dir).await? {
        Some((at, _)) if at != here => anyhow::bail!(
            "the graph data of tenant folder {} is in {at}, but the service is configured for {here}: set [graph] backend = \"{at}\", or move the data with `pinakes-migrate-graph --to {here}` while the service is stopped",
            tenant_dir.display()
        ),
        Some((_, true)) => Ok(()),
        _ if record => store.set_meta(META_KEY, here).await,
        _ => Ok(()),
    }
}

/// What moving one tenant did.
#[derive(Debug)]
pub struct Moved {
    pub tenant_dir: PathBuf,
    pub nodes: usize,
    pub edges: usize,
    /// It was in the target store already (or had no graph data): nothing was copied.
    pub already: bool,
}

/// Moves every tenant's graph data under `root` from `from` to `to`, tenant by tenant: all
/// nodes and edges are read from the old store, written to the new one, counted back, and only
/// then is the tenant's record changed and the old copy removed (deletion is hard: a copy no
/// deletion would reach must not stay). A tenant that has data in the new store already is
/// refused unless `replace` (that data is then removed first). Run it while the service is
/// stopped: writes made meanwhile would be missed.
pub async fn migrate(
    root: &Path,
    store_file: &str,
    from: &GraphBackend,
    to: &GraphBackend,
    replace: bool,
) -> anyhow::Result<Vec<Moved>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(root)
        .with_context(|| format!("reading {}", root.display()))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("t_"))
                && p.join(store_file).is_file()
        })
        .collect();
    dirs.sort();
    let mut out = Vec::new();
    for dir in dirs {
        out.push(
            move_tenant(&dir, store_file, from, to, replace)
                .await
                .with_context(|| format!("moving the graph data of {}", dir.display()))?,
        );
    }
    Ok(out)
}

async fn move_tenant(
    dir: &Path,
    store_file: &str,
    from: &GraphBackend,
    to: &GraphBackend,
    replace: bool,
) -> anyhow::Result<Moved> {
    let file = dir.join(store_file);
    let store = tokio::task::spawn_blocking(move || SqliteStore::open_existing(&file)).await??;
    let id = store
        .tenant_id()
        .await?
        .context("the folder has no tenant id")?;
    let mut moved = Moved {
        tenant_dir: dir.to_owned(),
        nodes: 0,
        edges: 0,
        already: false,
    };
    let (source, target) = (from.open(&id, dir)?, to.open(&id, dir)?);
    match location(&store, dir).await? {
        None => {
            store.set_meta(META_KEY, to.name()).await?;
            moved.already = true;
            return Ok(moved);
        }
        Some((at, recorded)) if at == to.name() => {
            // Recorded: an earlier run switched the tenant and may not have removed the old
            // copy. Only worked out (a tenant from before the record): no run made a copy, and
            // what the other store holds is not removed on a guess.
            if recorded {
                source.delete_all().await.context(OLD_COPY)?;
            }
            store.set_meta(META_KEY, to.name()).await?;
            moved.already = true;
            return Ok(moved);
        }
        Some((at, _)) if at != from.name() => {
            anyhow::bail!("its graph data is in {at}, not in {}", from.name())
        }
        Some(_) => {}
    }
    let data = source.load().await?;
    let there = target.load().await?;
    if !there.nodes.is_empty() || !there.edges.is_empty() {
        anyhow::ensure!(
            replace,
            "{} already holds {} nodes and {} edges for this tenant; pass --replace to remove them first",
            to.name(),
            there.nodes.len(),
            there.edges.len()
        );
        target.delete_all().await?;
    }
    for space in store.graph_spaces().await? {
        target.prepare_space(&space.space).await?;
    }
    // Nodes first: an edge needs both its ends stored.
    for chunk in data.nodes.chunks(CHUNK) {
        target.write(chunk, &[]).await?;
    }
    for chunk in data.edges.chunks(CHUNK) {
        target.write(&[], chunk).await?;
    }
    let copied = target.load().await?;
    anyhow::ensure!(
        copied.nodes.len() == data.nodes.len() && copied.edges.len() == data.edges.len(),
        "{} holds {} nodes and {} edges after the copy, not {} and {}; the tenant still reads {}",
        to.name(),
        copied.nodes.len(),
        copied.edges.len(),
        data.nodes.len(),
        data.edges.len(),
        from.name()
    );
    store.set_meta(META_KEY, to.name()).await?;
    source.delete_all().await.context(OLD_COPY)?;
    store.close();
    source.close();
    target.close();
    moved.nodes = data.nodes.len();
    moved.edges = data.edges.len();
    Ok(moved)
}
