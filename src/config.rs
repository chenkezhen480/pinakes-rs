//! Service configuration, loaded from `config.toml`.
//!
//! Sections that later milestones need (librarian, search, graph, ...) may already be present in
//! the file; serde ignores the ones this struct doesn't know yet.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub data: DataConfig,
    #[serde(default)]
    pub identity: IdentityConfig,
    #[serde(default)]
    pub store: StoreConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub trace: TraceConfig,
    #[serde(default)]
    pub graph: GraphConfig,
    #[serde(default)]
    pub neo4j: Neo4jConfig,
    #[serde(default)]
    pub librarian: LibrarianConfig,
    #[serde(default)]
    pub jobs: JobsConfig,
    #[serde(default)]
    pub converter: ConverterConfig,
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub acquisition: AcquisitionConfig,
    /// The file this was loaded from, which settings changed at run time are written back to;
    /// `None` for a config not read from a file (defaults, tests).
    #[serde(skip)]
    pub file: Option<PathBuf>,
}

/// Filling gaps from outside (design, "Filling gaps from outside, with approval"). Off unless
/// turned on here and, for each tenant, by the admin (`PUT /admin/tenants/{id}/acquisition`).
/// Only the tenant's trusted sites are read; nothing else is asked.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcquisitionConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Tokens acquisition's own model calls may use per tenant and day (UTC).
    #[serde(default = "default_daily_token_budget")]
    pub daily_token_budget: u64,
    /// Most pages read per trusted site.
    #[serde(default = "default_crawl_pages")]
    pub crawl_pages_per_site: usize,
    /// Days before a tenant's sites are read again (at once when its allowlist changes).
    #[serde(default = "default_crawl_days")]
    pub crawl_every_days: u32,
    /// The pause between two requests to the sites, so none is pressed.
    #[serde(default = "default_crawl_delay")]
    pub crawl_delay_ms: u64,
    /// Lets pages be fetched from this machine or private networks. Only for tests: a tenant's
    /// allowlist must never reach the service's own network.
    #[serde(default)]
    pub allow_private_hosts: bool,
    /// Read and ignored: the search providers of the configs shipped before M9 (`providers =
    /// []`), so they still start; no search engine is used.
    #[serde(default, skip_serializing)]
    pub providers: Option<serde::de::IgnoredAny>,
}

impl Default for AcquisitionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            daily_token_budget: default_daily_token_budget(),
            crawl_pages_per_site: default_crawl_pages(),
            crawl_every_days: default_crawl_days(),
            crawl_delay_ms: default_crawl_delay(),
            allow_private_hosts: false,
            providers: None,
        }
    }
}

fn default_crawl_pages() -> usize {
    500
}
fn default_crawl_days() -> u32 {
    7
}
fn default_crawl_delay() -> u64 {
    1000
}
fn default_daily_token_budget() -> u64 {
    200_000
}

/// Open tenants in memory (design, "How it stays fast").
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    /// A tenant no request used for this long gives up its search snapshot and graph; the next
    /// request loads them again. 0 keeps them for as long as the service runs.
    #[serde(default = "default_idle_minutes")]
    pub idle_minutes: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            idle_minutes: default_idle_minutes(),
        }
    }
}

fn default_idle_minutes() -> u64 {
    30
}

/// The local embedding model for meaning-based search (design, "Search signals").
#[derive(Debug, Clone, Default, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// Whether page vectors are computed and meaning-based search can run. Off by default, so
    /// a first start downloads nothing; turned on here or from the console, which applies at
    /// once (the model is downloaded and loaded in the background) and writes it back here.
    /// (A config from before this line existed, with a model and a meaning-based default
    /// mode, is read as on, as it was then.)
    #[serde(default)]
    pub enabled: bool,
    /// `jina-embeddings-v2-base-zh`, the one model the service runs. Unset, embeddings can't
    /// be turned on. Its files are downloaded into `<data>/models` the first time it is on.
    #[serde(default)]
    pub model: Option<String>,
}

/// The document converter (design, "Files are turned into Markdown first").
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConverterConfig {
    /// Where MarkItDown answers `POST /convert`, such as `http://127.0.0.1:8090`. Unset, the
    /// old binary Office formats are refused with what to do.
    #[serde(default)]
    pub url: Option<String>,
    /// Timeout of one conversion.
    #[serde(default = "default_convert_timeout")]
    pub timeout_secs: u64,
    /// The `pinakes-pdf` binary that reads PDFs in a process of its own; unset, the one next to
    /// the service's executable.
    #[serde(default)]
    pub pdf_reader: Option<PathBuf>,
}

impl Default for ConverterConfig {
    fn default() -> Self {
        Self {
            url: None,
            timeout_secs: default_convert_timeout(),
            pdf_reader: None,
        }
    }
}

fn default_convert_timeout() -> u64 {
    120
}

/// Which LLM API the librarian calls (design, "The librarian: how new content is filed").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize, Default)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum LlmProvider {
    #[default]
    Anthropic,
    OpenaiCompatible,
}

/// The librarian's model, from its own file, `model.conf` (next to `config.toml`; see
/// `model.conf.example`). It is the one place to say which LLM the service calls, and it may
/// hold the key, so it is kept out of git.
#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    #[serde(default)]
    pub provider: LlmProvider,
    /// API root; required for `openai_compatible`; for `anthropic`, unset is Anthropic itself.
    #[serde(default)]
    pub base_url: Option<String>,
    /// The one model for every step. There is no default: until it is set the librarian is off.
    #[serde(default)]
    pub model: Option<String>,
    /// The API key. When empty, the variable named by `key_env` is read instead.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Environment variable holding the key when `api_key` is empty. Defaults to
    /// `ANTHROPIC_API_KEY` or `OPENAI_API_KEY` by provider.
    #[serde(default)]
    pub key_env: Option<String>,
}

impl std::fmt::Debug for ModelConfig {
    // The key never reaches a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelConfig")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key", &self.api_key.as_ref().map(|_| "…"))
            .field("key_env", &self.key_env)
            .finish()
    }
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

impl ModelConfig {
    pub fn from_toml_str(text: &str) -> anyhow::Result<Self> {
        toml::from_str(text).context("parsing the model file")
    }

    /// The model's name, if one is set.
    pub fn model(&self) -> Option<String> {
        non_empty(&self.model)
    }

    pub fn key_env(&self) -> String {
        non_empty(&self.key_env).unwrap_or_else(|| {
            match self.provider {
                LlmProvider::OpenaiCompatible => "OPENAI_API_KEY",
                LlmProvider::Anthropic => "ANTHROPIC_API_KEY",
            }
            .to_owned()
        })
    }

    /// The key: `api_key` from the file, else the environment variable `key_env` names.
    pub fn key(&self) -> Option<String> {
        non_empty(&self.api_key).or_else(|| {
            std::env::var(self.key_env())
                .ok()
                .map(|k| k.trim().to_owned())
                .filter(|k| !k.is_empty())
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibrarianConfig {
    /// The model file, relative to the config file's folder.
    #[serde(default = "default_model_file")]
    pub model_file: PathBuf,
    /// Most tokens one reply may use.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    /// Timeout of one LLM call.
    #[serde(default = "default_llm_timeout")]
    pub timeout_secs: u64,
    /// How many related pages the librarian is shown (design: the best 5 to 10).
    #[serde(default = "default_related_pages")]
    pub related_pages: usize,
    /// About how many tokens of a document one read is shown, from the cursor on; it grows
    /// when a concept runs past it (design, "Reading like a person, one concept at a time").
    #[serde(default = "default_read_window_tokens")]
    pub read_window_tokens: usize,
    /// Approve every proposal at once instead of leaving it for an admin: what the verifier held
    /// (filing, lint's fixes, hand edits), the sources acquisition found and agents' graph
    /// drafts. Each goes through the same path as an admin's approval (code checks, the graph
    /// checked again, a source filed by the librarian) and is kept as an approved proposal.
    #[serde(default)]
    pub auto_approve: bool,
    /// What the model file says, once read ([`Config::load_model_file`]).
    #[serde(skip)]
    pub llm: ModelConfig,
    /// Where the model file is, once looked for, for telling the user where to fill it in.
    #[serde(skip)]
    pub llm_file: Option<PathBuf>,
}

impl LibrarianConfig {
    /// The model file's path as shown to the user.
    pub fn llm_file_name(&self) -> String {
        self.llm_file
            .as_deref()
            .unwrap_or(&self.model_file)
            .display()
            .to_string()
    }
}

impl Default for LibrarianConfig {
    fn default() -> Self {
        Self {
            model_file: default_model_file(),
            max_tokens: default_max_tokens(),
            timeout_secs: default_llm_timeout(),
            related_pages: default_related_pages(),
            read_window_tokens: default_read_window_tokens(),
            auto_approve: false,
            llm: ModelConfig::default(),
            llm_file: None,
        }
    }
}

/// The per-tenant write queue.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobsConfig {
    /// Finished jobs stay visible at `/jobs/{id}` this long.
    #[serde(default = "default_keep_finished")]
    pub keep_finished_minutes: u64,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            keep_finished_minutes: default_keep_finished(),
        }
    }
}

/// Where graph data lives, and the caps on walks (design, "The business graph").
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphConfig {
    #[serde(default)]
    pub backend: GraphBackendKind,
    /// Deepest `/graph` walk a caller may ask for.
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    /// Most nodes a `/graph` walk may return.
    #[serde(default = "default_max_nodes")]
    pub max_nodes: usize,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            backend: GraphBackendKind::default(),
            max_depth: default_max_depth(),
            max_nodes: default_max_nodes(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GraphBackendKind {
    /// `graph.sqlite` in each tenant's folder, the default: nothing to run beside the service.
    #[default]
    Sqlite,
    /// One Neo4j database shared by every tenant, filtered on the tenant id (optional).
    Neo4j,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Neo4jConfig {
    #[serde(default = "default_neo4j_uri")]
    pub uri: String,
    #[serde(default = "default_neo4j_user")]
    pub user: String,
    /// Environment variable holding the password; never in the file.
    #[serde(default = "default_neo4j_password_env")]
    pub password_env: String,
    /// Database name; the server's default database when unset.
    #[serde(default)]
    pub database: Option<String>,
}

impl Default for Neo4jConfig {
    fn default() -> Self {
        Self {
            uri: default_neo4j_uri(),
            user: default_neo4j_user(),
            password_env: default_neo4j_password_env(),
            database: None,
        }
    }
}

/// Search traces (design, "Search traces and replay").
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceConfig {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Trace files older than this many days are removed; 0 keeps them forever.
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

impl Default for TraceConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: default_retention_days(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct SearchConfig {
    /// Hits per slot when a search doesn't say.
    #[serde(default)]
    pub slots: crate::search::SlotSizes,
    /// Read-tracker sessions expire after this many idle minutes.
    #[serde(default = "default_session_idle_minutes")]
    pub session_idle_minutes: u64,
    /// The mode of a search that doesn't say.
    #[serde(default)]
    pub default_mode: crate::search::Mode,
    /// The memory-to-knowledge hop.
    #[serde(default)]
    pub hop: HopConfig,
    /// Switches for experiments; leave unset in normal use.
    #[serde(default)]
    pub ablation: crate::search::Ablation,
    /// From this many page vectors (rows) on, a tenant keeps a graph index over them and
    /// meaning searches walk it instead of comparing every row (0: never).
    #[serde(default = "default_vector_graph_rows")]
    pub vector_graph_rows: usize,
}

/// The memory-to-knowledge hop (design, "How one search runs", step 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HopConfig {
    /// How many of the top memories lead to the knowledge pages they are anchored to.
    pub top_memories: usize,
}

impl Default for HopConfig {
    fn default() -> Self {
        Self { top_memories: 3 }
    }
}

impl SearchConfig {
    /// 16 hex digits identifying the settings that shape search results (slot sizes, default
    /// mode, hop, ablation switches and the embedding model), recorded in traces and replay
    /// reports.
    pub fn fingerprint(&self, embedding_model: Option<&str>) -> String {
        use sha2::{Digest, Sha256};
        let shaping = serde_json::json!({
            "slots": self.slots,
            "ablation": self.ablation,
            "default_mode": self.default_mode,
            "hop": self.hop,
            "embedding": embedding_model,
            "vector_graph_rows": self.vector_graph_rows,
        });
        hex::encode(&Sha256::digest(shaping.to_string().as_bytes())[..8])
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            slots: crate::search::SlotSizes::default(),
            session_idle_minutes: default_session_idle_minutes(),
            default_mode: crate::search::Mode::default(),
            hop: HopConfig::default(),
            ablation: crate::search::Ablation::default(),
            vector_graph_rows: default_vector_graph_rows(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// How many threads the service computes on (see `par`); 0, the default, is 4 (fewer on a
    /// machine with fewer logical CPUs).
    #[serde(default)]
    pub threads: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            threads: 0,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct DataConfig {
    /// Root data folder; tenants live in `<dir>/tenants/`.
    #[serde(default = "default_data_dir")]
    pub dir: PathBuf,
}

impl Default for DataConfig {
    fn default() -> Self {
        Self {
            dir: default_data_dir(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize, Default)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum IdentityMode {
    #[default]
    ApiKey,
    Jwt,
}

/// Who the service is set up for (design, "Personal deployments").
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, Deserialize, Default)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum Deployment {
    /// Shared by many tenants; callers name their user in a header.
    #[default]
    MultiTenant,
    /// One person: the default tenant is created on first start, and a request without the
    /// user header acts as the default user.
    Personal,
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct IdentityConfig {
    #[serde(default)]
    pub mode: IdentityMode,
    #[serde(default)]
    pub deployment: Deployment,
    /// The tenant a personal deployment creates and uses.
    #[serde(default = "default_tenant")]
    pub default_tenant: String,
    /// The user a request without the user header acts as, in a personal deployment. An
    /// ordinary user id: it grants no admin rights.
    #[serde(default = "default_user")]
    pub default_user: String,
    /// Header carrying the API key. For `Authorization`, a `Bearer ` prefix is stripped.
    #[serde(default = "default_key_header")]
    pub key_header: String,
    /// Header carrying the end-user id in API key mode.
    #[serde(default = "default_user_header")]
    pub user_header: String,
    /// Header carrying the caller's group ids (comma-separated) in API key mode.
    #[serde(default = "default_group_header")]
    pub group_header: String,
    /// Header naming who an insert is for: `tenant`, `user` or `group:<group id>`, one of the
    /// caller's own groups. Without it, the kind's default.
    #[serde(default = "default_audience_header")]
    pub audience_header: String,
    /// Environment variable holding the service admin key. Unset or empty disables `/admin`.
    #[serde(default = "default_admin_key_env")]
    pub admin_key_env: String,
    /// JWT mode: the public key (PEM) the host signs tokens with, relative to the config file.
    #[serde(default)]
    pub public_key_file: Option<PathBuf>,
    /// JWT mode: the signing algorithm: RS256/384/512, PS256/384/512, ES256/384 or EdDSA.
    #[serde(default = "default_jwt_algorithm")]
    pub algorithm: String,
    /// JWT mode: the claims holding the tenant id, the user id and the group ids (a list).
    #[serde(default = "default_tenant_claim")]
    pub tenant_claim: String,
    #[serde(default = "default_user_claim")]
    pub user_claim: String,
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,
    /// JWT mode: when set, a token's `iss` must be this.
    #[serde(default)]
    pub issuer: Option<String>,
    /// JWT mode: when set, a token's `aud` must name this; when unset, a token with an `aud`
    /// is refused.
    #[serde(default)]
    pub audience: Option<String>,
    /// JWT mode: the key, read by [`Config::load_jwt_key`].
    #[serde(skip)]
    pub jwt: Option<std::sync::Arc<crate::identity::JwtKey>>,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            mode: IdentityMode::default(),
            deployment: Deployment::default(),
            default_tenant: default_tenant(),
            default_user: default_user(),
            key_header: default_key_header(),
            user_header: default_user_header(),
            group_header: default_group_header(),
            audience_header: default_audience_header(),
            admin_key_env: default_admin_key_env(),
            public_key_file: None,
            algorithm: default_jwt_algorithm(),
            tenant_claim: default_tenant_claim(),
            user_claim: default_user_claim(),
            groups_claim: default_groups_claim(),
            issuer: None,
            audience: None,
            jwt: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, Deserialize)]
pub struct StoreConfig {
    #[serde(default = "default_store_file")]
    pub file_name: String,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            file_name: default_store_file(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    8080
}
fn default_data_dir() -> PathBuf {
    PathBuf::from("./data")
}
fn yes() -> bool {
    true
}
fn default_retention_days() -> u32 {
    30
}
fn default_tenant() -> String {
    "000000".into()
}
fn default_user() -> String {
    "admin".into()
}
fn default_key_header() -> String {
    "Authorization".into()
}
fn default_user_header() -> String {
    "X-User-Id".into()
}
fn default_group_header() -> String {
    "X-User-Groups".into()
}
fn default_audience_header() -> String {
    "X-Audience".into()
}
fn default_admin_key_env() -> String {
    "PINAKES_ADMIN_KEY".into()
}
fn default_jwt_algorithm() -> String {
    "RS256".into()
}
fn default_tenant_claim() -> String {
    "tenant_id".into()
}
fn default_user_claim() -> String {
    "sub".into()
}
fn default_groups_claim() -> String {
    "groups".into()
}
fn default_session_idle_minutes() -> u64 {
    30
}
/// About 25,000 pages, where comparing every row starts to cost more than walking the graph
/// (design, "As built in M9").
fn default_vector_graph_rows() -> usize {
    100_000
}
fn default_store_file() -> String {
    "pinakes.sqlite".into()
}
fn default_max_depth() -> usize {
    3
}
fn default_max_nodes() -> usize {
    100
}
fn default_neo4j_uri() -> String {
    "bolt://localhost:7687".into()
}
fn default_neo4j_user() -> String {
    "neo4j".into()
}
fn default_neo4j_password_env() -> String {
    "NEO4J_PASSWORD".into()
}
fn default_max_tokens() -> u32 {
    16_000
}
fn default_llm_timeout() -> u64 {
    300
}
fn default_model_file() -> PathBuf {
    PathBuf::from("model.conf")
}
fn default_related_pages() -> usize {
    8
}
fn default_read_window_tokens() -> usize {
    20_000
}
fn default_keep_finished() -> u64 {
    60
}

/// Sets `key` in `[table]` of the config file at `path`; see [`write_values`].
pub fn write_value(
    path: &Path,
    table: &str,
    key: &str,
    value: toml_edit::Value,
) -> anyhow::Result<()> {
    write_values(path, &[(format!("{table}.{key}"), Some(value))])
}

/// Sets fields (`section.key` or `section.table.key`) of the config file at `path`, keeping
/// its comments and layout: a value replaced keeps the comment after it, a missing line or
/// section is added, and `None` removes the line (the field goes back to its default). The
/// result must still load as a config, or the file is left as it was; it is replaced whole,
/// never half written.
pub fn write_values(
    path: &Path,
    changes: &[(String, Option<toml_edit::Value>)],
) -> anyhow::Result<()> {
    // One write at a time: two at once would each read the file before the other wrote it.
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = WRITING.lock().unwrap_or_else(|e| e.into_inner());
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    for (field, value) in changes {
        let parts: Vec<&str> = field.split('.').collect();
        let (key, tables) = parts.split_last().context("an empty field name")?;
        let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
        // Unsetting a field whose table isn't there leaves nothing to do.
        if value.is_none()
            && tables
                .iter()
                .try_fold(&*table, |t, name| {
                    t.get(name).and_then(toml_edit::Item::as_table_like)
                })
                .is_none()
        {
            continue;
        }
        for t in tables {
            if table.get(t).is_none() {
                table.insert(t, toml_edit::table());
            }
            table = table
                .get_mut(t)
                .and_then(toml_edit::Item::as_table_like_mut)
                .with_context(|| format!("{field}: {t} in {} is not a table", path.display()))?;
        }
        match (value, table.get_mut(key)) {
            (None, _) => {
                table.remove(key);
            }
            // The value is replaced; the comment after it stays.
            (Some(value), Some(toml_edit::Item::Value(old))) => {
                let decor = old.decor().clone();
                *old = value.clone();
                *old.decor_mut() = decor;
            }
            (Some(value), _) => {
                table.insert(key, toml_edit::Item::Value(value.clone()));
            }
        }
    }
    let text = doc.to_string();
    let tmp = path.with_extension("toml.new");
    std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
    // Loaded as the service starts would load it (the model file, a JWT key too), beside the
    // real file so relative paths mean the same: a change that would stop the next start is
    // refused, and the file stays as it was.
    if let Err(e) = Config::load(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.context("the changed config doesn't load"));
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

impl Config {
    pub fn from_toml_str(text: &str) -> anyhow::Result<Self> {
        let mut cfg: Config = toml::from_str(text).context("parsing config")?;
        // Before the switch existed (M10), a named model meant embeddings on. A config from
        // then whose default_mode needs meaning (and has no `enabled` line) keeps starting as it
        // did, with them on; otherwise no line means off.
        if cfg.embedding.model.is_some()
            && cfg.search.default_mode.needs_meaning()
            && !cfg.embedding.enabled
        {
            let raw: toml::Table = toml::from_str(text).context("parsing config")?;
            let says = raw
                .get("embedding")
                .and_then(|e| e.get("enabled"))
                .is_some();
            if !says {
                cfg.embedding.enabled = true;
            }
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Loads a config file, then the model file it names (relative to its folder).
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let mut cfg = Self::from_toml_str(&text)?;
        cfg.file = Some(path.to_path_buf());
        let base = path.parent().unwrap_or(Path::new("."));
        cfg.load_model_file(base)?;
        cfg.load_jwt_key(base)?;
        Ok(cfg)
    }

    /// In JWT mode, reads the public key (`[identity] public_key_file`, relative to `base`) for
    /// the configured algorithm. A key that can't be read or doesn't fit the algorithm is an
    /// error: the service doesn't start without the means to check its callers.
    pub fn load_jwt_key(&mut self, base: &Path) -> anyhow::Result<()> {
        let id = &mut self.identity;
        if id.mode != IdentityMode::Jwt {
            return Ok(());
        }
        let file = base.join(id.public_key_file.as_ref().expect("checked by validate"));
        let pem = std::fs::read(&file).with_context(|| format!("reading {}", file.display()))?;
        let key = crate::identity::JwtKey::new(&id.algorithm, &pem)
            .with_context(|| format!("in {}", file.display()))?;
        id.jwt = Some(std::sync::Arc::new(key));
        Ok(())
    }

    /// Reads the model file (`[librarian] model_file`, relative to `base`). A missing file is
    /// not an error: the librarian stays off and the service says where to put it. A file that
    /// can't be parsed is an error.
    pub fn load_model_file(&mut self, base: &Path) -> anyhow::Result<()> {
        let path = base.join(&self.librarian.model_file);
        self.librarian.llm_file = Some(path.clone());
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.librarian.llm = ModelConfig::from_toml_str(&text)
                    .with_context(|| format!("in {}", path.display()))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    file = %path.display(),
                    "no model file: copy model.conf.example to it and fill it in; the librarian is off"
                );
            }
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("reading {}", path.display())));
            }
        }
        Ok(())
    }

    fn validate(&self) -> anyhow::Result<()> {
        let id = &self.identity;
        if id.mode == IdentityMode::Jwt && id.public_key_file.is_none() {
            anyhow::bail!("identity.mode = \"jwt\" needs identity.public_key_file");
        }
        crate::tenants::validate_id("identity.default_tenant", &id.default_tenant)
            .map_err(anyhow::Error::msg)?;
        crate::tenants::validate_id("identity.default_user", &id.default_user)
            .map_err(anyhow::Error::msg)?;
        let f = &self.store.file_name;
        if f.is_empty() || f.contains(['/', '\\']) || f.starts_with('.') {
            anyhow::bail!("store.file_name must be a plain file name, got {f:?}");
        }
        if let Some(m) = &self.embedding.model
            && !crate::embed::known_model(m)
        {
            anyhow::bail!(
                "embedding.model must be {:?}, got {m:?}",
                crate::embed::MODEL
            );
        }
        if self.embedding.enabled && self.embedding.model.is_none() {
            anyhow::bail!("embedding.enabled needs an [embedding] model");
        }
        if self.search.default_mode.needs_meaning() && !self.embedding.enabled {
            anyhow::bail!(
                "search.default_mode {:?} needs embeddings on ([embedding] enabled = true)",
                self.search.default_mode
            );
        }
        let s = &self.search.slots;
        let max = crate::search::MAX_SLOT;
        if s.memories > max || s.knowledge > max || s.relations > max {
            anyhow::bail!("search.slots values must be at most {max}");
        }
        let g = &self.graph;
        if !(1..=crate::graph::MAX_WALK_DEPTH).contains(&g.max_depth)
            || !(1..=crate::graph::MAX_WALK_NODES).contains(&g.max_nodes)
        {
            anyhow::bail!(
                "graph.max_depth must be 1 to {} and graph.max_nodes 1 to {}",
                crate::graph::MAX_WALK_DEPTH,
                crate::graph::MAX_WALK_NODES
            );
        }
        let l = &self.librarian;
        if !(1..=20).contains(&l.related_pages) || l.max_tokens == 0 || l.timeout_secs == 0 {
            anyhow::bail!(
                "librarian.related_pages must be 1 to 20, max_tokens and timeout_secs above 0"
            );
        }
        let a = &self.search.ablation;
        if !a.words && !a.bigrams {
            anyhow::bail!("search.ablation must keep words or bigrams on");
        }
        Ok(())
    }

    pub fn tenants_dir(&self) -> PathBuf {
        self.data.dir.join("tenants")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        let cfg = Config::from_toml_str("").unwrap();
        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.identity.mode, IdentityMode::ApiKey);
        assert_eq!(cfg.identity.user_header, "X-User-Id");
        assert_eq!(cfg.identity.group_header, "X-User-Groups");
        assert_eq!(cfg.identity.audience_header, "X-Audience");
        assert_eq!(cfg.store.file_name, "pinakes.sqlite");
        assert_eq!(cfg.search.slots.relations, 5);
        assert_eq!(cfg.search.session_idle_minutes, 30);
        assert!(cfg.trace.enabled);
        assert_eq!(cfg.trace.retention_days, 30);
        assert_eq!(cfg.graph.backend, GraphBackendKind::Sqlite);
        assert_eq!(cfg.graph.max_depth, 3);
        assert_eq!(cfg.neo4j.password_env, "NEO4J_PASSWORD");
    }

    #[test]
    fn search_slots_from_file() {
        let cfg = Config::from_toml_str(
            "[search]
slots = { memories = 4, knowledge = 2, relations = 6 }
",
        )
        .unwrap();
        assert_eq!(cfg.search.slots.memories, 4);
        assert!(
            Config::from_toml_str(
                "[search]
slots = { memories = 40, knowledge = 2, relations = 6 }
"
            )
            .is_err()
        );
    }

    #[test]
    fn unknown_sections_are_ignored() {
        let cfg = Config::from_toml_str("[server]\nport = 9000\n[acquisition]\nenabled = false\n")
            .unwrap();
        assert_eq!(cfg.server.port, 9000);
    }

    /// A config of before M9, with the search providers it named, still loads.
    #[test]
    fn an_older_acquisition_section_still_loads() {
        let cfg = Config::from_toml_str(
            "[acquisition]
enabled = false
providers = []
daily_token_budget = 200000
",
        )
        .unwrap();
        assert!(cfg.acquisition.providers.is_some());
        assert_eq!(cfg.acquisition.crawl_pages_per_site, 500);
        assert!(
            Config::from_toml_str(
                "[acquisition]
search_url = \"x\"
"
            )
            .is_err()
        );
    }

    #[test]
    fn librarian_defaults() {
        let cfg = Config::from_toml_str("").unwrap();
        assert_eq!(cfg.librarian.model_file, PathBuf::from("model.conf"));
        assert_eq!(cfg.librarian.llm.provider, LlmProvider::Anthropic);
        assert_eq!(cfg.librarian.llm.model(), None);
        assert_eq!(cfg.jobs.keep_finished_minutes, 60);
        assert!(Config::from_toml_str("[librarian]\nrelated_pages = 0\n").is_err());
        assert!(
            Config::from_toml_str("[librarian]\nmodel = \"x\"\n").is_err(),
            "the model has its own file"
        );
        let m = ModelConfig::from_toml_str(
            "provider = \"openai_compatible\"\nbase_url = \"http://localhost:11434/v1\"\nmodel = \" \"\napi_key = \"secret\"\n",
        )
        .unwrap();
        assert_eq!(m.key_env(), "OPENAI_API_KEY");
        assert_eq!(m.model(), None, "blank is unset");
        assert_eq!(m.key().as_deref(), Some("secret"), "the file's key first");
        assert!(
            !format!("{m:?}").contains("secret"),
            "the key never reaches a log"
        );
        assert!(ModelConfig::from_toml_str("modle = \"x\"\n").is_err());
    }

    #[test]
    fn a_meaning_based_default_mode_needs_embeddings_on() {
        let err = Config::from_toml_str("[search]\ndefault_mode = \"semantic\"\n").unwrap_err();
        assert!(
            format!("{err:#}").contains("needs embeddings on"),
            "{err:#}"
        );
        Config::from_toml_str("[search]\ndefault_mode = \"auto\"\n").unwrap();
        let model = "[embedding]\nmodel = \"jina-embeddings-v2-base-zh\"\n";
        let both = "[search]\ndefault_mode = \"both\"\n";
        // A config from before the switch (a model, no `enabled` line) starts with them on.
        assert!(
            Config::from_toml_str(&format!("{both}{model}"))
                .unwrap()
                .embedding
                .enabled
        );
        assert!(!Config::from_toml_str(model).unwrap().embedding.enabled);
        assert!(Config::from_toml_str(&format!("{both}{model}enabled = false\n")).is_err());
        Config::from_toml_str(&format!("{both}{model}enabled = true\n")).unwrap();
        assert!(Config::from_toml_str("[embedding]\nenabled = true\n").is_err());
        assert!(Config::from_toml_str("[embedding]\nmodel = \"multilingual-e5-small\"\n").is_err());
    }

    #[test]
    fn a_value_written_back_keeps_the_file_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let text = "# top\n[embedding]   # local\nenabled = false   # off: keywords only\nmodel = \"jina-embeddings-v2-base-zh\"\n";
        std::fs::write(&path, text).unwrap();
        write_value(&path, "embedding", "enabled", true.into()).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, text.replace("enabled = false", "enabled = true"));
        assert!(Config::load(&path).unwrap().embedding.enabled);

        // A missing line or section is added.
        write_value(&path, "cache", "idle_minutes", 5i64.into()).unwrap();
        assert_eq!(Config::load(&path).unwrap().cache.idle_minutes, 5);
        // A value the config refuses leaves the file as it was.
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(write_value(&path, "search", "default_mode", "nonsense".into()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // So does one that would stop the next start: a JWT key file that isn't there.
        let err = write_values(
            &path,
            &[
                ("identity.mode".into(), Some("jwt".into())),
                (
                    "identity.public_key_file".into(),
                    Some("missing.pem".into()),
                ),
            ],
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("missing.pem"), "{err:#}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        assert!(!path.with_extension("toml.new").exists());
    }

    #[test]
    fn shipped_config_and_model_example_load() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut cfg =
            Config::from_toml_str(&std::fs::read_to_string(root.join("config.toml")).unwrap())
                .unwrap();
        assert_eq!(cfg.search.slots, crate::search::SlotSizes::default());
        assert_eq!(cfg.librarian.model_file, PathBuf::from("model.conf"));
        // The image's config is the same apart from where things are.
        let docker = Config::from_toml_str(
            &std::fs::read_to_string(root.join("docker/config.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(docker.data.dir, PathBuf::from("/data"));
        assert_eq!(docker.neo4j.uri, "bolt://neo4j:7687");
        // Graph data in SQLite unless a deployment chooses Neo4j.
        assert_eq!(cfg.graph.backend, GraphBackendKind::Sqlite);
        assert_eq!(docker.graph.backend, GraphBackendKind::Sqlite);
        assert_eq!(docker.search.default_mode, cfg.search.default_mode);
        // The example is what a deployment copies to model.conf.
        cfg.librarian.model_file = PathBuf::from("model.conf.example");
        cfg.load_model_file(root).unwrap();
        let m = &cfg.librarian.llm;
        assert_eq!(
            m.base_url.as_deref(),
            Some("https://api.deepseek.com/anthropic")
        );
        assert_eq!(m.model().as_deref(), Some("deepseek-flash"));
        assert_eq!(m.key_env(), "DEEPSEEK_API_KEY");
        assert_eq!(m.api_key.as_deref(), Some(""), "no key in the example");
        assert!(crate::llm::client(&cfg.librarian, Some("k".into())).is_ok());
        // A missing model file leaves the librarian off and names the file.
        let mut cfg = Config::from_toml_str("").unwrap();
        let dir = tempfile::tempdir().unwrap();
        cfg.load_model_file(dir.path()).unwrap();
        let why = crate::llm::client(&cfg.librarian, None)
            .err()
            .unwrap()
            .to_string();
        assert!(why.contains("model.conf"), "{why}");
    }

    #[test]
    fn personal_deployment_defaults() {
        let cfg = Config::from_toml_str("[identity]\ndeployment = \"personal\"\n").unwrap();
        assert_eq!(cfg.identity.deployment, Deployment::Personal);
        assert_eq!(cfg.identity.default_tenant, "000000");
        assert_eq!(cfg.identity.default_user, "admin");
        assert_eq!(
            Config::from_toml_str("").unwrap().identity.deployment,
            Deployment::MultiTenant
        );
        assert!(Config::from_toml_str("[identity]\ndefault_user = \"\"\n").is_err());
    }

    #[test]
    fn jwt_mode_needs_a_public_key_file() {
        assert!(Config::from_toml_str("[identity]\nmode = \"jwt\"\n").is_err());
    }

    #[test]
    fn store_file_name_must_be_plain() {
        assert!(Config::from_toml_str("[store]\nfile_name = \"../x.sqlite\"\n").is_err());
    }
}
