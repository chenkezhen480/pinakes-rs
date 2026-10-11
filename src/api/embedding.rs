//! `/admin/embedding`: the embedding model's switch (design, "M10": vectors off by default).
//! Off, searches use keywords alone; turned on, the model is downloaded (the first time) and
//! loaded in the background, and every open tenant's pages are embedded once, without a
//! restart. The status says which step it is at.

use std::path::Path;

use axum::Json;
use axum::extract::State;
use axum::extract::rejection::JsonRejection;
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::embed::Status;
use crate::error::ApiError;
use crate::identity::Admin;

/// The size of the model's files, for the note shown before turning it on.
const DOWNLOAD_BYTES: u64 = 641_000_000;

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct EmbeddingStatus {
    /// `[embedding] model`; `None` when none is configured and embeddings can't be turned on.
    pub model: Option<String>,
    /// Whether it is turned on (it may still be loading).
    pub enabled: bool,
    /// The step it is at.
    pub status: Status,
    /// Its files on disk.
    pub files: ModelFiles,
    /// Where a first download comes from: Hugging Face, or the mirror in `HF_ENDPOINT`.
    pub download_from: String,
    /// About how much a first download takes.
    pub download_bytes: u64,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ModelFiles {
    pub dir: String,
    /// Bytes on disk; 0 when they were never downloaded.
    pub bytes: u64,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct SetEmbedding {
    pub enabled: bool,
}

fn status(app: &AppState) -> EmbeddingStatus {
    let models = app.config.data.dir.join("models");
    let dir = crate::embed::files_dir(&models);
    let (enabled, status) = match &app.embedding {
        Some(switch) => (switch.is_on(), switch.status()),
        // A model given whole (tests) is always on.
        None if app.embedder.is_some() => (true, Status::On),
        None => (false, Status::Off),
    };
    EmbeddingStatus {
        model: app.config.embedding.model.clone(),
        enabled,
        status,
        files: ModelFiles {
            bytes: dir_bytes(&dir),
            dir: dir.display().to_string(),
        },
        download_from: std::env::var("HF_ENDPOINT")
            .ok()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| "https://huggingface.co".into()),
        download_bytes: DOWNLOAD_BYTES,
    }
}

/// The size of the files under `dir` (symbolic links not followed).
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_bytes(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

pub async fn get(
    _admin: Admin,
    State(app): State<AppState>,
) -> Result<Json<EmbeddingStatus>, ApiError> {
    // The model's folder is measured on disk: off the async threads.
    let status = tokio::task::spawn_blocking(move || status(&app))
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(status))
}

#[tracing::instrument(name = "set_embedding", skip_all)]
pub async fn put(
    _admin: Admin,
    State(app): State<AppState>,
    body: Result<Json<SetEmbedding>, JsonRejection>,
) -> Result<Json<EmbeddingStatus>, ApiError> {
    let Json(req) = body?;
    // Writes the config file and measures the model's folder: off the async threads.
    let status = tokio::task::spawn_blocking(move || {
        app.set_embedding(req.enabled)?;
        Ok::<_, ApiError>(status(&app))
    })
    .await
    .map_err(anyhow::Error::from)??;
    Ok(Json(status))
}

/// Deletes the model's files, while embeddings are off; turning them on again downloads
/// them again. Stored page vectors stay.
#[tracing::instrument(name = "delete_model_files", skip_all)]
pub async fn delete_files(
    _admin: Admin,
    State(app): State<AppState>,
) -> Result<Json<EmbeddingStatus>, ApiError> {
    let busy = || {
        ApiError::Conflict(
            "the model's files can be deleted only while embeddings are off and nothing is loading"
                .into(),
        )
    };
    let switch = app.embedding.clone().ok_or_else(busy)?;
    let dir = crate::embed::files_dir(&app.config.data.dir.join("models"));
    // Deleted while the switch is marked so: a turn-on meanwhile waits, then loads afresh.
    let s = switch.clone();
    let (removed, start) = tokio::task::spawn_blocking(move || {
        s.while_off(|| match std::fs::remove_dir_all(&dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        })
    })
    .await
    .map_err(anyhow::Error::from)?
    .ok_or_else(busy)?;
    if start {
        app.run_loader(switch);
    }
    removed.map_err(|e| ApiError::Internal(anyhow::Error::new(e).context("deleting the model")))?;
    tracing::info!("embedding model files deleted");
    // The model's folder is measured on disk: off the async threads.
    let status = tokio::task::spawn_blocking(move || status(&app))
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(status))
}
