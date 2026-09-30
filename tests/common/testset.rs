//! Loading `testset/` into tenants: shared by the search tests and `examples/load_testset.rs`.
//! `tests/testset.rs` validates the same files in depth; this module only reads what is needed
//! to install the pages and ask the questions.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use kb::tenants::{group_dir_name, user_dir_name};
use serde::Deserialize;

pub fn testset_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testset")
}

#[derive(Debug, Deserialize)]
pub struct TenantFile {
    pub id: String,
    pub users: Vec<UserEntry>,
}

#[derive(Debug, Deserialize)]
pub struct UserEntry {
    pub id: String,
    #[serde(default)]
    pub groups: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct QuestionFile {
    pub tenant: String,
    pub questions: Vec<Question>,
}

#[derive(Debug, Deserialize)]
pub struct Question {
    pub id: String,
    pub category: String,
    pub milestone: String,
    pub lang: String,
    #[serde(default)]
    pub user: Option<String>,
    pub query: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub expect: Expect,
    #[serde(default)]
    pub expect_empty: Vec<String>,
    #[serde(default)]
    pub must_not: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Expect {
    #[serde(default)]
    pub memories: Vec<String>,
    #[serde(default)]
    pub knowledge: Vec<String>,
}

/// The scenario tenants, in name order.
pub fn tenants() -> Vec<TenantFile> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(testset_dir().join("tenants"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.iter()
        .map(|d| {
            serde_norway::from_str(&fs::read_to_string(d.join("tenant.yaml")).unwrap()).unwrap()
        })
        .collect()
}

pub fn questions(tenant: &str) -> Vec<Question> {
    let path = testset_dir()
        .join("questions")
        .join(format!("{tenant}.yaml"));
    let file: QuestionFile = serde_norway::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(file.tenant, tenant);
    file.questions
}

impl TenantFile {
    /// The groups the host sends for a user.
    pub fn groups_of(&self, user: &str) -> Vec<String> {
        self.users
            .iter()
            .find(|u| u.id == user)
            .map(|u| u.groups.clone())
            .unwrap_or_default()
    }
}

/// Maps a fixture path to a real page path: `users/<raw>/…` → `users/u_<hash>/…`, and
/// `groups/<raw>/…` → `groups/g_<hash>/…`.
pub fn page_path(fixture: &str) -> String {
    let mut segs: Vec<String> = fixture.split('/').map(str::to_owned).collect();
    match segs.first().map(String::as_str) {
        Some("users") if segs.len() > 1 => segs[1] = user_dir_name(&segs[1]),
        Some("groups") if segs.len() > 1 => segs[1] = group_dir_name(&segs[1]),
        _ => {}
    }
    segs.join("/")
}

/// Copies a scenario's wiki pages into a tenant's `wiki/` folder, mapping user and group
/// folders to their hashed names.
pub fn install_wiki(tenant: &str, wiki_dir: &Path) {
    let src = testset_dir().join("tenants").join(tenant).join("wiki");
    let mut files = Vec::new();
    walk(&src, &src, &mut files);
    for (rel, file) in files {
        let dest = wiki_dir.join(format!("{}.md", page_path(&rel)));
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::copy(&file, &dest).unwrap();
    }
}

fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(base, &p, out);
        } else if p.extension().is_some_and(|e| e == "md") {
            let rel = p.strip_prefix(base).unwrap().with_extension("");
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_str().unwrap())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, p));
        }
    }
}

/// Counts of passes per key, for the report.
#[derive(Default)]
pub struct Tally(pub HashMap<String, (usize, usize)>);

impl Tally {
    pub fn add(&mut self, key: &str, passed: bool) {
        let e = self.0.entry(key.to_owned()).or_default();
        e.1 += 1;
        if passed {
            e.0 += 1;
        }
    }

    pub fn lines(&self) -> Vec<String> {
        let mut keys: Vec<&String> = self.0.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|k| {
                let (p, n) = self.0[k];
                format!("  {k:<14} {p:>3}/{n:<3}")
            })
            .collect()
    }
}
