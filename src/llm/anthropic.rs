//! The Claude Messages API (`POST /v1/messages`), and servers that speak it, such as
//! DeepSeek's Anthropic-compatible endpoint (`https://api.deepseek.com/anthropic`).
//!
//! The reply's shape is always enforced by the API, in the way the endpoint supports:
//!
//! - Anthropic itself (no `base_url`): structured outputs, `output_config.format`.
//! - Any other endpoint (a `base_url`): compatible servers don't all take `output_config.format`
//!   (DeepSeek takes only `output_config.effort`), but they do take tools, so the reply's schema
//!   becomes the input schema of one `reply` tool the model is made to call
//!   (`tool_choice: {type: "tool"}`), and the reply is that call's input. Thinking is turned
//!   off there, since a forced tool call can't run in thinking mode.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{LlmClient, Prompt, post_json};

const DEFAULT_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
/// The tool a compatible endpoint's model answers through.
const REPLY_TOOL: &str = "reply";

/// How the reply's schema is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `output_config.format` with a JSON schema.
    OutputFormat,
    /// One tool whose input schema is the reply's, which the model must call.
    ReplyTool,
}

pub struct Anthropic {
    http: reqwest::Client,
    key: String,
    model: String,
    url: String,
    shape: Shape,
}

impl Anthropic {
    /// `base_url` is the API root without `/v1`, such as `https://api.deepseek.com/anthropic`;
    /// `None` is Anthropic's own API.
    pub fn new(
        key: String,
        model: &str,
        base_url: Option<&str>,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        let base = base_url.unwrap_or(DEFAULT_BASE).trim_end_matches('/');
        Ok(Self {
            http: reqwest::Client::builder().timeout(timeout).build()?,
            key,
            model: model.to_owned(),
            url: format!("{base}/v1/messages"),
            shape: if base_url.is_some() {
                Shape::ReplyTool
            } else {
                Shape::OutputFormat
            },
        })
    }
}

/// The request body for a prompt.
fn body(model: &str, prompt: &Prompt, shape: Shape) -> Value {
    let mut body = json!({
        "model": model,
        "max_tokens": prompt.max_tokens,
        "system": prompt.system,
        "messages": [{ "role": "user", "content": prompt.user }],
    });
    match shape {
        Shape::OutputFormat => {
            // The calls of one job share a long prefix (the rules and the whole document), so
            // Anthropic caches it; DeepSeek caches prefixes on its own.
            body["cache_control"] = json!({ "type": "ephemeral" });
            body["output_config"] =
                json!({ "format": { "type": "json_schema", "schema": prompt.schema } });
        }
        Shape::ReplyTool => {
            body["tools"] = json!([{
                "name": REPLY_TOOL,
                "description": "Give your answer. The input is the whole answer.",
                "input_schema": prompt.schema,
            }]);
            body["tool_choice"] = json!({ "type": "tool", "name": REPLY_TOOL });
            // A forced tool call can't run in thinking mode (DeepSeek answers 400), so the
            // shape wins over thinking on these endpoints.
            body["thinking"] = json!({ "type": "disabled" });
        }
    }
    body
}

#[async_trait]
impl LlmClient for Anthropic {
    async fn complete(&self, prompt: &Prompt) -> anyhow::Result<String> {
        Ok(self.complete_counted(prompt).await?.0)
    }

    async fn complete_counted(&self, prompt: &Prompt) -> anyhow::Result<(String, u64)> {
        let body = body(&self.model, prompt, self.shape);
        let reply = post_json(
            || {
                self.http
                    .post(&self.url)
                    .header("x-api-key", &self.key)
                    .header("anthropic-version", API_VERSION)
                    .json(&body)
            },
            "anthropic",
        )
        .await?;
        let text = reply_of(&reply, self.shape)?;
        let used = super::reported_usage(&reply).unwrap_or_else(|| {
            super::estimate_tokens(&prompt.system)
                + super::estimate_tokens(&prompt.user)
                + super::estimate_tokens(&text)
        });
        Ok((text, used))
    }
}

/// The reply as JSON text, after checking why the model stopped: the text blocks joined, or
/// the input of the call to the reply tool.
fn reply_of(reply: &Value, shape: Shape) -> anyhow::Result<String> {
    match reply["stop_reason"].as_str() {
        Some("refusal") => anyhow::bail!("the model declined the request"),
        Some("max_tokens") => anyhow::bail!("the reply was cut off at max_tokens"),
        _ => {}
    }
    let mut blocks = reply["content"].as_array().into_iter().flatten();
    match shape {
        Shape::OutputFormat => {
            let text: String = blocks
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect();
            anyhow::ensure!(!text.trim().is_empty(), "the reply has no text");
            Ok(text)
        }
        Shape::ReplyTool => {
            let call = blocks
                .rfind(|b| b["type"] == "tool_use" && b["name"] == REPLY_TOOL)
                .ok_or_else(|| anyhow::anyhow!("the model didn't answer with the reply tool"))?;
            Ok(call["input"].to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> Prompt {
        Prompt {
            system: "s".into(),
            user: "u".into(),
            max_tokens: 100,
            schema: json!({ "type": "object", "properties": {}, "required": [],
                            "additionalProperties": false }),
        }
    }

    #[test]
    fn anthropic_itself_gets_output_format() {
        let c = Anthropic::new("k".into(), "m", None, Duration::from_secs(1)).unwrap();
        assert_eq!(c.shape, Shape::OutputFormat);
        assert_eq!(c.url, "https://api.anthropic.com/v1/messages");
        let b = body("m", &prompt(), c.shape);
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(b["output_config"]["format"]["schema"], prompt().schema);
        assert!(b.get("tools").is_none());
        assert_eq!(b["cache_control"]["type"], "ephemeral");

        let ok = json!({ "stop_reason": "end_turn", "content": [
            { "type": "thinking", "thinking": "" },
            { "type": "text", "text": "{\"edits\":" }, { "type": "text", "text": "[]}" } ] });
        assert_eq!(
            reply_of(&ok, Shape::OutputFormat).unwrap(),
            "{\"edits\":[]}"
        );
    }

    #[test]
    fn compatible_endpoints_answer_through_the_reply_tool() {
        let c = Anthropic::new(
            "k".into(),
            "deepseek-v4-pro",
            Some("https://api.deepseek.com/anthropic/"),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(c.shape, Shape::ReplyTool);
        assert_eq!(c.url, "https://api.deepseek.com/anthropic/v1/messages");
        let b = body("deepseek-v4-pro", &prompt(), c.shape);
        assert!(b.get("output_config").is_none());
        assert!(
            b.get("cache_control").is_none(),
            "compatible endpoints cache on their own"
        );
        assert_eq!(b["tools"][0]["input_schema"], prompt().schema);
        assert_eq!(b["tool_choice"], json!({ "type": "tool", "name": "reply" }));
        assert_eq!(b["thinking"], json!({ "type": "disabled" }));

        let ok = json!({ "stop_reason": "tool_use", "content": [
            { "type": "text", "text": "Here it is." },
            { "type": "tool_use", "id": "t1", "name": "reply", "input": { "edits": [] } } ] });
        assert_eq!(reply_of(&ok, Shape::ReplyTool).unwrap(), "{\"edits\":[]}");
        let no_call = json!({ "stop_reason": "end_turn", "content": [
            { "type": "text", "text": "{\"edits\": []}" } ] });
        assert!(reply_of(&no_call, Shape::ReplyTool).is_err());
    }

    #[test]
    fn stop_reasons_fail_the_call() {
        let cut = json!({ "stop_reason": "max_tokens",
                          "content": [{ "type": "text", "text": "{" }] });
        assert!(reply_of(&cut, Shape::OutputFormat).is_err());
        let refused = json!({ "stop_reason": "refusal", "content": [] });
        assert!(reply_of(&refused, Shape::ReplyTool).is_err());
    }
}
