//! Past versions of a tenant's wiki, kept so a replay can rebuild the exact wiki a traced search
//! ran on (design, "Search traces and replay").
//!
//! Content-addressed, like a tiny git without the history graph: every page text is stored once
//! under the hash of its text in `history/objects/`, and every wiki version has a manifest in
//! `history/versions/<version>.json` mapping page paths to objects. A version is the search
//! snapshot's version, so a trace line names it directly. Versions no trace refers to any more
//! are removed with the traces (see `trace`).

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Page, PagePath, WikiError};

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    /// Page path → object name.
    pages: BTreeMap<String, String>,
}

/// The `history/` folder of one tenant.
#[derive(Debug, Clone)]
pub struct History {
    root: PathBuf,
}

/// Version names are 16 lowercase hex digits (see `search::version_of`); anything else is
/// refused, since names arrive from trace files.
fn valid_version(v: &str) -> bool {
    v.len() == 16
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Object names are 32 lowercase hex digits; a manifest naming anything else is malformed.
fn valid_object(name: &str) -> bool {
    name.len() == 32
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn object_name(text: &str) -> String {
    hex::encode(&Sha256::digest(text.as_bytes())[..16])
}

/// Writes a file through a temp file and a rename, so a reader never sees half of it.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", hex::encode(rand::random::<[u8; 8]>())));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

impl History {
    /// The history of the tenant whose folder is `tenant_dir`.
    pub fn new(tenant_dir: &Path) -> Self {
        Self {
            root: tenant_dir.join("history"),
        }
    }

    fn manifest_path(&self, version: &str) -> PathBuf {
        self.root.join("versions").join(format!("{version}.json"))
    }

    /// Stores a version of the wiki unless it is stored already, and marks it as in use now.
    pub fn save(&self, version: &str, pages: &[Page]) -> io::Result<()> {
        if !valid_version(version) {
            return Err(io::Error::other(format!("bad version name {version:?}")));
        }
        let manifest_path = self.manifest_path(version);
        // Already stored: mark it used so the garbage collector keeps it. If the collector
        // removed it just now, store it again below.
        match File::options().write(true).open(&manifest_path) {
            Ok(file) => return file.set_modified(SystemTime::now()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let objects = self.root.join("objects");
        std::fs::create_dir_all(&objects)?;
        std::fs::create_dir_all(manifest_path.parent().expect("has a parent"))?;
        let mut manifest = Manifest {
            pages: BTreeMap::new(),
        };
        for page in pages {
            let text = page.render();
            let name = object_name(&text);
            let object = objects.join(&name);
            if !object.exists() {
                write_atomic(&object, text.as_bytes())?;
            }
            manifest.pages.insert(page.path.to_string(), name);
        }
        let json = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
        write_atomic(&manifest_path, &json)
    }

    /// The pages of a stored version, or `None` if it isn't stored (or was collected).
    pub fn load(&self, version: &str) -> Result<Option<Vec<Page>>, WikiError> {
        if !valid_version(version) {
            return Ok(None);
        }
        let json = match std::fs::read(self.manifest_path(version)) {
            Ok(j) => j,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let manifest: Manifest =
            serde_json::from_slice(&json).map_err(|e| WikiError::Malformed {
                path: format!("history/versions/{version}.json"),
                reason: e.to_string(),
            })?;
        let mut pages = Vec::new();
        for (path, name) in manifest.pages {
            if !valid_object(&name) {
                return Err(WikiError::Malformed {
                    path: format!("history/versions/{version}.json"),
                    reason: format!("bad object name {name:?}"),
                });
            }
            let text = std::fs::read_to_string(self.root.join("objects").join(&name))?;
            pages.push(Page::parse(PagePath::parse(&path)?, &text)?);
        }
        Ok(Some(pages))
    }

    /// Removes versions that are not in `keep` and weren't used since `cutoff`, then the
    /// objects no remaining version refers to (only those older than `cutoff`, so a version
    /// being saved right now keeps its fresh objects).
    pub fn collect_garbage(&self, keep: &HashSet<String>, cutoff: SystemTime) -> io::Result<()> {
        let old = |path: &Path| {
            std::fs::metadata(path)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t < cutoff)
        };
        let mut referenced: HashSet<String> = HashSet::new();
        let versions = match std::fs::read_dir(self.root.join("versions")) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in versions.flatten() {
            let path = entry.path();
            let Some(version) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if !keep.contains(version) && old(&path) {
                std::fs::remove_file(&path)?;
                continue;
            }
            if let Ok(json) = std::fs::read(&path)
                && let Ok(m) = serde_json::from_slice::<Manifest>(&json)
            {
                referenced.extend(m.pages.into_values());
            }
        }
        if let Ok(objects) = std::fs::read_dir(self.root.join("objects")) {
            for entry in objects.flatten() {
                let path = entry.path();
                let name = entry.file_name();
                if !referenced.contains(name.to_str().unwrap_or("")) && old(&path) {
                    std::fs::remove_file(&path)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn page(path: &str, title: &str) -> Page {
        let mut p = Page::new(PagePath::parse(path).unwrap());
        p.front.title = Some(title.into());
        p.body = format!("# {title}\n");
        p
    }

    #[test]
    fn saves_loads_and_shares_objects() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let v1 = vec![page("expertise/a", "A"), page("expertise/b", "B")];
        let v2 = vec![page("expertise/a", "A"), page("expertise/b", "B2")];
        h.save("0000000000000001", &v1).unwrap();
        h.save("0000000000000002", &v2).unwrap();
        assert_eq!(h.load("0000000000000001").unwrap().unwrap(), v1);
        assert_eq!(h.load("0000000000000002").unwrap().unwrap(), v2);
        // Page A is stored once for both versions.
        let objects = std::fs::read_dir(dir.path().join("history/objects"))
            .unwrap()
            .count();
        assert_eq!(objects, 3);
        assert!(h.load("0000000000000003").unwrap().is_none());
        assert!(h.load("../../etc/passwd").unwrap().is_none());

        // A manifest that names a path outside the objects folder is refused.
        std::fs::write(
            dir.path().join("history/versions/0000000000000009.json"),
            r#"{"pages":{"expertise/a":"../../../secret"}}"#,
        )
        .unwrap();
        assert!(matches!(
            h.load("0000000000000009"),
            Err(WikiError::Malformed { .. })
        ));
    }

    /// A version the collector removed between two saves is stored again, not lost.
    #[test]
    fn saving_a_removed_version_stores_it_again() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let pages = vec![page("expertise/a", "A")];
        h.save("0000000000000001", &pages).unwrap();
        std::fs::remove_file(dir.path().join("history/versions/0000000000000001.json")).unwrap();
        h.save("0000000000000001", &pages).unwrap();
        assert_eq!(h.load("0000000000000001").unwrap().unwrap(), pages);
    }

    #[test]
    fn garbage_collection_keeps_referenced_and_recent_versions() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        h.save("0000000000000001", &[page("expertise/a", "A")])
            .unwrap();
        h.save("0000000000000002", &[page("expertise/b", "B")])
            .unwrap();

        // Everything is recent: nothing goes.
        let past = SystemTime::now() - Duration::from_secs(3600);
        h.collect_garbage(&HashSet::new(), past).unwrap();
        assert!(h.load("0000000000000001").unwrap().is_some());

        // Old and unreferenced goes, with its objects; referenced stays.
        let future = SystemTime::now() + Duration::from_secs(3600);
        let keep = HashSet::from(["0000000000000002".to_string()]);
        h.collect_garbage(&keep, future).unwrap();
        assert!(h.load("0000000000000001").unwrap().is_none());
        assert!(h.load("0000000000000002").unwrap().is_some());
        let objects = std::fs::read_dir(dir.path().join("history/objects"))
            .unwrap()
            .count();
        assert_eq!(objects, 1);
    }
}
