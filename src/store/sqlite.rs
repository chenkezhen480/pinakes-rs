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
use crate::librarian::documents::{Reading, ReadingState};
use crate::librarian::{Document, Proposal};

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
    r#"
    CREATE TABLE proposals (
        id         TEXT PRIMARY KEY,
        status     TEXT NOT NULL,   -- held or approved
        audience   TEXT NOT NULL,   -- tenant, or the group or user folder
        proposal   TEXT NOT NULL,   -- JSON of librarian::Proposal
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
    );
    CREATE INDEX proposals_status ON proposals (status);
"#,
    r#"
    CREATE TABLE documents (
        id         TEXT PRIMARY KEY,   -- d_ plus 16 hex, issued by the service
        audience   TEXT NOT NULL,      -- tenant, or the group folder
        title      TEXT NOT NULL,      -- the latest version's title
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
        updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
    );
    CREATE TABLE document_versions (
        document   TEXT NOT NULL REFERENCES documents (id),
        version    INTEGER NOT NULL,
        source     TEXT NOT NULL UNIQUE,   -- the raw record in sources/
        created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
        PRIMARY KEY (document, version)
    );
"#,
    r#"
    -- How each version was read: its ledger and main thread (JSON of documents::Reading).
    ALTER TABLE document_versions ADD COLUMN reading TEXT;
    -- A document being read, saved after every step so a stopped job can resume.
    CREATE TABLE readings (
        source       TEXT PRIMARY KEY,   -- the raw record being read
        document     TEXT NOT NULL,      -- the document it is a version of
        audience     TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        state        TEXT NOT NULL,      -- JSON of documents::ReadingState
        updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
    );
    CREATE INDEX readings_content ON readings (audience, content_hash);
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

    async fn put_proposal(&self, p: &Proposal) -> anyhow::Result<()> {
        let (id, status, audience, json) = (
            p.id.clone(),
            p.status.as_str().to_owned(),
            p.audience.clone(),
            serde_json::to_string(p)?,
        );
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO proposals (id, status, audience, proposal) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET status = excluded.status,
                 proposal = excluded.proposal",
                params![id, status, audience, json],
            )
            .map(|_| ())
        })
        .await
    }

    async fn proposal(&self, id: &str) -> anyhow::Result<Option<Proposal>> {
        let id = id.to_owned();
        let row: Option<String> = self
            .with_conn(move |c| {
                c.query_row(
                    "SELECT proposal FROM proposals WHERE id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()
            })
            .await?;
        row.map(|r| serde_json::from_str(&r).context("a stored proposal"))
            .transpose()
    }

    async fn held_proposals(&self) -> anyhow::Result<Vec<Proposal>> {
        let rows: Vec<String> = self
            .with_conn(|c| {
                let mut stmt = c.prepare(
                    "SELECT proposal FROM proposals WHERE status = 'held' ORDER BY created_at, id",
                )?;
                stmt.query_map([], |r| r.get(0))?.collect()
            })
            .await?;
        rows.iter()
            .map(|r| serde_json::from_str(r).context("a stored proposal"))
            .collect()
    }

    async fn document(&self, id: &str) -> anyhow::Result<Option<Document>> {
        let id = id.to_owned();
        self.with_conn(move |c| {
            c.query_row(
                "SELECT id, audience, title FROM documents WHERE id = ?1",
                params![id],
                |r| {
                    Ok(Document {
                        id: r.get(0)?,
                        audience: r.get(1)?,
                        title: r.get(2)?,
                    })
                },
            )
            .optional()
        })
        .await
    }

    async fn document_sources(&self, id: &str) -> anyhow::Result<Vec<String>> {
        let id = id.to_owned();
        self.with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT source FROM document_versions WHERE document = ?1 ORDER BY version",
            )?;
            stmt.query_map(params![id], |r| r.get(0))?.collect()
        })
        .await
    }

    async fn documents_by_source(&self) -> anyhow::Result<Vec<(String, Document)>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT v.source, d.id, d.audience, d.title
                 FROM document_versions v JOIN documents d ON d.id = v.document",
            )?;
            stmt.query_map([], |r| {
                Ok((
                    r.get(0)?,
                    Document {
                        id: r.get(1)?,
                        audience: r.get(2)?,
                        title: r.get(3)?,
                    },
                ))
            })?
            .collect()
        })
        .await
    }

    async fn add_document_version(
        &self,
        doc: &Document,
        source: &str,
        reading: Option<&Reading>,
    ) -> anyhow::Result<u32> {
        let (doc, source) = (doc.clone(), source.to_owned());
        let reading = reading.map(serde_json::to_string).transpose()?;
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || -> anyhow::Result<u32> {
            let mut c = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("sqlite mutex poisoned"))?;
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO documents (id, audience, title) VALUES (?1, ?2, ?3)
                 ON CONFLICT (id) DO UPDATE SET title = excluded.title,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                params![doc.id, doc.audience, doc.title],
            )?;
            let version: u32 = tx.query_row(
                "SELECT COALESCE(MAX(version), 0) + 1 FROM document_versions WHERE document = ?1",
                params![doc.id],
                |r| r.get(0),
            )?;
            tx.execute(
                "INSERT INTO document_versions (document, version, source, reading)
                 VALUES (?1, ?2, ?3, ?4)",
                params![doc.id, version, source, reading],
            )?;
            // The version is filed: its reading, and any reading of the document left
            // unfinished (an upload that changed again before it was resumed), are done.
            tx.execute(
                "DELETE FROM readings WHERE source = ?1 OR document = ?2",
                params![source, doc.id],
            )?;
            tx.commit()?;
            Ok(version)
        })
        .await
        .context("sqlite task panicked")?
    }

    async fn version_reading(&self, source: &str) -> anyhow::Result<Option<Reading>> {
        let source = source.to_owned();
        let text: Option<Option<String>> = self
            .with_conn(move |c| {
                c.query_row(
                    "SELECT reading FROM document_versions WHERE source = ?1",
                    params![source],
                    |r| r.get(0),
                )
                .optional()
            })
            .await?;
        text.flatten()
            .map(|t| serde_json::from_str(&t).context("a stored reading"))
            .transpose()
    }

    async fn put_reading_state(&self, state: &ReadingState) -> anyhow::Result<()> {
        let text = serde_json::to_string(state)?;
        let (source, document, audience, hash) = (
            state.source.clone(),
            state.document.id.clone(),
            state.document.audience.clone(),
            state.content_hash.clone(),
        );
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO readings (source, document, audience, content_hash, state)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (source) DO UPDATE SET state = excluded.state,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                params![source, document, audience, hash, text],
            )
            .map(|_| ())
        })
        .await
    }

    async fn reading_state(
        &self,
        audience: &str,
        content_hash: &str,
    ) -> anyhow::Result<Option<ReadingState>> {
        let (audience, hash) = (audience.to_owned(), content_hash.to_owned());
        let text: Option<String> = self
            .with_conn(move |c| {
                c.query_row(
                    "SELECT state FROM readings WHERE audience = ?1 AND content_hash = ?2
                     ORDER BY updated_at DESC LIMIT 1",
                    params![audience, hash],
                    |r| r.get(0),
                )
                .optional()
            })
            .await?;
        text.map(|t| serde_json::from_str(&t).context("a stored reading state"))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn documents_and_versions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pinakes.sqlite");
        SqliteStore::create_for_tenant(&path, "acme", "abc", "initial").unwrap();
        let store = SqliteStore::open_existing(&path).unwrap();
        let mut doc = Document {
            id: "d_0011223344556677".into(),
            audience: "tenant".into(),
            title: "Manual".into(),
        };
        assert_eq!(
            store
                .add_document_version(&doc, "a.json", None)
                .await
                .unwrap(),
            1
        );
        doc.title = "Manual v2".into();
        // A reading in progress is kept until its version is recorded.
        let reading = Reading {
            thread: "A manual.".into(),
            steps: vec![],
        };
        let state = ReadingState {
            source: "b.json".into(),
            content_hash: "h".into(),
            document: doc.clone(),
            how: crate::librarian::DocumentMatch::Given,
            version: 2,
            cursor: 7,
            reading: reading.clone(),
        };
        store.put_reading_state(&state).await.unwrap();
        let back = store.reading_state("tenant", "h").await.unwrap().unwrap();
        assert_eq!(back.cursor, 7);
        assert!(store.reading_state("g_x", "h").await.unwrap().is_none());
        assert_eq!(
            store
                .add_document_version(&doc, "b.json", Some(&reading))
                .await
                .unwrap(),
            2
        );
        assert!(store.reading_state("tenant", "h").await.unwrap().is_none());
        assert_eq!(
            store.version_reading("b.json").await.unwrap(),
            Some(reading)
        );
        assert_eq!(store.version_reading("a.json").await.unwrap(), None);
        assert!(
            store
                .add_document_version(&doc, "b.json", None)
                .await
                .is_err(),
            "a record is one version"
        );
        assert_eq!(store.document(&doc.id).await.unwrap(), Some(doc.clone()));
        assert_eq!(store.document("d_ffffffffffffffff").await.unwrap(), None);
        assert_eq!(
            store.document_sources(&doc.id).await.unwrap(),
            vec!["a.json", "b.json"]
        );
        let mut by_source = store.documents_by_source().await.unwrap();
        by_source.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(by_source.len(), 2);
        assert_eq!(by_source[1], ("b.json".to_owned(), doc));
    }

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
