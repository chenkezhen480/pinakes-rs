//! The librarian's LLM: one small trait with two HTTP backends, chosen once from config.
//! Nothing else in the service knows which provider is running, and nothing on the read path
//! ever calls it.

mod anthropic;
mod openai;

pub use anthropic::Anthropic;
pub use openai::OpenAiCompatible;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::config::{LibrarianConfig, LlmProvider};

pub struct Prompt {
    pub system: String,
    pub user: String,
    pub max_tokens: u32,
    /// The JSON schema the reply must follow, passed to the API's structured output parameter
    /// (Anthropic `output_config.format`, OpenAI `response_format`), so the reply is always one
    /// valid object of that shape.
    pub schema: serde_json::Value,
    /// Pictures shown with the user message, before it (a scan's page); most prompts have none.
    pub images: Vec<Image>,
}

/// A picture in a prompt.
#[derive(Debug, Clone)]
pub struct Image {
    /// Its type, such as `image/png`.
    pub media_type: &'static str,
    pub data: Vec<u8>,
}

impl Image {
    /// The picture in base64, as the APIs take it inline.
    pub(crate) fn base64(&self) -> String {
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(&self.data)
    }
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    /// The model's reply to one prompt: a JSON object of the prompt's schema, as text.
    async fn complete(&self, prompt: &Prompt) -> anyhow::Result<String>;

    /// The reply, and how many tokens the call used (input and output): as the API reports
    /// them, or estimated from the text when it doesn't.
    async fn complete_counted(&self, prompt: &Prompt) -> anyhow::Result<(String, u64)> {
        let reply = self.complete(prompt).await?;
        let used = estimate_tokens(&prompt.system)
            + estimate_tokens(&prompt.user)
            + estimate_tokens(&reply);
        Ok((reply, used))
    }
}

/// The error a call fails with when the model's reply ran past `max_tokens`: callers that can ask
/// for less (a shorter reading window) tell it from a failure of the service.
pub const CUT_OFF: &str = "the reply was cut off at max_tokens";

/// Whether a call failed because its reply was cut off at `max_tokens`.
pub fn cut_off(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.to_string() == CUT_OFF)
}

/// About how many tokens a text costs: a CJK character about one, other text about a quarter
/// of a token per character.
pub fn estimate_tokens(text: &str) -> u64 {
    let tenths: u64 = text
        .chars()
        .map(|c| if c as u32 >= 0x2E80 { 10 } else { 3 })
        .sum();
    tenths / 10 + 1
}

/// The tokens a reply's `usage` reports, if it does: input (cached or not) and output.
pub(crate) fn reported_usage(reply: &serde_json::Value) -> Option<u64> {
    let u = reply.get("usage")?;
    let n = |k: &str| u.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
    if let Some(total) = u.get("total_tokens").and_then(serde_json::Value::as_u64) {
        return Some(total);
    }
    let sum = n("input_tokens")
        + n("output_tokens")
        + n("cache_read_input_tokens")
        + n("cache_creation_input_tokens");
    (sum > 0).then_some(sum)
}

/// The librarian's model: one model for every step.
pub type Llm = Arc<dyn LlmClient>;

/// Why there is no model to call. Each names the model file, the one place to fix it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Missing {
    #[error("no model is set in {file}")]
    Model { file: String },
    #[error("no api_key in {file}, and {env} is not set")]
    Key { file: String, env: String },
    #[error("{0}")]
    Config(String),
}

/// The client the model file asks for, with this key. An OpenAI-compatible server may need no
/// key.
pub fn client(cfg: &LibrarianConfig, key: Option<String>) -> Result<Llm, Missing> {
    let m = &cfg.llm;
    let file = cfg.llm_file_name();
    let model = m.model().ok_or(Missing::Model { file: file.clone() })?;
    let timeout = Duration::from_secs(cfg.timeout_secs);
    let built: anyhow::Result<Llm> = match m.provider {
        LlmProvider::Anthropic => {
            let key = key.ok_or_else(|| Missing::Key {
                file: file.clone(),
                env: m.key_env(),
            })?;
            Anthropic::new(key, &model, m.base_url.as_deref(), timeout).map(|c| Arc::new(c) as Llm)
        }
        LlmProvider::OpenaiCompatible => {
            let base = m.base_url.as_deref().ok_or_else(|| {
                Missing::Config(format!(
                    "base_url is required for openai_compatible in {file}"
                ))
            })?;
            OpenAiCompatible::new(key, &model, base, timeout).map(|c| Arc::new(c) as Llm)
        }
    };
    built.map_err(|e| Missing::Config(e.to_string()))
}

/// Retries for one HTTP call: rate limits, overload, server errors and dropped connections.
/// Three, like every retry in the service; then the call fails.
pub(crate) const RETRIES: u32 = 3;

/// How long to wait before retry `attempt` (1-based): `retry-after` when the server sent one
/// (capped at a minute), otherwise 2, 4, ... seconds.
pub(crate) fn backoff(
    attempt: u32,
    retry_after: Option<&reqwest::header::HeaderValue>,
) -> Duration {
    let from_header = retry_after
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|s| Duration::from_secs(s.min(60)));
    from_header.unwrap_or(Duration::from_secs(
        2u64 << attempt.saturating_sub(1).min(4),
    ))
}

/// Whether an HTTP status is worth retrying.
pub(crate) fn retryable(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
        || status.as_u16() == 529
}

tokio::task_local! {
    /// Set while a call must be tried once only (the console's model test).
    static NO_RETRIES: bool;
}

/// Runs `call` with every model request in it tried once, not retried: for a test that
/// should answer at once.
pub async fn once<F: std::future::Future>(call: F) -> F::Output {
    NO_RETRIES.scope(true, call).await
}

/// Sends a JSON POST, retrying what is worth retrying, and returns the parsed reply body.
pub(crate) async fn post_json(
    req: impl Fn() -> reqwest::RequestBuilder,
    what: &str,
) -> anyhow::Result<serde_json::Value> {
    let retries = if NO_RETRIES.try_with(|n| *n).unwrap_or(false) {
        0
    } else {
        RETRIES
    };
    let mut attempt = 0;
    loop {
        attempt += 1;
        let resp = match req().send().await {
            Ok(r) => r,
            Err(e) if attempt <= retries && (e.is_connect() || e.is_timeout()) => {
                tracing::warn!(error = %e, attempt, "{what}: request failed, retrying");
                tokio::time::sleep(backoff(attempt, None)).await;
                continue;
            }
            Err(e) => return Err(anyhow::Error::new(e).context(format!("{what}: request"))),
        };
        let status = resp.status();
        if status.is_success() {
            return resp
                .json()
                .await
                .map_err(|e| anyhow::Error::new(e).context(format!("{what}: reply body")));
        }
        let wait = backoff(attempt, resp.headers().get(reqwest::header::RETRY_AFTER));
        let body = resp.text().await.unwrap_or_default();
        if attempt <= retries && retryable(status) {
            tracing::warn!(%status, attempt, "{what}: retrying");
            tokio::time::sleep(wait).await;
            continue;
        }
        let body: String = body.chars().take(500).collect();
        anyhow::bail!("{what}: HTTP {status}: {body}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_prefers_retry_after() {
        let v = reqwest::header::HeaderValue::from_static("7");
        assert_eq!(backoff(1, Some(&v)), Duration::from_secs(7));
        let big = reqwest::header::HeaderValue::from_static("600");
        assert_eq!(backoff(1, Some(&big)), Duration::from_secs(60));
        assert_eq!(backoff(1, None), Duration::from_secs(2));
        assert_eq!(backoff(2, None), Duration::from_secs(4));
    }

    #[test]
    fn what_is_retried() {
        assert!(retryable(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(retryable(reqwest::StatusCode::from_u16(529).unwrap()));
        assert!(retryable(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!retryable(reqwest::StatusCode::BAD_REQUEST));
        assert!(!retryable(reqwest::StatusCode::UNAUTHORIZED));
    }
}
