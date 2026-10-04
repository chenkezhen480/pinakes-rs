//! Embedding models for tests: the real one, loaded once per test process, and a fake one that
//! needs no model files.

#![allow(dead_code)]

use std::sync::{Arc, OnceLock};

use pinakes::embed::{Embedder, FastEmbedder};

/// The real model, when `PINAKES_TEST_EMBEDDING` is set; its files are downloaded into
/// `target/models` the first time. `None` without the variable, and the caller says it skipped.
pub fn real() -> Option<Arc<dyn Embedder>> {
    static MODEL_ONCE: OnceLock<Arc<dyn Embedder>> = OnceLock::new();
    std::env::var_os("PINAKES_TEST_EMBEDDING")?;
    Some(
        MODEL_ONCE
            .get_or_init(|| {
                let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/models");
                Arc::new(FastEmbedder::load(&dir).expect("loading the embedding model"))
            })
            .clone(),
    )
}

/// A stand-in that turns text into a bag of its character bigrams, so texts
/// sharing pieces are close. Enough to test how vectors are stored, kept and searched, not
/// to measure search quality.
pub struct Fake {
    /// Texts embedded so far, to check what was (re-)embedded.
    pub seen: std::sync::Mutex<Vec<String>>,
}

pub const FAKE_DIM: usize = 256;

impl Fake {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: std::sync::Mutex::new(Vec::new()),
        })
    }

    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Embedder for Fake {
    fn model(&self) -> &str {
        "fake"
    }

    fn dim(&self) -> usize {
        FAKE_DIM
    }

    fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.seen.lock().unwrap().extend(texts.iter().cloned());
        Ok(texts
            .iter()
            .map(|t| {
                let t = t.to_lowercase();
                let chars: Vec<char> = t.chars().filter(|c| c.is_alphanumeric()).collect();
                let mut v = vec![0f32; FAKE_DIM];
                for w in chars.windows(2) {
                    let h = (w[0] as usize).wrapping_mul(31).wrapping_add(w[1] as usize);
                    v[h % FAKE_DIM] += 1.0;
                }
                let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
                v.iter().map(|x| x / n).collect()
            })
            .collect())
    }
}
