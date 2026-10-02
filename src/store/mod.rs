//! Per-tenant operational storage: API keys, the audit log, graph schema definitions, the
//! registry of graph spaces, the librarian's proposals and the documents it has filed (ids and
//! versions); embeddings arrive in M5. Job status
//! lives in memory in version 1 (`crate::jobs`). Each tenant has its own
//! backend instance, so a store never sees another tenant.

mod sqlite;

pub use sqlite::SqliteStore;

use async_trait::async_trait;
use serde::Serialize;

use crate::graph::{SchemaDef, SpaceDef};
use crate::librarian::{Document, Proposal};

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

    /// Every graph schema definition, the authoritative copies.
    async fn graph_schemas(&self) -> anyhow::Result<Vec<SchemaDef>>;

    /// Creates or replaces a schema definition. Callers check that a change is additive.
    async fn put_graph_schema(&self, def: &SchemaDef) -> anyhow::Result<()>;

    /// Every registered graph space.
    async fn graph_spaces(&self) -> anyhow::Result<Vec<SpaceDef>>;

    /// Registers a new space. Fails if the name is taken.
    async fn add_graph_space(&self, def: &SpaceDef) -> anyhow::Result<()>;

    /// Stores a proposal, or replaces it (to change its status).
    async fn put_proposal(&self, p: &Proposal) -> anyhow::Result<()>;

    async fn proposal(&self, id: &str) -> anyhow::Result<Option<Proposal>>;

    /// Proposals still waiting for review, oldest first.
    async fn held_proposals(&self) -> anyhow::Result<Vec<Proposal>>;

    /// A document the service issued an id for.
    async fn document(&self, id: &str) -> anyhow::Result<Option<Document>>;

    /// The raw records of a document's versions, oldest first.
    async fn document_sources(&self, id: &str) -> anyhow::Result<Vec<String>>;

    /// Every raw record that is a document version, with its document.
    async fn documents_by_source(&self) -> anyhow::Result<Vec<(String, Document)>>;

    /// Records a raw record as a document's next version (creating the document, or updating
    /// its title), in one transaction. Returns the version number, from 1.
    async fn add_document_version(&self, doc: &Document, source: &str) -> anyhow::Result<u32>;
}
