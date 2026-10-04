//! Page vectors for meaning-based search (design, "Search signals" → "Meaning, when the agent
//! asks for it").
//!
//! Each page is embedded as a few units: its head (title, aliases, keywords and summary) and
//! each section of its body. A page's similarity to a query is that of its best unit, so a long
//! page is found by the one section that answers. The vectors live in memory with the search
//! snapshot; SQLite keeps them between runs (`Store::embeddings`), so only new or changed
//! units are embedded when the wiki changes.

use std::collections::{HashMap, HashSet};

use super::catalog::Catalog;
use crate::embed::{self, Embedder};
use crate::store::StoredVector;

/// The `heading` key of a page's head unit.
pub const HEAD: &str = "";

/// One piece of a page that gets its own vector.
#[derive(Debug, Clone)]
pub struct Unit {
    /// The page's position in the catalog.
    pub page: usize,
    /// Which part of the page: [`HEAD`], or a section's heading (`#<n>` for text before the
    /// first heading; a repeated heading gets ` #<n>`).
    pub heading: String,
    /// What the model is given.
    pub text: String,
    /// 16 hex digits of a SHA-256 over `text`.
    pub hash: String,
}

/// The units of every page of a catalog, in page order.
pub fn units(catalog: &Catalog) -> Vec<Unit> {
    let mut out = Vec::new();
    for (ord, page) in catalog.pages.iter().enumerate() {
        let mut push = |heading: String, text: String| {
            out.push(Unit {
                page: ord,
                hash: hash(&text),
                heading,
                text,
            });
        };
        let mut head = page.title.clone();
        for list in [&page.aliases, &page.keywords] {
            if !list.is_empty() {
                head.push('\n');
                head.push_str(&list.join("; "));
            }
        }
        if !page.summary.is_empty() {
            head.push('\n');
            head.push_str(&page.summary);
        }
        push(HEAD.to_owned(), head);

        // Keys are unique within a page, whatever the headings say ("Rules", "Rules #2", "").
        let mut used: HashSet<String> = HashSet::from([HEAD.to_owned()]);
        for (i, section) in page.sections.iter().enumerate() {
            if section.lines.is_empty() {
                continue;
            }
            let base = section.heading.clone().unwrap_or_else(|| format!("#{i}"));
            let mut key = base.clone();
            let mut n = 1;
            while !used.insert(key.clone()) {
                n += 1;
                key = format!("{base} #{n}");
            }
            let mut text = page.title.clone();
            if let Some(h) = section.heading.as_deref().filter(|h| *h != page.title) {
                text.push_str(" — ");
                text.push_str(h);
            }
            for line in &section.lines {
                text.push('\n');
                text.push_str(&line.text);
            }
            push(key, text);
        }
    }
    out
}

fn hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(text.as_bytes())[..8])
}

/// The vectors of a snapshot's pages, one row per unit, held in memory and compared by brute
/// force (a few milliseconds for 100,000 rows).
#[derive(Debug, Default)]
pub struct Vectors {
    /// The model that made them; a query must be embedded by the same one.
    pub model: String,
    dim: usize,
    /// Row-major, `dim` values per row.
    data: Vec<f32>,
    /// The page each row belongs to.
    pages: Vec<usize>,
    /// Each row's unit, (page path, heading) to (row, text hash), so the next build reuses
    /// rows whose text is unchanged without reading them back from SQLite.
    units: HashMap<(String, String), (usize, String)>,
}

/// One row of [`Vectors`]: the page's position, its unit's key and text hash, the vector.
struct Row {
    page: usize,
    key: (String, String),
    hash: String,
    vector: Vec<f32>,
}

impl Vectors {
    /// The rows' vectors all have the length of the first row's (`build` checks it).
    fn from_rows(model: &str, rows: Vec<Row>) -> Self {
        let dim = rows.first().map_or(0, |r| r.vector.len());
        let mut out = Self {
            model: model.to_owned(),
            dim,
            data: Vec::with_capacity(rows.len() * dim),
            pages: Vec::with_capacity(rows.len()),
            units: HashMap::with_capacity(rows.len()),
        };
        for r in rows {
            if r.vector.len() == dim {
                out.units.insert(r.key, (out.pages.len(), r.hash));
                out.data.extend_from_slice(&r.vector);
                out.pages.push(r.page);
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    fn row(&self, i: usize) -> &[f32] {
        &self.data[i * self.dim..(i + 1) * self.dim]
    }

    /// Each page `keep` accepts with its best similarity to the query, best first (ties by
    /// page order).
    pub fn best(&self, query: &[f32], keep: impl Fn(usize) -> bool) -> Vec<(usize, f32)> {
        let mut best: HashMap<usize, f32> = HashMap::new();
        if self.dim == 0 || query.len() != self.dim {
            return Vec::new();
        }
        for (row, &page) in self.data.chunks_exact(self.dim).zip(&self.pages) {
            if !keep(page) {
                continue;
            }
            let s = embed::similarity(row, query);
            let e = best.entry(page).or_insert(f32::MIN);
            if s > *e {
                *e = s;
            }
        }
        let mut out: Vec<(usize, f32)> = best.into_iter().collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }
}

/// What [`build`] found: the vectors of every unit, and how the stored ones must change.
pub struct Built {
    pub vectors: Vectors,
    /// Vectors of new or changed units, to store.
    pub put: Vec<StoredVector>,
    /// Stored vectors (page, heading) whose unit no longer exists, to remove.
    pub drop: Vec<(String, String)>,
}

/// Where [`build`] finds vectors made before.
pub enum Earlier<'a> {
    /// The previous snapshot's, which match what is stored.
    Snapshot(&'a Vectors),
    /// The stored ones, when no snapshot of this model is in memory yet.
    Stored(Vec<StoredVector>),
}

/// The vectors of a catalog's units: earlier ones whose text is unchanged (same hash), and new
/// ones from the model for the rest. Blocking; runs the model.
pub fn build(
    catalog: &Catalog,
    earlier: Earlier<'_>,
    embedder: &dyn Embedder,
) -> anyhow::Result<Built> {
    let units = units(catalog);
    let (prev, mut stored): (Option<&Vectors>, HashMap<(String, String), StoredVector>) =
        match earlier {
            Earlier::Snapshot(prev) => (Some(prev), HashMap::new()),
            Earlier::Stored(list) => (
                None,
                list.into_iter()
                    .map(|v| ((v.page.clone(), v.heading.clone()), v))
                    .collect(),
            ),
        };
    let mut gone: HashSet<&(String, String)> =
        prev.map(|p| p.units.keys().collect()).unwrap_or_default();
    // An earlier vector of another length than the model's (a damaged row) is made again.
    let dim = embedder.dim();
    let mut rows = Vec::with_capacity(units.len());
    let mut missing: Vec<&Unit> = Vec::new();
    for u in &units {
        let key = (
            catalog.pages[u.page].path.as_str().to_owned(),
            u.heading.clone(),
        );
        let reused = match prev {
            Some(prev) => {
                gone.remove(&key);
                prev.units
                    .get(&key)
                    .filter(|(_, hash)| *hash == u.hash)
                    .map(|&(i, _)| prev.row(i).to_vec())
                    .filter(|v| v.len() == dim)
            }
            None => stored
                .remove(&key)
                .filter(|v| v.hash == u.hash && v.vector.len() == dim)
                .map(|v| v.vector),
        };
        match reused {
            Some(vector) => rows.push(Row {
                page: u.page,
                key,
                hash: u.hash.clone(),
                vector,
            }),
            None => missing.push(u),
        }
    }
    let texts: Vec<String> = missing.iter().map(|u| u.text.clone()).collect();
    let fresh = embedder.embed(&texts)?;
    anyhow::ensure!(
        fresh.len() == missing.len(),
        "the embedding model returned {} vectors for {} texts",
        fresh.len(),
        missing.len()
    );
    let mut put = Vec::with_capacity(missing.len());
    for (u, v) in missing.into_iter().zip(fresh) {
        let key = (
            catalog.pages[u.page].path.as_str().to_owned(),
            u.heading.clone(),
        );
        put.push(StoredVector {
            page: key.0.clone(),
            heading: key.1.clone(),
            hash: u.hash.clone(),
            vector: v.clone(),
        });
        rows.push(Row {
            page: u.page,
            key,
            hash: u.hash.clone(),
            vector: v,
        });
    }
    let mut drop: Vec<(String, String)> = match prev {
        Some(_) => gone.into_iter().cloned().collect(),
        None => stored.into_keys().collect(),
    };
    drop.sort();
    Ok(Built {
        vectors: Vectors::from_rows(embedder.model(), rows),
        put,
        drop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchSnapshot;
    use crate::wiki::{Page, PagePath};

    #[test]
    fn a_page_has_a_head_unit_and_one_per_section() {
        let mut p = Page::new(PagePath::parse("expertise/returns").unwrap());
        p.front.title = Some("Returns".into());
        p.front.aliases = vec!["退货".into()];
        p.front.summary = Some("Seven days, no reason needed.".into());
        p.body = "# Returns\n\nIntro line.\n\n## Rules\n\nSeven days.\n\n## Rules\n\nFresh food excluded.\n\n## Empty\n"
            .into();
        let snap = SearchSnapshot::build(vec![p]).unwrap();
        let units = units(&snap.catalog);
        let keys: Vec<&str> = units.iter().map(|u| u.heading.as_str()).collect();
        assert_eq!(keys, vec![HEAD, "Returns", "Rules", "Rules #2"]);
        assert_eq!(
            units[0].text,
            "Returns\n退货\nSeven days, no reason needed."
        );
        assert_eq!(units[2].text, "Returns — Rules\nSeven days.");
        assert_ne!(units[2].hash, units[3].hash);
    }

    #[test]
    fn a_page_scores_by_its_best_unit() {
        let row = |i: usize, page: usize, vector: Vec<f32>| Row {
            page,
            key: (String::new(), format!("#{i}")),
            hash: String::new(),
            vector,
        };
        let v = Vectors::from_rows(
            "m",
            vec![
                row(0, 0, vec![1.0, 0.0]),
                row(1, 0, vec![0.0, 1.0]),
                row(2, 1, vec![0.6, 0.8]),
            ],
        );
        let got = v.best(&[0.0, 1.0], |_| true);
        assert_eq!(got[0], (0, 1.0));
        assert_eq!(got[1].0, 1);
        assert!(v.best(&[0.0, 1.0], |p| p == 1).iter().all(|(p, _)| *p == 1));
        assert!(v.best(&[1.0], |_| true).is_empty(), "wrong length");
    }

    /// Counts the texts it embeds; vectors are the text length, so any text gets one.
    struct Counting(std::sync::Mutex<Vec<String>>);

    impl Embedder for Counting {
        fn model(&self) -> &str {
            "counting"
        }
        fn dim(&self) -> usize {
            2
        }
        fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            self.0.lock().unwrap().extend(texts.iter().cloned());
            Ok(texts.iter().map(|t| vec![t.len() as f32, 1.0]).collect())
        }
    }

    fn snap_of(bodies: &[(&str, &str)]) -> SearchSnapshot {
        let pages = bodies
            .iter()
            .map(|(path, body)| {
                let mut p = Page::new(PagePath::parse(path).unwrap());
                p.front.title = Some("T".into());
                p.body = (*body).into();
                p
            })
            .collect();
        SearchSnapshot::build(pages).unwrap()
    }

    #[test]
    fn unit_keys_stay_unique_whatever_the_headings() {
        let snap = snap_of(&[(
            "expertise/a",
            "## Rules\n\nOne.\n\n## Rules\n\nTwo.\n\n## Rules #2\n\nThree.\n",
        )]);
        let keys: Vec<String> = units(&snap.catalog)
            .into_iter()
            .map(|u| u.heading)
            .collect();
        assert_eq!(keys, vec![HEAD, "Rules", "Rules #2", "Rules #2 #2"]);
    }

    #[test]
    fn a_new_build_reuses_the_previous_vectors_and_embeds_only_changes() {
        let model = Counting(Default::default());
        let first = snap_of(&[
            ("expertise/a", "## One\n\nSame.\n\n## Two\n\nOld.\n"),
            ("expertise/b", "## Gone\n\nBye.\n"),
        ]);
        let built = build(&first.catalog, Earlier::Stored(Vec::new()), &model).unwrap();
        assert_eq!(built.put.len(), 5);
        let second = snap_of(&[("expertise/a", "## One\n\nSame.\n\n## Two\n\nNew.\n")]);
        model.0.lock().unwrap().clear();
        let again = build(&second.catalog, Earlier::Snapshot(&built.vectors), &model).unwrap();
        let seen = model.0.lock().unwrap().clone();
        assert_eq!(seen, vec!["T — Two\nNew.".to_string()]);
        assert_eq!(again.vectors.len(), 3);
        assert_eq!(
            again.drop,
            vec![
                ("expertise/b".to_string(), HEAD.to_string()),
                ("expertise/b".to_string(), "Gone".to_string()),
            ]
        );
    }

    #[test]
    fn a_damaged_stored_vector_is_made_again() {
        let model = Counting(Default::default());
        let snap = snap_of(&[("expertise/a", "## One\n\nSame.\n")]);
        let built = build(&snap.catalog, Earlier::Stored(Vec::new()), &model).unwrap();
        let mut stored = built.put.clone();
        stored[0].vector.pop();
        model.0.lock().unwrap().clear();
        let again = build(&snap.catalog, Earlier::Stored(stored), &model).unwrap();
        assert_eq!(model.0.lock().unwrap().len(), 1, "only the damaged one");
        assert_eq!(again.vectors.len(), 2);
        assert_eq!(again.put.len(), 1);
    }
}
