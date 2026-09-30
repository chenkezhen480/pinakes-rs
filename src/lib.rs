//! A self-hosted knowledge service: an LLM-maintained wiki that agents search in milliseconds.
//! See `docs/design.md`.

pub mod api;
pub mod config;
pub mod error;
pub mod graph;
pub mod identity;
pub mod kinds;
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
pub const SERVICE: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("KB_GIT_COMMIT"));

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
}

impl AppState {
    /// `admin_key` is the service admin key; `None` or empty disables the `/admin` routes.
    pub fn new(config: Config, admin_key: Option<&str>) -> Self {
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
        Self(Arc::new(AppInner {
            search_fingerprint: config.search.fingerprint(),
            admin_key: admin_key.and_then(AdminKey::new),
            config,
            tenants,
            traces,
        }))
    }
}

/// Where a personal deployment keeps the API key of its default tenant.
pub const PERSONAL_KEY_FILE: &str = "personal-api-key.txt";

impl AppState {
    /// Startup work before serving. With the Neo4j backend, connects to Neo4j (retrying while
    /// it starts) and creates its constraints. In a personal deployment, creates the default
    /// tenant on first start and writes its API key to [`PERSONAL_KEY_FILE`] in the data
    /// folder. Returns the key when it was just created, so it can be shown once.
    pub async fn prepare(&self) -> anyhow::Result<Option<String>> {
        if let graph::GraphBackend::Neo4j(client) = self.tenants.graph() {
            client
                .connect_with_retries(30, std::time::Duration::from_secs(2))
                .await?;
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
