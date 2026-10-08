//! Page vectors for meaning-based search (design, "Search signals" → "Meaning, when the agent
//! asks for it").
//!
//! Each page is embedded as a few units: its head (title, aliases, keywords and summary) and
//! each section of its body. A page's similarity to a query is that of its best unit, so a long
//! page is found by the one section that answers. The vectors live in memory with the search
//! snapshot; SQLite keeps them between runs (`Store::embeddings`), so only new or changed
//! units are embedded when the wiki changes.
//!
//! In memory each value is one signed byte with a scale per row (int8 quantization): a quarter
//! of the size of 32-bit floats, and four times less memory to stream through per search, at a
//! cost to similarities below 0.01. A page's vectors are shared by every snapshot that has the
//! page unchanged.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::catalog::{Catalog, PageEntry};
use crate::embed::Embedder;
use crate::store::StoredVector;

/// The `heading` key of a page's head unit.
pub const HEAD: &str = "";

/// One piece of a page that gets its own vector.
#[derive(Debug, Clone)]
pub struct Unit {
    /// Which part of the page: [`HEAD`], or a section's heading (`#<n>` for text before the
    /// first heading; a repeated heading gets ` #<n>`).
    pub heading: String,
    /// What the model is given.
    pub text: String,
    /// 16 hex digits of a SHA-256 over `text`.
    pub hash: String,
}

/// The units of one page.
pub fn units(page: &PageEntry) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut push = |heading: String, text: String| {
        out.push(Unit {
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
    out
}

fn hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(text.as_bytes())[..8])
}

/// A vector as signed bytes and the scale that turns them back into values.
fn quantize(v: &[f32]) -> Row {
    let max = v.iter().fold(0f32, |m, x| m.max(x.abs()));
    if max == 0.0 {
        return (0.0, vec![0; v.len()]);
    }
    let scale = max / 127.0;
    (scale, v.iter().map(|x| (x / scale).round() as i8).collect())
}

/// The dot product of two byte vectors; whole numbers, so the compiler may add them in any
/// order and vectorizes the loop.
#[inline(always)]
fn dot(a: &[i8], b: &[i8]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| i32::from(*x) * i32::from(*y))
        .sum()
}

/// [`dot`] with AVX2: sixteen bytes a step, widened to 16 bits and multiplied and added in
/// pairs into 32-bit sums (the products of two bytes can't overflow them).
///
/// # Safety
/// Only on a CPU with AVX2.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_avx2(a: &[i8], b: &[i8]) -> i32 {
    use std::arch::x86_64::*;
    let n = a.len().min(b.len());
    let mut acc = _mm256_setzero_si256();
    let mut i = 0;
    while i + 16 <= n {
        // SAFETY: `i + 16 <= n` keeps both 16-byte loads inside the slices.
        let (va, vb) = unsafe {
            (
                _mm_loadu_si128(a.as_ptr().add(i).cast()),
                _mm_loadu_si128(b.as_ptr().add(i).cast()),
            )
        };
        let products = _mm256_madd_epi16(_mm256_cvtepi8_epi16(va), _mm256_cvtepi8_epi16(vb));
        acc = _mm256_add_epi32(acc, products);
        i += 16;
    }
    let s = _mm_add_epi32(
        _mm256_castsi256_si128(acc),
        _mm256_extracti128_si256(acc, 1),
    );
    let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b01_00_11_10));
    let s = _mm_add_epi32(s, _mm_shuffle_epi32(s, 0b10_11_00_01));
    _mm_cvtsi128_si32(s) + dot(&a[i..n], &b[i..n])
}

/// [`dot`] with the CPU's widest instructions, decided once.
struct Dot {
    #[cfg(target_arch = "x86_64")]
    avx2: bool,
}

impl Dot {
    fn new() -> Self {
        Self {
            #[cfg(target_arch = "x86_64")]
            avx2: std::arch::is_x86_feature_detected!("avx2"),
        }
    }

    #[inline]
    fn of(&self, a: &[i8], b: &[i8]) -> i32 {
        #[cfg(target_arch = "x86_64")]
        if self.avx2 {
            // SAFETY: the CPU has AVX2, checked in `new`.
            return unsafe { dot_avx2(a, b) };
        }
        dot(a, b)
    }
}

/// A row as bytes, with its scale.
type Row = (f32, Vec<i8>);

/// One page's vectors: a row per unit.
#[derive(Debug)]
pub struct PageVectors {
    /// The page's hash when they were made: an unchanged page keeps them as they are.
    page_hash: String,
    /// Per row: the unit's heading and text hash, and the row's scale.
    units: Vec<(String, String, f32)>,
    /// Row-major, `dim` values per row.
    data: Vec<i8>,
}

impl PageVectors {
    fn row(&self, i: usize, dim: usize) -> &[i8] {
        &self.data[i * dim..(i + 1) * dim]
    }
}

/// The vectors of a snapshot's pages, one [`PageVectors`] per catalog entry, compared with a
/// query by brute force.
#[derive(Debug, Default)]
pub struct Vectors {
    /// The model that made them; a query must be embedded by the same one.
    pub model: String,
    dim: usize,
    /// In catalog order: the vectors of page `ord`.
    pages: Vec<Arc<PageVectors>>,
    /// How many rows there are in all.
    rows: usize,
}

/// From this many rows on, a search compares the query with them on all cores.
const PARALLEL_ROWS: usize = 50_000;

impl Vectors {
    /// How many rows (units) there are.
    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Each page `keep` accepts with its best similarity to the query, best first (ties by
    /// page order). One pass over the vectors; a large set is split over the cores, since
    /// streaming it through one core is what costs (300 MB for 100,000 pages).
    pub fn best(&self, query: &[f32], keep: impl Fn(usize) -> bool + Sync) -> Vec<(usize, f32)> {
        if self.dim == 0 || query.len() != self.dim {
            return Vec::new();
        }
        let (qscale, q) = quantize(query);
        let dot = Dot::new();
        let scan = |from: usize, pages: &[Arc<PageVectors>]| -> Vec<(usize, f32)> {
            let mut out = Vec::new();
            for (i, page) in pages.iter().enumerate() {
                let ord = from + i;
                if page.units.is_empty() || !keep(ord) {
                    continue;
                }
                let best = page
                    .units
                    .iter()
                    .enumerate()
                    .map(|(r, (_, _, scale))| {
                        dot.of(&q, page.row(r, self.dim)) as f32 * scale * qscale
                    })
                    .fold(f32::MIN, f32::max);
                out.push((ord, best));
            }
            out
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let mut out = if self.rows < PARALLEL_ROWS || threads < 2 {
            scan(0, &self.pages)
        } else {
            let per = self.pages.len().div_ceil(threads);
            std::thread::scope(|s| {
                let parts: Vec<_> = self
                    .pages
                    .chunks(per)
                    .enumerate()
                    .map(|(i, chunk)| {
                        let scan = &scan;
                        s.spawn(move || scan(i * per, chunk))
                    })
                    .collect();
                parts
                    .into_iter()
                    .flat_map(|p| p.join().expect("a vector scan panicked"))
                    .collect()
            })
        };
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }
}

/// What [`build`] found: the vectors of every page, and how the stored ones must change.
pub struct Built {
    pub vectors: Vectors,
    /// Vectors of new or changed units, to store.
    pub put: Vec<StoredVector>,
    /// Stored vectors (page, heading) whose unit no longer exists, to remove.
    pub drop: Vec<(String, String)>,
}

/// Where [`build`] finds vectors made before.
pub enum Earlier<'a> {
    /// The previous snapshot's, with its catalog: they match what is stored.
    Snapshot(&'a Vectors, &'a Catalog),
    /// The stored ones, when no snapshot of this model is in memory yet.
    Stored(Vec<StoredVector>),
}

/// The vectors of a catalog's pages: an unchanged page keeps its earlier ones whole; for the
/// others, earlier rows whose unit text is unchanged (same hash) are kept and the model embeds
/// the rest. Blocking; runs the model.
pub fn build(
    catalog: &Catalog,
    earlier: Earlier<'_>,
    embedder: &dyn Embedder,
) -> anyhow::Result<Built> {
    let dim = embedder.dim();
    // Earlier rows by page: (page vectors, or stored rows by heading).
    let mut prev: HashMap<&str, &Arc<PageVectors>> = HashMap::new();
    let mut stored: HashMap<String, HashMap<String, StoredVector>> = HashMap::new();
    match earlier {
        Earlier::Snapshot(v, c) if v.dim == dim => {
            for (entry, pv) in c.pages.iter().zip(&v.pages) {
                prev.insert(entry.path.as_str(), pv);
            }
        }
        Earlier::Snapshot(..) => {}
        Earlier::Stored(list) => {
            for v in list {
                stored
                    .entry(v.page.clone())
                    .or_default()
                    .insert(v.heading.clone(), v);
            }
        }
    }
    let mut pages: Vec<Option<Arc<PageVectors>>> = Vec::with_capacity(catalog.pages.len());
    // Rows to embed: (catalog ord, unit).
    let mut missing: Vec<(usize, Unit)> = Vec::new();
    // Per page being made: its rows so far, by heading.
    let mut partial: HashMap<usize, Vec<(Unit, Option<Row>)>> = HashMap::new();
    let mut drop: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for (ord, entry) in catalog.pages.iter().enumerate() {
        let path = entry.path.as_str();
        seen.insert(path);
        let earlier = prev.get(path).copied();
        if let Some(pv) = earlier.filter(|pv| pv.page_hash == entry.hash) {
            pages.push(Some(pv.clone()));
            stored.remove(path);
            continue;
        }
        pages.push(None);
        let mut rows = Vec::new();
        let mut from_store = stored.remove(path).unwrap_or_default();
        let units = units(entry);
        let kept: HashSet<&str> = units.iter().map(|u| u.heading.as_str()).collect();
        for u in units.iter().cloned() {
            let reused = match earlier {
                Some(pv) => pv
                    .units
                    .iter()
                    .position(|(h, hash, _)| *h == u.heading && *hash == u.hash)
                    .map(|i| (pv.units[i].2, pv.row(i, dim).to_vec())),
                None => from_store
                    .remove(&u.heading)
                    .filter(|v| v.hash == u.hash && v.vector.len() == dim)
                    .map(|v| quantize(&v.vector)),
            };
            if reused.is_none() {
                missing.push((ord, u.clone()));
            }
            rows.push((u, reused));
        }
        // Units of this page that no longer exist.
        match earlier {
            Some(pv) => drop.extend(
                pv.units
                    .iter()
                    .filter(|(h, ..)| !kept.contains(h.as_str()))
                    .map(|(h, ..)| (path.to_owned(), h.clone())),
            ),
            None => drop.extend(from_store.into_keys().map(|h| (path.to_owned(), h))),
        }
        partial.insert(ord, rows);
    }
    // Pages that are gone.
    for (path, pv) in &prev {
        if !seen.contains(path) {
            drop.extend(
                pv.units
                    .iter()
                    .map(|(h, ..)| ((*path).to_owned(), h.clone())),
            );
        }
    }
    for (path, rows) in stored {
        drop.extend(rows.into_keys().map(|h| (path.clone(), h)));
    }

    let texts: Vec<String> = missing.iter().map(|(_, u)| u.text.clone()).collect();
    let fresh = embedder.embed(&texts)?;
    anyhow::ensure!(
        fresh.len() == missing.len(),
        "the embedding model returned {} vectors for {} texts",
        fresh.len(),
        missing.len()
    );
    let mut put = Vec::with_capacity(missing.len());
    let mut made: HashMap<(usize, String), Row> = HashMap::new();
    for ((ord, u), v) in missing.into_iter().zip(fresh) {
        anyhow::ensure!(
            v.len() == dim,
            "the embedding model returned a vector of another length"
        );
        made.insert((ord, u.heading.clone()), quantize(&v));
        put.push(StoredVector {
            page: catalog.pages[ord].path.to_string(),
            heading: u.heading,
            hash: u.hash,
            vector: v,
        });
    }
    for (ord, rows) in partial {
        let mut pv = PageVectors {
            page_hash: catalog.pages[ord].hash.clone(),
            units: Vec::with_capacity(rows.len()),
            data: Vec::with_capacity(rows.len() * dim),
        };
        for (u, row) in rows {
            let (scale, data) = match row {
                Some(r) => r,
                None => made
                    .remove(&(ord, u.heading.clone()))
                    .expect("every missing unit was embedded"),
            };
            pv.units.push((u.heading, u.hash, scale));
            pv.data.extend_from_slice(&data);
        }
        pages[ord] = Some(Arc::new(pv));
    }
    drop.sort();
    let pages: Vec<Arc<PageVectors>> = pages
        .into_iter()
        .map(|p| p.expect("every page has vectors"))
        .collect();
    Ok(Built {
        vectors: Vectors {
            model: embedder.model().to_owned(),
            dim,
            rows: pages.iter().map(|p| p.units.len()).sum(),
            pages,
        },
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
        let units = units(&snap.catalog.pages[0]);
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
        let page = |rows: &[[f32; 2]]| {
            let mut pv = PageVectors {
                page_hash: String::new(),
                units: Vec::new(),
                data: Vec::new(),
            };
            for (i, r) in rows.iter().enumerate() {
                let (scale, data) = quantize(r);
                pv.units.push((format!("#{i}"), String::new(), scale));
                pv.data.extend(data);
            }
            Arc::new(pv)
        };
        let v = Vectors {
            model: "m".into(),
            dim: 2,
            pages: vec![page(&[[1.0, 0.0], [0.0, 1.0]]), page(&[[0.6, 0.8]])],
            rows: 3,
        };
        let got = v.best(&[0.0, 1.0], |_| true);
        assert_eq!(got[0].0, 0);
        assert!((got[0].1 - 1.0).abs() < 0.01, "{got:?}");
        assert_eq!(got[1].0, 1);
        assert!((got[1].1 - 0.8).abs() < 0.01, "{got:?}");
        assert!(v.best(&[0.0, 1.0], |p| p == 1).iter().all(|(p, _)| *p == 1));
        assert!(v.best(&[1.0], |_| true).is_empty(), "wrong length");
    }

    /// Bytes keep similarities within a hundredth of the floats'.
    #[test]
    fn quantized_similarities_stay_close() {
        let mut seed = 7u64;
        let mut unit = || {
            let mut v: Vec<f32> = (0..768)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    ((seed >> 33) as f32 / u32::MAX as f32) - 0.25
                })
                .collect();
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter_mut().for_each(|x| *x /= n);
            v
        };
        for _ in 0..50 {
            let (a, b) = (unit(), unit());
            let exact: f32 = a.iter().zip(&b).map(|(x, y)| x * y).sum();
            let ((sa, qa), (sb, qb)) = (quantize(&a), quantize(&b));
            let approx = Dot::new().of(&qa, &qb) as f32 * sa * sb;
            assert_eq!(Dot::new().of(&qa, &qb), dot(&qa, &qb));
            assert!((exact - approx).abs() < 0.01, "{exact} vs {approx}");
        }
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
        let keys: Vec<String> = units(&snap.catalog.pages[0])
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
            ("expertise/c", "## Kept\n\nAs is.\n"),
        ]);
        let built = build(&first.catalog, Earlier::Stored(Vec::new()), &model).unwrap();
        assert_eq!(built.put.len(), 7);
        let second = snap_of(&[
            ("expertise/a", "## One\n\nSame.\n\n## Two\n\nNew.\n"),
            ("expertise/c", "## Kept\n\nAs is.\n"),
        ]);
        model.0.lock().unwrap().clear();
        let earlier = Earlier::Snapshot(&built.vectors, &first.catalog);
        let again = build(&second.catalog, earlier, &model).unwrap();
        let seen = model.0.lock().unwrap().clone();
        assert_eq!(seen, vec!["T — Two\nNew.".to_string()]);
        assert_eq!(again.vectors.len(), 5);
        // The unchanged page keeps its vectors as they were.
        assert!(Arc::ptr_eq(
            &again.vectors.pages[1],
            &built.vectors.pages[2]
        ));
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

    /// Split over the cores or not, the scan finds the same pages with the same scores.
    #[test]
    fn a_large_set_is_scanned_on_all_cores_alike() {
        let pages: Vec<Arc<PageVectors>> = (0..60_000)
            .map(|i| {
                let a = (i as f32 * 0.37).sin();
                let (scale, data) = quantize(&[a, (1.0 - a * a).sqrt()]);
                Arc::new(PageVectors {
                    page_hash: String::new(),
                    units: vec![(HEAD.to_owned(), String::new(), scale)],
                    data,
                })
            })
            .collect();
        let split = Vectors {
            model: "m".into(),
            dim: 2,
            rows: pages.len(),
            pages: pages.clone(),
        };
        let one = Vectors {
            model: "m".into(),
            dim: 2,
            pages,
            rows: 0,
        };
        let keep = |ord: usize| !ord.is_multiple_of(3);
        let got = split.best(&[0.6, 0.8], keep);
        assert_eq!(got.len(), 40_000);
        assert_eq!(got, one.best(&[0.6, 0.8], keep));
    }
}
