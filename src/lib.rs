//! A self-hosted knowledge service: an LLM-maintained wiki that agents search in milliseconds.
//! See `docs/design.md`.

pub mod acquisition;
pub mod api;
pub mod config;
pub mod convert;
pub mod deletion;
pub mod dev;
pub mod embed;
pub mod error;
pub mod graph;
pub mod identity;
pub mod jobs;
pub mod kinds;
pub mod librarian;
pub mod llm;
pub mod mcp;
pub mod par;
pub mod search;
pub mod store;
pub mod tenants;
pub mod trace;
pub mod wiki;

use std::ops::Deref;
use std::sync::Arc;

use config::Config;
use identity::AdminKey;
use tenants::Tenants;

/// The code answering requests: crate version plus the git commit it was built from
/// (`-dirty` with uncommitted changes), as traces and replay reports record it.
pub const SERVICE: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("PINAKES_GIT_COMMIT"));

/// State shared by every request.
#[derive(Clone)]
pub struct AppState(Arc<AppInner>);

pub struct AppInner {
    pub config: Config,
    pub tenants: Tenants,
    pub admin_key: Option<AdminKey>,
    pub traces: trace::TraceLog,
    /// The search settings' fingerprint with the embedding model, computed once.
    pub search_fingerprint: String,
    /// Embeds pages and queries for meaning-based search; `None` without `[embedding] model`
    /// or when the model can't be loaded.
    pub embedder: Option<Arc<dyn embed::Embedder>>,
    /// The per-tenant write queues.
    pub jobs: jobs::Jobs,
    /// Turns uploaded files into Markdown.
    pub converter: convert::Converter,
    /// The librarian, or why it is off. Set at startup; turned on later when the key is
    /// entered at the prompt (`main.rs`).
    librarian: std::sync::RwLock<Result<Arc<librarian::Librarian>, llm::Missing>>,
    /// Cancelled when the service stops, so long-lived streams (MCP) end and shutdown completes.
    pub shutdown: tokio_util::sync::CancellationToken,
}

impl AppState {
    /// `admin_key` is the service admin key; `None` or empty disables the `/admin` routes. The
    /// librarian's model comes from `[librarian]` and its key from the environment; without
    /// them the service still starts with the librarian off, and the writes that need it
    /// answer 503 until [`AppState::set_llm`] turns it on. The embedding model loads in the
    /// background (call this inside the runtime): meaning-based searches answer 503 until it is
    /// in, and the tenants opened meanwhile get their page vectors when it is.
    pub fn new(config: Config, admin_key: Option<&str>) -> Self {
        let llm = llm::client(&config.librarian, config.librarian.llm.key());
        match &llm {
            Ok(_) => tracing::info!(
                model = config.librarian.llm.model().unwrap_or_default(),
                file = %config.librarian.llm_file_name(),
                "librarian on"
            ),
            Err(e) => tracing::warn!(error = %e, "the librarian is off"),
        }
        let model = config
            .embedding
            .model
            .is_some()
            .then(|| Arc::new(embed::Background::default()));
        let embedder = model.clone().map(|m| m as Arc<dyn embed::Embedder>);
        let state = Self::build(config, admin_key, llm, embedder);
        if let Some(model) = model {
            state.load_in_background(model);
        }
        state
    }

    fn load_in_background(&self, model: Arc<embed::Background>) {
        let state = self.clone();
        tokio::spawn(async move {
            let config = state.config.clone();
            let loaded = tokio::task::spawn_blocking(move || load_embedder_model(&config)).await;
            let loaded = match loaded {
                Ok(Some(loaded)) => loaded,
                // Why it failed is already logged.
                Ok(None) => return,
                Err(e) => {
                    tracing::error!(error = %e, "loading the embedding model panicked; semantic search is off");
                    return;
                }
            };
            model.set(loaded);
            for tenant in state.tenants.open_tenants().await {
                if let Err(e) = tenant.add_missing_vectors().await {
                    tracing::error!(tenant = %tenant.dir_name, error = %format!("{e:#}"), "page vectors not added");
                }
            }
        });
    }

    /// Like [`AppState::new`] with the librarian's model given, as tests give a scripted one;
    /// `None` leaves the librarian off. No embedding model.
    pub fn with_llm(config: Config, admin_key: Option<&str>, llm: Option<llm::Llm>) -> Self {
        Self::with_models(config, admin_key, llm, None)
    }

    /// Like [`AppState::with_llm`], with the embedding model given too.
    pub fn with_models(
        config: Config,
        admin_key: Option<&str>,
        llm: Option<llm::Llm>,
        embedder: Option<Arc<dyn embed::Embedder>>,
    ) -> Self {
        let off = llm::Missing::Model {
            file: config.librarian.llm_file_name(),
        };
        Self::build(config, admin_key, llm.ok_or(off), embedder)
    }

    fn build(
        config: Config,
        admin_key: Option<&str>,
        llm: Result<llm::Llm, llm::Missing>,
        embedder: Option<Arc<dyn embed::Embedder>>,
    ) -> Self {
        let tenants = Tenants::new(
            config.tenants_dir(),
            config.store.file_name.clone(),
            std::time::Duration::from_secs(config.search.session_idle_minutes.saturating_mul(60)),
        )
        // Wiki versions are kept only for replaying traces.
        .keep_history(config.trace.enabled)
        .graph_backend(graph::GraphBackend::from_config(&config))
        .embedder(embedder.clone())
        .vector_graph_rows(config.search.vector_graph_rows);
        let traces = trace::TraceLog::start(
            config.tenants_dir(),
            config.trace.enabled,
            (config.trace.retention_days > 0).then_some(config.trace.retention_days),
        );
        let librarian = llm.map(|llm| librarian_for(&config, llm, embedder.clone()));
        let converter =
            convert::Converter::new(&config.converter).expect("an HTTP client for the converter");
        if config.acquisition.providers.is_some() {
            tracing::warn!(
                "[acquisition] providers is no longer read: only the sites a tenant trusts are read (remove the line)"
            );
        }
        Self(Arc::new(AppInner {
            converter,
            jobs: jobs::Jobs::new(std::time::Duration::from_secs(
                config.jobs.keep_finished_minutes.saturating_mul(60),
            )),
            librarian: std::sync::RwLock::new(librarian),
            shutdown: Default::default(),
            search_fingerprint: config
                .search
                .fingerprint(embedder.as_ref().map(|e| e.model())),
            embedder,
            admin_key: admin_key.and_then(AdminKey::new),
            config,
            tenants,
            traces,
        }))
    }

    /// The librarian, or a 503 saying why it is off.
    pub fn librarian(&self) -> Result<Arc<librarian::Librarian>, error::ApiError> {
        match &*self.librarian.read().unwrap_or_else(|e| e.into_inner()) {
            Ok(l) => Ok(l.clone()),
            Err(why) => Err(error::ApiError::Unavailable(format!(
                "the librarian is off: {why}"
            ))),
        }
    }

    /// Why the librarian is off, when only its key is missing (so it can be asked for).
    pub fn missing_key(&self) -> Option<llm::Missing> {
        match &*self.librarian.read().unwrap_or_else(|e| e.into_inner()) {
            Err(why @ llm::Missing::Key { .. }) => Some(why.clone()),
            _ => None,
        }
    }

    /// Turns the librarian on with this model.
    pub fn set_llm(&self, llm: llm::Llm) {
        let l = librarian_for(&self.config, llm, self.embedder.clone());
        *self.librarian.write().unwrap_or_else(|e| e.into_inner()) = Ok(l);
    }
}

/// Loads `[embedding] model`, downloading its files into `<data>/models` the first time. When
/// it can't be loaded the service still starts, and meaning-based searches answer 503.
pub fn load_embedder(config: &Config) -> Option<Arc<dyn embed::Embedder>> {
    load_embedder_model(config).map(|m| Arc::new(m) as Arc<dyn embed::Embedder>)
}

fn load_embedder_model(config: &Config) -> Option<embed::FastEmbedder> {
    let name = config.embedding.model.as_deref()?;
    let dir = config.data.dir.join("models");
    let started = std::time::Instant::now();
    match embed::FastEmbedder::load(&dir) {
        Ok(e) => {
            tracing::info!(
                model = name,
                ms = started.elapsed().as_millis() as u64,
                "embedding model loaded"
            );
            Some(e)
        }
        Err(e) => {
            tracing::error!(model = name, error = %format!("{e:#}"), "semantic search is off");
            None
        }
    }
}

fn librarian_for(
    config: &Config,
    llm: llm::Llm,
    embedder: Option<Arc<dyn embed::Embedder>>,
) -> Arc<librarian::Librarian> {
    Arc::new(librarian::Librarian::new(
        llm,
        config.librarian.clone(),
        config.search.clone(),
        embedder,
    ))
}

/// Where a personal deployment keeps the API key of its default tenant.
pub const PERSONAL_KEY_FILE: &str = "personal-api-key.txt";

impl AppState {
    /// Startup work before serving. With the Neo4j backend, connects to Neo4j (once) and
    /// creates its constraints. In a personal deployment, creates the default
    /// tenant on first start and writes its API key to [`PERSONAL_KEY_FILE`] in the data
    /// folder. Returns the key when it was just created, so it can be shown once.
    pub async fn prepare(&self) -> anyhow::Result<Option<String>> {
        if let graph::GraphBackend::Neo4j(client) = self.tenants.graph() {
            client.connect().await?;
            tracing::info!(uri = %self.config.neo4j.uri, "Neo4j reachable");
        }
        self.tenants.remove_leftovers().await;
        let id = &self.config.identity;
        if id.deployment != config::Deployment::Personal {
            return Ok(None);
        }
        let key_file = self.config.data.dir.join(PERSONAL_KEY_FILE);
        match self.tenants.create(&id.default_tenant, "personal").await {
            Ok(created) => {
                // The key is returned (and shown) even when the file can't be written: it is
                // the only copy.
                if let Err(e) = std::fs::write(&key_file, format!("{}\n", created.api_key)) {
                    tracing::error!(
                        file = %key_file.display(),
                        error = %e,
                        "could not write the key file; keep the key shown below"
                    );
                }
                Ok(Some(created.api_key))
            }
            Err(tenants::CreateTenantError::AlreadyExists) => {
                if !key_file.exists() {
                    tracing::warn!(
                        file = %key_file.display(),
                        "the default tenant exists but its key file is missing"
                    );
                }
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
}

impl AppState {
    /// Every minute, releases the in-memory copies of tenants idle longer than `[cache]
    /// idle_minutes`. Stops when the state is dropped.
    pub fn start_cache_sweeper(&self) {
        let idle = self.config.cache.idle_minutes;
        if idle == 0 {
            return;
        }
        let idle = std::time::Duration::from_secs(idle.saturating_mul(60));
        let state = Arc::downgrade(&self.0);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(std::time::Duration::from_secs(60));
            every.tick().await;
            loop {
                every.tick().await;
                let Some(state) = state.upgrade() else {
                    return;
                };
                state.tenants.release_idle(idle).await;
            }
        });
    }
}

impl Deref for AppState {
    type Target = AppInner;

    fn deref(&self) -> &AppInner {
        &self.0
    }
}

pub use api::router;
