//! Past versions of a tenant's wiki, kept so a replay can rebuild the exact wiki a traced search
//! ran on (design, "Search traces and replay").
//!
//! Content-addressed, like a tiny git without the history graph: every page text is stored once
//! under the hash of its text in `history/objects/`, and every wiki version has a manifest in
//! `history/versions/<version>.json` mapping page paths to objects. A version is the search
//! snapshot's version, so a trace line names it directly. Versions no trace refers to any more
//! are removed with the traces (see `trace`).
//!
//! A write changes a few pages of a wiki that may hold 100,000, so a version saved right after
//! another one records only what changed since it (its `base`): the pages set and the pages
//! removed. Every [`MAX_DEPTH`]th version of such a chain lists every page again, so loading
//! one reads a bounded number of manifests.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Page, PagePath, WikiError};

/// Most manifests one version's chain of bases has, itself included.
const MAX_DEPTH: u32 = 32;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    /// The version this one is a change of; none when it lists every page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base: Option<String>,
    /// Manifests in the chain, this one included: 1 when it lists every page.
    #[serde(default = "one")]
    depth: u32,
    /// Page path → object name: every page, or those set since the base.
    pages: BTreeMap<String, String>,
    /// Pages of the base that this version no longer has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed: Vec<String>,
}

fn one() -> u32 {
    1
}

/// The `history/` folder of one tenant.
#[derive(Debug, Clone)]
pub struct History {
    root: PathBuf,
}

/// Version names are 16 lowercase hex digits (see `Catalog::version`); anything else is
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

/// The name a page's text is stored under: 32 hex digits of a SHA-256 over the text.
pub fn object_name(text: &str) -> String {
    hex::encode(&Sha256::digest(text.as_bytes())[..16])
}

/// Writes a file through a temp file and a rename, so a reader never sees half of it.
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", hex::encode(rand::random::<[u8; 8]>())));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
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

    fn manifest(&self, version: &str) -> Result<Option<Manifest>, WikiError> {
        let json = match std::fs::read(self.manifest_path(version)) {
            Ok(j) => j,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let bad = |reason: String| WikiError::Malformed {
            path: format!("history/versions/{version}.json"),
            reason,
        };
        let m: Manifest = serde_json::from_slice(&json).map_err(|e| bad(e.to_string()))?;
        if let Some(name) = m.pages.values().find(|n| !valid_object(n)) {
            return Err(bad(format!("bad object name {name:?}")));
        }
        if let Some(base) = m.base.as_ref().filter(|b| !valid_version(b)) {
            return Err(bad(format!("bad base version {base:?}")));
        }
        Ok(Some(m))
    }

    /// Every page of a stored version, path → object, following its bases; `None` if it or one
    /// of its bases isn't stored.
    fn pages_of(&self, version: &str) -> Result<Option<BTreeMap<String, String>>, WikiError> {
        let mut chain = Vec::new();
        let mut next = Some(version.to_owned());
        while let Some(v) = next {
            if chain.len() >= MAX_DEPTH as usize {
                return Err(WikiError::Malformed {
                    path: format!("history/versions/{version}.json"),
                    reason: "chain of bases too long".into(),
                });
            }
            let Some(m) = self.manifest(&v)? else {
                return Ok(None);
            };
            next = m.base.clone();
            chain.push(m);
        }
        let mut pages = BTreeMap::new();
        for m in chain.into_iter().rev() {
            for removed in &m.removed {
                pages.remove(removed);
            }
            pages.extend(m.pages);
        }
        Ok(Some(pages))
    }

    /// Stores a version of the wiki unless it is stored already, and marks it as in use now.
    /// `pages` maps every page path to its object name; `text` gives a page's text for objects
    /// not stored yet. When it can't (the page changed again meanwhile), nothing is stored: a
    /// version missing a page would replay a wiki that never was. With a stored `base` whose
    /// chain isn't too long, only the change since it is written.
    pub fn save(
        &self,
        version: &str,
        base: Option<&str>,
        pages: &BTreeMap<String, String>,
        text: impl Fn(&str) -> Option<String>,
    ) -> io::Result<()> {
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
        let base = base
            .filter(|b| valid_version(b) && *b != version)
            .and_then(|b| match (self.manifest(b), self.pages_of(b)) {
                (Ok(Some(m)), Ok(Some(old))) if m.depth < MAX_DEPTH => Some((b, m.depth, old)),
                _ => None,
            });
        let mut manifest = Manifest {
            depth: 1,
            ..Default::default()
        };
        let mut new: Vec<(&String, &String)> = pages.iter().collect();
        if let Some((b, depth, old)) = &base {
            manifest.base = Some((*b).to_owned());
            manifest.depth = depth + 1;
            manifest.removed = old
                .keys()
                .filter(|p| !pages.contains_key(*p))
                .cloned()
                .collect();
            new.retain(|(path, name)| old.get(*path) != Some(*name));
        }
        for (path, name) in new {
            let object = objects.join(name);
            if !object.exists() {
                match text(path) {
                    Some(t) if object_name(&t) == *name => {
                        // Named by its content: if another save wrote it meanwhile (Windows
                        // refuses to rename over a file in use), what is there is right.
                        if let Err(e) = write_atomic(&object, t.as_bytes())
                            && !object.exists()
                        {
                            return Err(e);
                        }
                    }
                    _ => {
                        return Err(io::Error::other(format!(
                            "{path} changed before this version was saved"
                        )));
                    }
                }
            }
            manifest.pages.insert(path.clone(), name.clone());
        }
        let json = serde_json::to_vec(&manifest).map_err(io::Error::other)?;
        write_atomic(&manifest_path, &json)
    }

    /// Stores a version made of these pages, in full.
    pub fn save_pages(&self, version: &str, pages: &[Page]) -> io::Result<()> {
        let texts: BTreeMap<String, String> = pages
            .iter()
            .map(|p| (p.path.to_string(), p.render()))
            .collect();
        let names = texts
            .iter()
            .map(|(p, t)| (p.clone(), object_name(t)))
            .collect();
        self.save(version, None, &names, |p| texts.get(p).cloned())
    }

    /// The pages of a stored version, or `None` if it isn't stored (or was collected).
    pub fn load(&self, version: &str) -> Result<Option<Vec<Page>>, WikiError> {
        if !valid_version(version) {
            return Ok(None);
        }
        let Some(manifest) = self.pages_of(version)? else {
            return Ok(None);
        };
        let mut pages = Vec::new();
        for (path, name) in manifest {
            let text = std::fs::read_to_string(self.root.join("objects").join(&name))?;
            pages.push(Page::parse(PagePath::parse(&path)?, &text)?);
        }
        Ok(Some(pages))
    }

    /// Removes versions that are not in `keep` and weren't used since `cutoff`, unless a kept
    /// version is a change of them, then the objects no remaining version refers to (only
    /// those older than `cutoff`, so a version being saved right now keeps its fresh objects).
    pub fn collect_garbage(&self, keep: &HashSet<String>, cutoff: SystemTime) -> io::Result<()> {
        let old = |path: &Path| {
            std::fs::metadata(path)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t < cutoff)
        };
        let versions = match std::fs::read_dir(self.root.join("versions")) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut manifests: BTreeMap<String, (PathBuf, Option<Manifest>)> = BTreeMap::new();
        for entry in versions.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let Some(version) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let m = std::fs::read(&path)
                .ok()
                .and_then(|json| serde_json::from_slice::<Manifest>(&json).ok());
            manifests.insert(version.to_owned(), (path, m));
        }
        // Kept: the versions asked for, those used recently, and every base they build on.
        let mut kept: HashSet<String> = HashSet::new();
        let mut todo: Vec<String> = manifests
            .iter()
            .filter(|(v, (path, _))| keep.contains(*v) || !old(path))
            .map(|(v, _)| v.clone())
            .collect();
        while let Some(v) = todo.pop() {
            if let Some((_, Some(m))) = manifests.get(&v)
                && let Some(base) = &m.base
                && !kept.contains(base)
            {
                todo.push(base.clone());
            }
            kept.insert(v);
        }
        let mut referenced: HashSet<String> = HashSet::new();
        for (version, (path, m)) in manifests {
            if !kept.contains(&version) {
                std::fs::remove_file(&path)?;
            } else if let Some(m) = m {
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
        h.save_pages("0000000000000001", &v1).unwrap();
        h.save_pages("0000000000000002", &v2).unwrap();
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
        h.save_pages("0000000000000001", &pages).unwrap();
        std::fs::remove_file(dir.path().join("history/versions/0000000000000001.json")).unwrap();
        h.save_pages("0000000000000001", &pages).unwrap();
        assert_eq!(h.load("0000000000000001").unwrap().unwrap(), pages);
    }

    #[test]
    fn garbage_collection_keeps_referenced_and_recent_versions() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        h.save_pages("0000000000000001", &[page("expertise/a", "A")])
            .unwrap();
        h.save_pages("0000000000000002", &[page("expertise/b", "B")])
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

    fn names(pages: &[Page]) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
        let texts: BTreeMap<String, String> = pages
            .iter()
            .map(|p| (p.path.to_string(), p.render()))
            .collect();
        let names = texts
            .iter()
            .map(|(p, t)| (p.clone(), object_name(t)))
            .collect();
        (names, texts)
    }

    fn save_on(h: &History, version: &str, base: Option<&str>, pages: &[Page]) {
        let (names, texts) = names(pages);
        h.save(version, base, &names, |p| texts.get(p).cloned())
            .unwrap();
    }

    /// A version saved on a stored base records only the change, loads in full, and keeps its
    /// base from the collector; every `MAX_DEPTH`th one lists every page again.
    #[test]
    fn versions_are_saved_as_changes_of_their_base() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let v1 = vec![page("expertise/a", "A"), page("expertise/b", "B")];
        let v2 = vec![page("expertise/a", "A2"), page("expertise/c", "C")];
        save_on(&h, "0000000000000001", None, &v1);
        save_on(&h, "0000000000000002", Some("0000000000000001"), &v2);
        let m = h.manifest("0000000000000002").unwrap().unwrap();
        assert_eq!(m.base.as_deref(), Some("0000000000000001"));
        assert_eq!(m.pages.len(), 2, "a and c changed");
        assert_eq!(m.removed, vec!["expertise/b"]);
        assert_eq!(h.load("0000000000000002").unwrap().unwrap(), v2);
        assert_eq!(h.load("0000000000000001").unwrap().unwrap(), v1);

        // The collector keeps the base of a kept version, and the objects of both.
        let future = SystemTime::now() + Duration::from_secs(3600);
        let keep = HashSet::from(["0000000000000002".to_string()]);
        h.collect_garbage(&keep, future).unwrap();
        assert_eq!(h.load("0000000000000002").unwrap().unwrap(), v2);

        // A long chain starts over with a full list.
        let mut base = "0000000000000002".to_string();
        for i in 3..=(MAX_DEPTH as u64 + 3) {
            let version = format!("{i:016x}");
            let pages = vec![
                page("expertise/a", &format!("A{i}")),
                page("expertise/c", "C"),
            ];
            save_on(&h, &version, Some(&base), &pages);
            assert_eq!(h.load(&version).unwrap().unwrap(), pages);
            base = version;
        }
        let depths: Vec<u32> = (3..=(MAX_DEPTH as u64 + 3))
            .map(|i| h.manifest(&format!("{i:016x}")).unwrap().unwrap().depth)
            .collect();
        assert!(depths.iter().all(|d| *d <= MAX_DEPTH));
        assert!(depths.contains(&1), "{depths:?}");

        // Without its base the change can't be told: the version is saved in full.
        save_on(&h, "00000000000000ff", Some("00000000000000ee"), &v1);
        assert_eq!(h.manifest("00000000000000ff").unwrap().unwrap().base, None);
        assert_eq!(h.load("00000000000000ff").unwrap().unwrap(), v1);
    }
}
