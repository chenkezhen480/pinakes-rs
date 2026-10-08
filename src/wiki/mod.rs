//! Wiki pages as Markdown files with YAML front matter, one folder tree per tenant.

mod history;
mod page;
mod path;

pub use history::{History, object_name};
pub use page::{FrontMatter, Page};
pub use path::{Audience, PagePath};

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use crate::kinds::Kind;

#[derive(Debug, thiserror::Error)]
pub enum WikiError {
    #[error("invalid page path: {0}")]
    InvalidPath(String),
    #[error("page not found")]
    NotFound,
    #[error("malformed page {path}: {reason}")]
    Malformed { path: String, reason: String },
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// What `/pages` lists for each page: enough to pick one, not its body.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PageSummary {
    /// The page id, as search hits and `/read` name it.
    #[serde(rename = "page")]
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
}

/// When a page file was last written and how long it is, as its folder lists it: how a later
/// look at the folder tells a changed page from an unchanged one without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub modified: SystemTime,
    pub len: u64,
}

impl Stamp {
    fn of(meta: &std::fs::Metadata) -> io::Result<Self> {
        Ok(Self {
            modified: meta.modified()?,
            len: meta.len(),
        })
    }
}

/// The `wiki/` folder of one tenant. It only ever touches paths built from a validated
/// [`PagePath`], so nothing outside the folder is reachable.
#[derive(Debug, Clone)]
pub struct WikiFs {
    root: PathBuf,
}

impl WikiFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn file_for(&self, path: &PagePath) -> PathBuf {
        let mut file = self.root.clone();
        for seg in path.segments() {
            file.push(seg);
        }
        file.set_extension("md");
        file
    }

    pub fn read(&self, path: &PagePath) -> Result<Page, WikiError> {
        let file = self.file_for(path);
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(WikiError::NotFound),
            // Not UTF-8: a bad page, like broken front matter, not a failure of the service.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                return Err(WikiError::Malformed {
                    path: path.to_string(),
                    reason: "not valid UTF-8".into(),
                });
            }
            Err(e) => return Err(e.into()),
        };
        Page::parse(path.clone(), &text)
    }

    /// Writes a page atomically: a temp file next to it, then a rename.
    pub fn write(&self, page: &Page) -> Result<(), WikiError> {
        let file = self.file_for(&page.path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = file.with_extension(format!("md.tmp-{}", hex::encode(rand::random::<[u8; 8]>())));
        std::fs::write(&tmp, page.render())?;
        std::fs::rename(&tmp, &file)?;
        Ok(())
    }

    /// A page file's text as it is on disk, or `None` when there is no such page.
    pub fn read_text(&self, path: &PagePath) -> Result<Option<String>, WikiError> {
        match std::fs::read(self.file_for(path)) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Puts a page file back to text read with [`WikiFs::read_text`]: that text, or no file.
    pub fn restore_text(&self, path: &PagePath, text: Option<String>) -> Result<(), WikiError> {
        let file = self.file_for(path);
        match text {
            Some(t) => {
                let tmp = file
                    .with_extension(format!("md.tmp-{}", hex::encode(rand::random::<[u8; 8]>())));
                std::fs::write(&tmp, t)?;
                std::fs::rename(&tmp, &file)?;
            }
            None => match std::fs::remove_file(&file) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                _ => {}
            },
        }
        Ok(())
    }

    /// Lists pages visible to a caller: `index`, `log`, every tenant-level page, the pages of the
    /// caller's own groups, and the user pages under `users/<own_user_dir>/` when a user is
    /// given. Other groups' and users' folders are never opened.
    pub fn list(
        &self,
        own_user_dir: Option<&str>,
        own_group_dirs: &[String],
    ) -> Result<Vec<PageSummary>, WikiError> {
        let user_dirs: Vec<String> = own_user_dir.map(str::to_owned).into_iter().collect();
        let mut paths: Vec<PagePath> = ["index", "log"]
            .iter()
            .filter_map(|p| PagePath::parse(p).ok())
            .collect();
        paths.extend(self.page_paths(own_group_dirs, &user_dirs)?);
        let mut out: Vec<PageSummary> = self.read_all(paths)?.iter().map(Page::summary).collect();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Every page of the tenant, for every audience, except `index` and `log`, in path order.
    /// Only the search snapshot uses this; it filters by the caller's scope on every query.
    pub fn all_pages(&self) -> Result<Vec<Page>, WikiError> {
        Ok(self
            .read_stamped(self.stamps()?)?
            .into_iter()
            .map(|(page, _)| page)
            .collect())
    }

    /// Every page file of the tenant, for every audience, except `index` and `log`, with its
    /// stamp, in path order. Reads folders only, never a page.
    pub fn stamps(&self) -> Result<Vec<(PagePath, Stamp)>, WikiError> {
        let groups = self.subdirs("groups")?;
        let users = self.subdirs("users")?;
        let mut out = self.page_files(&groups, &users)?;
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// One page file's stamp, or `None` when there is no such page.
    pub fn stamp(&self, path: &PagePath) -> Result<Option<Stamp>, WikiError> {
        match std::fs::metadata(self.file_for(path)) {
            Ok(meta) if meta.is_file() => Ok(Some(Stamp::of(&meta)?)),
            Ok(_) => Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Reads pages with the stamps they were listed with, spread over the cores, in the order
    /// given; missing pages are skipped, malformed ones logged and skipped.
    pub fn read_stamped(
        &self,
        list: Vec<(PagePath, Stamp)>,
    ) -> Result<Vec<(Page, Stamp)>, WikiError> {
        crate::par::map(list, |(path, stamp)| match self.read(&path) {
            Ok(page) => Ok(Some((page, stamp))),
            Err(WikiError::NotFound) => Ok(None),
            Err(WikiError::Malformed { path, reason }) => {
                tracing::warn!(%path, %reason, "skipping malformed page");
                Ok(None)
            }
            Err(e) => Err(e),
        })
        .into_iter()
        .filter_map(Result::transpose)
        .collect()
    }

    /// The paths of the pages that exist for the tenant and for the given group and user
    /// folders.
    fn page_paths(
        &self,
        group_dirs: &[String],
        user_dirs: &[String],
    ) -> Result<Vec<PagePath>, WikiError> {
        Ok(self
            .page_files(group_dirs, user_dirs)?
            .into_iter()
            .map(|(path, _)| path)
            .collect())
    }

    /// The pages that exist for the tenant and for the given group and user folders, with their
    /// stamps. The one place that walks the wiki layout; it agrees with `PagePath::parse`, and
    /// names that aren't valid page paths (such as raw ids) are left out.
    fn page_files(
        &self,
        group_dirs: &[String],
        user_dirs: &[String],
    ) -> Result<Vec<(PagePath, Stamp)>, WikiError> {
        let mut dirs: Vec<String> = ["experience", "expertise", "graph"]
            .map(String::from)
            .to_vec();
        let mut rels = Vec::new();
        for group in group_dirs {
            for sub in ["experience", "expertise", "decisions"] {
                dirs.push(format!("groups/{group}/{sub}"));
            }
        }
        for user in user_dirs {
            let rel = format!("users/{user}/preferences");
            match std::fs::metadata(self.root.join(format!("{rel}.md"))) {
                Ok(meta) if meta.is_file() => rels.push((rel, Stamp::of(&meta)?)),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            dirs.push(format!("users/{user}/decisions"));
        }
        for dir in &dirs {
            for (stem, stamp) in self.page_stems(dir)? {
                rels.push((format!("{dir}/{stem}"), stamp));
            }
        }
        Ok(rels
            .into_iter()
            .filter_map(|(r, stamp)| PagePath::parse(&r).ok().map(|p| (p, stamp)))
            .collect())
    }

    /// Reads pages, skipping missing ones and logging and skipping malformed ones.
    fn read_all(&self, paths: Vec<PagePath>) -> Result<Vec<Page>, WikiError> {
        let mut out = Vec::new();
        for path in paths {
            match self.read(&path) {
                Ok(page) => out.push(page),
                Err(WikiError::NotFound) => {}
                Err(WikiError::Malformed { path, reason }) => {
                    tracing::warn!(%path, %reason, "skipping malformed page");
                }
                Err(e) => return Err(e),
            }
        }
        Ok(out)
    }

    fn subdirs(&self, rel_dir: &str) -> Result<Vec<String>, WikiError> {
        let entries = match std::fs::read_dir(self.root.join(rel_dir)) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(name) = entry.file_name().to_str()
            {
                out.push(name.to_owned());
            }
        }
        Ok(out)
    }

    /// The page files of one folder, by name without `.md`, with their stamps. On Windows the
    /// listing carries each file's size and time, so no file is opened.
    fn page_stems(&self, rel_dir: &str) -> Result<Vec<(String, Stamp)>, WikiError> {
        let entries = match std::fs::read_dir(self.root.join(rel_dir)) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if let Some(stem) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".md"))
            {
                out.push((stem.to_owned(), Stamp::of(&entry.metadata()?)?));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(path: &str, title: &str) -> Page {
        let mut p = Page::new(PagePath::parse(path).unwrap());
        p.front.title = Some(title.into());
        p.body = format!("# {title}\n");
        p
    }

    #[test]
    fn write_read_list() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = WikiFs::new(dir.path());
        let me = "u_0123456789abcdef";
        let other = "u_fedcba9876543210";
        let team = "g_00112233445566aa".to_string();
        let other_team = "g_aa66554433221100";
        wiki.write(&page("expertise/refunds", "Refunds")).unwrap();
        wiki.write(&page(&format!("users/{me}/preferences"), "Mine"))
            .unwrap();
        wiki.write(&page(&format!("users/{other}/decisions/x"), "Theirs"))
            .unwrap();
        wiki.write(&page(&format!("groups/{team}/decisions/y"), "Team"))
            .unwrap();
        wiki.write(&page(
            &format!("groups/{other_team}/experience/z"),
            "Other team",
        ))
        .unwrap();

        let read = wiki
            .read(&PagePath::parse("expertise/refunds").unwrap())
            .unwrap();
        assert_eq!(read.front.title.as_deref(), Some("Refunds"));

        let paths: Vec<_> = wiki
            .list(Some(me), std::slice::from_ref(&team))
            .unwrap()
            .into_iter()
            .map(|s| s.path)
            .collect();
        assert_eq!(
            paths,
            vec![
                "expertise/refunds".to_string(),
                format!("groups/{team}/decisions/y"),
                format!("users/{me}/preferences")
            ]
        );
        let anon: Vec<_> = wiki
            .list(None, &[])
            .unwrap()
            .into_iter()
            .map(|s| s.path)
            .collect();
        assert_eq!(anon, vec!["expertise/refunds"]);
    }

    #[test]
    fn non_utf8_pages_are_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = WikiFs::new(dir.path());
        wiki.write(&page("expertise/good", "Good")).unwrap();
        std::fs::write(
            dir.path().join("expertise").join("bad.md"),
            [0xff, 0xfe, 0x00],
        )
        .unwrap();
        let bad = wiki.read(&PagePath::parse("expertise/bad").unwrap());
        assert!(matches!(bad, Err(WikiError::Malformed { .. })));
        let all: Vec<_> = wiki
            .all_pages()
            .unwrap()
            .into_iter()
            .map(|p| p.path.to_string())
            .collect();
        assert_eq!(all, vec!["expertise/good"]);
        assert_eq!(wiki.list(None, &[]).unwrap().len(), 1);
    }

    #[test]
    fn missing_page_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = WikiFs::new(dir.path());
        let err = wiki.read(&PagePath::parse("expertise/nope").unwrap());
        assert!(matches!(err, Err(WikiError::NotFound)));
    }
}
