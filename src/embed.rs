//! Local text embeddings, for meaning-based search (design, "Search signals" → "Meaning, when
//! the agent asks for it").
//!
//! Only wiki content is embedded, at write time: each page's head (title, aliases, keywords,
//! summary) and each of its sections. The vectors are kept in the tenant's SQLite file, keyed by
//! page, heading and model, with a hash of the text they were built from, so a hand edit is
//! caught and re-embedded. A search embeds only its query. The model runs inside the process
//! (fastembed on ONNX Runtime): jina-embeddings-v2-base-zh, a Chinese–English model, chosen on
//! the test set (design, "As built in M5"). It takes queries and stored text as they are, with
//! no prefixes.

use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::Context;

/// The one model the service runs, as `[embedding] model` names it.
pub const MODEL: &str = "jina-embeddings-v2-base-zh";
/// Where its files come from: the Hugging Face repository, at a fixed revision.
const REPO: &str = "jinaai/jina-embeddings-v2-base-zh";
const REVISION: &str = "c1ff9086a89a1123d7b5eff58055a665db4fb4b9";
/// The name stored with every vector and recorded in search fingerprints: the model and its
/// revision, so vectors from other weights are never compared with a query. Change it with
/// `REVISION`.
const MODEL_ID: &str = "jina-embeddings-v2-base-zh@c1ff9086";
/// Tokens of one text the model reads: all it can take (8,192), so a section is never cut
/// short of the model's limit. Pages are the librarian's condensed account, so sections that
/// long are rare; one costs a few seconds and some memory to embed, at write time only.
const MAX_TOKENS: usize = 8192;
/// Values per vector.
const DIM: usize = 768;

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

    /// Whether it can embed now; a model still loading can't (see [`Background`]).
    fn ready(&self) -> bool {
        true
    }
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

/// The model, loaded after the service starts listening, so a first start that downloads it
/// (about 640 MB) doesn't keep the port closed. Until it is in, it embeds nothing: snapshots are
/// built without vectors and the meaning-based modes answer 503; the open tenants get their
/// vectors once it arrives (`AppState::new`).
#[derive(Default)]
pub struct Background {
    model: OnceLock<FastEmbedder>,
}

impl Background {
    pub fn set(&self, model: FastEmbedder) {
        let _ = self.model.set(model);
    }
}

impl Embedder for Background {
    fn model(&self) -> &str {
        MODEL_ID
    }

    fn dim(&self) -> usize {
        DIM
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        match self.model.get() {
            Some(m) => m.embed(texts),
            None => anyhow::bail!("the embedding model is not loaded"),
        }
    }

    fn ready(&self) -> bool {
        self.model.get().is_some()
    }
}

/// The model, run by fastembed. Its files are downloaded into `cache_dir` on first use.
pub struct FastEmbedder {
    model: Mutex<fastembed::TextEmbedding>,
}

impl FastEmbedder {
    /// Loads the model, downloading its files (about 640 MB) into `cache_dir` the first time,
    /// from Hugging Face or the mirror `HF_ENDPOINT` names; a broken download resumes, up to
    /// three tries per file. Slow: call it off the async threads.
    pub fn load(cache_dir: &Path) -> anyhow::Result<Self> {
        use hf_hub::{Repo, RepoType, api::sync::ApiBuilder};
        let api = ApiBuilder::from_env()
            .with_cache_dir(cache_dir.to_path_buf())
            .with_progress(false)
            .with_retries(3)
            .build()
            .context("the model download client")?;
        let repo = api.repo(Repo::with_revision(
            REPO.to_owned(),
            RepoType::Model,
            REVISION.to_owned(),
        ));
        let read = |file: &str| -> anyhow::Result<Vec<u8>> {
            let path = repo
                .get(file)
                .with_context(|| format!("downloading {REPO}/{file}"))?;
            std::fs::read(&path).with_context(|| format!("reading {}", path.display()))
        };
        let tokenizer = fastembed::TokenizerFiles {
            tokenizer_file: read("tokenizer.json")?,
            config_file: read("config.json")?,
            special_tokens_map_file: read("special_tokens_map.json")?,
            tokenizer_config_file: read("tokenizer_config.json")?,
        };
        let model = fastembed::UserDefinedEmbeddingModel::new(read("onnx/model.onnx")?, tokenizer)
            .with_pooling(fastembed::Pooling::Mean);
        let model = fastembed::TextEmbedding::try_new_from_user_defined(
            model,
            fastembed::InitOptionsUserDefined::new().with_max_length(MAX_TOKENS),
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
