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

#[derive(Debug, Clone, Deserialize)]
pub struct IdentityConfig {
    #[serde(default)]
    pub mode: IdentityMode,
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
fn default_store_file() -> String {
    "kb.sqlite".into()
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
        let f = &self.store.file_name;
        if f.is_empty() || f.contains(['/', '\\']) || f.starts_with('.') {
            anyhow::bail!("store.file_name must be a plain file name, got {f:?}");
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
    }

    #[test]
    fn unknown_sections_are_ignored() {
        let cfg =
            Config::from_toml_str("[server]\nport = 9000\n[librarian]\nprovider = \"anthropic\"\n")
                .unwrap();
        assert_eq!(cfg.server.port, 9000);
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
