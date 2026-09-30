//! Wiki pages as Markdown files with YAML front matter, one folder tree per tenant.

mod page;
mod path;

pub use page::{FrontMatter, Page};
pub use path::PagePath;

use std::io;
use std::path::{Path, PathBuf};

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

    /// Lists pages visible to a caller: every tenant-level page, plus the user pages under
    /// `users/<own_user_dir>/` when a user is given. Other users' folders are never opened.
    pub fn list(&self, own_user_dir: Option<&str>) -> Result<Vec<PageSummary>, WikiError> {
        let mut out = Vec::new();
        for root in ["index", "log"] {
            self.push_if_page(root, &mut out)?;
        }
        for dir in ["experience", "expertise", "graph"] {
            self.list_dir(dir, &mut out)?;
        }
        if let Some(user_dir) = own_user_dir {
            self.push_if_page(&format!("users/{user_dir}/preferences"), &mut out)?;
            self.list_dir(&format!("users/{user_dir}/decisions"), &mut out)?;
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn list_dir(&self, rel_dir: &str, out: &mut Vec<PageSummary>) -> Result<(), WikiError> {
        let dir = self.root.join(rel_dir);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".md")) else {
                continue;
            };
            self.push_if_page(&format!("{rel_dir}/{stem}"), out)?;
        }
        Ok(())
    }

    fn push_if_page(&self, rel: &str, out: &mut Vec<PageSummary>) -> Result<(), WikiError> {
        // Files whose names aren't valid page paths are skipped rather than exposed.
        let Ok(path) = PagePath::parse(rel) else {
            return Ok(());
        };
        match self.read(&path) {
            Ok(page) => out.push(page.summary()),
            Err(WikiError::NotFound) => {}
            Err(WikiError::Malformed { path, reason }) => {
                tracing::warn!(%path, %reason, "skipping malformed page");
            }
            Err(e) => return Err(e),
        }
        Ok(())
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
        wiki.write(&page("expertise/refunds", "Refunds")).unwrap();
        wiki.write(&page(&format!("users/{me}/preferences"), "Mine"))
            .unwrap();
        wiki.write(&page(&format!("users/{other}/decisions/x"), "Theirs"))
            .unwrap();

        let read = wiki
            .read(&PagePath::parse("expertise/refunds").unwrap())
            .unwrap();
        assert_eq!(read.front.title.as_deref(), Some("Refunds"));

        let paths: Vec<_> = wiki
            .list(Some(me))
            .unwrap()
            .into_iter()
            .map(|s| s.path)
            .collect();
        assert_eq!(
            paths,
            vec![
                "expertise/refunds".to_string(),
                format!("users/{me}/preferences")
            ]
        );
        let anon: Vec<_> = wiki
            .list(None)
            .unwrap()
            .into_iter()
            .map(|s| s.path)
            .collect();
        assert_eq!(anon, vec!["expertise/refunds"]);
    }

    #[test]
    fn missing_page_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = WikiFs::new(dir.path());
        let err = wiki.read(&PagePath::parse("expertise/nope").unwrap());
        assert!(matches!(err, Err(WikiError::NotFound)));
    }
}
