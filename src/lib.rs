//! A self-hosted knowledge service: an LLM-maintained wiki that agents search in milliseconds.
//! See `docs/design.md`.

pub mod acquisition;
pub mod admin;
pub mod api;
pub mod config;
pub mod console;
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
pub mod login;
pub mod mcp;
pub mod par;
pub mod search;
pub mod settings;
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
    /// The console's account and its sessions.
    pub login: login::Login,
    pub traces: trace::TraceLog,
    /// The search settings' fingerprints, computed once: with the embedding model, and
    /// without it (while it is off), as [`AppState::search_fingerprint`] picks.
    fingerprints: (String, String),
    /// Embeds pages and queries for meaning-based search; `None` without `[embedding] model`.
    /// Asked per use whether it is ready: the model can be turned on and off at run time.
    pub embedder: Option<Arc<dyn embed::Embedder>>,
    /// The switch behind `embedder` for the service's model, turned on and off by the admin
    /// (`/admin/embedding`). `None` without `[embedding] model`, or with a model given whole
    /// (tests).
    pub embedding: Option<Arc<embed::Switch>>,
    /// The per-tenant write queues.
    pub jobs: jobs::Jobs,
    /// Turns uploaded files into Markdown.
    pub converter: convert::Converter,
    /// The librarian, or why it is off. Set at startup; turned on later when the key is
    /// entered at the prompt (`main.rs`), or switched when the model is changed (console).
    librarian: std::sync::RwLock<Result<Arc<librarian::Librarian>, llm::Missing>>,
    /// The librarian's model as it is now (`model.conf`, changed at run time by the console).
    model: std::sync::RwLock<admin::ModelState>,
    /// The service's audit log: configuration, model and embedding changes, tenants.
    pub audit: admin::Audit,
    /// Cancelled when the service stops, so long-lived streams (MCP) end and shutdown completes.
    pub shutdown: tokio_util::sync::CancellationToken,
}

impl AppState {
    /// `admin_key` is the service admin key, for scripts and host code; `None` or empty leaves
    /// the `/admin` routes to the console's signed-in admin alone (`login`). The
    /// librarian's model comes from `[librarian]` and its key from the environment; without
    /// them the service still starts with the librarian off, and the writes that need it
    /// answer 503 until [`AppState::set_llm`] turns it on. With `[embedding] enabled` the
    /// embedding model loads in the background (call this inside the runtime): meaning-based
    /// searches answer 503 until it is in, and the tenants opened meanwhile get their page
    /// vectors when it is.
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
        let switch = config
            .embedding
            .model
            .is_some()
            .then(|| Arc::new(embed::Switch::fastembed(config.data.dir.join("models"))));
        if switch.is_some() && !config.embedding.enabled {
            tracing::info!(
                "embeddings are off: searches use keywords; turn them on in the console or with [embedding] enabled = true"
            );
        }
        let login = login::Login::from_env(&config.data.dir);
        Self::start(config, admin_key, llm, switch, login)
    }

    /// Like [`AppState::new`], with the librarian's model (or why it is off) and the embedding
    /// switch given, as tests give a scripted model and a switch that loads a fake one. The
    /// switch is turned on when `[embedding] enabled` says so.
    pub fn with_switch(
        config: Config,
        admin_key: Option<&str>,
        llm: Result<llm::Llm, llm::Missing>,
        switch: Option<Arc<embed::Switch>>,
    ) -> Self {
        // The default console account: tests never read the developer's own variables.
        let login = login::Login::new(&config.data.dir, None, None);
        Self::start(config, admin_key, llm, switch, login)
    }

    fn start(
        config: Config,
        admin_key: Option<&str>,
        llm: Result<llm::Llm, llm::Missing>,
        switch: Option<Arc<embed::Switch>>,
        login: login::Login,
    ) -> Self {
        let embedder = switch.clone().map(|m| m as Arc<dyn embed::Embedder>);
        let on = config.embedding.enabled;
        let state = Self::build(config, admin_key, llm, embedder, switch, login);
        if on {
            state.start_embedding();
        }
        state
    }

    /// Turns the embedding switch on and, unless it already was, loads the model in the
    /// background and then gives the open tenants their page vectors, reporting each step in
    /// the switch's status.
    fn start_embedding(&self) {
        let Some(switch) = self.embedding.clone() else {
            return;
        };
        if !switch.turn_on() {
            return;
        }
        self.run_loader(switch);
    }

    /// Loads the model in the background and gives the open tenants their page vectors, after
    /// [`embed::Switch::turn_on`] (or a deletion that a turn-on waited for) said to.
    pub(crate) fn run_loader(&self, switch: Arc<embed::Switch>) {
        let state = self.clone();
        tokio::spawn(async move {
            loop {
                let s = switch.clone();
                let started = std::time::Instant::now();
                match tokio::task::spawn_blocking(move || s.load()).await {
                    Ok(Ok(true)) => {
                        tracing::info!(
                            model = embed::Embedder::model(&*switch),
                            ms = started.elapsed().as_millis() as u64,
                            "embedding model loaded"
                        );
                        let open = state.tenants.open_tenants().await;
                        let tenants = open.len();
                        for (i, tenant) in open.into_iter().enumerate() {
                            let step = embed::Status::Embedding {
                                tenant: i + 1,
                                tenants,
                                done: 0,
                                total: 0,
                            };
                            if !switch.report(step) {
                                break;
                            }
                            if let Err(e) = tenant.add_missing_vectors().await {
                                tracing::error!(tenant = %tenant.dir_name, error = %format!("{e:#}"), "page vectors not added");
                            }
                        }
                    }
                    Ok(Ok(false)) => return,
                    Ok(Err(e)) => {
                        tracing::error!(error = %format!("{e:#}"), "the embedding model can't be loaded; semantic search is off");
                        return;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "loading the embedding model panicked; semantic search is off");
                        switch.turn_off();
                        switch.finish();
                        return;
                    }
                }
                if !switch.finish() {
                    return;
                }
            }
        });
    }

    /// Turns embeddings on or off while the service runs, and writes `[embedding] enabled`
    /// back to the config file it was loaded from. On: the model is downloaded (the first
    /// time) and loaded in the background, then every open tenant's pages are embedded once
    /// (what isn't stored yet); searches stay keyword-only until then. Off: the model leaves
    /// memory and no vectors are computed; stored ones are kept.
    pub fn set_embedding(&self, on: bool) -> Result<(), error::ApiError> {
        if self.embedding.is_none() {
            return Err(error::ApiError::Conflict(
                "no embedding model is configured: set [embedding] model in config.toml".into(),
            ));
        }
        if !on {
            self.can_turn_off_embedding()?;
        }
        // The file as it is now decides too: turning embeddings off while its default_mode
        // needs them doesn't load, and is refused.
        if let Some(file) = &self.config.file {
            config::write_value(file, "embedding", "enabled", on.into())
                .map_err(|e| admin::not_saved(file, e))?;
        }
        self.apply_embedding(on);
        Ok(())
    }

    /// Embeddings can't be turned off while the running service's default mode needs them (a
    /// change of `default_mode` saved to the file applies only after a restart).
    pub(crate) fn can_turn_off_embedding(&self) -> Result<(), error::ApiError> {
        let mode = self.config.search.default_mode;
        if mode.needs_meaning() {
            return Err(error::ApiError::Conflict(format!(
                "the running default_mode {mode:?} needs embeddings: change it, restart, then turn them off"
            )));
        }
        Ok(())
    }

    /// Turns the embedding switch on or off once the change is saved (or there is no file).
    pub(crate) fn apply_embedding(&self, on: bool) {
        let Some(switch) = &self.embedding else {
            return;
        };
        self.audit.record(
            "admin",
            "embedding.switch",
            serde_json::json!({ "enabled": on }),
        );
        if on {
            self.start_embedding();
        } else {
            switch.turn_off();
            tracing::info!("embeddings turned off");
        }
    }

    /// Why a search in a meaning-based mode can't run now, for its 503.
    pub fn meaning_off(&self) -> String {
        use embed::Status;
        let Some(switch) = &self.embedding else {
            return if self.embedder.is_none() {
                "semantic search is off: no [embedding] model is configured".into()
            } else {
                crate::search::NO_VECTORS.into()
            };
        };
        let turn_on = "an admin turns them on in the console (Settings, Embeddings) or with PUT /admin/embedding, or sets [embedding] enabled = true in config.toml";
        match switch.status() {
            Status::Off => format!("semantic search is off: embeddings are turned off; {turn_on}"),
            Status::Failed { error } => format!(
                "semantic search is off: the embedding model couldn't be loaded ({error}); {turn_on} to try again"
            ),
            Status::Downloading { .. } => {
                "semantic search isn't ready: the embedding model is downloading; keyword search works meanwhile".into()
            }
            Status::Loading => {
                "semantic search isn't ready: the embedding model is loading; keyword search works meanwhile".into()
            }
            Status::Embedding { .. } => {
                "semantic search isn't ready: pages are being embedded; keyword search works meanwhile".into()
            }
            Status::On => crate::search::NO_VECTORS.into(),
        }
    }

    /// The search settings' fingerprint as searches run now: with the embedding model only
    /// while it is loaded.
    pub fn search_fingerprint(&self) -> &str {
        match &self.embedder {
            Some(e) if e.ready() => &self.fingerprints.0,
            _ => &self.fingerprints.1,
        }
    }

    /// Like [`AppState::new`] with the librarian's model given, as tests give a scripted one;
    /// `None` leaves the librarian off. No embedding model.
    pub fn with_llm(config: Config, admin_key: Option<&str>, llm: Option<llm::Llm>) -> Self {
        Self::with_models(config, admin_key, llm, None)
    }

    /// Like [`AppState::with_llm`], with the embedding model given too, always on.
    pub fn with_models(
        config: Config,
        admin_key: Option<&str>,
        llm: Option<llm::Llm>,
        embedder: Option<Arc<dyn embed::Embedder>>,
    ) -> Self {
        let off = llm::Missing::Model {
            file: config.librarian.llm_file_name(),
        };
        let login = login::Login::new(&config.data.dir, None, None);
        Self::build(config, admin_key, llm.ok_or(off), embedder, None, login)
    }

    fn build(
        config: Config,
        admin_key: Option<&str>,
        llm: Result<llm::Llm, llm::Missing>,
        embedder: Option<Arc<dyn embed::Embedder>>,
        embedding: Option<Arc<embed::Switch>>,
        login: login::Login,
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
            model: std::sync::RwLock::new(admin::ModelState {
                config: config.librarian.llm.clone(),
                entered_key: None,
            }),
            audit: admin::Audit::new(&config.data.dir),
            login,
            shutdown: Default::default(),
            fingerprints: (
                config
                    .search
                    .fingerprint(embedder.as_ref().map(|e| e.model())),
                config.search.fingerprint(None),
            ),
            embedder,
            embedding,
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

    /// Turns the librarian on with a key typed at the terminal prompt, kept until the service
    /// stops.
    pub fn set_entered_key(&self, key: String) -> Result<(), llm::Missing> {
        let mut state = self.model_state();
        state.entered_key = Some(key);
        let mut cfg = self.config.librarian.clone();
        cfg.llm = state.config.clone();
        let built = llm::client(&cfg, state.key().map(|(k, _)| k));
        let on = built.as_ref().map(|_| ()).map_err(Clone::clone);
        self.replace_model(state, built);
        on
    }

    /// The librarian's model as it is now.
    pub fn model_state(&self) -> admin::ModelState {
        self.model.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Switches to a model: the librarian is on with its client, or off with why.
    pub(crate) fn replace_model(
        &self,
        state: admin::ModelState,
        built: Result<llm::Llm, llm::Missing>,
    ) {
        match &built {
            Ok(_) => tracing::info!(
                model = state.config.model().unwrap_or_default(),
                "librarian on"
            ),
            Err(e) => tracing::warn!(error = %e, "the librarian is off"),
        }
        let librarian = built.map(|llm| librarian_for(&self.config, llm, self.embedder.clone()));
        *self.model.write().unwrap_or_else(|e| e.into_inner()) = state;
        *self.librarian.write().unwrap_or_else(|e| e.into_inner()) = librarian;
    }
}

/// Loads `[embedding] model`, downloading its files into `<data>/models` the first time, when
/// `[embedding] enabled`; `None` when it is off or can't be loaded. For tools that run beside
/// the service (replays); the service itself loads it in the background.
pub fn load_embedder(config: &Config) -> Option<Arc<dyn embed::Embedder>> {
    let name = config.embedding.model.as_deref()?;
    if !config.embedding.enabled {
        return None;
    }
    let dir = config.data.dir.join("models");
    let started = std::time::Instant::now();
    match embed::FastEmbedder::load(&dir) {
        Ok(e) => {
            tracing::info!(
                model = name,
                ms = started.elapsed().as_millis() as u64,
                "embedding model loaded"
            );
            Some(Arc::new(e))
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
