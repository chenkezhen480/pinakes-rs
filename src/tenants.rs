//! Tenants: id-to-folder mapping, creation, API key checks and the cache of open tenants.
//!
//! Each tenant owns one folder under `<data>/tenants/`, named from a hash of its id, holding its
//! wiki files, raw sources and its SQLite file. Nothing here ever builds a path from a raw id.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, RwLock};

use crate::graph::{GraphBackend, GraphSnapshot, GraphStore};
use crate::search::SearchSnapshot;
use crate::search::tracker::ReadTracker;
use crate::store::{SqliteStore, Store};
use crate::wiki::{History, WikiFs};

const MAX_ID_LEN: usize = 256;
const HASH_HEX_LEN: usize = 16;
const KEY_PREFIX: &str = "kb_";

/// Wiki folders created with every tenant.
const WIKI_DIRS: &[&str] = &["experience", "expertise", "graph", "users"];

/// Folder name for a tenant id: `t_` plus 16 hex digits of a SHA-256 of the id.
pub fn tenant_dir_name(tenant_id: &str) -> String {
    format!("t_{}", short_hash(b"tenant", tenant_id))
}

/// Folder name for a user id inside a tenant: `u_` plus 16 hex digits.
pub fn user_dir_name(user_id: &str) -> String {
    format!("u_{}", short_hash(b"user", user_id))
}

/// Folder name for a group id inside a tenant: `g_` plus 16 hex digits.
pub fn group_dir_name(group_id: &str) -> String {
    format!("g_{}", short_hash(b"group", group_id))
}

fn short_hash(domain: &[u8], id: &str) -> String {
    let mut h = Sha256::new();
    h.update(domain);
    h.update([0u8]);
    h.update(id.as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..HASH_HEX_LEN / 2])
}

/// Hex SHA-256 of an API key, the form stored in a tenant's SQLite file.
pub fn hash_api_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Ids are opaque strings; the service only checks they are usable.
pub fn validate_id(what: &str, id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("{what} id is empty"));
    }
    if id.len() > MAX_ID_LEN {
        return Err(format!("{what} id is longer than {MAX_ID_LEN} bytes"));
    }
    if id.chars().any(char::is_control) {
        return Err(format!("{what} id contains control characters"));
    }
    Ok(())
}

/// API keys look like `kb_<16 hex: tenant folder hash>_<64 hex: secret>`. The prefix routes the
/// key to its tenant's folder without a global table; only the hash of the whole key is stored.
fn new_api_key(tenant_dir: &str) -> String {
    let hash = tenant_dir.strip_prefix("t_").expect("tenant dir name");
    let secret: [u8; 32] = rand::random();
    format!("{KEY_PREFIX}{hash}_{}", hex::encode(secret))
}

/// The tenant folder a key claims to belong to, if the key is well-formed.
fn tenant_dir_from_key(key: &str) -> Option<String> {
    let rest = key.strip_prefix(KEY_PREFIX)?;
    let (hash, secret) = rest.split_once('_')?;
    let is_hex = |s: &str, len: usize| {
        s.len() == len
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    (is_hex(hash, HASH_HEX_LEN) && is_hex(secret, 64)).then(|| format!("t_{hash}"))
}

pub struct Tenant {
    pub id: String,
    pub dir_name: String,
    pub dir: PathBuf,
    pub store: Arc<dyn Store>,
    /// API key hashes, loaded from the store when the tenant opens. Key management (not in v1
    /// yet) must update this set as well as the store.
    key_hashes: RwLock<HashSet<String>>,
    /// What searches read, built on the first search after a caller has authenticated (never
    /// while checking a key). Swapped whole by `reindex`, so a running search keeps its copy.
    search: std::sync::RwLock<Option<Arc<SearchSnapshot>>>,
    /// Lets only one build of the snapshot run at a time.
    building: Mutex<()>,
    /// Which pages each agent session has read.
    pub reads: ReadTracker,
    keep_history: bool,
    /// Where this tenant's graph nodes and edges are stored.
    pub graph_store: Arc<dyn GraphStore>,
    /// The in-memory graph searches and walks read, loaded on first use.
    graph: std::sync::RwLock<Option<Arc<GraphSnapshot>>>,
    graph_loading: Mutex<()>,
    /// When loading the graph last failed, if it did since the last success.
    graph_failed_at: std::sync::Mutex<Option<Instant>>,
    /// Held by every graph write (schemas, spaces, data), so they apply one at a time.
    pub graph_writer: Mutex<()>,
}

/// How long after a failed graph load requests fail at once rather than try again.
pub const GRAPH_RETRY_AFTER: Duration = Duration::from_secs(10);

impl std::fmt::Debug for Tenant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tenant")
            .field("id", &self.id)
            .field("dir_name", &self.dir_name)
            .finish_non_exhaustive()
    }
}

impl Tenant {
    pub fn wiki(&self) -> WikiFs {
        WikiFs::new(self.dir.join("wiki"))
    }

    /// The tenant's wiki history, when it is kept.
    fn history(&self) -> Option<History> {
        self.keep_history.then(|| History::new(&self.dir))
    }

    fn current_snapshot(&self) -> Option<Arc<SearchSnapshot>> {
        self.search
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The current search snapshot, built from the wiki on first use. Concurrent first
    /// searches wait for one build instead of each running their own.
    pub async fn snapshot(&self) -> anyhow::Result<Arc<SearchSnapshot>> {
        if let Some(s) = self.current_snapshot() {
            return Ok(s);
        }
        let _building = self.building.lock().await;
        if let Some(s) = self.current_snapshot() {
            return Ok(s);
        }
        let snapshot = Arc::new(build_snapshot(self.wiki(), self.history()).await?);
        *self.search.write().unwrap_or_else(|e| e.into_inner()) = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Rebuilds the search snapshot from the wiki files and swaps it in. Searches already
    /// running finish on the old one.
    #[tracing::instrument(name = "reindex", skip_all, fields(tenant = %self.dir_name))]
    pub async fn reindex(&self) -> anyhow::Result<()> {
        let _building = self.building.lock().await;
        let snapshot = build_snapshot(self.wiki(), self.history()).await?;
        *self.search.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(snapshot));
        Ok(())
    }

    fn current_graph(&self) -> Option<Arc<GraphSnapshot>> {
        self.graph.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The in-memory graph, read from the stores on first use: schema definitions and spaces
    /// from SQLite, nodes and edges from the graph store.
    ///
    /// After a failed load, calls fail at once for [`GRAPH_RETRY_AFTER`] instead of each
    /// waiting on an unreachable store again; requests queued behind the failed load too.
    pub async fn graph(&self) -> anyhow::Result<Arc<GraphSnapshot>> {
        if let Some(g) = self.current_graph() {
            return Ok(g);
        }
        self.check_graph_failure()?;
        let _loading = self.graph_loading.lock().await;
        if let Some(g) = self.current_graph() {
            return Ok(g);
        }
        self.check_graph_failure()?;
        let loaded = async {
            let schemas = self.store.graph_schemas().await?;
            let spaces = self.store.graph_spaces().await?;
            let data = self.graph_store.load().await?;
            tokio::task::spawn_blocking(move || GraphSnapshot::build(schemas, spaces, data))
                .await
                .context("building the graph snapshot panicked")
        }
        .await;
        let failed = &self.graph_failed_at;
        match loaded {
            Ok(graph) => {
                *failed.lock().unwrap_or_else(|e| e.into_inner()) = None;
                let graph = Arc::new(graph);
                *self.graph.write().unwrap_or_else(|e| e.into_inner()) = Some(graph.clone());
                Ok(graph)
            }
            Err(e) => {
                // Logged here, once per attempt; the requests that fail fast after it don't.
                tracing::error!(tenant = %self.dir_name, error = %format!("{e:#}"), "could not load the graph");
                *failed.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
                Err(e)
            }
        }
    }

    /// The graph for a read that can do without it: when it can't be loaded (Neo4j down), an
    /// empty graph, so the caller answers from the wiki alone. `graph` logs the failure.
    pub async fn graph_or_empty(&self) -> Arc<GraphSnapshot> {
        self.graph()
            .await
            .unwrap_or_else(|_| Arc::new(GraphSnapshot::empty()))
    }

    fn check_graph_failure(&self) -> anyhow::Result<()> {
        let failed = *self
            .graph_failed_at
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match failed {
            Some(at) if at.elapsed() < GRAPH_RETRY_AFTER => {
                anyhow::bail!("the graph could not be loaded; retrying soon")
            }
            _ => Ok(()),
        }
    }

    /// Swaps in a new graph after a write. Callers hold `graph_writer`.
    pub fn set_graph(&self, graph: GraphSnapshot) {
        *self.graph.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(graph));
    }

    async fn has_key_hash(&self, hash: &str) -> bool {
        self.key_hashes.read().await.contains(hash)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CreateTenantError {
    #[error("{0}")]
    InvalidId(String),
    #[error("tenant already exists")]
    AlreadyExists,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub struct CreatedTenant {
    pub tenant: Arc<Tenant>,
    pub api_key: String,
}

/// All tenants under one data folder, and the ones currently open.
pub struct Tenants {
    root: PathBuf,
    store_file: String,
    session_idle: Duration,
    /// Keep every wiki version a snapshot is built from, for replays (see `wiki::History`).
    keep_history: bool,
    graph_backend: GraphBackend,
    /// Open tenants. The lock is only ever held for a map lookup or insert, never across disk
    /// work, so loading one tenant can't stall requests for the others.
    open: RwLock<HashMap<String, Arc<Tenant>>>,
    /// Serializes tenant creation.
    creating: Mutex<()>,
}

impl Tenants {
    pub fn new(
        root: impl Into<PathBuf>,
        store_file: impl Into<String>,
        session_idle: Duration,
    ) -> Self {
        Self {
            root: root.into(),
            store_file: store_file.into(),
            session_idle,
            keep_history: false,
            graph_backend: GraphBackend::Sqlite,
            open: RwLock::new(HashMap::new()),
            creating: Mutex::new(()),
        }
    }

    /// Keeps each wiki version a search snapshot is built from in the tenant's history, so
    /// traced searches can be replayed on the wiki they ran on.
    pub fn keep_history(mut self, on: bool) -> Self {
        self.keep_history = on;
        self
    }

    /// Where tenants' graph data is stored (SQLite in each tenant's folder by default).
    pub fn graph_backend(mut self, backend: GraphBackend) -> Self {
        self.graph_backend = backend;
        self
    }

    /// The graph backend tenants open their stores from.
    pub fn graph(&self) -> &GraphBackend {
        &self.graph_backend
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates a tenant folder, its SQLite file and a first API key. The folder is built under a
    /// temporary name and renamed into place, so a failure never leaves a half-made tenant.
    #[tracing::instrument(skip_all)]
    pub async fn create(
        &self,
        tenant_id: &str,
        key_label: &str,
    ) -> Result<CreatedTenant, CreateTenantError> {
        validate_id("tenant", tenant_id).map_err(CreateTenantError::InvalidId)?;
        let dir_name = tenant_dir_name(tenant_id);
        let final_dir = self.root.join(&dir_name);

        let _creating = self.creating.lock().await;
        if tokio::fs::try_exists(&final_dir)
            .await
            .context("checking for an existing tenant folder")?
        {
            return Err(CreateTenantError::AlreadyExists);
        }
        // A new tenant starts with no graph data, even if a deleted tenant with the same id left
        // some in a shared graph database. If that can't be checked, nothing is created.
        self.graph_backend
            .clear_tenant(tenant_id)
            .await
            .context("clearing old graph data for this tenant id")?;

        let api_key = new_api_key(&dir_name);
        let key_hash = hash_api_key(&api_key);
        let tmp_dir = self.root.join(format!(
            ".tmp-{dir_name}-{}",
            hex::encode(rand::random::<[u8; 4]>())
        ));
        let store_file = self.store_file.clone();
        let (id, hash, label) = (tenant_id.to_owned(), key_hash.clone(), key_label.to_owned());
        let tmp = tmp_dir.clone();
        let fin = final_dir.clone();

        let built = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            build_tenant_dir(&tmp, &store_file, &id, &hash, &label)?;
            std::fs::rename(&tmp, &fin)
                .with_context(|| format!("moving tenant folder into {}", fin.display()))
        })
        .await
        .context("tenant creation task panicked")?;
        if let Err(e) = built {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e.into());
        }

        let tenant = Arc::new(self.load(&dir_name).await?);
        self.open.write().await.insert(dir_name, tenant.clone());
        Ok(CreatedTenant { tenant, api_key })
    }

    /// Looks up the tenant an API key belongs to. Unknown folders, malformed keys and wrong
    /// secrets all give `None`, so a caller can't tell them apart.
    #[tracing::instrument(skip_all)]
    pub async fn authenticate(&self, api_key: &str) -> anyhow::Result<Option<Arc<Tenant>>> {
        let Some(dir_name) = tenant_dir_from_key(api_key) else {
            return Ok(None);
        };
        let Some(tenant) = self.open_by_dir(&dir_name).await? else {
            return Ok(None);
        };
        Ok(tenant
            .has_key_hash(&hash_api_key(api_key))
            .await
            .then_some(tenant))
    }

    /// Opens a tenant by its folder name (`t_` plus 16 hex digits), for offline tools such as
    /// `pinakes-replay`. Anything else, and unknown folders, give `None`.
    pub(crate) async fn open(&self, dir_name: &str) -> anyhow::Result<Option<Arc<Tenant>>> {
        let well_formed = dir_name.strip_prefix("t_").is_some_and(|h| {
            h.len() == HASH_HEX_LEN
                && h.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if !well_formed {
            return Ok(None);
        }
        self.open_by_dir(dir_name).await
    }

    /// Returns an open tenant, loading it from disk on first use.
    async fn open_by_dir(&self, dir_name: &str) -> anyhow::Result<Option<Arc<Tenant>>> {
        if let Some(t) = self.open.read().await.get(dir_name) {
            return Ok(Some(t.clone()));
        }
        let file = self.root.join(dir_name).join(&self.store_file);
        let exists = tokio::fs::metadata(&file).await.is_ok_and(|m| m.is_file());
        if !exists {
            return Ok(None);
        }
        // Load without holding the lock. If two requests race on a cold tenant, both load it and
        // the first insert wins; the other copy is dropped.
        let tenant = Arc::new(self.load(dir_name).await?);
        let mut open = self.open.write().await;
        Ok(Some(
            open.entry(dir_name.to_owned()).or_insert(tenant).clone(),
        ))
    }

    async fn load(&self, dir_name: &str) -> anyhow::Result<Tenant> {
        let dir = self.root.join(dir_name);
        let file = dir.join(&self.store_file);
        let store = tokio::task::spawn_blocking(move || SqliteStore::open_existing(&file))
            .await
            .context("opening store panicked")??;
        let id = store
            .tenant_id()
            .await?
            .with_context(|| format!("tenant folder {dir_name} has no tenant id"))?;
        anyhow::ensure!(
            tenant_dir_name(&id) == dir_name,
            "tenant folder {dir_name} records id that maps elsewhere"
        );
        let key_hashes = store.api_key_hashes().await?.into_iter().collect();
        let (backend, tenant_id, tenant_dir) =
            (self.graph_backend.clone(), id.clone(), dir.clone());
        let graph_store =
            tokio::task::spawn_blocking(move || backend.open(&tenant_id, &tenant_dir))
                .await
                .context("opening the graph store panicked")??;
        Ok(Tenant {
            id,
            dir_name: dir_name.to_owned(),
            dir,
            store: Arc::new(store),
            key_hashes: RwLock::new(key_hashes),
            search: std::sync::RwLock::new(None),
            building: Mutex::new(()),
            reads: ReadTracker::new(self.session_idle),
            keep_history: self.keep_history,
            graph_store,
            graph: std::sync::RwLock::new(None),
            graph_loading: Mutex::new(()),
            graph_failed_at: std::sync::Mutex::new(None),
            graph_writer: Mutex::new(()),
        })
    }
}

/// Reads every page of a wiki and builds its search snapshot, off the async threads. With a
/// history, the version is stored there first; failing to store it only logs a warning.
async fn build_snapshot(wiki: WikiFs, history: Option<History>) -> anyhow::Result<SearchSnapshot> {
    tokio::task::spawn_blocking(move || {
        let pages = wiki.all_pages()?;
        if let Some(history) = history {
            let version = crate::search::version_of(&pages);
            if let Err(e) = history.save(&version, &pages) {
                tracing::warn!(%version, error = %e, "could not keep this wiki version");
            }
        }
        SearchSnapshot::build(pages)
    })
    .await
    .context("building the search snapshot panicked")?
}

fn build_tenant_dir(
    dir: &Path,
    store_file: &str,
    tenant_id: &str,
    key_hash: &str,
    key_label: &str,
) -> anyhow::Result<()> {
    let wiki = dir.join("wiki");
    for sub in WIKI_DIRS {
        std::fs::create_dir_all(wiki.join(sub))?;
    }
    std::fs::create_dir_all(dir.join("sources"))?;
    std::fs::write(wiki.join("index.md"), "# Index\n")?;
    std::fs::write(wiki.join("log.md"), "# Log\n")?;

    // The store is closed again before the folder is renamed (Windows can't rename open files).
    SqliteStore::create_for_tenant(&dir.join(store_file), tenant_id, key_hash, key_label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_names_are_stable_and_safe() {
        let a = tenant_dir_name("acme");
        assert_eq!(a, tenant_dir_name("acme"));
        assert_ne!(a, tenant_dir_name("acme2"));
        assert!(a.starts_with("t_") && a.len() == 18);
        let evil = tenant_dir_name("../other");
        assert!(evil.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        // The same string as a tenant, a user and a group maps to different names.
        assert_ne!(&tenant_dir_name("x")[2..], &user_dir_name("x")[2..]);
        assert_ne!(&user_dir_name("x")[2..], &group_dir_name("x")[2..]);
        assert!(group_dir_name("risk-team").starts_with("g_"));
    }

    #[test]
    fn key_format_roundtrips() {
        let dir = tenant_dir_name("acme");
        let key = new_api_key(&dir);
        assert_eq!(tenant_dir_from_key(&key).as_deref(), Some(dir.as_str()));
        assert_ne!(key, new_api_key(&dir));
    }

    #[test]
    fn malformed_keys_route_nowhere() {
        for k in [
            "",
            "kb_",
            "kb_0123456789abcdef",
            "kb_0123456789abcdef_short",
            "kb_../../etc_0000000000000000000000000000000000000000000000000000000000000000",
            "xx_0123456789abcdef_0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert_eq!(tenant_dir_from_key(k), None, "{k}");
        }
    }

    #[test]
    fn id_validation() {
        assert!(validate_id("tenant", "acme").is_ok());
        assert!(validate_id("tenant", "客户 A").is_ok());
        assert!(validate_id("tenant", "").is_err());
        assert!(validate_id("tenant", "a\nb").is_err());
        assert!(validate_id("tenant", &"x".repeat(257)).is_err());
    }

    #[tokio::test]
    async fn create_then_authenticate() {
        let dir = tempfile::tempdir().unwrap();
        let tenants = Tenants::new(dir.path(), "kb.sqlite", Duration::from_secs(60));
        let created = tenants.create("acme", "initial").await.unwrap();
        assert_eq!(created.tenant.id, "acme");
        assert!(matches!(
            tenants.create("acme", "again").await,
            Err(CreateTenantError::AlreadyExists)
        ));

        let found = tenants.authenticate(&created.api_key).await.unwrap();
        assert_eq!(found.unwrap().id, "acme");

        // A fresh registry (as after a restart) loads the tenant from disk.
        let reopened = Tenants::new(dir.path(), "kb.sqlite", Duration::from_secs(60));
        assert!(
            reopened
                .authenticate(&created.api_key)
                .await
                .unwrap()
                .is_some()
        );

        let mut wrong = created.api_key.clone();
        wrong.pop();
        wrong.push(if created.api_key.ends_with('0') {
            '1'
        } else {
            '0'
        });
        assert!(tenants.authenticate(&wrong).await.unwrap().is_none());

        // Many concurrent requests on a cold tenant all succeed.
        let cold = Arc::new(Tenants::new(
            dir.path(),
            "kb.sqlite",
            Duration::from_secs(60),
        ));
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let (t, k) = (cold.clone(), created.api_key.clone());
                tokio::spawn(async move { t.authenticate(&k).await.unwrap().is_some() })
            })
            .collect();
        for task in tasks {
            assert!(task.await.unwrap());
        }

        // No temp folders are left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec![tenant_dir_name("acme")]);
    }

    /// Checking a key never builds the search snapshot: a wrong secret costs one hash, not an
    /// index build.
    #[tokio::test]
    async fn authentication_builds_no_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let created = Tenants::new(dir.path(), "kb.sqlite", Duration::from_secs(60))
            .create("acme", "initial")
            .await
            .unwrap();
        let cold = Tenants::new(dir.path(), "kb.sqlite", Duration::from_secs(60));
        let mut wrong = created.api_key.clone();
        wrong.replace_range(
            wrong.len() - 1..,
            if wrong.ends_with('0') { "1" } else { "0" },
        );
        assert!(cold.authenticate(&wrong).await.unwrap().is_none());
        let tenant = cold.authenticate(&created.api_key).await.unwrap().unwrap();
        assert!(tenant.current_snapshot().is_none());
        tenant.snapshot().await.unwrap();
        assert!(tenant.current_snapshot().is_some());
    }

    #[tokio::test]
    async fn reindex_swaps_in_new_pages() {
        let dir = tempfile::tempdir().unwrap();
        let tenants = Tenants::new(dir.path(), "kb.sqlite", Duration::from_secs(60));
        let tenant = tenants.create("acme", "initial").await.unwrap().tenant;
        let before = tenant.snapshot().await.unwrap();
        assert!(before.catalog.pages.is_empty());

        let mut page = crate::wiki::Page::new(crate::wiki::PagePath::parse("expertise/x").unwrap());
        page.front.title = Some("X".into());
        tenant.wiki().write(&page).unwrap();
        tenant.reindex().await.unwrap();

        assert_eq!(tenant.snapshot().await.unwrap().catalog.pages.len(), 1);
        // A search that started earlier still holds the old snapshot.
        assert!(before.catalog.pages.is_empty());
    }
}
