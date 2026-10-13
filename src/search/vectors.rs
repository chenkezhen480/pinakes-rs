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
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;

use super::catalog::{Catalog, PageEntry, Slot};
use super::hnsw::{Graph, Rows};
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

/// A unit's key in the graph index: from its heading and its text's hash, so it is unique
/// within its page and changes with its text.
fn unit_key(heading: &str, hash: &str) -> u64 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(heading.as_bytes());
    h.update([0]);
    h.update(hash.as_bytes());
    u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
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
#[derive(Debug, Clone, Copy)]
pub(super) struct Dot {
    #[cfg(target_arch = "x86_64")]
    avx2: bool,
}

impl Dot {
    pub(super) fn new() -> Self {
        Self {
            #[cfg(target_arch = "x86_64")]
            avx2: std::arch::is_x86_feature_detected!("avx2"),
        }
    }

    #[inline]
    pub(super) fn of(&self, a: &[i8], b: &[i8]) -> i32 {
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
    /// Per row: a key from the unit's heading and hash, which the graph index stores.
    keys: Vec<u64>,
    /// Row-major, `dim` values per row.
    data: Vec<i8>,
}

impl PageVectors {
    fn row(&self, i: usize, dim: usize) -> &[i8] {
        &self.data[i * dim..(i + 1) * dim]
    }
}

/// The vectors of a snapshot's pages, one [`PageVectors`] per catalog entry, compared with a
/// query by brute force, or through a graph index when there are many ([`index`]).
#[derive(Debug, Default)]
pub struct Vectors {
    /// The model that made them; a query must be embedded by the same one.
    pub model: String,
    dim: usize,
    /// In catalog order: the vectors of page `ord`.
    pages: Vec<Arc<PageVectors>>,
    /// How many rows there are in all.
    rows: usize,
    /// The graph index over the rows, when the tenant has enough of them.
    graph: Option<Arc<Indexed>>,
}

/// The graph indexes of one snapshot's rows: one per search slot, so a slot's search never
/// walks past the other slot's pages.
#[derive(Debug)]
struct Indexed {
    /// Memories, then knowledge.
    parts: [Part; 2],
    /// Per page: where its rows start in a part's `row_slots`.
    page_start: Vec<u32>,
    /// A search uses a graph only when it may see at least this many rows of its slot.
    min_visible: usize,
    /// The audiences pages are for (`tenant`, group and user folders), and per page the
    /// index of its audience and its part, so a search filters pages without comparing text.
    audiences: Vec<String>,
    page_audience: Vec<u32>,
    page_part: Vec<u8>,
    /// Per audience: how many rows it has in each part.
    rows_of: Vec<[usize; 2]>,
}

/// The graph of one slot's rows.
#[derive(Debug, Clone, Default)]
struct Part {
    graph: Graph,
    /// Per graph slot: the page (catalog order) and unit it holds; [`NO_ROW`] for a free one.
    slot_rows: Vec<(u32, u16)>,
    /// Per row, in catalog order and then unit order: its graph slot (`u32::MAX` for rows of
    /// the other search slot).
    row_slots: Vec<u32>,
}

/// The part of a search slot.
fn part_of(slot: Slot) -> usize {
    match slot {
        Slot::Memories => 0,
        Slot::Knowledge => 1,
    }
}

const NO_ROW: (u32, u16) = (u32::MAX, 0);

/// The rows of one snapshot, as the graph reads them.
struct View<'a> {
    pages: &'a [Arc<PageVectors>],
    slot_rows: &'a [(u32, u16)],
    dim: usize,
}

impl Rows for View<'_> {
    fn row(&self, slot: u32) -> (&[i8], f32) {
        let (ord, unit) = self.slot_rows[slot as usize];
        let page = &self.pages[ord as usize];
        (
            page.row(unit as usize, self.dim),
            page.units[unit as usize].2,
        )
    }

    fn prefetch(&self, slot: u32) {
        #[cfg(target_arch = "x86_64")]
        {
            let (ord, unit) = self.slot_rows[slot as usize];
            let row = self.pages[ord as usize].row(unit as usize, self.dim);
            for at in (0..row.len()).step_by(64) {
                // SAFETY: a prefetch only hints the cache; `at` lies within the row.
                unsafe {
                    std::arch::x86_64::_mm_prefetch(
                        row.as_ptr().add(at).cast(),
                        std::arch::x86_64::_MM_HINT_T0,
                    )
                };
            }
        }
        #[cfg(not(target_arch = "x86_64"))]
        let _ = slot;
    }
}

/// From this many rows on, a search compares the query with them on the compute pool.
const PARALLEL_ROWS: usize = 50_000;
/// A search walks the graph only when the rows it may see are at least this many (or the
/// configured threshold, when that is lower): fewer are compared exactly, which is fast for so
/// few, and a walk that has to pass most rows by loses what it looks for.
const GRAPH_VISIBLE: usize = 50_000;
/// How many rows a graph search keeps while it walks, before it is widened for rows the caller
/// can't see.
const EF_SEARCH: usize = 100;
const MAX_EF: usize = 1024;

/// Pages with their best similarity, best first, and how they were found.
pub type Nearest = (Vec<(usize, f32)>, Method);

/// How a meaning search compared the query with the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// With every row the caller may see.
    Exact,
    /// Through the graph index.
    Graph,
}

impl Vectors {
    /// How many rows (units) there are.
    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the rows have a graph index.
    pub fn has_graph(&self) -> bool {
        self.graph.is_some()
    }

    /// The pages of a search slot `keep` accepts that are most similar to the query, best
    /// first, with their best similarity: at least `want` of them when there are, through the
    /// slot's graph index when there is one and the caller may see enough of its rows;
    /// otherwise every page `keep` accepts, compared exactly ([`Vectors::best`]). `keep` must
    /// accept only pages of the slot.
    pub fn nearest(
        &self,
        query: &[f32],
        slot: Slot,
        keep: impl Fn(usize) -> bool + Sync,
        want: usize,
    ) -> (Vec<(usize, f32)>, Method) {
        let Some(ix) = self.graph.as_ref().filter(|_| query.len() == self.dim) else {
            return (self.best(query, keep), Method::Exact);
        };
        let visible: usize = self
            .pages
            .iter()
            .enumerate()
            .filter(|(ord, _)| keep(*ord))
            .map(|(_, p)| p.units.len())
            .sum();
        self.search_part(ix, part_of(slot), query, keep, visible, want)
    }

    /// [`Vectors::nearest`] for both slots at once, side by side on the compute pool, as a
    /// caller who may see these audiences (`tenant`, group and user folders) searches: memories
    /// first, then knowledge. `None` when the rows have no graph index.
    pub fn nearest_both(
        &self,
        query: &[f32],
        visible: &[String],
        want: usize,
    ) -> Option<[Nearest; 2]> {
        let ix = self.graph.as_ref().filter(|_| query.len() == self.dim)?;
        let seen: Vec<bool> = ix.audiences.iter().map(|a| visible.contains(a)).collect();
        let run = |p: usize| {
            let rows: usize = ix
                .rows_of
                .iter()
                .zip(&seen)
                .filter(|(_, s)| **s)
                .map(|(r, _)| r[p])
                .sum();
            let keep = |ord: usize| {
                ix.page_part[ord] as usize == p && seen[ix.page_audience[ord] as usize]
            };
            self.search_part(ix, p, query, keep, rows, want)
        };
        let (memories, knowledge) = crate::par::run(|| rayon::join(|| run(0), || run(1)));
        Some([memories, knowledge])
    }

    /// One part's search, given how many of its rows `keep` accepts.
    fn search_part(
        &self,
        ix: &Indexed,
        p: usize,
        query: &[f32],
        keep: impl Fn(usize) -> bool + Sync,
        visible: usize,
        want: usize,
    ) -> (Vec<(usize, f32)>, Method) {
        let part = &ix.parts[p];
        if visible == 0 || visible < ix.min_visible || part.graph.is_empty() {
            return (self.best(query, keep), Method::Exact);
        }
        // Widened by the share of the slot's rows the caller can't see, so as many visible
        // ones are kept.
        let share = (visible as f32 / part.graph.len().max(1) as f32).min(1.0);
        let ef = ((EF_SEARCH.max(want) as f32 / share) as usize).min(MAX_EF);
        let (qscale, q) = quantize(query);
        let view = View {
            pages: &self.pages,
            slot_rows: &part.slot_rows,
            dim: self.dim,
        };
        let keep_slot = |s: u32| keep(part.slot_rows[s as usize].0 as usize);
        let found = part.graph.search(&view, (&q, qscale), ef, &keep_slot);
        let mut best: HashMap<usize, f32> = HashMap::new();
        for (sim, s) in found {
            let ord = part.slot_rows[s as usize].0 as usize;
            let e = best.entry(ord).or_insert(f32::MIN);
            *e = e.max(sim);
        }
        let mut out: Vec<(usize, f32)> = best.into_iter().collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        (out, Method::Graph)
    }

    /// One page's best similarity to the query, compared exactly.
    pub fn similarity(&self, query: &[f32], ord: usize) -> Option<f32> {
        let page = self.pages.get(ord)?;
        if self.dim == 0 || query.len() != self.dim || page.units.is_empty() {
            return None;
        }
        let (qscale, q) = quantize(query);
        let dot = Dot::new();
        Some(
            page.units
                .iter()
                .enumerate()
                .map(|(r, (_, _, scale))| dot.of(&q, page.row(r, self.dim)) as f32 * scale * qscale)
                .fold(f32::MIN, f32::max),
        )
    }

    /// Each page `keep` accepts with its best similarity to the query, best first (ties by
    /// page order). One pass over the vectors; a large set is split over the compute pool
    /// (`par`), since streaming it through one core is what costs (300 MB for 100,000 pages).
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
        let threads = crate::par::threads();
        let mut out = if self.rows < PARALLEL_ROWS || threads < 2 {
            scan(0, &self.pages)
        } else {
            use rayon::prelude::*;
            let per = self.pages.len().div_ceil(threads);
            crate::par::run(|| {
                self.pages
                    .par_chunks(per)
                    .enumerate()
                    .flat_map_iter(|(i, chunk)| scan(i * per, chunk))
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

/// The new vectors [`build_keeping`] hands over at a time: a few minutes of embedding a large
/// wiki on a laptop's CPU at most.
pub(crate) const KEEP_STEP: usize = 256;

/// The vectors of a catalog's pages: an unchanged page keeps its earlier ones whole; for the
/// others, earlier rows whose unit text is unchanged (same hash) are kept and the model embeds
/// the rest. Blocking; runs the model.
pub fn build(
    catalog: &Catalog,
    earlier: Earlier<'_>,
    embedder: &dyn Embedder,
) -> anyhow::Result<Built> {
    build_keeping(catalog, earlier, embedder, &mut |_| {})
}

/// [`build`], handing each step of new vectors to `keep` as soon as they are made (to store
/// them), so a build cut short doesn't lose them: the next one finds them stored. They are in
/// [`Built::put`] too.
pub fn build_keeping(
    catalog: &Catalog,
    earlier: Earlier<'_>,
    embedder: &dyn Embedder,
    keep: &mut dyn FnMut(&[StoredVector]),
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

    embedder.will_embed(missing.len());
    let mut put = Vec::with_capacity(missing.len());
    let mut made: HashMap<(usize, String), Row> = HashMap::new();
    for step in missing.chunks(KEEP_STEP) {
        let texts: Vec<String> = step.iter().map(|(_, u)| u.text.clone()).collect();
        let fresh = embedder.embed(&texts)?;
        anyhow::ensure!(
            fresh.len() == step.len(),
            "the embedding model returned {} vectors for {} texts",
            fresh.len(),
            step.len()
        );
        let from = put.len();
        for ((ord, u), v) in step.iter().zip(fresh) {
            anyhow::ensure!(
                v.len() == dim,
                "the embedding model returned a vector of another length"
            );
            made.insert((*ord, u.heading.clone()), quantize(&v));
            put.push(StoredVector {
                page: catalog.pages[*ord].path.to_string(),
                heading: u.heading.clone(),
                hash: u.hash.clone(),
                vector: v,
            });
        }
        keep(&put[from..]);
    }
    for (ord, rows) in partial {
        let mut pv = PageVectors {
            page_hash: catalog.pages[ord].hash.clone(),
            units: Vec::with_capacity(rows.len()),
            keys: Vec::with_capacity(rows.len()),
            data: Vec::with_capacity(rows.len() * dim),
        };
        for (u, row) in rows {
            let (scale, data) = match row {
                Some(r) => r,
                None => made
                    .remove(&(ord, u.heading.clone()))
                    .expect("every missing unit was embedded"),
            };
            pv.keys.push(unit_key(&u.heading, &u.hash));
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
            graph: None,
        },
        put,
        drop,
    })
}

/// What [`index`] changed in the graph.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GraphChange {
    /// Rows kept as they were.
    pub kept: usize,
    pub removed: usize,
    pub added: usize,
    /// Whether the graph started from its file.
    pub from_file: bool,
}

/// Gives the vectors graph indexes, one per search slot, when they have at least `min_rows`
/// rows (0: never), and keeps them in step with the pages: from the previous snapshot's graphs
/// (unchanged pages keep their rows' places), else from their files in `dir`, else built
/// whole; the rows of changed and removed pages are taken out and new ones put in. A file is
/// replaced when its graph changed, and the files are removed when there is no index.
/// Blocking; runs on the compute pool.
pub fn index(
    vectors: &mut Vectors,
    catalog: &Catalog,
    previous: Option<(&Vectors, &Catalog)>,
    dir: Option<&Path>,
    min_rows: usize,
) -> anyhow::Result<Option<GraphChange>> {
    vectors.graph = None;
    let files = |d: &Path| {
        [
            d.join("vectors-memories.hnsw"),
            d.join("vectors-knowledge.hnsw"),
        ]
    };
    if min_rows == 0 || vectors.rows < min_rows || vectors.dim == 0 {
        if let Some(d) = dir {
            for f in files(d) {
                match std::fs::remove_file(&f) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e).with_context(|| format!("removing {}", f.display())),
                }
            }
        }
        return Ok(None);
    }
    let mut page_start = Vec::with_capacity(vectors.pages.len());
    let mut at = 0u32;
    for p in &vectors.pages {
        page_start.push(at);
        at += p.units.len() as u32;
    }
    let earlier = previous.and_then(|(v, c)| {
        v.graph
            .as_ref()
            .filter(|_| v.dim == vectors.dim && v.model == vectors.model)
            .map(|ix| (v, c, ix.as_ref()))
    });
    let mut change = GraphChange::default();
    let mut parts: [Part; 2] = Default::default();
    for (i, slot) in [Slot::Memories, Slot::Knowledge].into_iter().enumerate() {
        let file = dir.map(|d| files(d)[i].clone());
        let before = earlier.map(|(v, c, ix)| (v, c, &ix.parts[i], ix.page_start.as_slice()));
        let (part, c) = index_part(vectors, catalog, slot, &page_start, before, file.as_deref());
        change.kept += c.kept;
        change.added += c.added;
        change.removed += c.removed;
        change.from_file |= c.from_file;
        parts[i] = part;
    }
    let mut audiences: Vec<String> = Vec::new();
    let mut page_audience = Vec::with_capacity(catalog.pages.len());
    let mut page_part = Vec::with_capacity(catalog.pages.len());
    let mut rows_of: Vec<[usize; 2]> = Vec::new();
    for (entry, pv) in catalog.pages.iter().zip(&vectors.pages) {
        let a = match audiences.iter().position(|a| *a == entry.audience) {
            Some(a) => a,
            None => {
                audiences.push(entry.audience.clone());
                rows_of.push([0, 0]);
                audiences.len() - 1
            }
        };
        let p = part_of(entry.slot);
        rows_of[a][p] += pv.units.len();
        page_audience.push(a as u32);
        page_part.push(p as u8);
    }
    vectors.graph = Some(Arc::new(Indexed {
        parts,
        page_start,
        min_visible: GRAPH_VISIBLE.min(min_rows),
        audiences,
        page_audience,
        page_part,
        rows_of,
    }));
    Ok(Some(change))
}

/// One search slot's graph, brought in step with its pages (see [`index`]).
fn index_part(
    vectors: &Vectors,
    catalog: &Catalog,
    slot: Slot,
    page_start: &[u32],
    earlier: Option<(&Vectors, &Catalog, &Part, &[u32])>,
    file: Option<&Path>,
) -> (Part, GraphChange) {
    let dim = vectors.dim;
    let mine = |c: &Catalog, ord: usize| c.pages[ord].slot == slot;
    let mut row_slots = vec![u32::MAX; vectors.rows];
    let mut gone: Vec<u32> = Vec::new();
    let mut add: Vec<(usize, usize)> = Vec::new();
    let mut change = GraphChange::default();
    let (mut graph, mut slot_rows) = match earlier {
        Some((pv, pc, part, pstart)) => {
            let graph = part.graph.clone();
            let mut slot_rows = vec![NO_ROW; graph.slots()];
            let range = |pord: usize| {
                let from = pstart[pord] as usize;
                from..from + pv.pages[pord].units.len()
            };
            let mut seen = vec![false; pv.pages.len()];
            for (ord, entry) in catalog.pages.iter().enumerate() {
                if !mine(catalog, ord) {
                    continue;
                }
                let page = &vectors.pages[ord];
                let start = page_start[ord] as usize;
                if let Some((pord, _)) = pc.get(entry.path.as_str()) {
                    seen[pord] = true;
                    if Arc::ptr_eq(page, &pv.pages[pord]) {
                        for (u, &s) in part.row_slots[range(pord)].iter().enumerate() {
                            slot_rows[s as usize] = (ord as u32, u as u16);
                            row_slots[start + u] = s;
                        }
                        continue;
                    }
                    gone.extend_from_slice(&part.row_slots[range(pord)]);
                }
                add.extend((0..page.units.len()).map(|u| (ord, u)));
            }
            for (pord, seen) in seen.into_iter().enumerate() {
                if !seen && mine(pc, pord) {
                    gone.extend_from_slice(&part.row_slots[range(pord)]);
                }
            }
            (graph, slot_rows)
        }
        None => {
            let loaded = match file {
                Some(f) => Graph::load(f, dim, &vectors.model).unwrap_or_else(|e| {
                    tracing::warn!(error = %format!("{e:#}"), "a vector graph's file can't be read; building it again");
                    None
                }),
                None => None,
            };
            change.from_file = loaded.is_some();
            let graph = loaded.unwrap_or_else(Graph::new);
            let mut slot_rows = vec![NO_ROW; graph.slots()];
            let mut by_page: HashMap<u64, HashMap<u64, u32>> = HashMap::new();
            for (s, (page, unit)) in graph.keys() {
                if let Some(twice) = by_page.entry(page).or_default().insert(unit, s) {
                    gone.push(twice);
                }
            }
            for (ord, entry) in catalog.pages.iter().enumerate() {
                if !mine(catalog, ord) {
                    continue;
                }
                let page = &vectors.pages[ord];
                let start = page_start[ord] as usize;
                let mut have = by_page.remove(&entry.key).unwrap_or_default();
                for (u, key) in page.keys.iter().enumerate() {
                    match have.remove(key) {
                        Some(s) => {
                            slot_rows[s as usize] = (ord as u32, u as u16);
                            row_slots[start + u] = s;
                        }
                        None => add.push((ord, u)),
                    }
                }
                gone.extend(have.into_values());
            }
            gone.extend(by_page.into_values().flat_map(|m| m.into_values()));
            (graph, slot_rows)
        }
    };
    change.removed = gone.len();
    change.added = add.len();
    graph.remove(
        &View {
            pages: &vectors.pages,
            slot_rows: &slot_rows,
            dim,
        },
        &gone,
    );
    let mut fresh = Vec::with_capacity(add.len());
    for (ord, u) in add {
        let s = graph.alloc((catalog.pages[ord].key, vectors.pages[ord].keys[u]));
        if s as usize >= slot_rows.len() {
            slot_rows.resize(s as usize + 1, NO_ROW);
        }
        slot_rows[s as usize] = (ord as u32, u as u16);
        row_slots[page_start[ord] as usize + u] = s;
        fresh.push(s);
    }
    crate::par::run(|| {
        graph.insert(
            &View {
                pages: &vectors.pages,
                slot_rows: &slot_rows,
                dim,
            },
            &fresh,
        )
    });
    change.kept = graph.len() - change.added;
    if let Some(f) = file
        && (change.removed > 0 || change.added > 0 || !f.exists())
        && let Err(e) = graph.save(f, dim, &vectors.model)
    {
        tracing::warn!(error = %format!("{e:#}"), "a vector graph could not be saved; it is built again when the tenant opens");
    }
    (
        Part {
            graph,
            slot_rows,
            row_slots,
        },
        change,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchSnapshot;
    use crate::search::hnsw::Key;
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
                keys: Vec::new(),
                data: Vec::new(),
            };
            for (i, r) in rows.iter().enumerate() {
                let (scale, data) = quantize(r);
                pv.keys.push(i as u64);
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
            graph: None,
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
                    keys: vec![0],
                    data,
                })
            })
            .collect();
        let split = Vectors {
            model: "m".into(),
            dim: 2,
            rows: pages.len(),
            graph: None,
            pages: pages.clone(),
        };
        let one = Vectors {
            model: "m".into(),
            dim: 2,
            pages,
            rows: 0,
            graph: None,
        };
        let keep = |ord: usize| !ord.is_multiple_of(3);
        let got = split.best(&[0.6, 0.8], keep);
        assert_eq!(got.len(), 40_000);
        assert_eq!(got, one.best(&[0.6, 0.8], keep));
    }

    /// A unit vector for a seed: points around a few centres, as real pages cluster by topic.
    fn clustered(seed: u64, dim: usize) -> Vec<f32> {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 40) as f32 / (1u64 << 24) as f32 - 0.5
        };
        let centre = seed % 20;
        let mut v: Vec<f32> = (0..dim)
            .map(|i| {
                let c = if (i as u64 * 7 + centre * 13) % 20 < 3 {
                    1.0
                } else {
                    0.0
                };
                c + next() * 0.8
            })
            .collect();
        let n = v.iter().map(|a| a * a).sum::<f32>().sqrt();
        v.iter_mut().for_each(|a| *a /= n);
        v
    }

    /// Pages `p<i>` with `units` rows each, vectors from `seed(i, unit)`.
    fn indexed_set(
        ids: &[usize],
        units: usize,
        dim: usize,
        seed: impl Fn(usize, usize) -> u64,
    ) -> (SearchSnapshot, Vectors) {
        let snap = snap_of(
            &ids.iter()
                .map(|i| (format!("expertise/p{i}"), String::new()))
                .collect::<Vec<_>>()
                .iter()
                .map(|(p, b)| (p.as_str(), b.as_str()))
                .collect::<Vec<_>>(),
        );
        let pages: Vec<Arc<PageVectors>> = snap
            .catalog
            .pages
            .iter()
            .map(|e| {
                let i: usize = e.path.as_str()["expertise/p".len()..].parse().unwrap();
                let mut pv = PageVectors {
                    page_hash: e.hash.clone(),
                    units: Vec::new(),
                    keys: Vec::new(),
                    data: Vec::new(),
                };
                for u in 0..units {
                    let s = seed(i, u);
                    let (scale, data) = quantize(&clustered(s, dim));
                    pv.units.push((format!("#{u}"), s.to_string(), scale));
                    pv.keys.push(unit_key(&format!("#{u}"), &s.to_string()));
                    pv.data.extend(data);
                }
                Arc::new(pv)
            })
            .collect();
        let rows = pages.len() * units;
        let v = Vectors {
            model: "m".into(),
            dim,
            pages,
            rows,
            graph: None,
        };
        (snap, v)
    }

    fn top(found: &[(usize, f32)], n: usize) -> HashSet<usize> {
        found.iter().take(n).map(|(o, _)| *o).collect()
    }

    /// Through the graph a search finds the pages the exact scan finds first.
    #[test]
    fn the_graph_finds_what_the_exact_scan_finds() {
        let ids: Vec<usize> = (0..3000).collect();
        let (snap, mut v) = indexed_set(&ids, 2, 64, |i, u| (i * 2 + u) as u64);
        let change = index(&mut v, &snap.catalog, None, None, 1)
            .unwrap()
            .unwrap();
        assert_eq!(change.added, 6000);
        let mut hits = 0;
        for q in 0..50u64 {
            let query = clustered(100_000 + q, 64);
            let (graph, method) = v.nearest(&query, Slot::Knowledge, |_| true, 10);
            assert_eq!(method, Method::Graph);
            let exact = v.best(&query, |_| true);
            hits += top(&graph, 10).intersection(&top(&exact, 10)).count();
            // Scores are the exact ones.
            let (ord, sim) = graph[0];
            assert!((v.similarity(&query, ord).unwrap() - sim).abs() < 1e-6);
        }
        let recall = hits as f32 / 500.0;
        assert!(recall >= 0.95, "recall@10 {recall}");
        // A filter is applied, and few visible rows are compared exactly.
        let (some, _) = v.nearest(&clustered(7, 64), Slot::Knowledge, |ord| ord % 3 == 0, 10);
        assert!(!some.is_empty() && some.iter().all(|(o, _)| o % 3 == 0));
        let (few, method) = v.nearest(&clustered(7, 64), Slot::Knowledge, |ord| ord == 5, 10);
        assert_eq!((few.len(), few[0].0), (1, 5));
        assert_eq!(method, Method::Graph, "the threshold is 1 here");
    }

    /// A write keeps unchanged pages' rows, takes out changed and removed ones, and the file
    /// follows: a removed page's rows are in neither.
    #[test]
    fn the_graph_follows_writes_and_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let index_dir = dir.path().join("index");
        let file = index_dir.join("vectors-knowledge.hnsw");
        let ids: Vec<usize> = (0..800).collect();
        let (snap, mut v) = indexed_set(&ids, 3, 32, |i, u| (i * 3 + u) as u64);
        index(&mut v, &snap.catalog, None, Some(&index_dir), 1).unwrap();
        assert!(file.exists());

        // Pages 0..100 removed, 100..200 changed, 900..950 new.
        let next_ids: Vec<usize> = (100..800).chain(900..950).collect();
        let (snap2, mut v2) = indexed_set(&next_ids, 3, 32, |i, u| {
            if (100..200).contains(&i) {
                50_000 + (i * 3 + u) as u64
            } else {
                (i * 3 + u) as u64
            }
        });
        // Unchanged pages share their vectors with the snapshot before, as `build` makes them.
        for (ord, e) in snap2.catalog.pages.iter().enumerate() {
            let i: usize = e.path.as_str()["expertise/p".len()..].parse().unwrap();
            if (200..800).contains(&i) {
                let (pord, _) = snap.catalog.get(e.path.as_str()).unwrap();
                v2.pages[ord] = v.pages[pord].clone();
            }
        }
        let change = index(
            &mut v2,
            &snap2.catalog,
            Some((&v, &snap.catalog)),
            Some(&index_dir),
            1,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (change.removed, change.added, change.kept),
            (600, 450, 1800)
        );
        let ix = &v2.graph.as_ref().unwrap().parts[1];
        assert_eq!(ix.graph.len(), v2.rows);
        let removed: HashSet<u64> = (0..100)
            .map(|i| snap.catalog.get(&format!("expertise/p{i}")).unwrap().1.key)
            .collect();
        assert!(
            ix.graph
                .keys()
                .all(|(_, (page, _))| !removed.contains(&page))
        );
        let saved = Graph::load(&file, 32, "m").unwrap().unwrap();
        let mut a: Vec<Key> = saved.keys().map(|(_, k)| k).collect();
        let mut b: Vec<Key> = ix.graph.keys().map(|(_, k)| k).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
        // Every row is still found: each one's own vector comes back first.
        for ord in (0..v2.pages.len()).step_by(37) {
            let q = clustered(v2.pages[ord].units[0].1.parse().unwrap(), 32);
            let (found, _) = v2.nearest(&q, Slot::Knowledge, |_| true, 5);
            assert!(top(&found, 3).contains(&ord), "page {ord}");
        }

        // Opened again from the file, with one more page changed while the service was down.
        let (snap3, mut v3) = indexed_set(&next_ids, 3, 32, |i, u| {
            if i == 300 {
                90_000 + u as u64
            } else if (100..200).contains(&i) {
                50_000 + (i * 3 + u) as u64
            } else {
                (i * 3 + u) as u64
            }
        });
        let change = index(&mut v3, &snap3.catalog, None, Some(&index_dir), 1)
            .unwrap()
            .unwrap();
        assert!(change.from_file);
        assert_eq!((change.removed, change.added), (3, 3));
        assert_eq!(v3.graph.as_ref().unwrap().parts[1].graph.len(), v3.rows);

        // Below the threshold there is no graph and no file.
        index(&mut v3, &snap3.catalog, None, Some(&index_dir), 1_000_000).unwrap();
        assert!(!v3.has_graph() && !file.exists());
    }
}
