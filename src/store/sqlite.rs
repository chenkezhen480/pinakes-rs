//! The SQLite backend: one file per tenant, in WAL mode so reads carry on while the tenant's
//! single writer works.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use super::{AuditEntry, Store, StoredJob, StoredVector};
use crate::graph::{SchemaDef, SpaceDef};
use crate::librarian::documents::{Reading, ReadingState};
use crate::librarian::lint::{LintRun, Stored};
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
    r#"
    -- Vectors of wiki pages for meaning-based search, derived data like the search index.
    CREATE TABLE embeddings (
        page    TEXT NOT NULL,   -- e.g. experience/refund-handling
        heading TEXT NOT NULL,   -- the part of the page: '' for its head, else a section heading
        model   TEXT NOT NULL,   -- the model and its revision, e.g. jina-embeddings-v2-base-zh@c1ff9086
        hash    TEXT NOT NULL,   -- hash of the text it was built from
        vector  BLOB NOT NULL,   -- float32 values, little-endian
        PRIMARY KEY (page, heading, model)
    );
"#,
    r#"
    -- Who each raw record in sources/ is for, so deleting a user or group finds its records.
    CREATE TABLE records (
        name     TEXT PRIMARY KEY,   -- the record's file name in sources/
        audience TEXT NOT NULL       -- tenant, or the group or user folder
    );
    CREATE INDEX records_audience ON records (audience);
"#,
    r#"
    -- The last lint run's findings (JSON of lint::Stored), replaced by each run.
    CREATE TABLE lint_findings (
        id       INTEGER PRIMARY KEY AUTOINCREMENT,
        audience TEXT NOT NULL,   -- tenant, or the group or user folder that may see it
        finding  TEXT NOT NULL
    );
    -- Each page as the last lint review of it saw it, so a run reviews only what changed.
    CREATE TABLE lint_reviewed (
        page TEXT PRIMARY KEY,
        hash TEXT NOT NULL
    );
"#,
    r#"
    -- The trusted sites' pages acquisition's crawl indexed: no text, only what matching needs
    -- (JSON of acquisition::crawl::CrawledPage).
    CREATE TABLE crawled_pages (
        url  TEXT PRIMARY KEY,
        site TEXT NOT NULL,
        page TEXT NOT NULL
    );
"#,
    r#"
    -- Job status, kept across restarts (JSON of jobs::JobInfo); finished jobs are removed
    -- once they expire ([jobs] keep_finished_minutes).
    CREATE TABLE jobs (
        id       TEXT PRIMARY KEY,
        owner    TEXT,      -- the user folder that queued it, if any
        info     TEXT NOT NULL,
        finished INTEGER    -- Unix seconds; NULL while queued or running
    );
"#,
];

pub struct SqliteStore {
    /// `None` once closed (the tenant is being deleted): every call then fails.
    conn: Arc<Mutex<Option<Connection>>>,
    /// Whether the store holds no vectors of other embedding models: set when they were
    /// removed, or when reading this model's vectors found none. The model doesn't change while
    /// the service runs, so they are looked for at most once (that reads the whole table, over
    /// a second at 100,000 pages), never at every write.
    other_models_cleared: Arc<std::sync::atomic::AtomicBool>,
}

/// A tenant store's id, read with a read-only connection: no migration, no journal change,
/// nothing written (for listing tenants without opening them).
pub fn read_tenant_id(path: &Path) -> anyhow::Result<Option<String>> {
    use rusqlite::OptionalExtension;
    let c = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(
        c.query_row("SELECT value FROM meta WHERE key = 'tenant_id'", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()?,
    )
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
        let mut guard = store
            .conn
            .lock()
            .map_err(|_| anyhow::anyhow!("sqlite mutex poisoned"))?;
        let conn = guard.as_mut().context("the store is closed")?;
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
        // Deleted rows are overwritten, not left in free pages (user and group deletion).
        conn.pragma_update(None, "secure_delete", "ON")?;
        migrate(&mut conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(Some(conn))),
            other_models_cleared: Arc::default(),
        })
    }

    /// Runs `f` on the connection in a blocking thread, so async handlers never stall on disk.
    async fn with_conn<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        self.with_conn_mut(move |c| f(c).map_err(anyhow::Error::from))
            .await
    }

    /// Like `with_conn`, for work that needs a transaction.
    async fn with_conn_mut<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> anyhow::Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn
                .lock()
                .map_err(|_| anyhow::anyhow!("sqlite mutex poisoned"))?;
            f(guard.as_mut().context("the store is closed")?)
        })
        .await
        .context("sqlite task panicked")?
    }
}

/// Copies the write-ahead log into the database and truncates it, so old copies of deleted
/// rows leave it; a reader still in the log makes it busy, so it is tried again three times.
pub(crate) fn checkpoint(conn: &Connection) -> anyhow::Result<()> {
    for attempt in 0..4 {
        let busy: i64 = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))?;
        if busy == 0 {
            return Ok(());
        }
        if attempt < 3 {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    anyhow::bail!("the write-ahead log is still in use: deleted rows may remain in it; try again")
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

    async fn api_key_hashes(&self) -> anyhow::Result<Vec<String>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare("SELECT hash FROM api_keys")?;
            stmt.query_map([], |r| r.get(0))?.collect()
        })
        .await
    }

    async fn meta(&self, key: &str) -> anyhow::Result<Option<String>> {
        let key = key.to_owned();
        self.with_conn(move |c| {
            c.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
        })
        .await
    }

    async fn set_meta(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let (key, value) = (key.to_owned(), value.to_owned());
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![key, value],
            )
            .map(|_| ())
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

    async fn proposals(&self) -> anyhow::Result<Vec<Proposal>> {
        let rows: Vec<String> = self
            .with_conn(|c| {
                let mut stmt =
                    c.prepare("SELECT proposal FROM proposals ORDER BY created_at, id")?;
                stmt.query_map([], |r| r.get(0))?.collect()
            })
            .await?;
        rows.iter()
            .map(|r| serde_json::from_str(r).context("a stored proposal"))
            .collect()
    }

    async fn delete_proposal(&self, id: &str) -> anyhow::Result<()> {
        let id = id.to_owned();
        self.with_conn(move |c| c.execute("DELETE FROM proposals WHERE id = ?1", params![id]))
            .await?;
        Ok(())
    }

    async fn crawled_pages(&self) -> anyhow::Result<Vec<crate::acquisition::crawl::CrawledPage>> {
        let rows: Vec<String> = self
            .with_conn(|c| {
                let mut stmt = c.prepare("SELECT page FROM crawled_pages ORDER BY url")?;
                stmt.query_map([], |r| r.get(0))?.collect()
            })
            .await?;
        rows.iter()
            .map(|r| serde_json::from_str(r).context("a stored crawled page"))
            .collect()
    }

    async fn put_crawled_pages(
        &self,
        pages: &[crate::acquisition::crawl::CrawledPage],
    ) -> anyhow::Result<()> {
        let rows: Vec<(String, String, String)> = pages
            .iter()
            .map(|p| Ok((p.url.clone(), p.site.clone(), serde_json::to_string(p)?)))
            .collect::<anyhow::Result<_>>()?;
        self.with_conn_mut(move |c| {
            let tx = c.transaction()?;
            for (url, site, page) in &rows {
                tx.execute(
                    "INSERT INTO crawled_pages (url, site, page) VALUES (?1, ?2, ?3)
                     ON CONFLICT(url) DO UPDATE SET site = excluded.site, page = excluded.page",
                    params![url, site, page],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        Ok(())
    }

    async fn delete_crawled_pages(&self, urls: &[String]) -> anyhow::Result<()> {
        let urls = urls.to_vec();
        self.with_conn_mut(move |c| {
            let tx = c.transaction()?;
            for url in &urls {
                tx.execute("DELETE FROM crawled_pages WHERE url = ?1", params![url])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await?;
        Ok(())
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
        self.with_conn_mut(move |c| {
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

    async fn add_record(&self, name: &str, audience: &str) -> anyhow::Result<()> {
        let (name, audience) = (name.to_owned(), audience.to_owned());
        self.with_conn(move |c| {
            c.execute(
                "INSERT OR REPLACE INTO records (name, audience) VALUES (?1, ?2)",
                params![name, audience],
            )
            .map(|_| ())
        })
        .await
    }

    async fn records(&self) -> anyhow::Result<Vec<(String, String)>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare("SELECT name, audience FROM records")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
        .await
    }

    async fn records_of(&self, audience: &str) -> anyhow::Result<Vec<String>> {
        let audience = audience.to_owned();
        self.with_conn(move |c| {
            let mut stmt =
                c.prepare("SELECT name FROM records WHERE audience = ?1 ORDER BY name")?;
            stmt.query_map(params![audience], |r| r.get(0))?.collect()
        })
        .await
    }

    async fn delete_audience(&self, audience: &str) -> anyhow::Result<()> {
        let audience = audience.to_owned();
        self.with_conn_mut(move |c| {
            let prefix = match audience.as_bytes().first() {
                Some(b'u') => format!("users/{audience}/"),
                Some(b'g') => format!("groups/{audience}/"),
                _ => anyhow::bail!("only a user's or a group's data can be deleted"),
            };
            let tx = c.transaction()?;
            tx.execute(
                "DELETE FROM proposals WHERE audience = ?1",
                params![audience],
            )?;
            tx.execute("DELETE FROM records WHERE audience = ?1", params![audience])?;
            tx.execute(
                "DELETE FROM readings WHERE audience = ?1",
                params![audience],
            )?;
            tx.execute(
                "DELETE FROM document_versions WHERE document IN
                 (SELECT id FROM documents WHERE audience = ?1)",
                params![audience],
            )?;
            tx.execute(
                "DELETE FROM documents WHERE audience = ?1",
                params![audience],
            )?;
            for table in ["embeddings", "lint_reviewed"] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE substr(page, 1, ?2) = ?1"),
                    params![prefix, i64::try_from(prefix.len())?],
                )?;
            }
            tx.execute(
                "DELETE FROM lint_findings WHERE audience = ?1",
                params![audience],
            )?;
            // A user's own actions, whose details can hold names they chose.
            tx.execute("DELETE FROM audit_log WHERE actor = ?1", params![audience])?;
            // The jobs it queued, and any whose results name it (its pages, or gaps an
            // acquisition run found for it; an approval of its proposal runs without a user).
            tx.execute(
                "DELETE FROM jobs WHERE owner = ?1 OR instr(info, ?1) > 0",
                params![audience],
            )?;
            tx.commit()?;
            // Old copies of the rows also leave the write-ahead log.
            checkpoint(c)?;
            Ok(())
        })
        .await
    }

    async fn put_job(&self, job: &StoredJob) -> anyhow::Result<()> {
        let (id, owner, finished) = (job.info.job.clone(), job.owner.clone(), job.finished);
        let info = serde_json::to_string(&job.info)?;
        self.with_conn(move |c| {
            c.execute(
                "INSERT INTO jobs (id, owner, info, finished) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (id) DO UPDATE SET info = excluded.info, finished = excluded.finished",
                params![id, owner, info, finished],
            )
        })
        .await?;
        Ok(())
    }

    async fn jobs(&self) -> anyhow::Result<Vec<StoredJob>> {
        let rows: Vec<(Option<String>, String, Option<i64>)> = self
            .with_conn(|c| {
                let mut stmt = c.prepare("SELECT owner, info, finished FROM jobs")?;
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect()
            })
            .await?;
        rows.into_iter()
            .map(|(owner, info, finished)| {
                Ok(StoredJob {
                    owner,
                    info: serde_json::from_str(&info).context("a stored job")?,
                    finished,
                })
            })
            .collect()
    }

    async fn prune_jobs(&self, before: i64) -> anyhow::Result<()> {
        self.with_conn(move |c| {
            c.execute(
                "DELETE FROM jobs WHERE finished IS NOT NULL AND finished < ?1",
                params![before],
            )
        })
        .await?;
        Ok(())
    }

    async fn put_graph_space(&self, def: &SpaceDef) -> anyhow::Result<()> {
        let (name, json) = (def.space.clone(), serde_json::to_string(def)?);
        self.with_conn(move |c| {
            c.execute(
                "UPDATE graph_spaces SET definition = ?2 WHERE name = ?1",
                params![name, json],
            )
            .map(|_| ())
        })
        .await
    }

    async fn remove_graph_space(&self, name: &str) -> anyhow::Result<()> {
        let name = name.to_owned();
        self.with_conn(move |c| {
            c.execute("DELETE FROM graph_spaces WHERE name = ?1", params![name])
                .map(|_| ())
        })
        .await
    }

    async fn lint_run(&self) -> anyhow::Result<Option<(LintRun, Vec<Stored>)>> {
        let (run, rows): (Option<String>, Vec<String>) = self
            .with_conn(|c| {
                let run = c
                    .query_row("SELECT value FROM meta WHERE key = 'lint_run'", [], |r| {
                        r.get(0)
                    })
                    .optional()?;
                let mut stmt = c.prepare("SELECT finding FROM lint_findings ORDER BY id")?;
                let rows = stmt
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?;
                Ok((run, rows))
            })
            .await?;
        let Some(run) = run else {
            return Ok(None);
        };
        let findings = rows
            .iter()
            .map(|r| serde_json::from_str(r).context("a stored lint finding"))
            .collect::<anyhow::Result<_>>()?;
        Ok(Some((
            serde_json::from_str(&run).context("the stored lint run")?,
            findings,
        )))
    }

    async fn lint_reviewed(&self) -> anyhow::Result<Vec<(String, String)>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare("SELECT page, hash FROM lint_reviewed")?;
            stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect()
        })
        .await
    }

    async fn put_lint_run(
        &self,
        run: &LintRun,
        findings: &[Stored],
        reviewed: &[(String, String)],
    ) -> anyhow::Result<()> {
        let run = serde_json::to_string(run)?;
        let findings: Vec<(String, String)> = findings
            .iter()
            .map(|f| Ok((f.audience.clone(), serde_json::to_string(f)?)))
            .collect::<anyhow::Result<_>>()?;
        let reviewed = reviewed.to_vec();
        self.with_conn_mut(move |c| {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM lint_findings", [])?;
            for (audience, f) in &findings {
                tx.execute(
                    "INSERT INTO lint_findings (audience, finding) VALUES (?1, ?2)",
                    params![audience, f],
                )?;
            }
            for (page, hash) in &reviewed {
                tx.execute(
                    "INSERT INTO lint_reviewed (page, hash) VALUES (?1, ?2)
                     ON CONFLICT (page) DO UPDATE SET hash = excluded.hash",
                    params![page, hash],
                )?;
            }
            tx.execute(
                "INSERT INTO meta (key, value) VALUES ('lint_run', ?1)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value",
                params![run],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }

    fn close(&self) {
        let mut guard = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(conn) = guard.take()
            && let Err((_, e)) = conn.close()
        {
            tracing::warn!(error = %e, "closing the store failed");
        }
    }

    async fn embeddings(&self, model: &str) -> anyhow::Result<Vec<StoredVector>> {
        let model = model.to_owned();
        let cleared = self.other_models_cleared.clone();
        self.with_conn(move |c| {
            // Every row is read once anyway (the model isn't a key prefix), so this read also
            // tells whether another model's rows are there; without any, `sync_embeddings`
            // needn't look for them. Another model's vectors are never read.
            let mut stmt = c.prepare("SELECT model, page, heading, hash, vector FROM embeddings")?;
            let mut rows = stmt.query([])?;
            let (mut out, mut others) = (Vec::new(), false);
            while let Some(r) = rows.next()? {
                let ours = matches!(r.get_ref(0)?, rusqlite::types::ValueRef::Text(m) if m == model.as_bytes());
                if !ours {
                    others = true;
                    continue;
                }
                let bytes: Vec<u8> = r.get(4)?;
                out.push(StoredVector {
                    page: r.get(1)?,
                    heading: r.get(2)?,
                    hash: r.get(3)?,
                    vector: bytes
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b))
                        .collect(),
                });
            }
            if !others {
                cleared.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(out)
        })
        .await
    }

    async fn sync_embeddings(
        &self,
        model: &str,
        put: Vec<StoredVector>,
        drop: Vec<(String, String)>,
    ) -> anyhow::Result<()> {
        let model = model.to_owned();
        let cleared = self.other_models_cleared.clone();
        self.with_conn_mut(move |conn| {
            use std::sync::atomic::Ordering;
            let tx = conn.transaction()?;
            let clear = !cleared.load(Ordering::SeqCst);
            if clear {
                tx.execute("DELETE FROM embeddings WHERE model != ?1", params![model])?;
            }
            for (page, heading) in &drop {
                tx.execute(
                    "DELETE FROM embeddings WHERE page = ?1 AND heading = ?2 AND model = ?3",
                    params![page, heading, model],
                )?;
            }
            for v in &put {
                let bytes: Vec<u8> = v.vector.iter().flat_map(|x| x.to_le_bytes()).collect();
                tx.execute(
                    "INSERT INTO embeddings (page, heading, model, hash, vector)
                     VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT (page, heading, model)
                     DO UPDATE SET hash = excluded.hash, vector = excluded.vector",
                    params![v.page, v.heading, model, v.hash, bytes],
                )?;
            }
            tx.commit()?;
            if clear {
                cleared.store(true, Ordering::SeqCst);
            }
            Ok(())
        })
        .await
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
        assert_eq!(store.api_key_hashes().await.unwrap(), vec!["abc123"]);
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

    #[tokio::test]
    async fn another_models_vectors_go_once_the_model_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pinakes.sqlite");
        SqliteStore::create_for_tenant(&path, "acme", "abc", "initial").unwrap();
        let v = |page: &str| StoredVector {
            page: page.into(),
            heading: String::new(),
            hash: "h".into(),
            vector: vec![0.5, -0.25],
        };
        let store = SqliteStore::open_existing(&path).unwrap();
        store
            .sync_embeddings("old", vec![v("expertise/a"), v("expertise/b")], Vec::new())
            .await
            .unwrap();
        // A later start with another model: none of the old rows are read as its own, and its
        // first write removes them.
        drop(store);
        let store = SqliteStore::open_existing(&path).unwrap();
        assert!(store.embeddings("new").await.unwrap().is_empty());
        store
            .sync_embeddings("new", vec![v("expertise/a")], Vec::new())
            .await
            .unwrap();
        assert!(store.embeddings("old").await.unwrap().is_empty());
        let now = store.embeddings("new").await.unwrap();
        assert_eq!(now.len(), 1);
        assert_eq!(now[0].vector, vec![0.5, -0.25]);
    }
}
