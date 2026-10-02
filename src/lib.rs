//! A self-hosted knowledge service: an LLM-maintained wiki that agents search in milliseconds.
//! See `docs/design.md`.

pub mod api;
pub mod config;
pub mod convert;
pub mod error;
pub mod graph;
pub mod identity;
pub mod jobs;
pub mod kinds;
pub mod librarian;
pub mod llm;
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
    /// `config.search.fingerprint()`, computed once.
    pub search_fingerprint: String,
    /// The per-tenant write queues.
    pub jobs: jobs::Jobs,
    /// Turns uploaded files into Markdown.
    pub converter: convert::Converter,
    /// The librarian, or why it is off. Set at startup; turned on later when the key is
    /// entered at the prompt (`main.rs`).
    librarian: std::sync::RwLock<Result<Arc<librarian::Librarian>, llm::Missing>>,
}

impl AppState {
    /// `admin_key` is the service admin key; `None` or empty disables the `/admin` routes. The
    /// librarian's model comes from `[librarian]` and its key from the environment; without
    /// them the service still starts with the librarian off, and the writes that need it
    /// answer 503 until [`AppState::set_llm`] turns it on.
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
        Self::build(config, admin_key, llm)
    }

    /// Like [`AppState::new`] with the librarian's model given, as tests give a scripted one;
    /// `None` leaves the librarian off.
    pub fn with_llm(config: Config, admin_key: Option<&str>, llm: Option<llm::Llm>) -> Self {
        let off = llm::Missing::Model {
            file: config.librarian.llm_file_name(),
        };
        Self::build(config, admin_key, llm.ok_or(off))
    }

    fn build(config: Config, admin_key: Option<&str>, llm: Result<llm::Llm, llm::Missing>) -> Self {
        let tenants = Tenants::new(
            config.tenants_dir(),
            config.store.file_name.clone(),
            std::time::Duration::from_secs(config.search.session_idle_minutes.saturating_mul(60)),
        )
        // Wiki versions are kept only for replaying traces.
        .keep_history(config.trace.enabled)
        .graph_backend(graph::GraphBackend::from_config(&config));
        let traces = trace::TraceLog::start(
            config.tenants_dir(),
            config.trace.enabled,
            (config.trace.retention_days > 0).then_some(config.trace.retention_days),
        );
        let librarian = llm.map(|llm| librarian_for(&config, llm));
        let converter =
            convert::Converter::new(&config.converter).expect("an HTTP client for the converter");
        Self(Arc::new(AppInner {
            converter,
            jobs: jobs::Jobs::new(std::time::Duration::from_secs(
                config.jobs.keep_finished_minutes.saturating_mul(60),
            )),
            librarian: std::sync::RwLock::new(librarian),
            search_fingerprint: config.search.fingerprint(),
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
        let l = librarian_for(&self.config, llm);
        *self.librarian.write().unwrap_or_else(|e| e.into_inner()) = Ok(l);
    }
}

fn librarian_for(config: &Config, llm: llm::Llm) -> Arc<librarian::Librarian> {
    Arc::new(librarian::Librarian::new(
        llm,
        config.librarian.clone(),
        config.search.clone(),
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

impl Deref for AppState {
    type Target = AppInner;

    fn deref(&self) -> &AppInner {
        &self.0
    }
}

pub use api::router;
