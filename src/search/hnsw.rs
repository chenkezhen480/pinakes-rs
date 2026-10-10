//! A graph index over page vectors (HNSW: Malkov and Yashunin, "Efficient and robust
//! approximate nearest neighbor search using Hierarchical Navigable Small World graphs") for
//! tenants too large to compare a query with every row (design, "Plan for the HNSW index").
//!
//! The graph holds only neighbour lists. A row's values are read from the snapshot's vectors
//! ([`Rows`]) and compared with the same int8 dot product as the exact scan, so the vectors are
//! never copied. Each row has a slot; a removed row's slot is freed and the lists that named it
//! are repaired from its own neighbours, so a deleted page leaves nothing in the graph. On disk
//! the graph is the lists and two 64-bit keys per row (hashes of its page and unit): no text
//! and no vectors.
//!
//! Points are inserted in batches: each point of a batch searches for its neighbours in
//! parallel on the compute pool against the graph as it was before the batch, then all are
//! linked. A batch is at most a sixteenth of the graph, so it misses little of it.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use anyhow::Context;

use super::vectors::Dot;

/// Neighbours a point keeps on the upper layers; twice as many on layer 0.
const M: usize = 16;
const M0: usize = 2 * M;
/// How many candidates an insertion considers per layer.
const EF_CONSTRUCTION: usize = 200;
const MAX_LEVEL: u8 = 15;
/// The level of a free slot.
const FREE: u8 = u8::MAX;
const NONE: u32 = u32::MAX;
/// The largest batch inserted against one frozen graph.
const MAX_BATCH: usize = 8192;
const MAGIC: &[u8; 8] = b"PNXHNSW1";

/// Where the graph reads a slot's row.
pub trait Rows: Sync {
    /// The row of a slot in use: its bytes and scale.
    fn row(&self, slot: u32) -> (&[i8], f32);

    /// Asks the CPU to start loading a row that is about to be compared.
    fn prefetch(&self, _slot: u32) {}
}

/// A row's place: its page's key and its unit's key.
pub type Key = (u64, u64);

/// A similarity and a slot, ordered by similarity (then slot, so ties are stable).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Near(f32, u32);

impl Eq for Near {}

impl PartialOrd for Near {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Near {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0).then(other.1.cmp(&self.1))
    }
}

/// The graph.
#[derive(Debug, Clone, Default)]
pub struct Graph {
    /// Per slot: its row's key, and its level ([`FREE`] for a free slot).
    keys: Vec<Key>,
    levels: Vec<u8>,
    /// Layer 0's lists, [`M0`] places per slot, and how many of them are used.
    layer0: Vec<u32>,
    counts0: Vec<u8>,
    /// Per slot, its lists on layers 1 and up.
    upper: Vec<Vec<Vec<u32>>>,
    entry: u32,
    top: u8,
    free: Vec<u32>,
    live: usize,
}

/// The level of a new point, from its key: the same row gets the same level whenever the graph
/// is built, and levels fall off by a factor of [`M`].
fn level_of(key: Key) -> u8 {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(key.0.to_le_bytes());
    h.update(key.1.to_le_bytes());
    let bits = u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"));
    let u = ((bits >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
    let level = (-u.ln() / (M as f64).ln()).floor();
    level.min(f64::from(MAX_LEVEL)) as u8
}

fn max_neighbours(layer: usize) -> usize {
    if layer == 0 { M0 } else { M }
}

/// Which slots a search has seen: a mark per slot, reset by moving to the next epoch.
#[derive(Default)]
struct Visited {
    marks: Vec<u32>,
    epoch: u32,
}

impl Visited {
    fn start(&mut self, slots: usize) {
        if self.marks.len() < slots {
            self.marks.resize(slots, 0);
        }
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.fill(0);
            self.epoch = 1;
        }
    }

    /// Marks a slot; says whether it was new.
    fn insert(&mut self, slot: u32) -> bool {
        let m = &mut self.marks[slot as usize];
        if *m == self.epoch {
            false
        } else {
            *m = self.epoch;
            true
        }
    }
}

thread_local! {
    static VISITED: RefCell<Visited> = RefCell::new(Visited::default());
}

fn similarity(dot: &Dot, a: (&[i8], f32), b: (&[i8], f32)) -> f32 {
    dot.of(a.0, b.0) as f32 * a.1 * b.1
}

impl Graph {
    pub fn new() -> Self {
        Self {
            entry: NONE,
            ..Self::default()
        }
    }

    /// How many rows the graph holds.
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Every slot, used or free.
    pub fn slots(&self) -> usize {
        self.levels.len()
    }

    /// The key of every slot in use, with its slot.
    pub fn keys(&self) -> impl Iterator<Item = (u32, Key)> + '_ {
        self.keys
            .iter()
            .zip(&self.levels)
            .enumerate()
            .filter(|(_, (_, l))| **l != FREE)
            .map(|(i, (k, _))| (i as u32, *k))
    }

    fn neighbours(&self, slot: u32, layer: usize) -> &[u32] {
        let s = slot as usize;
        if layer == 0 {
            &self.layer0[s * M0..s * M0 + self.counts0[s] as usize]
        } else {
            &self.upper[s][layer - 1]
        }
    }

    fn set_neighbours(&mut self, slot: u32, layer: usize, list: &[u32]) {
        let s = slot as usize;
        if layer == 0 {
            let n = list.len().min(M0);
            self.layer0[s * M0..s * M0 + n].copy_from_slice(&list[..n]);
            self.counts0[s] = n as u8;
        } else {
            self.upper[s][layer - 1] = list.to_vec();
        }
    }

    /// A slot for a new row, not linked yet: its row must be readable through [`Rows`] before
    /// [`Graph::insert`] links it.
    pub fn alloc(&mut self, key: Key) -> u32 {
        let level = level_of(key);
        let slot = match self.free.pop() {
            Some(s) => s,
            None => {
                let s = self.levels.len() as u32;
                self.keys.push(key);
                self.levels.push(FREE);
                self.layer0.extend(std::iter::repeat_n(NONE, M0));
                self.counts0.push(0);
                self.upper.push(Vec::new());
                s
            }
        };
        let s = slot as usize;
        self.keys[s] = key;
        self.levels[s] = level;
        self.counts0[s] = 0;
        self.upper[s] = vec![Vec::new(); level as usize];
        slot
    }

    /// Links slots made by [`Graph::alloc`] into the graph, in batches.
    pub fn insert(&mut self, rows: &dyn Rows, slots: &[u32]) {
        let mut rest = slots;
        while !rest.is_empty() {
            if self.entry == NONE {
                self.entry = rest[0];
                self.top = self.levels[rest[0] as usize];
                self.live += 1;
                rest = &rest[1..];
                continue;
            }
            let n = (self.live / 16).clamp(1, MAX_BATCH).min(rest.len());
            self.insert_batch(rows, &rest[..n], false);
            rest = &rest[n..];
        }
    }

    /// Links a batch of points, each searched for against the graph as it was before the
    /// batch. With `relink` the points are in the graph already and get their lists anew.
    fn insert_batch(&mut self, rows: &dyn Rows, batch: &[u32], relink: bool) {
        let found: Vec<Vec<Vec<u32>>> = {
            let frozen = &*self;
            crate::par::map(batch.to_vec(), |s| {
                let level = frozen.levels[s as usize];
                frozen.connect(rows, rows.row(s), level, &|m| m == s)
            })
        };
        // Lists that grow past their size while linking, with the links waiting to join them.
        let mut over: HashMap<(u32, usize), Vec<u32>> = HashMap::new();
        for (&s, lists) in batch.iter().zip(found) {
            for (layer, list) in lists.iter().enumerate() {
                self.set_neighbours(s, layer, list);
                for &n in list {
                    if self.neighbours(n, layer).contains(&s) {
                        continue;
                    }
                    if let Some(extra) = over.get_mut(&(n, layer)) {
                        extra.push(s);
                        continue;
                    }
                    if self.neighbours(n, layer).len() < max_neighbours(layer) {
                        self.push_link(n, layer, s);
                    } else {
                        over.insert((n, layer), vec![s]);
                    }
                }
            }
            if relink {
                continue;
            }
            self.live += 1;
            if self.levels[s as usize] > self.top {
                self.top = self.levels[s as usize];
                self.entry = s;
            }
        }
        let over: Vec<((u32, usize), Vec<u32>)> = over.into_iter().collect();
        let shrunk: Vec<(u32, usize, Vec<u32>)> = {
            let graph = &*self;
            crate::par::map(over, |((n, layer), extra)| {
                let mut cands: Vec<u32> = graph.neighbours(n, layer).to_vec();
                cands.extend(extra);
                (n, layer, graph.select(rows, n, cands, layer))
            })
        };
        for (n, layer, list) in shrunk {
            self.set_neighbours(n, layer, &list);
        }
    }

    fn push_link(&mut self, slot: u32, layer: usize, to: u32) {
        let s = slot as usize;
        if layer == 0 {
            let c = self.counts0[s] as usize;
            self.layer0[s * M0 + c] = to;
            self.counts0[s] += 1;
        } else {
            self.upper[s][layer - 1].push(to);
        }
    }

    /// The best of `cands` as `slot`'s neighbours on a layer, by the heuristic of the paper: a
    /// candidate closer to one already kept than to `slot` is left out, so the lists reach in
    /// every direction.
    fn select(&self, rows: &dyn Rows, slot: u32, mut cands: Vec<u32>, layer: usize) -> Vec<u32> {
        let dot = Dot::new();
        let base = rows.row(slot);
        cands.sort_unstable();
        cands.dedup();
        let mut scored: Vec<Near> = cands
            .into_iter()
            .filter(|&c| c != slot)
            .map(|c| Near(similarity(&dot, base, rows.row(c)), c))
            .collect();
        scored.sort_unstable_by(|a, b| b.cmp(a));
        heuristic(&dot, rows, &scored, max_neighbours(layer))
    }

    /// The lists a point at `level` gets in the graph as it is: per layer from 0, its
    /// neighbours. Slots `skip` names are passed through but never chosen.
    fn connect(
        &self,
        rows: &dyn Rows,
        q: (&[i8], f32),
        level: u8,
        skip: &dyn Fn(u32) -> bool,
    ) -> Vec<Vec<u32>> {
        if self.entry == NONE {
            return Vec::new();
        }
        let dot = Dot::new();
        let mut ep = Near(similarity(&dot, q, rows.row(self.entry)), self.entry);
        for layer in (level as usize + 1..=self.top as usize).rev() {
            ep = self.greedy(rows, &dot, q, ep, layer);
        }
        let low = level.min(self.top) as usize;
        let mut out = vec![Vec::new(); low + 1];
        let mut eps = vec![ep];
        for layer in (0..=low).rev() {
            let found = self.search_layer(rows, &dot, q, &eps, EF_CONSTRUCTION, layer, &|_| true);
            let kept: Vec<Near> = found.iter().copied().filter(|n| !skip(n.1)).collect();
            out[layer] = heuristic(&dot, rows, &kept, max_neighbours(layer));
            if !found.is_empty() {
                eps = found;
            }
        }
        out
    }

    /// Moves to closer neighbours on one layer until none is closer.
    fn greedy(
        &self,
        rows: &dyn Rows,
        dot: &Dot,
        q: (&[i8], f32),
        mut ep: Near,
        layer: usize,
    ) -> Near {
        loop {
            let mut moved = false;
            for &n in self.neighbours(ep.1, layer) {
                let s = similarity(dot, q, rows.row(n));
                if s > ep.0 {
                    ep = Near(s, n);
                    moved = true;
                }
            }
            if !moved {
                return ep;
            }
        }
    }

    /// The `ef` closest slots `keep` accepts on one layer, best first, from the entry points.
    /// Slots it doesn't accept are walked through but not returned.
    #[allow(clippy::too_many_arguments)]
    fn search_layer(
        &self,
        rows: &dyn Rows,
        dot: &Dot,
        q: (&[i8], f32),
        eps: &[Near],
        ef: usize,
        layer: usize,
        keep: &dyn Fn(u32) -> bool,
    ) -> Vec<Near> {
        VISITED.with_borrow_mut(|visited| {
            visited.start(self.slots());
            let mut cands: BinaryHeap<Near> = BinaryHeap::new();
            let mut found: BinaryHeap<std::cmp::Reverse<Near>> = BinaryHeap::new();
            for &ep in eps {
                if visited.insert(ep.1) {
                    cands.push(ep);
                    if keep(ep.1) {
                        found.push(std::cmp::Reverse(ep));
                    }
                }
            }
            while found.len() > ef {
                found.pop();
            }
            let mut fresh: Vec<u32> = Vec::with_capacity(M0);
            while let Some(c) = cands.pop() {
                if found.len() >= ef && found.peek().is_some_and(|w| c.0 < w.0.0) {
                    break;
                }
                // The rows not seen yet are loaded together, before any is compared: walking
                // the graph waits on memory more than it computes.
                fresh.clear();
                for &n in self.neighbours(c.1, layer) {
                    if visited.insert(n) {
                        rows.prefetch(n);
                        fresh.push(n);
                    }
                }
                for &n in &fresh {
                    let s = similarity(dot, q, rows.row(n));
                    if found.len() < ef || found.peek().is_some_and(|w| s > w.0.0) {
                        cands.push(Near(s, n));
                        if keep(n) {
                            found.push(std::cmp::Reverse(Near(s, n)));
                            if found.len() > ef {
                                found.pop();
                            }
                        }
                    }
                }
            }
            let mut out: Vec<Near> = found.into_iter().map(|r| r.0).collect();
            out.sort_unstable_by(|a, b| b.cmp(a));
            out
        })
    }

    /// The rows closest to a query that `keep` accepts, best first: at most `ef`, as
    /// (similarity, slot).
    pub fn search(
        &self,
        rows: &dyn Rows,
        q: (&[i8], f32),
        ef: usize,
        keep: &dyn Fn(u32) -> bool,
    ) -> Vec<(f32, u32)> {
        if self.entry == NONE {
            return Vec::new();
        }
        let dot = Dot::new();
        let mut ep = Near(similarity(&dot, q, rows.row(self.entry)), self.entry);
        for layer in (1..=self.top as usize).rev() {
            ep = self.greedy(rows, &dot, q, ep, layer);
        }
        self.search_layer(rows, &dot, q, &[ep], ef, 0, keep)
            .into_iter()
            .map(|n| (n.0, n.1))
            .collect()
    }

    /// Takes rows out of the graph: every list that named one is chosen again from what is
    /// left of it and the removed rows' own neighbours, and their slots are freed. A point left
    /// with no neighbours on a layer is linked again through the graph. `rows` needs to read
    /// only the slots that stay.
    pub fn remove(&mut self, rows: &dyn Rows, gone: &[u32]) {
        if gone.is_empty() {
            return;
        }
        let mut dead = vec![false; self.slots()];
        for &g in gone {
            dead[g as usize] = self.levels[g as usize] != FREE;
        }
        let affected: Vec<(u32, usize)> = {
            let graph = &*self;
            let dead = &dead;
            let slots: Vec<u32> = (0..graph.slots() as u32)
                .filter(|&s| graph.levels[s as usize] != FREE && !dead[s as usize])
                .collect();
            crate::par::map(slots, |s| {
                (0..=graph.levels[s as usize] as usize)
                    .filter(|&layer| graph.neighbours(s, layer).iter().any(|&n| dead[n as usize]))
                    .map(|layer| (s, layer))
                    .collect::<Vec<_>>()
            })
            .into_iter()
            .flatten()
            .collect()
        };
        let repaired: Vec<(u32, usize, Vec<u32>)> = {
            let graph = &*self;
            let dead = &dead;
            crate::par::map(affected, |(s, layer)| {
                let mut cands = Vec::new();
                for &n in graph.neighbours(s, layer) {
                    if !dead[n as usize] {
                        cands.push(n);
                    } else if graph.levels[n as usize] as usize >= layer {
                        cands.extend(
                            graph
                                .neighbours(n, layer)
                                .iter()
                                .copied()
                                .filter(|&m| !dead[m as usize] && m != s),
                        );
                    }
                }
                (s, layer, graph.select(rows, s, cands, layer))
            })
        };
        // Points whose every neighbour on a layer went.
        let mut orphans: Vec<u32> = Vec::new();
        for (s, layer, list) in repaired {
            if list.is_empty() {
                orphans.push(s);
            }
            self.set_neighbours(s, layer, &list);
        }
        for (s, is_dead) in dead.iter().enumerate() {
            if *is_dead {
                self.keys[s] = (0, 0);
                self.levels[s] = FREE;
                self.counts0[s] = 0;
                self.upper[s] = Vec::new();
                self.free.push(s as u32);
                self.live -= 1;
            }
        }
        if self.entry != NONE && dead[self.entry as usize] {
            let best = (0..self.slots())
                .filter(|&s| self.levels[s] != FREE)
                .max_by_key(|&s| (self.levels[s], std::cmp::Reverse(s)));
            match best {
                Some(s) => {
                    self.entry = s as u32;
                    self.top = self.levels[s];
                }
                None => {
                    self.entry = NONE;
                    self.top = 0;
                }
            }
        }
        orphans.sort_unstable();
        orphans.dedup();
        if self.live > 1 && !orphans.is_empty() {
            self.insert_batch(rows, &orphans, true);
        }
    }

    /// Writes the graph to `path`, replacing the file whole (through a temporary file, renamed
    /// over it).
    pub fn save(&self, path: &Path, dim: usize, model: &str) -> anyhow::Result<()> {
        let tmp = path.with_extension("hnsw.tmp");
        // Not `create_dir_all`: a tenant deleted meanwhile must not get its folder back.
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            std::fs::create_dir(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        {
            let file = std::fs::File::create(&tmp)
                .with_context(|| format!("creating {}", tmp.display()))?;
            let mut w = BufWriter::new(file);
            w.write_all(MAGIC)?;
            w.write_all(&(dim as u32).to_le_bytes())?;
            w.write_all(&model_hash(model).to_le_bytes())?;
            w.write_all(&(self.slots() as u32).to_le_bytes())?;
            w.write_all(&self.entry.to_le_bytes())?;
            w.write_all(&[self.top])?;
            for s in 0..self.slots() {
                let level = self.levels[s];
                w.write_all(&[level])?;
                if level == FREE {
                    continue;
                }
                w.write_all(&self.keys[s].0.to_le_bytes())?;
                w.write_all(&self.keys[s].1.to_le_bytes())?;
                for layer in 0..=level as usize {
                    let list = self.neighbours(s as u32, layer);
                    w.write_all(&[list.len() as u8])?;
                    for n in list {
                        w.write_all(&n.to_le_bytes())?;
                    }
                }
            }
            // Not synced to disk: the file is a cache of the graph. One a crash left damaged
            // fails to load and the graph is built again; one left behind the rows is brought
            // in step by their keys when the tenant opens. Syncing cost 0.6 to 1.2 s a write
            // at 150,000 rows on the development laptop, against 50 ms for the rest.
            w.flush()?;
        }
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
        Ok(())
    }

    /// The graph saved at `path` for vectors of this length and model; `None` when there is no
    /// file, or it was written for others. A damaged file is an error.
    pub fn load(path: &Path, dim: usize, model: &str) -> anyhow::Result<Option<Graph>> {
        let file = match std::fs::File::open(path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
        };
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut r = BufReader::new(file);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC
            || read_u32(&mut r)? as usize != dim
            || read_u64(&mut r)? != model_hash(model)
        {
            return Ok(None);
        }
        let slots = read_u32(&mut r)? as usize;
        // Every slot takes a byte at least: a damaged count can't ask for more memory than
        // the file could describe.
        anyhow::ensure!(slots as u64 <= size, "more slots than the file holds");
        let entry = read_u32(&mut r)?;
        let top = read_u8(&mut r)?;
        let mut g = Graph {
            keys: vec![(0, 0); slots],
            levels: vec![FREE; slots],
            layer0: vec![NONE; slots * M0],
            counts0: vec![0; slots],
            upper: vec![Vec::new(); slots],
            entry,
            top,
            free: Vec::new(),
            live: 0,
        };
        for s in 0..slots {
            let level = read_u8(&mut r)?;
            if level == FREE {
                g.free.push(s as u32);
                continue;
            }
            anyhow::ensure!(level <= MAX_LEVEL, "a level out of range");
            g.levels[s] = level;
            g.keys[s] = (read_u64(&mut r)?, read_u64(&mut r)?);
            g.upper[s] = vec![Vec::new(); level as usize];
            for layer in 0..=level as usize {
                let n = read_u8(&mut r)? as usize;
                anyhow::ensure!(n <= max_neighbours(layer), "a list too long");
                let mut list = Vec::with_capacity(n);
                for _ in 0..n {
                    let to = read_u32(&mut r)?;
                    anyhow::ensure!((to as usize) < slots, "a neighbour out of range");
                    list.push(to);
                }
                g.set_neighbours(s as u32, layer, &list);
            }
            g.live += 1;
        }
        // Lists name only slots in use, and the entry is one of them.
        for s in 0..slots {
            if g.levels[s] == FREE {
                continue;
            }
            for layer in 0..=g.levels[s] as usize {
                for &n in g.neighbours(s as u32, layer) {
                    anyhow::ensure!(
                        g.levels[n as usize] != FREE && g.levels[n as usize] as usize >= layer,
                        "a list names a free slot"
                    );
                }
            }
        }
        match g.live {
            0 => {
                g.entry = NONE;
                g.top = 0;
            }
            _ => anyhow::ensure!(
                (entry as usize) < slots && g.levels[entry as usize] == top,
                "a bad entry point"
            ),
        }
        Ok(Some(g))
    }
}

/// The paper's heuristic over candidates sorted best first.
/// The candidates it passes over fill the list up to `m` afterwards, closest first: among
/// near-duplicates (pages made of the same lines) the heuristic alone keeps so few neighbours
/// that parts of the graph are hard to reach.
fn heuristic(dot: &Dot, rows: &dyn Rows, sorted: &[Near], m: usize) -> Vec<u32> {
    let mut kept: Vec<(u32, (&[i8], f32))> = Vec::with_capacity(m);
    let mut passed: Vec<u32> = Vec::new();
    for c in sorted {
        if kept.len() >= m {
            break;
        }
        let row = rows.row(c.1);
        if kept.iter().all(|(_, k)| similarity(dot, row, *k) <= c.0) {
            kept.push((c.1, row));
        } else {
            passed.push(c.1);
        }
    }
    let mut out: Vec<u32> = kept.into_iter().map(|(s, _)| s).collect();
    out.extend(passed.into_iter().take(m.saturating_sub(out.len())));
    out
}

fn model_hash(model: &str) -> u64 {
    use sha2::{Digest, Sha256};
    u64::from_le_bytes(
        Sha256::digest(model.as_bytes())[..8]
            .try_into()
            .expect("8 bytes"),
    )
}

fn read_u8(r: &mut impl Read) -> std::io::Result<u8> {
    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_u32(r: &mut impl Read) -> std::io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64(r: &mut impl Read) -> std::io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
