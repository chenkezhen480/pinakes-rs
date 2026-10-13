//! Service-wide administration behind the console's admin pages (design, "M10"): the
//! service's audit log, the librarian's model (`model.conf`) changed while the service runs,
//! and the environment variables the service reads. Tenant-level actions keep their own audit
//! log in the tenant's store.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{LlmProvider, ModelConfig};
use crate::error::ApiError;
use crate::{AppState, llm};

/// The service's own audit log, `<data>/audit.jsonl`: changes to the configuration, the model
/// and embeddings, and tenants created or deleted. A key is never written to it.
pub struct Audit {
    file: PathBuf,
    lock: Mutex<()>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct AuditEntry {
    pub at: String,
    pub actor: String,
    pub action: String,
    #[serde(default)]
    pub detail: Value,
}

impl Audit {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            file: data_dir.join("audit.jsonl"),
            lock: Mutex::new(()),
        }
    }

    /// Appends an entry; a failure to write is logged, never fatal.
    pub fn record(&self, actor: &str, action: &str, detail: Value) {
        let entry = AuditEntry {
            at: crate::trace::now_rfc3339(),
            actor: actor.to_owned(),
            action: action.to_owned(),
            detail,
        };
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let written = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)
            .and_then(|mut f| writeln!(f, "{}", serde_json::to_string(&entry).unwrap_or_default()));
        if let Err(e) = written {
            tracing::error!(file = %self.file.display(), error = %e, "the audit log can't be written");
        }
    }

    /// The most recent entries, newest first.
    /// The newest `limit` entries, newest first, read back from the end of the file (the log
    /// is never rotated, so it isn't read whole). Writes are whole appended lines, so no lock
    /// is needed: a line still being written doesn't parse and is skipped.
    pub fn recent(&self, limit: usize) -> Vec<AuditEntry> {
        use std::io::{Read, Seek, SeekFrom};
        const BLOCK: u64 = 64 * 1024;
        let Ok(mut file) = std::fs::File::open(&self.file) else {
            return Vec::new();
        };
        let Ok(mut pos) = file.seek(SeekFrom::End(0)) else {
            return Vec::new();
        };
        let mut tail: Vec<u8> = Vec::new();
        let mut lines = 0;
        while pos > 0 && lines <= limit {
            let start = pos.saturating_sub(BLOCK);
            let mut block = vec![0; (pos - start) as usize];
            if file.seek(SeekFrom::Start(start)).is_err() || file.read_exact(&mut block).is_err() {
                break;
            }
            lines += block.iter().filter(|&&b| b == b'\n').count();
            block.extend_from_slice(&tail);
            tail = block;
            pos = start;
        }
        let text = String::from_utf8_lossy(&tail);
        let mut all: Vec<&str> = text.lines().collect();
        // The first line read may begin before the block did.
        if pos > 0 && !all.is_empty() {
            all.remove(0);
        }
        all.iter()
            .rev()
            .filter_map(|l| serde_json::from_str(l).ok())
            .take(limit)
            .collect()
    }
}

/// Where the librarian's key comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum KeySource {
    /// `api_key` in the model file.
    File,
    /// The environment variable `key_env` names.
    Environment,
    /// Typed at the terminal prompt; lasts until the service stops.
    Entered,
}

/// The model as it is now, which may differ from what the service started with.
#[derive(Clone, Default)]
pub struct ModelState {
    pub config: ModelConfig,
    /// A key typed at the terminal prompt, when that is where it came from.
    pub entered_key: Option<String>,
}

impl ModelState {
    pub fn key(&self) -> Option<(String, KeySource)> {
        let file = self
            .config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty());
        if let Some(k) = file {
            return Some((k.to_owned(), KeySource::File));
        }
        if let Some(k) = std::env::var(self.config.key_env())
            .ok()
            .filter(|k| !k.trim().is_empty())
        {
            return Some((k.trim().to_owned(), KeySource::Environment));
        }
        self.entered_key.clone().map(|k| (k, KeySource::Entered))
    }
}

/// The model as the console shows it; the key is never in it, only where it comes from.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ModelView {
    /// The model file.
    pub file: String,
    pub provider: LlmProvider,
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// The variable read for the key when the file holds none.
    pub key_env: String,
    /// Where the key comes from, or `None` when there is none.
    pub key: Option<KeySource>,
    /// Whether the librarian is on, and why not when it is off.
    pub librarian: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub off_because: Option<String>,
    pub presets: Vec<Preset>,
}

/// A common endpoint, to fill the form with.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Preset {
    pub name: &'static str,
    pub provider: LlmProvider,
    pub base_url: Option<&'static str>,
    pub model: &'static str,
    pub key_env: &'static str,
}

fn presets() -> Vec<Preset> {
    vec![
        Preset {
            name: "DeepSeek (Anthropic-compatible)",
            provider: LlmProvider::Anthropic,
            base_url: Some("https://api.deepseek.com/anthropic"),
            model: "deepseek-flash",
            key_env: "DEEPSEEK_API_KEY",
        },
        Preset {
            name: "Anthropic",
            provider: LlmProvider::Anthropic,
            base_url: None,
            model: "claude-sonnet-5-5",
            key_env: "ANTHROPIC_API_KEY",
        },
        Preset {
            name: "OpenAI-compatible (local Ollama)",
            provider: LlmProvider::OpenaiCompatible,
            base_url: Some("http://127.0.0.1:11434/v1"),
            model: "qwen3:8b",
            key_env: "OPENAI_API_KEY",
        },
    ]
}

/// A change to the model: a field left out stays as it is; `null` unsets it. The key is
/// write-only: sent to set it, `null` or `""` to remove it from the file.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ModelChange {
    #[serde(default)]
    #[cfg_attr(test, ts(optional))]
    pub provider: Option<LlmProvider>,
    #[serde(default, deserialize_with = "present")]
    #[cfg_attr(test, ts(optional))]
    pub base_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[cfg_attr(test, ts(optional))]
    pub model: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[cfg_attr(test, ts(optional))]
    pub key_env: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    #[cfg_attr(test, ts(optional))]
    pub api_key: Option<Option<String>>,
}

/// A field that is present: `null` as `Some(None)`.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<String>>, D::Error> {
    Ok(Some(Option::<String>::deserialize(d)?))
}

fn blank_to_none(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

impl ModelChange {
    /// The model with this change applied; the key stays unless the change sets or removes it.
    fn applied(&self, to: &ModelConfig) -> ModelConfig {
        let mut m = to.clone();
        if let Some(p) = self.provider {
            m.provider = p;
        }
        if let Some(v) = &self.base_url {
            m.base_url = blank_to_none(v);
        }
        if let Some(v) = &self.model {
            m.model = blank_to_none(v);
        }
        if let Some(v) = &self.key_env {
            m.key_env = blank_to_none(v);
        }
        if let Some(v) = &self.api_key {
            m.api_key = blank_to_none(v);
        }
        m
    }
}

/// What the model file looks like before it is first written: the shipped example.
const MODEL_TEMPLATE: &str = include_str!("../model.conf.example");

/// Writes the model to its file, keeping the file's comments (the example's, for a new file).
fn write_model_file(path: &Path, m: &ModelConfig) -> anyhow::Result<()> {
    use anyhow::Context;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MODEL_TEMPLATE.to_owned(),
        Err(e) => return Err(anyhow::Error::new(e).context(format!("reading {}", path.display()))),
    };
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("parsing {}", path.display()))?;
    let provider = match m.provider {
        LlmProvider::Anthropic => "anthropic",
        LlmProvider::OpenaiCompatible => "openai_compatible",
    };
    let mut set = |key: &str, value: Option<&str>| match value {
        Some(v) => match doc.get_mut(key) {
            Some(toml_edit::Item::Value(old)) => {
                let decor = old.decor().clone();
                *old = v.into();
                *old.decor_mut() = decor;
            }
            _ => {
                doc.insert(key, toml_edit::value(v));
            }
        },
        None => {
            doc.remove(key);
        }
    };
    set("provider", Some(provider));
    set("base_url", m.base_url.as_deref());
    set("model", m.model.as_deref());
    // An unset key stays as an empty line, as in the example, so it is clear where it goes.
    set("api_key", Some(m.api_key.as_deref().unwrap_or("")));
    set("key_env", m.key_env.as_deref());
    let text = doc.to_string();
    ModelConfig::from_toml_str(&text)?;
    let tmp = path.with_extension("conf.new");
    std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

/// Why a file the console changes wasn't written: a file the service may not write (a
/// read-only mount) is a 409 that says so and what to do; anything else is internal.
pub fn not_written(path: &Path, e: anyhow::Error) -> ApiError {
    let denied = e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
    });
    if denied {
        ApiError::Conflict(format!(
            "{} can't be written by the service (a read-only file or mount): change it where it lives and restart the service",
            path.display()
        ))
    } else {
        ApiError::Internal(e)
    }
}

/// Why a change to the config file wasn't saved: a file the service may not write is
/// [`not_written`]'s 409; a change the config refuses (it wouldn't load) is a 409 with why.
pub fn not_saved(path: &Path, e: anyhow::Error) -> ApiError {
    match not_written(path, e) {
        ApiError::Internal(e) => ApiError::Conflict(format!("{e:#}")),
        other => other,
    }
}

/// The result of one tiny call to a model.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ModelTest {
    pub ok: bool,
    /// How long the call took.
    pub ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AppState {
    fn model_file(&self) -> PathBuf {
        self.config
            .librarian
            .llm_file
            .clone()
            .unwrap_or_else(|| self.config.librarian.model_file.clone())
    }

    fn librarian_config_for(&self, m: &ModelConfig) -> crate::config::LibrarianConfig {
        let mut cfg = self.config.librarian.clone();
        cfg.llm = m.clone();
        cfg
    }

    /// The model as it is now.
    pub fn model_view(&self) -> ModelView {
        let state = self.model_state();
        let off_because = self.librarian().err().map(|e| e.to_string());
        ModelView {
            file: self.model_file().display().to_string(),
            provider: state.config.provider,
            base_url: state.config.base_url.clone(),
            model: state.config.model(),
            key_env: state.config.key_env(),
            key: state.key().map(|(_, s)| s),
            librarian: off_because.is_none(),
            off_because,
            presets: presets(),
        }
    }

    /// Changes the model: written to the model file (its comments kept), and the librarian
    /// switched to it at once. A model that can't be called yet (no key, no name) is saved,
    /// and the librarian is off until it can. Audited, without the key.
    pub fn set_model(&self, change: &ModelChange) -> Result<ModelView, ApiError> {
        let mut state = self.model_state();
        let next = change.applied(&state.config);
        if next.provider == LlmProvider::OpenaiCompatible && next.base_url.is_none() {
            return Err(ApiError::BadRequest(
                "an OpenAI-compatible model needs base_url".into(),
            ));
        }
        let file = self.model_file();
        write_model_file(&file, &next).map_err(|e| not_written(&file, e))?;
        state.config = next;
        let key = state.key().map(|(k, _)| k);
        let built = llm::client(&self.librarian_config_for(&state.config), key);
        self.audit.record(
            "admin",
            "model.write",
            serde_json::json!({
                "provider": state.config.provider,
                "base_url": state.config.base_url,
                "model": state.config.model,
                "key_env": state.config.key_env,
                "api_key_changed": change.api_key.is_some(),
            }),
        );
        self.replace_model(state, built);
        Ok(self.model_view())
    }

    /// One tiny call to the model as it is, or as `change` would make it (unsaved), timed.
    pub async fn test_model(&self, change: &ModelChange) -> ModelTest {
        let mut state = self.model_state();
        state.config = change.applied(&state.config);
        let started = Instant::now();
        let client = match llm::client(
            &self.librarian_config_for(&state.config),
            state.key().map(|(k, _)| k),
        ) {
            Ok(c) => c,
            Err(e) => {
                return ModelTest {
                    ok: false,
                    ms: 0,
                    error: Some(e.to_string()),
                };
            }
        };
        let prompt = llm::Prompt {
            system: "You answer with JSON only.".into(),
            user: "Reply with {\"ok\": true}.".into(),
            max_tokens: 64,
            schema: serde_json::json!({
                "type": "object",
                "properties": { "ok": { "type": "boolean" } },
                "required": ["ok"],
                "additionalProperties": false
            }),
            images: Vec::new(),
        };
        let reply = llm::once(client.complete(&prompt)).await;
        let ms = started.elapsed().as_millis() as u64;
        match reply {
            Ok(text) if serde_json::from_str::<Value>(&text).is_ok() => ModelTest {
                ok: true,
                ms,
                error: None,
            },
            Ok(text) => ModelTest {
                ok: false,
                ms,
                error: Some(format!(
                    "the reply isn't JSON: {}",
                    text.chars().take(200).collect::<String>()
                )),
            },
            Err(e) => ModelTest {
                ok: false,
                ms,
                error: Some(format!("{e:#}")),
            },
        }
    }
}

/// An environment variable the service or its compose file reads.
#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct EnvVar {
    pub name: String,
    pub description: String,
    pub set: bool,
    /// Its value, for variables that aren't secret and are set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub secret: bool,
    /// Where it is set: the service's environment, or the compose file's `.env` too.
    pub set_in: &'static str,
}

/// Every variable the service reads, with whether it is set; a secret's value is never shown.
pub fn environment(app: &AppState) -> Vec<EnvVar> {
    let model = app.model_state();
    let cfg = &app.config;
    let shell = "the service's environment (the shell that starts it, or the container's)";
    let compose = "the shell or .env next to docker-compose.yml, read by docker compose";
    let mut vars: Vec<(String, String, bool, &'static str)> = vec![
        ("PINAKES_CONFIG".into(), "The config file to read; config.toml in the working folder when unset.".into(), false, shell),
        (cfg.identity.admin_key_env.clone(), "The service admin key, for scripts and host code calling the admin routes (the console signs in with its account instead). Named by identity.admin_key_env.".into(), true, compose),
        (crate::login::USER_ENV.into(), "The console's user name until it is changed on the console; root when unset.".into(), false, compose),
        (crate::login::PASSWORD_ENV.into(), "The console's password until it is changed on the console; 1234 when unset (change it before others can reach the service).".into(), true, compose),
        ("PINAKES_DEV".into(), "1 turns on the developer chat at /dev.".into(), false, shell),
        ("RUST_LOG".into(), "What the service logs, such as info or info,pinakes=debug.".into(), false, compose),
        ("HF_ENDPOINT".into(), "A Hugging Face mirror for the embedding model's first download.".into(), false, compose),
        (model.config.key_env(), "The librarian's key, read when the model file holds none. Named by key_env in the model file.".into(), true, shell),
        (cfg.neo4j.password_env.clone(), "The Neo4j password, with graph.backend = \"neo4j\". Named by neo4j.password_env.".into(), true, compose),
        ("PINAKES_PORT".into(), "docker compose only: where the service's port is published; 127.0.0.1:8080 when unset, 8080 exposes it on every interface.".into(), false, compose),
        ("PINAKES_GIT_COMMIT".into(), "Build only: the commit the image is built from, named in search traces.".into(), false, compose),
        ("HTTPS_PROXY".into(), "A proxy for the service's outgoing calls (the model, model downloads, acquisition).".into(), true, shell),
        ("NO_PROXY".into(), "Hosts reached without the proxy.".into(), false, shell),
    ];
    // The same variable named twice (a custom key_env equal to another) is listed once.
    let mut seen = std::collections::HashSet::new();
    vars.retain(|(n, ..)| seen.insert(n.clone()));
    vars.into_iter()
        .map(|(name, description, secret, set_in)| {
            let value = if name == "PINAKES_GIT_COMMIT" {
                Some(env!("PINAKES_GIT_COMMIT").to_owned())
            } else {
                std::env::var(&name).ok().filter(|v| !v.is_empty())
            };
            EnvVar {
                set: value.is_some(),
                value: value.filter(|_| !secret),
                name,
                description,
                secret,
                set_in,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_file_keeps_its_comments_and_the_key_stays_write_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model.conf");
        let change = ModelChange {
            model: Some(Some("deepseek-pro".into())),
            api_key: Some(Some("sk-secret".into())),
            ..Default::default()
        };
        let base = ModelConfig::from_toml_str(MODEL_TEMPLATE).unwrap();
        write_model_file(&path, &change.applied(&base)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# The librarian's model"), "{text}");
        assert!(text.contains("model = \"deepseek-pro\""), "{text}");
        assert!(text.contains("# the one model for every step"), "{text}");
        let m = ModelConfig::from_toml_str(&text).unwrap();
        assert_eq!(m.key().as_deref(), Some("sk-secret"));
        // Removing the key leaves the line, empty.
        let clear = ModelChange {
            api_key: Some(None),
            ..Default::default()
        };
        write_model_file(&path, &clear.applied(&m)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("api_key = \"\""), "{text}");
        assert!(!text.contains("sk-secret"));
    }

    #[test]
    fn a_file_the_service_may_not_write_is_a_conflict_that_says_so() {
        let denied = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            .context("writing /app/config.toml");
        match not_written(Path::new("/app/config.toml"), denied) {
            ApiError::Conflict(m) => assert!(m.contains("can't be written"), "{m}"),
            other => panic!("{other:?}"),
        }
        let other = anyhow::anyhow!("disk full");
        assert!(matches!(
            not_written(Path::new("x"), other),
            ApiError::Internal(_)
        ));
    }

    #[test]
    fn a_change_keeps_the_key_unless_it_sets_one() {
        let saved = ModelConfig::from_toml_str(
            "provider = \"anthropic\"
base_url = \"https://api.deepseek.com/anthropic\"
model = \"m\"
api_key = \"sk-saved\"
",
        )
        .unwrap();
        let elsewhere = ModelChange {
            base_url: Some(Some("https://other.example".into())),
            model: Some(Some("m2".into())),
            ..Default::default()
        };
        let m = elsewhere.applied(&saved);
        assert_eq!(m.base_url.as_deref(), Some("https://other.example"));
        assert_eq!(m.api_key.as_deref(), Some("sk-saved"));
        let with_key = ModelChange {
            api_key: Some(Some("sk-new".into())),
            ..Default::default()
        };
        assert_eq!(with_key.applied(&saved).api_key.as_deref(), Some("sk-new"));
        // `null` or a blank key removes it.
        for removed in [None, Some("  ".to_owned())] {
            let change = ModelChange {
                api_key: Some(removed),
                ..Default::default()
            };
            assert_eq!(change.applied(&saved).api_key, None);
        }
    }

    #[test]
    fn the_audit_log_reads_back_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let audit = Audit::new(dir.path());
        audit.record("admin", "a", Value::Null);
        audit.record("admin", "b", serde_json::json!({"x": 1}));
        let recent = audit.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].action, "b");
        assert_eq!(audit.recent(1).len(), 1);
        // A long log is read back from its end, across blocks.
        for i in 0..3000 {
            audit.record(
                "admin",
                &format!("n{i}"),
                serde_json::json!({ "pad": "x".repeat(40) }),
            );
        }
        let last = audit.recent(5);
        let names: Vec<&str> = last.iter().map(|e| e.action.as_str()).collect();
        assert_eq!(names, ["n2999", "n2998", "n2997", "n2996", "n2995"]);
        assert_eq!(audit.recent(4000).len(), 3002);
    }
}
