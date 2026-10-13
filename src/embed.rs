//! Local text embeddings, for meaning-based search (design, "Search signals" → "Meaning, when
//! the agent asks for it").
//!
//! Only wiki content is embedded, at write time: each page's head (title, aliases, keywords,
//! summary) and each of its sections. The vectors are kept in the tenant's SQLite file, keyed by
//! page, heading and model, with a hash of the text they were built from, so a hand edit is
//! caught and re-embedded. A search embeds only its query. The model runs inside the process
//! (fastembed on ONNX Runtime): jina-embeddings-v2-base-zh, a Chinese–English model, chosen on
//! the test set (design, "As built in M5"). It takes queries and stored text as they are, with
//! no prefixes. It runs with 8-bit weights, quantized by the service after the download
//! ([`quantize`]): a query embeds in half the time, with the test set's answers unchanged
//! (design, "As built in M10" → "An 8-bit model").

mod quantize;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use sha2::Digest;

/// The one model the service runs, as `[embedding] model` names it.
pub const MODEL: &str = "jina-embeddings-v2-base-zh";
/// Where its files come from: the Hugging Face repository, at a fixed revision.
const REPO: &str = "jinaai/jina-embeddings-v2-base-zh";
const REVISION: &str = "c1ff9086a89a1123d7b5eff58055a665db4fb4b9";
/// The name stored with every vector and recorded in search fingerprints: the model, its
/// revision and its 8-bit weights, so vectors from other weights are never compared with a
/// query. Change it with `REVISION` or the quantization.
const MODEL_ID: &str = "jina-embeddings-v2-base-zh@c1ff9086-q8";
/// The quantized weights, kept next to the downloaded files, and their SHA-256: quantizing is
/// deterministic, so anything else is a damaged file (made again) or a changed quantizer (an
/// error until this hash, and `MODEL_ID`, are changed with it).
const QUANTIZED_FILE: &str = "pinakes/model-c1ff9086-q8.onnx";
const QUANTIZED_SHA256: &str = "00e0bfa30d2506170bdbc95adee34b0da3163ae92d65c0119409a39a70ae5ee1";
/// Tokens of one text the model reads: all it can take (8,192), so a section is never cut
/// short of the model's limit. Pages are the librarian's condensed account, so sections that
/// long are rare; one costs a few seconds and some memory to embed, at write time only.
const MAX_TOKENS: usize = 8192;
/// Values per vector.
const DIM: usize = 768;
/// The files it is made of, in the order they are read.
const FILES: [&str; 5] = [
    "tokenizer.json",
    "config.json",
    "special_tokens_map.json",
    "tokenizer_config.json",
    "onnx/model.onnx",
];

/// Whether `[embedding] model` names the model the service can load.
pub fn known_model(name: &str) -> bool {
    name == MODEL
}

/// Turns texts into unit-length vectors.
pub trait Embedder: Send + Sync {
    /// The model's name, stored next to every vector it made.
    fn model(&self) -> &str;

    /// How many values each of its vectors has.
    fn dim(&self) -> usize;

    /// One unit-length vector per text.
    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Whether it can embed now; a model turned off or still loading can't (see [`Switch`]).
    fn ready(&self) -> bool {
        true
    }

    /// Told how many texts are about to be embedded, for progress reports.
    fn will_embed(&self, _count: usize) {}
}

/// The cosine similarity of two unit-length vectors.
pub fn similarity(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

/// What the embedding model is doing, as the service reports it (`GET /admin/embedding`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[cfg_attr(test, derive(ts_rs::TS), ts(rename = "EmbeddingState"))]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Turned off: no model in memory, no vectors computed; keyword search answers everything.
    Off,
    /// Its files are being downloaded (the first time only).
    Downloading {
        file: String,
        done_bytes: u64,
        total_bytes: u64,
        /// Files already in place, of all it needs.
        files_done: usize,
        files: usize,
    },
    /// Its files are on disk and being loaded into memory.
    Loading,
    /// Loaded; the open tenants' pages are being embedded (what isn't stored yet).
    Embedding {
        /// The tenant being worked on (from 1), of the tenants open.
        tenant: usize,
        tenants: usize,
        /// Texts (page heads and sections) embedded, of those the tenant lacks.
        done: usize,
        total: usize,
    },
    /// Loaded, and every open tenant's pages have their vectors.
    On,
    /// Turned on, but it couldn't be loaded.
    Failed { error: String },
}

/// Loads a model, reporting each step to `progress`, which answers whether to go on (`false`
/// once the model was turned off meanwhile). Slow and blocking.
pub type Loader =
    Arc<dyn Fn(&dyn Fn(Status) -> bool) -> anyhow::Result<Arc<dyn Embedder>> + Send + Sync>;

/// The model as a switch the admin turns on and off while the service runs (design, "M10":
/// vectors off by default). Every part of the service holds this one switch and asks it per
/// use whether it is [`Embedder::ready`]; while it is off or still loading it embeds nothing:
/// snapshots are built without vectors and the meaning-based modes answer 503. Turning it on
/// downloads (the first time) and loads the model in the background, then the open tenants'
/// pages get their vectors (`AppState::set_embedding`); turning it off drops the model from
/// memory and keeps the stored vectors, so turning it on again embeds only what changed.
pub struct Switch {
    model_id: String,
    dim: usize,
    loader: Loader,
    inner: Mutex<SwitchInner>,
}

struct SwitchInner {
    /// Whether it is turned on (it may still be loading).
    want: bool,
    /// Whether a loader is at work.
    running: bool,
    /// How many times it was turned off, so a loader can tell that a failure was a turn-off
    /// it no longer stands for (turned on again meanwhile).
    offs: u64,
    /// The model's files are being deleted: a turn-on waits for that, then loads.
    deleting: bool,
    status: Status,
    model: Option<Arc<dyn Embedder>>,
}

/// Texts embedded between two progress updates while the open tenants catch up.
const PROGRESS_STEP: usize = 8;

impl Switch {
    /// A switch, off, for a model named `model_id` (as stored with its vectors) with vectors
    /// of `dim` values, loaded by `loader`.
    pub fn new(model_id: impl Into<String>, dim: usize, loader: Loader) -> Self {
        Self {
            model_id: model_id.into(),
            dim,
            loader,
            inner: Mutex::new(SwitchInner {
                want: false,
                running: false,
                offs: 0,
                deleting: false,
                status: Status::Off,
                model: None,
            }),
        }
    }

    /// The service's model, loaded from (and downloaded into) `cache_dir`.
    pub fn fastembed(cache_dir: PathBuf) -> Self {
        let loader: Loader = Arc::new(move |progress| {
            Ok(Arc::new(FastEmbedder::load_with(&cache_dir, progress)?) as Arc<dyn Embedder>)
        });
        Self::new(MODEL_ID, DIM, loader)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SwitchInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn status(&self) -> Status {
        self.lock().status.clone()
    }

    /// Whether it is turned on, loaded or not.
    pub fn is_on(&self) -> bool {
        self.lock().want
    }

    /// Whether a loader is still at work (it may be finishing a download after being turned
    /// off).
    pub fn is_busy(&self) -> bool {
        self.lock().running
    }

    /// Turns it on. `true` when the caller must now run [`Switch::load`] (off the async
    /// threads) and then catch the tenants up; `false` when it is already on or loading.
    pub fn turn_on(&self) -> bool {
        let mut inner = self.lock();
        inner.want = true;
        if inner.deleting {
            // Loads once the files are gone (see `while_off`).
            inner.status = Status::Loading;
            return false;
        }
        if inner.model.is_some() {
            return false;
        }
        if inner.running {
            // A loader still at work (turned off while it ran) picks this up when it ends:
            // say it is loading meanwhile, not off.
            inner.status = Status::Loading;
            return false;
        }
        inner.running = true;
        inner.status = Status::Loading;
        true
    }

    /// Turns it off: the model leaves memory at once (a running download stops after its
    /// current file).
    pub fn turn_off(&self) {
        let mut inner = self.lock();
        inner.want = false;
        inner.offs += 1;
        inner.model = None;
        inner.status = Status::Off;
    }

    /// Sets the status, unless it was turned off meanwhile; answers whether it is still on.
    pub fn report(&self, status: Status) -> bool {
        let mut inner = self.lock();
        // "Embedding pages" only while there is a model to embed with.
        let embedding = matches!(status, Status::Embedding { .. });
        if inner.want && (!embedding || inner.model.is_some()) {
            inner.status = status;
        }
        inner.want
    }

    /// Loads the model with the loader, after [`Switch::turn_on`] answered `true`. Blocking.
    /// `Ok(true)` when it is in (status: embedding, for the tenants to catch up), `Ok(false)`
    /// when it was turned off meanwhile; on an error the switch is off again, its status
    /// failed with the reason.
    pub fn load(&self) -> anyhow::Result<bool> {
        loop {
            let offs = self.lock().offs;
            let loaded = (self.loader)(&|s| self.report(s));
            let mut inner = self.lock();
            match loaded {
                Ok(model) if inner.want => {
                    inner.model = Some(model);
                    inner.status = Status::Embedding {
                        tenant: 0,
                        tenants: 0,
                        done: 0,
                        total: 0,
                    };
                    return Ok(true);
                }
                Ok(_) => {
                    inner.running = false;
                    return Ok(false);
                }
                // Turned off (which stopped this load) and on again meanwhile: the last word
                // was "on", so load again rather than fail.
                Err(_) if inner.want && inner.offs != offs => {
                    inner.status = Status::Loading;
                }
                Err(e) => {
                    inner.running = false;
                    if inner.want {
                        inner.want = false;
                        inner.status = Status::Failed {
                            error: format!("{e:#}"),
                        };
                    }
                    return Err(e);
                }
            }
        }
    }

    /// Ends a loader's work after the tenants caught up. `true` when it must load again: it
    /// was turned off and on while the tenants were catching up, and the model is gone.
    pub fn finish(&self) -> bool {
        let mut inner = self.lock();
        if inner.want && inner.model.is_none() {
            inner.status = Status::Loading;
            return true;
        }
        inner.running = false;
        inner.status = if inner.want { Status::On } else { Status::Off };
        false
    }

    /// Runs `f` (deleting the model's files) while it is off and nothing loads, without
    /// holding the switch meanwhile (searches ask it whether it is ready). A turn-on that comes
    /// in meanwhile is kept, and loads once `f` is done: the answer's `true` says the caller
    /// must now run the loader. `None` when it is on, loading or already deleting.
    pub fn while_off<R>(&self, f: impl FnOnce() -> R) -> Option<(R, bool)> {
        {
            let mut inner = self.lock();
            if inner.want || inner.running || inner.deleting {
                return None;
            }
            inner.deleting = true;
        }
        let r = f();
        let mut inner = self.lock();
        inner.deleting = false;
        let start = inner.want && !inner.running && inner.model.is_none();
        if start {
            inner.running = true;
            inner.status = Status::Loading;
        }
        Some((r, start))
    }

    fn model_now(&self) -> Option<Arc<dyn Embedder>> {
        self.lock().model.clone()
    }
}

impl Embedder for Switch {
    fn model(&self) -> &str {
        &self.model_id
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let Some(model) = self.model_now() else {
            anyhow::bail!("the embedding model is off");
        };
        let mut out = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(PROGRESS_STEP) {
            // Turned off meanwhile: stop, and let the model go (a long catch-up would keep it).
            if self.lock().model.is_none() {
                anyhow::bail!("the embedding model was turned off");
            }
            out.extend(model.embed(chunk)?);
            // Counted toward the catch-up's step only (a query, or another tenant's write
            // meanwhile, would make it run past its total).
            if let Status::Embedding { done, total, .. } = &mut self.lock().status {
                *done = (*done + chunk.len()).min(*total);
            }
        }
        Ok(out)
    }

    fn will_embed(&self, count: usize) {
        // Set once per tenant step (the catch-up resets the step to 0 of 0 before each tenant);
        // a snapshot another write rebuilds meanwhile doesn't move it back.
        if let Status::Embedding { done, total, .. } = &mut self.lock().status
            && *total == 0
        {
            (*done, *total) = (0, count);
        }
    }

    fn ready(&self) -> bool {
        self.lock().model.is_some()
    }
}

/// The folder in `cache_dir` holding the service model's files.
pub fn files_dir(cache_dir: &Path) -> PathBuf {
    cache_dir.join(format!("models--{}", REPO.replace('/', "--")))
}

/// The quantized weights in `cache_dir`, when they are there and whole.
fn read_quantized(cache_dir: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(files_dir(cache_dir).join(QUANTIZED_FILE)).ok()?;
    (hex::encode(sha2::Sha256::digest(&bytes)) == QUANTIZED_SHA256).then_some(bytes)
}

/// Quantizes the downloaded weights, checks the result and keeps it in `cache_dir` (written
/// whole or not at all).
fn make_quantized(cache_dir: &Path, full: &[u8]) -> anyhow::Result<Vec<u8>> {
    let bytes = quantize::quantize(full).context("quantizing the embedding model")?;
    let hash = hex::encode(sha2::Sha256::digest(&bytes));
    if hash != QUANTIZED_SHA256 {
        anyhow::bail!(
            "the quantized embedding model has SHA-256 {hash}, not the expected {QUANTIZED_SHA256}"
        );
    }
    let path = files_dir(cache_dir).join(QUANTIZED_FILE);
    let dir = path.parent().expect("a folder");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension("onnx.new");
    std::fs::write(&tmp, &bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(bytes)
}

/// Removes full-precision weights `cache_dir` still holds next to a good 8-bit copy: a removal
/// that failed once (the file was open elsewhere), or a cache copied from one made before the
/// 8-bit copy existed. Run at every load, so they never stay.
fn remove_full_weights(cache_dir: &Path) {
    use hf_hub::{Cache, Repo, RepoType};
    let id = Repo::with_revision(REPO.to_owned(), RepoType::Model, REVISION.to_owned());
    if let Some(path) = Cache::new(cache_dir.to_path_buf()).repo(id).get(FILES[4]) {
        tracing::info!(file = %path.display(), "removing the full-precision model weights the 8-bit copy replaced");
        remove_download(&path);
    }
}

/// Removes the downloaded full-precision weights once the 8-bit copy is kept: only that copy
/// is ever loaded, and if it goes missing or is damaged, they are downloaded again. hf-hub may
/// keep the file as a link to a blob: both go. A failure only leaves the file behind.
fn remove_download(path: &Path) {
    let blob = std::fs::symlink_metadata(path)
        .is_ok_and(|m| m.file_type().is_symlink())
        .then(|| std::fs::canonicalize(path).ok())
        .flatten();
    for file in blob.iter().map(PathBuf::as_path).chain([path]) {
        if let Err(e) = std::fs::remove_file(file) {
            tracing::warn!(file = %file.display(), error = %e, "the downloaded model weights were not removed");
        }
    }
}

/// Reports a file's download to the switch.
struct DownloadProgress<'a> {
    report: &'a dyn Fn(Status) -> bool,
    file: String,
    files_done: usize,
    done: u64,
    total: u64,
    /// Bytes at the last report, so the switch isn't locked for every block read.
    reported: u64,
}

impl DownloadProgress<'_> {
    fn send(&mut self) {
        self.reported = self.done;
        (self.report)(Status::Downloading {
            file: self.file.clone(),
            done_bytes: self.done,
            total_bytes: self.total,
            files_done: self.files_done,
            files: FILES.len(),
        });
    }
}

impl hf_hub::api::Progress for DownloadProgress<'_> {
    fn init(&mut self, size: usize, _filename: &str) {
        (self.total, self.done) = (size as u64, 0);
        self.send();
    }

    fn update(&mut self, size: usize) {
        self.done += size as u64;
        if self.done - self.reported >= 1 << 20 {
            self.send();
        }
    }

    fn finish(&mut self) {
        self.send();
    }
}

/// The model, run by fastembed. Its files are downloaded into `cache_dir` on first use.
pub struct FastEmbedder {
    model: Mutex<fastembed::TextEmbedding>,
}

impl FastEmbedder {
    /// Loads the model, downloading its files (about 640 MB) into `cache_dir` the first time,
    /// from Hugging Face or the mirror `HF_ENDPOINT` names, and quantizing them (about a second;
    /// 316 MB kept, the full weights removed);
    /// a broken download resumes, up to three tries per file. Slow: call it off the async
    /// threads.
    pub fn load(cache_dir: &Path) -> anyhow::Result<Self> {
        Self::load_with(cache_dir, &|_| true)
    }

    /// Like [`FastEmbedder::load`], reporting the download and load to `progress`, and
    /// stopping (with an error) before the next file once it answers `false`.
    pub fn load_with(cache_dir: &Path, progress: &dyn Fn(Status) -> bool) -> anyhow::Result<Self> {
        use hf_hub::{Cache, Repo, RepoType, api::sync::ApiBuilder};
        let api = ApiBuilder::from_env()
            .with_cache_dir(cache_dir.to_path_buf())
            .with_progress(false)
            .with_retries(3)
            .build()
            .context("the model download client")?;
        let id = Repo::with_revision(REPO.to_owned(), RepoType::Model, REVISION.to_owned());
        let cached = Cache::new(cache_dir.to_path_buf()).repo(id.clone());
        let repo = api.repo(id);
        let files_done = std::cell::Cell::new(0);
        let read = |file: &str| -> anyhow::Result<Vec<u8>> {
            if !progress(Status::Loading) {
                anyhow::bail!("the embedding model was turned off");
            }
            let path = match cached.get(file) {
                Some(path) => path,
                None => repo
                    .download_with_progress(
                        file,
                        DownloadProgress {
                            report: progress,
                            file: file.to_owned(),
                            files_done: files_done.get(),
                            done: 0,
                            total: 0,
                            reported: 0,
                        },
                    )
                    .with_context(|| format!("downloading {REPO}/{file}"))?,
            };
            files_done.set(files_done.get() + 1);
            std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
        };
        let [tokenizer, config, special_tokens, tokenizer_config, weights] = FILES;
        let tokenizer = fastembed::TokenizerFiles {
            tokenizer_file: read(tokenizer)?,
            config_file: read(config)?,
            special_tokens_map_file: read(special_tokens)?,
            tokenizer_config_file: read(tokenizer_config)?,
        };
        let weights = match read_quantized(cache_dir) {
            Some(bytes) => {
                remove_full_weights(cache_dir);
                bytes
            }
            None => {
                let full = read(weights)?;
                if !progress(Status::Loading) {
                    anyhow::bail!("the embedding model was turned off");
                }
                let bytes = make_quantized(cache_dir, &full)?;
                drop(full);
                if let Some(path) = cached.get(weights) {
                    remove_download(&path);
                }
                bytes
            }
        };
        progress(Status::Loading);
        let model = fastembed::UserDefinedEmbeddingModel::new(weights, tokenizer)
            .with_pooling(fastembed::Pooling::Mean);
        let model = fastembed::TextEmbedding::try_new_from_user_defined(
            model,
            fastembed::InitOptionsUserDefined::new()
                .with_max_length(MAX_TOKENS)
                // Within the service's compute budget (`par`), not every core.
                .with_intra_threads(crate::par::threads()),
        )
        .with_context(|| format!("loading embedding model {MODEL}"))?;
        Ok(Self {
            model: Mutex::new(model),
        })
    }
}

impl Embedder for FastEmbedder {
    fn model(&self) -> &str {
        MODEL_ID
    }

    fn dim(&self) -> usize {
        DIM
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        // One text per call: a batch is padded to its longest text, and a long section would
        // make the whole batch costly. The lock is taken per text, so a query waits for one
        // text at most, not for a whole reindex.
        for text in texts {
            let mut model = self.model.lock().unwrap_or_else(|e| e.into_inner());
            let v = model
                .embed([text.as_str()], None)
                .context("embedding text")?;
            out.extend(v.into_iter().map(unit));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{OnceLock, Weak};

    /// Full weights left in a cache (as hf-hub lays it out) go; the tokenizer stays.
    #[test]
    fn leftover_full_weights_are_removed() {
        let cache = tempfile::tempdir().unwrap();
        let repo = files_dir(cache.path());
        let snapshot = repo.join("snapshots").join(REVISION);
        std::fs::create_dir_all(snapshot.join("onnx")).unwrap();
        std::fs::create_dir_all(repo.join("refs")).unwrap();
        std::fs::write(repo.join("refs").join(REVISION), REVISION).unwrap();
        std::fs::write(snapshot.join(FILES[4]), b"full weights").unwrap();
        std::fs::write(snapshot.join(FILES[0]), b"{}").unwrap();
        remove_full_weights(cache.path());
        assert!(!snapshot.join(FILES[4]).exists());
        assert!(snapshot.join(FILES[0]).exists());
        // Nothing to remove is no error.
        remove_full_weights(cache.path());
        remove_full_weights(&cache.path().join("missing"));
    }

    struct Stub;

    impl Embedder for Stub {
        fn model(&self) -> &str {
            "stub"
        }
        fn dim(&self) -> usize {
            2
        }
        fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
        }
    }

    #[test]
    fn turned_off_and_on_while_loading_ends_on() {
        // The first load is stopped by a turn-off; before it gives up, it is turned on again.
        let me: Arc<OnceLock<Weak<Switch>>> = Arc::new(OnceLock::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let (me2, calls2) = (me.clone(), calls.clone());
        let switch = Arc::new(Switch::new(
            "stub",
            2,
            Arc::new(move |_progress| {
                if calls2.fetch_add(1, Ordering::SeqCst) == 0 {
                    let s = me2.get().and_then(Weak::upgrade).expect("the switch");
                    s.turn_off();
                    assert!(!s.turn_on(), "a loader is still at work");
                    anyhow::bail!("the embedding model was turned off");
                }
                Ok(Arc::new(Stub) as Arc<dyn Embedder>)
            }),
        ));
        me.set(Arc::downgrade(&switch)).ok();
        assert!(switch.turn_on());
        assert!(switch.load().unwrap(), "loaded on the second try");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(switch.is_on());
        assert!(!switch.finish());
        assert!(matches!(switch.status(), Status::On));
    }

    #[test]
    fn turning_it_off_stops_a_long_batch() {
        struct OffAfterFirst(Arc<OnceLock<Weak<Switch>>>);
        impl Embedder for OffAfterFirst {
            fn model(&self) -> &str {
                "stub"
            }
            fn dim(&self) -> usize {
                2
            }
            fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
                if let Some(s) = self.0.get().and_then(Weak::upgrade) {
                    s.turn_off();
                }
                Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
            }
        }
        let me: Arc<OnceLock<Weak<Switch>>> = Arc::new(OnceLock::new());
        let model = Arc::new(OffAfterFirst(me.clone())) as Arc<dyn Embedder>;
        let switch = Arc::new(Switch::new("stub", 2, Arc::new(move |_| Ok(model.clone()))));
        me.set(Arc::downgrade(&switch)).ok();
        assert!(switch.turn_on());
        assert!(switch.load().unwrap());
        let texts: Vec<String> = (0..PROGRESS_STEP * 3).map(|i| i.to_string()).collect();
        assert!(
            switch.embed(&texts).is_err(),
            "stopped after the first chunk"
        );
    }

    #[test]
    fn a_turn_on_during_deletion_loads_after_it() {
        let switch = Arc::new(Switch::new(
            "stub",
            2,
            Arc::new(|_| Ok(Arc::new(Stub) as Arc<dyn Embedder>)),
        ));
        let s = switch.clone();
        let (_, start) = switch
            .while_off(|| {
                // Searches still get an answer meanwhile, and a turn-on is kept for later.
                assert!(!s.ready());
                assert!(!s.turn_on(), "no loader while files are deleted");
            })
            .unwrap();
        assert!(start, "the kept turn-on loads now");
        assert!(switch.load().unwrap());
        assert!(switch.while_off(|| ()).is_none(), "not while it is on");
    }

    #[test]
    fn a_failed_load_turns_it_off_with_the_reason() {
        let switch = Switch::new("stub", 2, Arc::new(|_| anyhow::bail!("no network")));
        assert!(switch.turn_on());
        assert!(switch.load().is_err());
        assert!(!switch.is_on());
        assert!(matches!(switch.status(), Status::Failed { .. }));
    }
}
