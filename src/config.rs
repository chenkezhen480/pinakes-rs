//! Service configuration, loaded from `config.toml`.
//!
//! Sections that later milestones need (librarian, search, graph, ...) may already be present in
//! the file; serde ignores the ones this struct doesn't know yet.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
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
}

/// Where graph data lives, and the caps on walks (design, "The business graph").
#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GraphBackendKind {
    /// `graph.sqlite` in each tenant's folder: for tests and trying the service out.
    #[default]
    Sqlite,
    /// One Neo4j database shared by every tenant, filtered on the tenant id.
    Neo4j,
}

#[derive(Debug, Clone, Deserialize)]
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
#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Deserialize)]
pub struct SearchConfig {
    /// Hits per slot when a search doesn't say.
    #[serde(default)]
    pub slots: crate::search::SlotSizes,
    /// Read-tracker sessions expire after this many idle minutes.
    #[serde(default = "default_session_idle_minutes")]
    pub session_idle_minutes: u64,
    /// Switches for experiments; leave unset in normal use.
    #[serde(default)]
    pub ablation: crate::search::Ablation,
}

impl SearchConfig {
    /// 16 hex digits identifying the settings that shape search results (slot sizes and
    /// ablation switches), recorded in traces and replay reports.
    pub fn fingerprint(&self) -> String {
        use sha2::{Digest, Sha256};
        let shaping = serde_json::json!({ "slots": self.slots, "ablation": self.ablation });
        hex::encode(&Sha256::digest(shaping.to_string().as_bytes())[..8])
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            slots: crate::search::SlotSizes::default(),
            session_idle_minutes: default_session_idle_minutes(),
            ablation: crate::search::Ablation::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdentityMode {
    #[default]
    ApiKey,
    Jwt,
}

/// Who the service is set up for (design, "Personal deployments").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Deployment {
    /// Shared by many tenants; callers name their user in a header.
    #[default]
    MultiTenant,
    /// One person: the default tenant is created on first start, and a request without the
    /// user header acts as the default user.
    Personal,
}

#[derive(Debug, Clone, Deserialize)]
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
    /// Environment variable holding the service admin key. Unset or empty disables `/admin`.
    #[serde(default = "default_admin_key_env")]
    pub admin_key_env: String,
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
            admin_key_env: default_admin_key_env(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
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
fn default_admin_key_env() -> String {
    "KB_ADMIN_KEY".into()
}
fn default_session_idle_minutes() -> u64 {
    30
}
fn default_store_file() -> String {
    "kb.sqlite".into()
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

impl Config {
    pub fn from_toml_str(text: &str) -> anyhow::Result<Self> {
        let cfg: Config = toml::from_str(text).context("parsing config")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        Self::from_toml_str(&text)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.identity.mode == IdentityMode::Jwt {
            anyhow::bail!("identity.mode = \"jwt\" is not implemented yet (milestone M7)");
        }
        let id = &self.identity;
        crate::tenants::validate_id("identity.default_tenant", &id.default_tenant)
            .map_err(anyhow::Error::msg)?;
        crate::tenants::validate_id("identity.default_user", &id.default_user)
            .map_err(anyhow::Error::msg)?;
        let f = &self.store.file_name;
        if f.is_empty() || f.contains(['/', '\\']) || f.starts_with('.') {
            anyhow::bail!("store.file_name must be a plain file name, got {f:?}");
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
        assert_eq!(cfg.store.file_name, "kb.sqlite");
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
        let cfg =
            Config::from_toml_str("[server]\nport = 9000\n[librarian]\nprovider = \"anthropic\"\n")
                .unwrap();
        assert_eq!(cfg.server.port, 9000);
    }

    #[test]
    fn shipped_config_file_loads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.toml");
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.search.slots, crate::search::SlotSizes::default());
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
    fn jwt_mode_is_rejected_until_m7() {
        assert!(Config::from_toml_str("[identity]\nmode = \"jwt\"\n").is_err());
    }

    #[test]
    fn store_file_name_must_be_plain() {
        assert!(Config::from_toml_str("[store]\nfile_name = \"../x.sqlite\"\n").is_err());
    }
}
