//! Per-tenant operational storage: API keys, the audit log, graph schema definitions, the
//! registry of graph spaces, the librarian's proposals and the documents it has filed (ids and
//! versions) and the vectors of wiki pages. Job status lives in memory in version 1
//! (`crate::jobs`). Each tenant has its own backend instance, so a store never sees another
//! tenant.

mod sqlite;

pub use sqlite::SqliteStore;

use async_trait::async_trait;
use serde::Serialize;

use crate::graph::{SchemaDef, SpaceDef};
use crate::librarian::documents::{Reading, ReadingState};
use crate::librarian::{Document, Proposal};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditEntry {
    pub at: String,
    pub actor: String,
    pub action: String,
    pub detail: String,
}

/// The vector of one part of a page (`search::vectors::Unit`), as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredVector {
    pub page: String,
    pub heading: String,
    /// The hash of the text it was built from.
    pub hash: String,
    pub vector: Vec<f32>,
}

#[async_trait]
pub trait Store: Send + Sync {
    /// The tenant id recorded when the store was created.
    async fn tenant_id(&self) -> anyhow::Result<Option<String>>;

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
    async fn add_document_version(
        &self,
        doc: &Document,
        source: &str,
        reading: Option<&Reading>,
    ) -> anyhow::Result<u32>;

    /// How a version was read (its ledger and main thread), if it was read step by step.
    async fn version_reading(&self, source: &str) -> anyhow::Result<Option<Reading>>;

    /// Saves where a document's reading is, after a step.
    async fn put_reading_state(&self, state: &ReadingState) -> anyhow::Result<()>;

    /// A reading left unfinished of the same text for the same audience, to resume.
    async fn reading_state(
        &self,
        audience: &str,
        content_hash: &str,
    ) -> anyhow::Result<Option<ReadingState>>;

    /// Notes who a raw record in `sources/` is for (`tenant`, or a group or user folder).
    async fn add_record(&self, name: &str, audience: &str) -> anyhow::Result<()>;

    /// Every raw record noted, with its audience.
    async fn records(&self) -> anyhow::Result<Vec<(String, String)>>;

    /// The raw records noted for an audience.
    async fn records_of(&self, audience: &str) -> anyhow::Result<Vec<String>>;

    /// Removes everything stored for a group or user folder, in one transaction: its held and
    /// approved proposals, its records' entries, its documents with their versions and
    /// readings, its pages' vectors and its lint findings and review notes. Deleted rows are overwritten, also in the WAL.
    async fn delete_audience(&self, audience: &str) -> anyhow::Result<()>;

    /// Replaces a registered space's definition (its owner and members).
    async fn put_graph_space(&self, def: &SpaceDef) -> anyhow::Result<()>;

    /// Removes a space from the registry.
    async fn remove_graph_space(&self, name: &str) -> anyhow::Result<()>;

    /// The last lint run and its findings, if lint has run.
    async fn lint_run(
        &self,
    ) -> anyhow::Result<
        Option<(
            crate::librarian::lint::LintRun,
            Vec<crate::librarian::lint::Stored>,
        )>,
    >;

    /// Each page as the last lint review of it saw it: page, hash.
    async fn lint_reviewed(&self) -> anyhow::Result<Vec<(String, String)>>;

    /// Stores a lint run in one transaction: its findings replace the last run's, and the
    /// pages it reviewed are noted with their hashes.
    async fn put_lint_run(
        &self,
        run: &crate::librarian::lint::LintRun,
        findings: &[crate::librarian::lint::Stored],
        reviewed: &[(String, String)],
    ) -> anyhow::Result<()>;

    /// Closes the store for good (the tenant is being deleted); every later call fails.
    fn close(&self);

    /// Every stored vector of one embedding model.
    async fn embeddings(&self, model: &str) -> anyhow::Result<Vec<StoredVector>>;

    /// Brings the stored vectors up to date in one transaction: writes `put` (replacing the
    /// same page and heading), removes `drop` (page, heading), and removes every vector of
    /// another model.
    async fn sync_embeddings(
        &self,
        model: &str,
        put: Vec<StoredVector>,
        drop: Vec<(String, String)>,
    ) -> anyhow::Result<()>;
}
