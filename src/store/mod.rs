//! Per-tenant operational storage: API keys, the audit log, and later jobs, proposals and
//! embeddings. Each tenant has its own backend instance, so a store never sees another tenant.

mod sqlite;

pub use sqlite::SqliteStore;

use async_trait::async_trait;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditEntry {
    pub at: String,
    pub actor: String,
    pub action: String,
    pub detail: String,
}

#[async_trait]
pub trait Store: Send + Sync {
    /// The tenant id recorded when the store was created.
    async fn tenant_id(&self) -> anyhow::Result<Option<String>>;

    /// Stores the SHA-256 hash (hex) of an API key. The key itself is never stored.
    async fn add_api_key(&self, key_hash: &str, label: &str) -> anyhow::Result<()>;

    async fn api_key_hashes(&self) -> anyhow::Result<Vec<String>>;

    async fn audit(&self, actor: &str, action: &str, detail: &str) -> anyhow::Result<()>;

    /// The most recent audit entries, newest first.
    async fn audit_log(&self, limit: usize) -> anyhow::Result<Vec<AuditEntry>>;
}
