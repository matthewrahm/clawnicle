//! Anthropic Messages API provider.
//!
//! Builds a request body matching the Anthropic `/v1/messages` schema,
//! sends it via `reqwest`, parses the response into [`LlmResponse`].
//!
//! System messages (`LlmRole::System`) in the input `LlmRequest` are
//! flattened into Anthropic's top-level `system` field (joined by blank
//! lines). User and assistant messages pass through unchanged.
//!
//! Requires the `anthropic` feature.

use std::future::Future;
use std::time::Duration;

use clawnicle_core::{Error, LlmRequest, LlmResponse, LlmRole, Result};
use serde::{Deserialize, Serialize};

use crate::provider::LlmProvider;

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 1024;

pub struct AnthropicProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| Error::Tool(format!("reqwest client: {e}")))?;
        Ok(Self {
            client,
            api_key: api_key.into(),
            base_url: ANTHROPIC_URL.to_string(),
        })
    }

    /// Override the API endpoint. Useful for integration tests against a
    /// local mock server.
    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }
}

impl LlmProvider for AnthropicProvider {
    fn complete<'a>(
        &'a self,
        request: &'a LlmRequest,
    ) -> impl Future<Output = Result<LlmResponse>> + Send + 'a {
        async move {
            let body = build_request_body(request);
            let resp = self
                .client
                .post(&self.base_url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_API_VERSION)
                .header("content-type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Tool(format!("anthropic send: {e}")))?;

            let status = resp.status();
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| Error::Tool(format!("anthropic body: {e}")))?;

            if !status.is_success() {
                let preview = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_string();
                return Err(Error::Tool(format!(
                    "anthropic http {}: {preview}",
                    status.as_u16()
                )));
            }

            parse_response(&bytes)
        }
    }
}

pub(crate) fn build_request_body(request: &LlmRequest) -> AnthropicRequest {
    let mut system = String::new();
    let mut messages = Vec::with_capacity(request.messages.len());
    for m in &request.messages {
        match m.role {
            LlmRole::System => {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&m.content);
            }
            LlmRole::User => messages.push(AnthropicMessage {
                role: "user",
                content: m.content.clone(),
            }),
            LlmRole::Assistant => messages.push(AnthropicMessage {
                role: "assistant",
                content: m.content.clone(),
            }),
        }
    }

    AnthropicRequest {
        model: request.model.clone(),
        max_tokens: request.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS),
        messages,
        system: if system.is_empty() { None } else { Some(system) },
        temperature: request.temperature,
    }
}

pub(crate) fn parse_response(bytes: &[u8]) -> Result<LlmResponse> {
    let raw: AnthropicResponse = serde_json::from_slice(bytes)
        .map_err(|e| Error::Tool(format!("anthropic parse: {e}")))?;

    let content = raw
        .content
        .into_iter()
        .find_map(|b| match b {
            AnthropicContentBlock::Text { text } => Some(text),
        })
        .unwrap_or_default();

    Ok(LlmResponse {
        model: raw.model,
        content,
        tokens_in: raw.usage.input_tokens,
        tokens_out: raw.usage.output_tokens,
    })
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicRequest {
    pub model: String,
    pub max_tokens: u32,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    model: String,
    content: Vec<AnthropicContentBlock>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnthropicContentBlock {
    Text { text: String },
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clawnicle_core::{LlmMessage, LlmRequest};

    #[test]
    fn build_request_hoists_system_messages_to_top_level_field() {
        let req = LlmRequest::new(
            "claude-haiku-4-5",
            vec![
                LlmMessage::system("you are terse"),
                LlmMessage::system("no emojis"),
                LlmMessage::user("hi"),
            ],
        );
        let body = build_request_body(&req);
        assert_eq!(body.model, "claude-haiku-4-5");
        assert_eq!(body.messages.len(), 1);
        assert_eq!(body.messages[0].role, "user");
        assert_eq!(body.system.as_deref(), Some("you are terse\n\nno emojis"));
    }

    #[test]
    fn build_request_default_max_tokens_when_none() {
        let req = LlmRequest::new("m", vec![LlmMessage::user("hi")]);
        let body = build_request_body(&req);
        assert_eq!(body.max_tokens, DEFAULT_MAX_TOKENS);
    }

    #[test]
    fn build_request_user_specified_max_tokens_wins() {
        let mut req = LlmRequest::new("m", vec![LlmMessage::user("hi")]);
        req.max_tokens = Some(64);
        let body = build_request_body(&req);
        assert_eq!(body.max_tokens, 64);
    }

    #[test]
    fn parse_response_extracts_text_and_usage() {
        let raw = br#"{
            "id": "msg_01",
            "type": "message",
            "role": "assistant",
            "model": "claude-haiku-4-5",
            "content": [
                {"type": "text", "text": "hello world"}
            ],
            "usage": {
                "input_tokens": 42,
                "output_tokens": 7
            }
        }"#;
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.content, "hello world");
        assert_eq!(resp.tokens_in, 42);
        assert_eq!(resp.tokens_out, 7);
        assert_eq!(resp.model, "claude-haiku-4-5");
    }

    #[test]
    fn parse_response_handles_multiple_content_blocks() {
        let raw = br#"{
            "model": "m",
            "content": [
                {"type": "text", "text": "first"}
            ],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }"#;
        let resp = parse_response(raw).unwrap();
        assert_eq!(resp.content, "first");
    }

    #[test]
    fn parse_response_error_on_malformed_json() {
        let res = parse_response(b"not-json");
        assert!(matches!(res, Err(Error::Tool(_))));
    }
}
