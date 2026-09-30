//! The SQLite graph backend: `graph.sqlite` in the tenant's folder. Every row still carries the
//! tenant id and every query filters on it, as with Neo4j.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use rusqlite::{Connection, params};

use super::def::{EdgeRecord, GraphData, NodeRecord};
use super::store::GraphStore;

const SCHEMA: &str = r#"
    CREATE TABLE IF NOT EXISTS nodes (
        tenant  TEXT NOT NULL,
        space   TEXT NOT NULL,
        type    TEXT NOT NULL,
        key     TEXT NOT NULL,
        name    TEXT NOT NULL,
        aliases TEXT NOT NULL,   -- JSON array
        PRIMARY KEY (tenant, space, type, key)
    );
    CREATE TABLE IF NOT EXISTS edges (
        tenant    TEXT NOT NULL,
        space     TEXT NOT NULL,
        type      TEXT NOT NULL,
        from_type TEXT NOT NULL,
        from_key  TEXT NOT NULL,
        to_type   TEXT NOT NULL,
        to_key    TEXT NOT NULL,
        PRIMARY KEY (tenant, space, type, from_type, from_key, to_type, to_key)
    );
"#;

pub struct SqliteGraphStore {
    tenant: String,
    path: PathBuf,
    /// Opened on first use, so a tenant that never stores graph data (or a request that only
    /// checks an API key) never creates the file.
    conn: Arc<Mutex<Option<Connection>>>,
}

impl SqliteGraphStore {
    /// The graph file of one tenant; nothing is opened or created until it is used.
    pub fn open(path: &Path, tenant: &str) -> anyhow::Result<Self> {
        Ok(Self {
            tenant: tenant.to_owned(),
            path: path.to_owned(),
            conn: Arc::new(Mutex::new(None)),
        })
    }

    fn connect(path: &Path) -> anyhow::Result<Connection> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(conn)
    }

    /// Runs `f` on the connection in a blocking thread. Without `create`, a missing file is
    /// not created and `f` isn't run: there is nothing stored yet.
    async fn with_conn<T, F>(&self, create: bool, f: F) -> anyhow::Result<Option<T>>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &str) -> anyhow::Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        let tenant = self.tenant.clone();
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("sqlite mutex poisoned"))?;
            if guard.is_none() {
                if !create && !path.exists() {
                    return Ok(None);
                }
                *guard = Some(Self::connect(&path)?);
            }
            let conn = guard.as_mut().expect("connected above");
            f(conn, &tenant).map(Some)
        })
        .await
        .context("graph store task panicked")?
    }
}

#[async_trait]
impl GraphStore for SqliteGraphStore {
    async fn prepare_space(&self, _space: &str) -> anyhow::Result<()> {
        // The primary keys already index everything a space needs.
        Ok(())
    }

    async fn write(&self, nodes: &[NodeRecord], edges: &[EdgeRecord]) -> anyhow::Result<()> {
        let (nodes, edges) = (nodes.to_vec(), edges.to_vec());
        self.with_conn(true, move |c, tenant| {
            let tx = c.transaction()?;
            {
                let mut put = tx.prepare(
                    "INSERT INTO nodes (tenant, space, type, key, name, aliases)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT (tenant, space, type, key)
                     DO UPDATE SET name = excluded.name, aliases = excluded.aliases",
                )?;
                for n in &nodes {
                    let aliases = serde_json::to_string(&n.aliases)?;
                    put.execute(params![tenant, n.space, n.ty, n.key, n.name, aliases])?;
                }
                let mut link = tx.prepare(
                    "INSERT OR IGNORE INTO edges
                     (tenant, space, type, from_type, from_key, to_type, to_key)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )?;
                for e in &edges {
                    link.execute(params![
                        tenant,
                        e.space,
                        e.ty,
                        e.from_type,
                        e.from_key,
                        e.to_type,
                        e.to_key
                    ])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
        .map(|_| ())
    }

    async fn load(&self) -> anyhow::Result<GraphData> {
        self.with_conn(false, |c, tenant| {
            let mut stmt = c.prepare(
                "SELECT space, type, key, name, aliases FROM nodes WHERE tenant = ?1
                 ORDER BY space, type, key",
            )?;
            let nodes = stmt
                .query_map(params![tenant], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })?
                .map(|row| {
                    let (space, ty, key, name, aliases) = row?;
                    Ok(NodeRecord {
                        space,
                        ty,
                        key,
                        name,
                        aliases: serde_json::from_str(&aliases)?,
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            let mut stmt = c.prepare(
                "SELECT space, type, from_type, from_key, to_type, to_key FROM edges
                 WHERE tenant = ?1 ORDER BY space, type, from_type, from_key, to_type, to_key",
            )?;
            let edges = stmt
                .query_map(params![tenant], |r| {
                    Ok(EdgeRecord {
                        space: r.get(0)?,
                        ty: r.get(1)?,
                        from_type: r.get(2)?,
                        from_key: r.get(3)?,
                        to_type: r.get(4)?,
                        to_key: r.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(GraphData { nodes, edges })
        })
        .await
        .map(Option::unwrap_or_default)
    }

    async fn delete_all(&self) -> anyhow::Result<()> {
        self.with_conn(false, |c, tenant| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM edges WHERE tenant = ?1", params![tenant])?;
            tx.execute("DELETE FROM nodes WHERE tenant = ?1", params![tenant])?;
            tx.commit()?;
            Ok(())
        })
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> (Vec<NodeRecord>, Vec<EdgeRecord>) {
        let node = |ty: &str, key: &str, name: &str| NodeRecord {
            space: "mainland".into(),
            ty: ty.into(),
            key: key.into(),
            name: name.into(),
            aliases: vec![format!("{name} alias")],
        };
        (
            vec![
                node("order", "1042", "Order 1042"),
                node("customer", "c_5521", "林晓"),
            ],
            vec![EdgeRecord {
                space: "mainland".into(),
                ty: "placed_by".into(),
                from_type: "order".into(),
                from_key: "1042".into(),
                to_type: "customer".into(),
                to_key: "c_5521".into(),
            }],
        )
    }

    #[tokio::test]
    async fn write_load_and_tenant_filter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graph.sqlite");
        let a = SqliteGraphStore::open(&path, "a").unwrap();
        let b = SqliteGraphStore::open(&path, "b").unwrap();
        // Reading and clearing never create the file; only a write does.
        assert_eq!(a.load().await.unwrap(), GraphData::default());
        a.delete_all().await.unwrap();
        assert!(!path.exists());
        let (nodes, edges) = sample();
        a.write(&nodes, &edges).await.unwrap();
        // Writing again replaces names and keeps one copy of each edge.
        let mut renamed = nodes.clone();
        renamed[0].name = "Order #1042".into();
        a.write(&renamed, &edges).await.unwrap();
        let loaded = a.load().await.unwrap();
        assert_eq!(loaded.nodes.len(), 2);
        assert!(loaded.nodes.iter().any(|n| n.name == "Order #1042"));
        assert_eq!(loaded.edges, edges);
        // Another tenant sees nothing, even in the same file.
        assert_eq!(b.load().await.unwrap(), GraphData::default());
        a.delete_all().await.unwrap();
        assert_eq!(a.load().await.unwrap(), GraphData::default());
    }
}
