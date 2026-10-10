//! The Neo4j graph backend. Neo4j Community has one database, so every tenant shares it: each
//! node is labelled `PinakesNode` plus its type, and carries `tenant`, `space`, `type` and `key`;
//! each edge is a `PINAKES_REL` relationship carrying `tenant`, `space` and `type`. Every query
//! binds the tenant, and nothing else ever reaches Cypher text except type names, which are
//! validated identifiers.
//!
//! Each deployment has its own Neo4j, so `tenant` is the tenant id.
//!
//! Checked against Neo4j 5.26 Community: composite property uniqueness constraints are
//! available there (node key and existence constraints are Enterprise only), so a key is unique
//! within its tenant, space and type; code validates the rest.

use std::collections::{BTreeMap, HashMap};

use anyhow::Context;
use async_trait::async_trait;
use neo4rs::{BoltType, Graph, query};
use tokio::sync::OnceCell;

use super::def::{EdgeRecord, GraphData, NodeKey, NodeRecord, validate_name};
use super::store::GraphStore;
use crate::config::Neo4jConfig;

/// Constraints and indexes, created once per connection. Idempotent.
const SETUP: &[&str] = &[
    "CREATE CONSTRAINT pinakes_node_identity IF NOT EXISTS FOR (n:PinakesNode) \
     REQUIRE (n.tenant, n.space, n.type, n.key) IS UNIQUE",
    "CREATE INDEX pinakes_node_tenant IF NOT EXISTS FOR (n:PinakesNode) ON (n.tenant)",
    "CREATE INDEX pinakes_rel_tenant IF NOT EXISTS FOR ()-[r:PINAKES_REL]-() ON (r.tenant)",
];

/// Rows per `UNWIND` statement.
const CHUNK: usize = 1_000;

/// One connection pool for the whole service, opened on first use.
pub struct Neo4jClient {
    cfg: Neo4jConfig,
    graph: OnceCell<Graph>,
}

impl Neo4jClient {
    pub fn new(cfg: Neo4jConfig) -> Self {
        Self {
            cfg,
            graph: OnceCell::new(),
        }
    }

    /// The pool, connecting and creating the constraints on first use.
    pub async fn graph(&self) -> anyhow::Result<&Graph> {
        self.graph
            .get_or_try_init(|| async {
                let password = std::env::var(&self.cfg.password_env).with_context(|| {
                    format!("the Neo4j password is read from ${}", self.cfg.password_env)
                })?;
                let mut config = neo4rs::ConfigBuilder::default()
                    .uri(self.cfg.uri.as_str())
                    .user(self.cfg.user.as_str())
                    .password(password);
                if let Some(db) = &self.cfg.database {
                    config = config.db(db.as_str());
                }
                let graph = Graph::connect(config.build()?)
                    .with_context(|| format!("connecting to Neo4j at {}", self.cfg.uri))?;
                for q in SETUP {
                    graph
                        .run(query(q))
                        .await
                        .with_context(|| format!("Neo4j setup: {q}"))?;
                }
                anyhow::Ok(graph)
            })
            .await
    }

    /// Connects at startup, once: if Neo4j can't be reached the service doesn't start, with one
    /// error saying why. A missing password is a configuration error.
    pub async fn connect(&self) -> anyhow::Result<()> {
        if std::env::var_os(&self.cfg.password_env).is_none() {
            anyhow::bail!(
                "graph.backend is \"neo4j\" but ${} (the Neo4j password) is not set; set it, or use backend = \"sqlite\" to run without Neo4j",
                self.cfg.password_env
            );
        }
        self.graph()
            .await
            .map(|_| ())
            .context("Neo4j can't be reached")
    }
}

/// One tenant's view of Neo4j.
pub struct Neo4jGraphStore {
    client: std::sync::Arc<Neo4jClient>,
    tenant: String,
}

impl Neo4jGraphStore {
    pub fn new(client: std::sync::Arc<Neo4jClient>, tenant_id: &str) -> Self {
        Self {
            tenant: tenant_id.to_owned(),
            client,
        }
    }

    /// The pool and the `tenant` value every query binds.
    async fn open(&self) -> anyhow::Result<(&Graph, &str)> {
        Ok((self.client.graph().await?, self.tenant.as_str()))
    }
}

fn node_row(n: &NodeRecord) -> HashMap<String, BoltType> {
    HashMap::from([
        ("space".to_owned(), n.space.as_str().into()),
        ("key".to_owned(), n.key.as_str().into()),
        ("name".to_owned(), n.name.as_str().into()),
        ("aliases".to_owned(), n.aliases.clone().into()),
        (
            "properties".to_owned(),
            serde_json::to_string(&n.properties)
                .unwrap_or_else(|_| "{}".into())
                .into(),
        ),
    ])
}

fn edge_row(e: &EdgeRecord) -> HashMap<String, BoltType> {
    HashMap::from([
        ("space".to_owned(), e.space.as_str().into()),
        ("type".to_owned(), e.ty.as_str().into()),
        ("from_type".to_owned(), e.from_type.as_str().into()),
        ("from_key".to_owned(), e.from_key.as_str().into()),
        ("to_type".to_owned(), e.to_type.as_str().into()),
        ("to_key".to_owned(), e.to_key.as_str().into()),
    ])
}

#[async_trait]
impl GraphStore for Neo4jGraphStore {
    async fn prepare_space(&self, _space: &str) -> anyhow::Result<()> {
        // The constraints and indexes are per label, shared by every space; connecting creates
        // them if they are missing.
        self.client.graph().await.map(|_| ())
    }

    async fn write(&self, nodes: &[NodeRecord], edges: &[EdgeRecord]) -> anyhow::Result<()> {
        self.apply(&[], &[], nodes, edges).await
    }

    async fn apply(
        &self,
        remove_nodes: &[NodeKey],
        remove_edges: &[EdgeRecord],
        nodes: &[NodeRecord],
        edges: &[EdgeRecord],
    ) -> anyhow::Result<()> {
        let (graph, tenant) = self.open().await?;
        let mut txn = graph.start_txn().await?;
        for chunk in remove_edges.chunks(CHUNK) {
            let rows: Vec<HashMap<String, BoltType>> = chunk.iter().map(edge_row).collect();
            txn.run(
                query(
                    "UNWIND $rows AS row \
                     MATCH (a:PinakesNode {tenant: $tenant, space: row.space, type: row.from_type, key: row.from_key}) \
                           -[r:PINAKES_REL {tenant: $tenant, space: row.space, type: row.type}]-> \
                           (b:PinakesNode {tenant: $tenant, space: row.space, type: row.to_type, key: row.to_key}) \
                     DELETE r",
                )
                .param("tenant", tenant)
                .param("rows", rows),
            )
            .await?;
        }
        for chunk in remove_nodes.chunks(CHUNK) {
            let rows: Vec<HashMap<String, BoltType>> = chunk
                .iter()
                .map(|n| {
                    HashMap::from([
                        ("space".to_owned(), n.space.as_str().into()),
                        ("type".to_owned(), n.ty.as_str().into()),
                        ("key".to_owned(), n.key.as_str().into()),
                    ])
                })
                .collect();
            txn.run(
                query(
                    "UNWIND $rows AS row \
                     MATCH (n:PinakesNode {tenant: $tenant, space: row.space, type: row.type, key: row.key}) \
                     DETACH DELETE n",
                )
                .param("tenant", tenant)
                .param("rows", rows),
            )
            .await?;
        }
        let mut by_type: BTreeMap<&str, Vec<&NodeRecord>> = BTreeMap::new();
        for n in nodes {
            by_type.entry(n.ty.as_str()).or_default().push(n);
        }
        for (ty, list) in by_type {
            // A label can't be a parameter; type names are validated identifiers.
            validate_name("node type", ty).map_err(anyhow::Error::msg)?;
            let cypher = format!(
                "UNWIND $rows AS row \
                 MERGE (n:PinakesNode {{tenant: $tenant, space: row.space, type: $type, key: row.key}}) \
                 SET n.name = row.name, n.aliases = row.aliases, \
                     n.properties = row.properties, n:`{ty}`"
            );
            for chunk in list.chunks(CHUNK) {
                let rows: Vec<HashMap<String, BoltType>> =
                    chunk.iter().map(|n| node_row(n)).collect();
                txn.run(
                    query(&cypher)
                        .param("tenant", tenant)
                        .param("type", ty)
                        .param("rows", rows),
                )
                .await?;
            }
        }
        for chunk in edges.chunks(CHUNK) {
            let rows: Vec<HashMap<String, BoltType>> = chunk.iter().map(edge_row).collect();
            txn.run(
                query(
                    "UNWIND $rows AS row \
                     MATCH (a:PinakesNode {tenant: $tenant, space: row.space, type: row.from_type, key: row.from_key}) \
                     MATCH (b:PinakesNode {tenant: $tenant, space: row.space, type: row.to_type, key: row.to_key}) \
                     MERGE (a)-[:PINAKES_REL {tenant: $tenant, space: row.space, type: row.type}]->(b)",
                )
                .param("tenant", tenant)
                .param("rows", rows),
            )
            .await?;
        }
        txn.commit().await?;
        Ok(())
    }

    async fn load(&self) -> anyhow::Result<GraphData> {
        let (graph, tenant) = self.open().await?;
        let mut out = GraphData::default();
        let mut rows = graph
            .execute(
                query(
                    "MATCH (n:PinakesNode {tenant: $tenant}) \
                     RETURN n.space AS space, n.type AS type, n.key AS key, n.name AS name, \
                            n.aliases AS aliases, n.properties AS properties \
                     ORDER BY space, type, key",
                )
                .param("tenant", tenant),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            out.nodes.push(NodeRecord {
                space: row.get("space")?,
                ty: row.get("type")?,
                key: row.get("key")?,
                name: row.get("name")?,
                aliases: row
                    .get::<Option<Vec<String>>>("aliases")?
                    .unwrap_or_default(),
                properties: match row.get::<Option<String>>("properties")? {
                    Some(json) => serde_json::from_str(&json)?,
                    None => Default::default(),
                },
            });
        }
        let mut rows = graph
            .execute(
                query(
                    "MATCH (a:PinakesNode {tenant: $tenant})-[r:PINAKES_REL {tenant: $tenant}]->(b:PinakesNode {tenant: $tenant}) \
                     RETURN r.space AS space, r.type AS type, a.type AS from_type, a.key AS from_key, \
                            b.type AS to_type, b.key AS to_key \
                     ORDER BY space, type, from_type, from_key, to_type, to_key",
                )
                .param("tenant", tenant),
            )
            .await?;
        while let Some(row) = rows.next().await? {
            out.edges.push(EdgeRecord {
                space: row.get("space")?,
                ty: row.get("type")?,
                from_type: row.get("from_type")?,
                from_key: row.get("from_key")?,
                to_type: row.get("to_type")?,
                to_key: row.get("to_key")?,
            });
        }
        Ok(out)
    }

    async fn delete_all(&self) -> anyhow::Result<()> {
        let (graph, tenant) = self.open().await?;
        graph
            .run(
                query("MATCH (n:PinakesNode {tenant: $tenant}) DETACH DELETE n")
                    .param("tenant", tenant),
            )
            .await?;
        Ok(())
    }

    async fn delete_space(&self, space: &str) -> anyhow::Result<()> {
        let (graph, tenant) = self.open().await?;
        graph
            .run(
                query("MATCH (n:PinakesNode {tenant: $tenant, space: $space}) DETACH DELETE n")
                    .param("tenant", tenant)
                    .param("space", space),
            )
            .await?;
        Ok(())
    }
}
