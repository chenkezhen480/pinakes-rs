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
    /// When it was saved (RFC 3339); versions saved before M10 don't say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_at: Option<String>,
}

fn one() -> u32 {
    1
}

/// One text a page had, as [`History::page_versions`] lists it.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct PageVersion {
    /// The stored wiki version that holds it.
    pub version: String,
    /// When the wiki came to that text (a page changed back lists its earlier text again,
    /// with the time it came back).
    pub saved_at: String,
    /// The text's object name (a hash of the text).
    #[serde(skip)]
    pub object: String,
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

/// One line of a page's list.
#[derive(Debug, Deserialize)]
struct LogLine {
    v: String,
    o: String,
    at: String,
}

/// Appends one line to a page's list.
fn append_line(path: &Path, version: &str, object: &str, at: &str) -> io::Result<()> {
    use std::io::Write as _;
    let line = serde_json::json!({ "v": version, "o": object, "at": at }).to_string();
    let mut f = File::options().create(true).append(true).open(path)?;
    f.write_all(format!("{line}\n").as_bytes())
}

/// Removes a file; one already gone is fine.
fn remove_if_there(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
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

    /// A page's own list of the texts it came to: one line per change, `{v, o, at}` (version,
    /// object, time), named by a hash of the path, so it holds no text and no path.
    fn log_path(&self, page: &str) -> PathBuf {
        self.root
            .join("pages")
            .join(format!("{}.log", object_name(page)))
    }

    /// The version saved last, so a save without a base still notes only what changed.
    fn last_path(&self) -> PathBuf {
        self.root.join("last")
    }

    fn last(&self) -> Option<String> {
        std::fs::read_to_string(self.last_path())
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|v| valid_version(v))
    }

    /// Appends `version` to the lists of the pages whose text differs from `prev` (all of them
    /// without one), and notes it as the last version. A history from before the lists
    /// existed gets them first, from its manifests.
    fn note(
        &self,
        version: &str,
        at: &str,
        pages: &BTreeMap<String, String>,
        prev: Option<&BTreeMap<String, String>>,
    ) -> io::Result<()> {
        let dir = self.root.join("pages");
        if !dir.exists() {
            self.backfill()?;
            std::fs::create_dir_all(&dir)?;
        }
        for (path, object) in pages {
            if prev.and_then(|p| p.get(path)) == Some(object) {
                continue;
            }
            append_line(&self.log_path(path), version, object, at)?;
        }
        write_atomic(&self.last_path(), version.as_bytes())
    }

    /// Writes the pages' lists from the stored manifests (each lists the pages it set), oldest
    /// first: once, for a history kept before the lists existed.
    fn backfill(&self) -> io::Result<()> {
        let found = self.manifests_oldest_first()?;
        if found.is_empty() {
            return Ok(());
        }
        std::fs::create_dir_all(self.root.join("pages"))?;
        for (at, version, m) in &found {
            for (path, object) in &m.pages {
                append_line(&self.log_path(path), version, object, at)?;
            }
        }
        Ok(())
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
        // What changed is told against the base, or else (none given, or no longer stored)
        // the version saved last.
        let stored = |v: Option<String>| {
            v.filter(|v| valid_version(v) && v != version)
                .and_then(|v| self.pages_of(&v).ok().flatten())
        };
        let prev = stored(base.map(str::to_owned)).or_else(|| stored(self.last()));
        // Already stored: mark it used so the garbage collector keeps it, and note the pages
        // that came back to it. If the collector removed it just now, store it again below.
        match File::options().write(true).open(&manifest_path) {
            Ok(file) => {
                file.set_modified(SystemTime::now())?;
                match &prev {
                    Some(prev) => {
                        self.note(version, &crate::trace::now_rfc3339(), pages, Some(prev))?
                    }
                    // Nothing to tell the change against: the wiki is at this version now, at
                    // least, for the next save to compare with.
                    None => write_atomic(&self.last_path(), version.as_bytes())?,
                }
                return Ok(());
            }
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
            saved_at: Some(crate::trace::now_rfc3339()),
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
        write_atomic(&manifest_path, &json)?;
        let at = manifest.saved_at.clone().unwrap_or_default();
        self.note(version, &at, pages, prev.as_ref())
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

    /// The stored versions of one page: each text it had once, with the first version that
    /// holds it and when that was saved (or last used, for versions that don't say), oldest
    /// first. Versions are kept as long as the search traces that name them.
    pub fn page_versions(&self, path: &str) -> Result<Vec<PageVersion>, WikiError> {
        if let Ok(text) = std::fs::read_to_string(self.log_path(path)) {
            let mut found: Vec<PageVersion> = text
                .lines()
                .filter_map(|l| serde_json::from_str::<LogLine>(l).ok())
                .filter(|l| valid_version(&l.v) && valid_object(&l.o))
                // A version the trace retention removed is gone from the list too.
                .filter(|l| self.manifest_path(&l.v).exists())
                .map(|l| PageVersion {
                    version: l.v,
                    saved_at: l.at,
                    object: l.o,
                })
                .collect();
            found.sort_by(|a, b| a.saved_at.cmp(&b.saved_at));
            found.dedup_by(|later, earlier| later.object == earlier.object);
            return Ok(found);
        }
        self.page_versions_from_manifests(path)
    }

    /// The same, read from every manifest: for a history kept before the pages' lists.
    fn page_versions_from_manifests(&self, path: &str) -> Result<Vec<PageVersion>, WikiError> {
        let mut found: Vec<PageVersion> = self
            .manifests_oldest_first()?
            .into_iter()
            .filter_map(|(at, version, m)| {
                m.pages.get(path).map(|object| PageVersion {
                    version,
                    saved_at: at,
                    object: object.clone(),
                })
            })
            .collect();
        // A version is listed when its text differs from the one before it.
        found.dedup_by(|later, earlier| later.object == earlier.object);
        Ok(found)
    }

    /// Every stored manifest with when it was saved (its own record, else its file's time),
    /// oldest first; unreadable ones are left out.
    fn manifests_oldest_first(&self) -> io::Result<Vec<(String, String, Manifest)>> {
        let entries = match std::fs::read_dir(self.root.join("versions")) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let file = entry.path();
            let Some(version) = file
                .file_stem()
                .and_then(|s| s.to_str())
                .filter(|v| valid_version(v))
                .map(str::to_owned)
            else {
                continue;
            };
            let Ok(Some(m)) = self.manifest(&version) else {
                continue;
            };
            let at = m
                .saved_at
                .clone()
                .or_else(|| {
                    entry
                        .metadata()
                        .and_then(|meta| meta.modified())
                        .map(crate::trace::rfc3339)
                        .ok()
                })
                .unwrap_or_default();
            found.push((at, version, m));
        }
        found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        Ok(found)
    }

    /// A page's text in a stored version, or `None` when the version (or the page in it)
    /// isn't stored.
    pub fn page_text(&self, version: &str, path: &str) -> Result<Option<String>, WikiError> {
        if !valid_version(version) {
            return Ok(None);
        }
        let Some(pages) = self.pages_of(version)? else {
            return Ok(None);
        };
        let Some(name) = pages.get(path) else {
            return Ok(None);
        };
        match std::fs::read_to_string(self.root.join("objects").join(name)) {
            Ok(t) => Ok(Some(t)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Takes the pages under `prefix` (a deleted user's or group's folder) out of every stored
    /// version, then removes every object no version refers to any more, so their text is gone
    /// from the history. A version keeps its name and when it was last used. Callers make sure
    /// no save runs meanwhile.
    pub fn purge(&self, prefix: &str) -> io::Result<()> {
        let versions = match std::fs::read_dir(self.root.join("versions")) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        let mut referenced: HashSet<String> = HashSet::new();
        let mut gone: HashSet<String> = HashSet::new();
        for entry in versions.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let json = match std::fs::read(&path) {
                Ok(j) => j,
                // Removed meanwhile by the trace retention's cleanup.
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let Ok(mut m) = serde_json::from_slice::<Manifest>(&json) else {
                // A malformed manifest can't be cleaned, nor replayed: it goes.
                remove_if_there(&path)?;
                continue;
            };
            let before = (m.pages.len(), m.removed.len());
            gone.extend(m.pages.keys().filter(|p| p.starts_with(prefix)).cloned());
            m.pages.retain(|p, _| !p.starts_with(prefix));
            m.removed.retain(|p| !p.starts_with(prefix));
            if (m.pages.len(), m.removed.len()) != before {
                let used = match std::fs::metadata(&path) {
                    Ok(meta) => meta.modified()?,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                };
                write_atomic(&path, &serde_json::to_vec(&m).map_err(io::Error::other)?)?;
                File::options()
                    .write(true)
                    .open(&path)?
                    .set_modified(used)?;
            }
            referenced.extend(m.pages.into_values());
        }
        for page in &gone {
            remove_if_there(&self.log_path(page))?;
        }
        if let Ok(objects) = std::fs::read_dir(self.root.join("objects")) {
            for entry in objects.flatten() {
                if !referenced.contains(entry.file_name().to_str().unwrap_or("")) {
                    remove_if_there(&entry.path())?;
                }
            }
        }
        Ok(())
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

    fn texts_of(h: &History, page: &str) -> Vec<(String, String)> {
        h.page_versions(page)
            .unwrap()
            .into_iter()
            .map(|v| (v.version, v.object))
            .collect()
    }

    /// A page changed back to an earlier text, the wiki coming back to a stored version,
    /// lists that text again, after the one between.
    #[test]
    fn a_page_changed_back_lists_its_text_again() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let v1 = vec![page("expertise/a", "A")];
        let v2 = vec![page("expertise/a", "A2")];
        save_on(&h, "0000000000000001", None, &v1);
        save_on(&h, "0000000000000002", Some("0000000000000001"), &v2);
        save_on(&h, "0000000000000001", Some("0000000000000002"), &v1);
        let list = texts_of(&h, "expertise/a");
        let versions: Vec<&str> = list.iter().map(|(v, _)| v.as_str()).collect();
        assert_eq!(
            versions,
            ["0000000000000001", "0000000000000002", "0000000000000001"]
        );
        assert_eq!(list[0].1, list[2].1);
        assert_ne!(list[0].1, list[1].1);
    }

    /// Changed back while the base given is no longer stored: told against the version saved
    /// last instead, so the return is still listed.
    #[test]
    fn a_return_is_listed_though_its_base_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let v1 = vec![page("expertise/a", "A")];
        save_on(&h, "0000000000000001", None, &v1);
        save_on(
            &h,
            "0000000000000002",
            Some("0000000000000001"),
            &[page("expertise/a", "A2")],
        );
        save_on(&h, "0000000000000001", Some("00000000000000ff"), &v1);
        assert_eq!(texts_of(&h, "expertise/a").len(), 3);
    }

    /// A save without a base (the first after a restart) notes only the pages that changed
    /// since the version saved last.
    #[test]
    fn a_save_without_a_base_notes_only_what_changed() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        save_on(
            &h,
            "0000000000000001",
            None,
            &[page("expertise/a", "A"), page("expertise/b", "B")],
        );
        save_on(
            &h,
            "0000000000000002",
            None,
            &[page("expertise/a", "A"), page("expertise/b", "B2")],
        );
        assert_eq!(texts_of(&h, "expertise/a").len(), 1);
        assert_eq!(texts_of(&h, "expertise/b").len(), 2);
    }

    /// A history kept before the pages' lists existed gets them from its manifests at its next
    /// save, and lists the same versions as before.
    #[test]
    fn a_history_from_before_the_lists_gets_them() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        save_on(&h, "0000000000000001", None, &[page("expertise/a", "A")]);
        save_on(
            &h,
            "0000000000000002",
            Some("0000000000000001"),
            &[page("expertise/a", "A2")],
        );
        std::fs::remove_dir_all(dir.path().join("history/pages")).unwrap();
        std::fs::remove_file(dir.path().join("history/last")).unwrap();
        let before = texts_of(&h, "expertise/a");
        assert_eq!(before.len(), 2, "read from the manifests");
        save_on(
            &h,
            "0000000000000003",
            Some("0000000000000002"),
            &[page("expertise/a", "A3")],
        );
        let after = texts_of(&h, "expertise/a");
        assert_eq!(after.len(), 3, "{after:?}");
        assert_eq!(after[..2], before[..]);
    }

    /// Deleting a user's folder takes its pages' lists with it.
    #[test]
    fn a_purged_folder_loses_its_pages_lists() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::new(dir.path());
        let mine = "users/u_0123456789abcdef/preferences";
        save_on(
            &h,
            "0000000000000001",
            None,
            &[page("expertise/a", "A"), page(mine, "Mine")],
        );
        assert!(h.log_path(mine).exists());
        h.purge("users/u_0123456789abcdef/").unwrap();
        assert!(!h.log_path(mine).exists());
        assert!(h.log_path("expertise/a").exists());
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
