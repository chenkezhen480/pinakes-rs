//! The SQLite backend: one file per tenant, in WAL mode so reads carry on while the tenant's
//! single writer works.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::{AuditEntry, Store};
use crate::graph::{SchemaDef, SpaceDef};

/// Schema migrations, applied in order. `PRAGMA user_version` records how many have run.
const MIGRATIONS: &[&str] = &[
    r#"
    CREATE TABLE meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    CREATE TABLE api_keys (
        hash       TEXT PRIMARY KEY,   -- SHA-256 of the key, hex
        label      TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
    );
    CREATE TABLE audit_log (
        id     INTEGER PRIMARY KEY AUTOINCREMENT,
        at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
        actor  TEXT NOT NULL,
        action TEXT NOT NULL,
        detail TEXT NOT NULL
    );
"#,
    r#"
    CREATE TABLE graph_schemas (
        name       TEXT PRIMARY KEY,
        definition TEXT NOT NULL,   -- JSON of graph::SchemaDef
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
        updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
    );
    CREATE TABLE graph_spaces (
        name       TEXT PRIMARY KEY,
        schema     TEXT NOT NULL,
        definition TEXT NOT NULL,   -- JSON of graph::SpaceDef
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
    );
"#,
];

pub struct SqliteStore {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    /// Creates a new database file. Fails if it already exists.
    pub fn create(path: &Path) -> anyhow::Result<Self> {
        if path.exists() {
            anyhow::bail!("store file {} already exists", path.display());
        }
        Self::open_with(path, OpenFlags::default())
    }

    /// Creates a new tenant database holding its id, a first API key hash and the creation audit
    /// entry, all in one transaction, then closes it.
    pub fn create_for_tenant(
        path: &Path,
        tenant_id: &str,
        key_hash: &str,
        key_label: &str,
    ) -> anyhow::Result<()> {
        let store = Self::create(path)?;
        let mut conn = store
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("sqlite mutex poisoned"))?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('tenant_id', ?1)",
            params![tenant_id],
        )?;
        tx.execute(
            "INSERT INTO api_keys (hash, label) VALUES (?1, ?2)",
            params![key_hash, key_label],
        )?;
        tx.execute(
            "INSERT INTO audit_log (actor, action, detail) VALUES ('admin', 'tenant.create', '')",
            [],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Opens an existing database file. Never creates one, so probing an unknown tenant can't
    /// leave an empty file behind.
    pub fn open_existing(path: &Path) -> anyhow::Result<Self> {
        let flags = OpenFlags::default() - OpenFlags::SQLITE_OPEN_CREATE;
        Self::open_with(path, flags)
    }

    fn open_with(path: &Path, flags: OpenFlags) -> anyhow::Result<Self> {
        let mut conn = Connection::open_with_flags(path, flags)
            .with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` on the connection in a blocking thread, so async handlers never stall on disk.
    async fn with_conn<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("sqlite mutex poisoned"))?;
            f(&guard).map_err(anyhow::Error::from)
        })
        .await
        .context("sqlite task panicked")?
    }
}

fn migrate(conn: &mut Connection) -> anyhow::Result<()> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = usize::try_from(current).context("negative store schema version")?;
    if current > MIGRATIONS.len() {
        anyhow::bail!(
            "store schema version {current} is newer than this build ({})",
            MIGRATIONS.len()
        );
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)
            .with_context(|| format!("store migration {}", i + 1))?;
        tx.pragma_update(None, "user_version", i64::try_from(i + 1)?)?;
        tx.commit()?;
    }
    Ok(())
}

#[async_trait]
impl Store for SqliteStore {
    async fn tenant_id(&self) -> anyhow::Result<Option<String>> {
        self.with_conn(|c| {
            c.query_row("SELECT value FROM meta WHERE key = 'tenant_id'", [], |r| {
                r.get(0)
            })
            .optional()
        })
        .await
    }

    async fn add_api_key(&self, key_hash: &str, label: &str) -> anyhow::Result<()> {
        let (hash, label) = (key_hash.to_owned(), label.to_owned());
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO api_keys (hash, label) VALUES (?1, ?2)",
                params![hash, label],
            )
            .map(|_| ())
        })
        .await
    }

    async fn api_key_hashes(&self) -> anyhow::Result<Vec<String>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare("SELECT hash FROM api_keys")?;
            stmt.query_map([], |r| r.get(0))?.collect()
        })
        .await
    }

    async fn audit(&self, actor: &str, action: &str, detail: &str) -> anyhow::Result<()> {
        let (actor, action, detail) = (actor.to_owned(), action.to_owned(), detail.to_owned());
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO audit_log (actor, action, detail) VALUES (?1, ?2, ?3)",
                params![actor, action, detail],
            )
            .map(|_| ())
        })
        .await
    }

    async fn audit_log(&self, limit: usize) -> anyhow::Result<Vec<AuditEntry>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT at, actor, action, detail FROM audit_log ORDER BY id DESC LIMIT ?1",
            )?;
            stmt.query_map(params![limit], |r| {
                Ok(AuditEntry {
                    at: r.get(0)?,
                    actor: r.get(1)?,
                    action: r.get(2)?,
                    detail: r.get(3)?,
                })
            })?
            .collect()
        })
        .await
    }

    async fn graph_schemas(&self) -> anyhow::Result<Vec<SchemaDef>> {
        let rows: Vec<String> = self
            .with_conn(|c| {
                let mut stmt = c.prepare("SELECT definition FROM graph_schemas ORDER BY name")?;
                stmt.query_map([], |r| r.get(0))?.collect()
            })
            .await?;
        rows.iter()
            .map(|r| serde_json::from_str(r).context("a stored schema definition"))
            .collect()
    }

    async fn put_graph_schema(&self, def: &SchemaDef) -> anyhow::Result<()> {
        let (name, json) = (def.schema.clone(), serde_json::to_string(def)?);
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO graph_schemas (name, definition) VALUES (?1, ?2)
                 ON CONFLICT (name) DO UPDATE SET definition = excluded.definition,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                params![name, json],
            )
            .map(|_| ())
        })
        .await
    }

    async fn graph_spaces(&self) -> anyhow::Result<Vec<SpaceDef>> {
        let rows: Vec<String> = self
            .with_conn(|c| {
                let mut stmt = c.prepare("SELECT definition FROM graph_spaces ORDER BY name")?;
                stmt.query_map([], |r| r.get(0))?.collect()
            })
            .await?;
        rows.iter()
            .map(|r| serde_json::from_str(r).context("a stored space definition"))
            .collect()
    }

    async fn add_graph_space(&self, def: &SpaceDef) -> anyhow::Result<()> {
        let (name, schema, json) = (
            def.space.clone(),
            def.schema.clone(),
            serde_json::to_string(def)?,
        );
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO graph_spaces (name, schema, definition) VALUES (?1, ?2, ?3)",
                params![name, schema, json],
            )
            .map(|_| ())
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pinakes.sqlite");
        SqliteStore::create_for_tenant(&path, "acme", "abc123", "initial").unwrap();
        let store = SqliteStore::open_existing(&path).unwrap();
        assert_eq!(store.tenant_id().await.unwrap().as_deref(), Some("acme"));
        store.add_api_key("def456", "second").await.unwrap();
        let mut keys = store.api_key_hashes().await.unwrap();
        keys.sort();
        assert_eq!(keys, vec!["abc123", "def456"]);
        store.audit("admin", "key.create", "second").await.unwrap();
        let log = store.audit_log(10).await.unwrap();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].action, "key.create");
        assert_eq!(log[1].action, "tenant.create");
    }

    #[tokio::test]
    async fn graph_schemas_and_spaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pinakes.sqlite");
        SqliteStore::create_for_tenant(&path, "acme", "abc", "initial").unwrap();
        let store = SqliteStore::open_existing(&path).unwrap();
        let mut def: SchemaDef = serde_json::from_value(serde_json::json!({
            "schema": "commerce", "nodes": { "order": { "key": "order_id" } }
        }))
        .unwrap();
        store.put_graph_schema(&def).await.unwrap();
        def.summary = Some("Orders.".into());
        store.put_graph_schema(&def).await.unwrap();
        assert_eq!(store.graph_schemas().await.unwrap(), vec![def]);

        let space = crate::graph::def::SpaceRequest {
            space: "mainland".into(),
            schema: "commerce".into(),
            name: None,
            aliases: vec![],
            visibility: crate::graph::Visibility::Shared,
            members: Default::default(),
        }
        .into_def(None)
        .unwrap();
        store.add_graph_space(&space).await.unwrap();
        assert!(
            store.add_graph_space(&space).await.is_err(),
            "names are unique"
        );
        assert_eq!(store.graph_spaces().await.unwrap(), vec![space]);
    }

    #[test]
    fn open_existing_never_creates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.sqlite");
        assert!(SqliteStore::open_existing(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn create_refuses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pinakes.sqlite");
        drop(SqliteStore::create(&path).unwrap());
        assert!(SqliteStore::create(&path).is_err());
    }
}
