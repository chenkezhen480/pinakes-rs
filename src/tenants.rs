//! Tenants: id-to-folder mapping, creation, API key checks and the cache of open tenants.
//!
//! Each tenant owns one folder under `<data>/tenants/`, named from a hash of its id, holding its
//! wiki files, raw sources and its SQLite file. Nothing here ever builds a path from a raw id.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::store::{SqliteStore, Store};
use crate::wiki::WikiFs;

const MAX_ID_LEN: usize = 256;
const HASH_HEX_LEN: usize = 16;
const KEY_PREFIX: &str = "kb_";

/// Wiki folders created with every tenant.
const WIKI_DIRS: &[&str] = &["experience", "expertise", "graph", "users"];

/// Folder name for a tenant id: `t_` plus 16 hex digits of a SHA-256 of the id.
pub fn tenant_dir_name(tenant_id: &str) -> String {
    format!("t_{}", short_hash(b"tenant", tenant_id))
}

/// Folder name for a user id inside a tenant: `u_` plus 16 hex digits.
pub fn user_dir_name(user_id: &str) -> String {
    format!("u_{}", short_hash(b"user", user_id))
}

fn short_hash(domain: &[u8], id: &str) -> String {
    let mut h = Sha256::new();
    h.update(domain);
    h.update([0u8]);
    h.update(id.as_bytes());
    let digest = h.finalize();
    hex::encode(&digest[..HASH_HEX_LEN / 2])
}

/// Hex SHA-256 of an API key, the form stored in a tenant's SQLite file.
pub fn hash_api_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Ids are opaque strings; the service only checks they are usable.
pub fn validate_id(what: &str, id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(format!("{what} id is empty"));
    }
    if id.len() > MAX_ID_LEN {
        return Err(format!("{what} id is longer than {MAX_ID_LEN} bytes"));
    }
    if id.chars().any(char::is_control) {
        return Err(format!("{what} id contains control characters"));
    }
    Ok(())
}

/// API keys look like `kb_<16 hex: tenant folder hash>_<64 hex: secret>`. The prefix routes the
/// key to its tenant's folder without a global table; only the hash of the whole key is stored.
fn new_api_key(tenant_dir: &str) -> String {
    let hash = tenant_dir.strip_prefix("t_").expect("tenant dir name");
    let secret: [u8; 32] = rand::random();
    format!("{KEY_PREFIX}{hash}_{}", hex::encode(secret))
}

/// The tenant folder a key claims to belong to, if the key is well-formed.
fn tenant_dir_from_key(key: &str) -> Option<String> {
    let rest = key.strip_prefix(KEY_PREFIX)?;
    let (hash, secret) = rest.split_once('_')?;
    let is_hex = |s: &str, len: usize| {
        s.len() == len
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    (is_hex(hash, HASH_HEX_LEN) && is_hex(secret, 64)).then(|| format!("t_{hash}"))
}

pub struct Tenant {
    pub id: String,
    pub dir_name: String,
    pub dir: PathBuf,
    pub store: Arc<dyn Store>,
    key_hashes: RwLock<HashSet<String>>,
}

impl std::fmt::Debug for Tenant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tenant")
            .field("id", &self.id)
            .field("dir_name", &self.dir_name)
            .finish_non_exhaustive()
    }
}

impl Tenant {
    pub fn wiki(&self) -> WikiFs {
        WikiFs::new(self.dir.join("wiki"))
    }

    async fn has_key_hash(&self, hash: &str) -> bool {
        self.key_hashes.read().await.contains(hash)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CreateTenantError {
    #[error("{0}")]
    InvalidId(String),
    #[error("tenant already exists")]
    AlreadyExists,
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

pub struct CreatedTenant {
    pub tenant: Arc<Tenant>,
    pub api_key: String,
}

/// All tenants under one data folder, and the ones currently open.
pub struct Tenants {
    root: PathBuf,
    store_file: String,
    open: RwLock<HashMap<String, Arc<Tenant>>>,
}

impl Tenants {
    pub fn new(root: impl Into<PathBuf>, store_file: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            store_file: store_file.into(),
            open: RwLock::new(HashMap::new()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates a tenant folder, its SQLite file and a first API key. The folder is built under a
    /// temporary name and renamed into place, so a failure never leaves a half-made tenant.
    #[tracing::instrument(skip_all)]
    pub async fn create(
        &self,
        tenant_id: &str,
        key_label: &str,
    ) -> Result<CreatedTenant, CreateTenantError> {
        validate_id("tenant", tenant_id).map_err(CreateTenantError::InvalidId)?;
        let dir_name = tenant_dir_name(tenant_id);
        let final_dir = self.root.join(&dir_name);

        // Holding the write lock serializes creations and keeps the cache consistent.
        let mut open = self.open.write().await;
        if final_dir.exists() {
            return Err(CreateTenantError::AlreadyExists);
        }

        let api_key = new_api_key(&dir_name);
        let key_hash = hash_api_key(&api_key);
        let tmp_dir = self.root.join(format!(
            ".tmp-{dir_name}-{}",
            hex::encode(rand::random::<[u8; 4]>())
        ));
        let store_file = self.store_file.clone();
        let (id, hash, label) = (tenant_id.to_owned(), key_hash.clone(), key_label.to_owned());
        let tmp = tmp_dir.clone();
        let fin = final_dir.clone();

        let built = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            build_tenant_dir(&tmp, &store_file, &id, &hash, &label)?;
            std::fs::rename(&tmp, &fin)
                .with_context(|| format!("moving tenant folder into {}", fin.display()))
        })
        .await
        .context("tenant creation task panicked")?;
        if let Err(e) = built {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(e.into());
        }

        let tenant = Arc::new(self.load(&dir_name).await?);
        open.insert(dir_name, tenant.clone());
        Ok(CreatedTenant { tenant, api_key })
    }

    /// Looks up the tenant an API key belongs to. Unknown folders, malformed keys and wrong
    /// secrets all give `None`, so a caller can't tell them apart.
    #[tracing::instrument(skip_all)]
    pub async fn authenticate(&self, api_key: &str) -> anyhow::Result<Option<Arc<Tenant>>> {
        let Some(dir_name) = tenant_dir_from_key(api_key) else {
            return Ok(None);
        };
        let Some(tenant) = self.open_by_dir(&dir_name).await? else {
            return Ok(None);
        };
        Ok(tenant
            .has_key_hash(&hash_api_key(api_key))
            .await
            .then_some(tenant))
    }

    /// Returns an open tenant, loading it from disk on first use.
    async fn open_by_dir(&self, dir_name: &str) -> anyhow::Result<Option<Arc<Tenant>>> {
        if let Some(t) = self.open.read().await.get(dir_name) {
            return Ok(Some(t.clone()));
        }
        let mut open = self.open.write().await;
        if let Some(t) = open.get(dir_name) {
            return Ok(Some(t.clone()));
        }
        if !self.root.join(dir_name).join(&self.store_file).is_file() {
            return Ok(None);
        }
        let tenant = Arc::new(self.load(dir_name).await?);
        open.insert(dir_name.to_owned(), tenant.clone());
        Ok(Some(tenant))
    }

    async fn load(&self, dir_name: &str) -> anyhow::Result<Tenant> {
        let dir = self.root.join(dir_name);
        let file = dir.join(&self.store_file);
        let store = tokio::task::spawn_blocking(move || SqliteStore::open_existing(&file))
            .await
            .context("opening store panicked")??;
        let id = store
            .tenant_id()
            .await?
            .with_context(|| format!("tenant folder {dir_name} has no tenant id"))?;
        anyhow::ensure!(
            tenant_dir_name(&id) == dir_name,
            "tenant folder {dir_name} records id that maps elsewhere"
        );
        let key_hashes = store.api_key_hashes().await?.into_iter().collect();
        Ok(Tenant {
            id,
            dir_name: dir_name.to_owned(),
            dir,
            store: Arc::new(store),
            key_hashes: RwLock::new(key_hashes),
        })
    }
}

fn build_tenant_dir(
    dir: &Path,
    store_file: &str,
    tenant_id: &str,
    key_hash: &str,
    key_label: &str,
) -> anyhow::Result<()> {
    let wiki = dir.join("wiki");
    for sub in WIKI_DIRS {
        std::fs::create_dir_all(wiki.join(sub))?;
    }
    std::fs::create_dir_all(dir.join("sources"))?;
    std::fs::write(wiki.join("index.md"), "# Index\n")?;
    std::fs::write(wiki.join("log.md"), "# Log\n")?;

    // The store is closed again before the folder is renamed (Windows can't rename open files).
    SqliteStore::create_for_tenant(&dir.join(store_file), tenant_id, key_hash, key_label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_names_are_stable_and_safe() {
        let a = tenant_dir_name("acme");
        assert_eq!(a, tenant_dir_name("acme"));
        assert_ne!(a, tenant_dir_name("acme2"));
        assert!(a.starts_with("t_") && a.len() == 18);
        let evil = tenant_dir_name("../other");
        assert!(evil.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'));
        // The same string as a tenant and as a user maps to different names.
        assert_ne!(&tenant_dir_name("x")[2..], &user_dir_name("x")[2..]);
    }

    #[test]
    fn key_format_roundtrips() {
        let dir = tenant_dir_name("acme");
        let key = new_api_key(&dir);
        assert_eq!(tenant_dir_from_key(&key).as_deref(), Some(dir.as_str()));
        assert_ne!(key, new_api_key(&dir));
    }

    #[test]
    fn malformed_keys_route_nowhere() {
        for k in [
            "",
            "kb_",
            "kb_0123456789abcdef",
            "kb_0123456789abcdef_short",
            "kb_../../etc_0000000000000000000000000000000000000000000000000000000000000000",
            "xx_0123456789abcdef_0000000000000000000000000000000000000000000000000000000000000000",
        ] {
            assert_eq!(tenant_dir_from_key(k), None, "{k}");
        }
    }

    #[test]
    fn id_validation() {
        assert!(validate_id("tenant", "acme").is_ok());
        assert!(validate_id("tenant", "客户 A").is_ok());
        assert!(validate_id("tenant", "").is_err());
        assert!(validate_id("tenant", "a\nb").is_err());
        assert!(validate_id("tenant", &"x".repeat(257)).is_err());
    }

    #[tokio::test]
    async fn create_then_authenticate() {
        let dir = tempfile::tempdir().unwrap();
        let tenants = Tenants::new(dir.path(), "kb.sqlite");
        let created = tenants.create("acme", "initial").await.unwrap();
        assert_eq!(created.tenant.id, "acme");
        assert!(matches!(
            tenants.create("acme", "again").await,
            Err(CreateTenantError::AlreadyExists)
        ));

        let found = tenants.authenticate(&created.api_key).await.unwrap();
        assert_eq!(found.unwrap().id, "acme");

        // A fresh registry (as after a restart) loads the tenant from disk.
        let reopened = Tenants::new(dir.path(), "kb.sqlite");
        assert!(
            reopened
                .authenticate(&created.api_key)
                .await
                .unwrap()
                .is_some()
        );

        let mut wrong = created.api_key.clone();
        wrong.pop();
        wrong.push(if created.api_key.ends_with('0') {
            '1'
        } else {
            '0'
        });
        assert!(tenants.authenticate(&wrong).await.unwrap().is_none());

        // No temp folders are left behind.
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec![tenant_dir_name("acme")]);
    }
}
