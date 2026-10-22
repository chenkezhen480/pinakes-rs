//! Any server with an OpenAI-style `POST /chat/completions`, such as a local Ollama or vLLM.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{LlmClient, Prompt, post_json};

pub struct OpenAiCompatible {
    http: reqwest::Client,
    key: Option<String>,
    model: String,
    url: String,
}

impl OpenAiCompatible {
    /// `base_url` is the API root, such as `http://localhost:11434/v1`.
    pub fn new(
        key: Option<String>,
        model: &str,
        base_url: &str,
        timeout: Duration,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder().timeout(timeout).build()?,
            key,
            model: model.to_owned(),
            url: format!("{}/chat/completions", base_url.trim_end_matches('/')),
        })
    }
}

#[async_trait]
impl LlmClient for OpenAiCompatible {
    async fn complete(&self, prompt: &Prompt) -> anyhow::Result<String> {
        Ok(self.complete_counted(prompt).await?.0)
    }

    async fn complete_counted(&self, prompt: &Prompt) -> anyhow::Result<(String, u64)> {
        let user = if prompt.images.is_empty() {
            json!(prompt.user)
        } else {
            let mut parts: Vec<Value> = prompt
                .images
                .iter()
                .map(|i| {
                    json!({ "type": "image_url", "image_url": {
                        "url": format!("data:{};base64,{}", i.media_type, i.base64()) } })
                })
                .collect();
            parts.push(json!({ "type": "text", "text": prompt.user }));
            Value::Array(parts)
        };
        let body = json!({
            "model": self.model,
            "max_tokens": prompt.max_tokens,
            "messages": [
                { "role": "system", "content": prompt.system },
                { "role": "user", "content": user },
            ],
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "reply", "strict": true, "schema": prompt.schema },
            },
        });
        let reply = post_json(
            || {
                let req = self.http.post(&self.url).json(&body);
                match &self.key {
                    Some(k) => req.bearer_auth(k),
                    None => req,
                }
            },
            "openai_compatible",
        )
        .await?;
        let text = text_of(&reply)?;
        let used = super::reported_usage(&reply).unwrap_or_else(|| {
            super::estimate_tokens(&prompt.system)
                + super::estimate_tokens(&prompt.user)
                + super::estimate_tokens(&text)
        });
        Ok((text, used))
    }
}

fn text_of(reply: &Value) -> anyhow::Result<String> {
    let choice = &reply["choices"][0];
    if choice["finish_reason"] == "length" {
        anyhow::bail!(super::CUT_OFF);
    }
    let text = choice["message"]["content"].as_str().unwrap_or_default();
    anyhow::ensure!(!text.trim().is_empty(), "the reply has no text");
    Ok(text.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_first_choice() {
        let ok =
            json!({ "choices": [{ "finish_reason": "stop", "message": { "content": "{}" } }] });
        assert_eq!(text_of(&ok).unwrap(), "{}");
        let cut =
            json!({ "choices": [{ "finish_reason": "length", "message": { "content": "{" } }] });
        assert!(text_of(&cut).is_err());
    }
}
