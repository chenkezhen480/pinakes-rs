//! Where graph nodes and edges are stored: Neo4j, or SQLite for tests and for trying the
//! service without Neo4j. Searches never read a store; it is read into memory when a tenant
//! opens (see `graph::GraphSnapshot`).
//!
//! Each tenant gets its own `GraphStore` instance with the tenant id built in, so no call can
//! forget the tenant filter.

use async_trait::async_trait;

use super::def::{EdgeRecord, GraphData, NodeKey, NodeRecord};

#[async_trait]
pub trait GraphStore: Send + Sync {
    /// Makes sure the constraints and indexes a space needs exist. Idempotent.
    async fn prepare_space(&self, space: &str) -> anyhow::Result<()>;

    /// Writes nodes and edges in one transaction. A node with the type and key of a stored one
    /// in the same space replaces its name and aliases; an edge already stored is kept once.
    /// Callers check that both ends of every edge exist.
    async fn write(&self, nodes: &[NodeRecord], edges: &[EdgeRecord]) -> anyhow::Result<()>;

    /// Applies an approved change in one transaction: removes the edges listed and the nodes
    /// listed with every edge of theirs, then writes nodes and edges as [`GraphStore::write`]
    /// does.
    async fn apply(
        &self,
        remove_nodes: &[NodeKey],
        remove_edges: &[EdgeRecord],
        nodes: &[NodeRecord],
        edges: &[EdgeRecord],
    ) -> anyhow::Result<()>;

    /// Every node and edge of the tenant.
    async fn load(&self) -> anyhow::Result<GraphData>;

    /// Removes every node and edge of the tenant.
    async fn delete_all(&self) -> anyhow::Result<()>;

    /// Removes every node and edge of one space.
    async fn delete_space(&self, space: &str) -> anyhow::Result<()>;

    /// Lets go of any file the store holds open (the tenant is being deleted); later calls fail.
    fn close(&self) {}
}
