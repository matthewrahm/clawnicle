use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LlmRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: String,
}

impl LlmMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::System,
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::User,
            content: content.into(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: LlmRole::Assistant,
            content: content.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<LlmMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl LlmRequest {
    pub fn new(model: impl Into<String>, messages: Vec<LlmMessage>) -> Self {
        Self {
            model: model.into(),
            messages,
            temperature: None,
            max_tokens: None,
        }
    }

    /// Stable 64-hex-char SHA-256 of the serialized request. Serves as the
    /// prompt-cache key: two requests with identical models, messages, and
    /// parameters produce the same hash.
    pub fn prompt_hash(&self) -> String {
        let bytes = serde_json::to_vec(self)
            .expect("LlmRequest always serializes (no non-string map keys, no NaN)");
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let digest = hasher.finalize();
        hex(&digest)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmResponse {
    pub model: String,
    pub content: String,
    pub tokens_in: u32,
    pub tokens_out: u32,
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_hash_is_stable_across_calls() {
        let req = LlmRequest::new(
            "claude-haiku-4-5",
            vec![LlmMessage::user("hi")],
        );
        assert_eq!(req.prompt_hash(), req.prompt_hash());
    }

    #[test]
    fn different_messages_produce_different_hashes() {
        let a = LlmRequest::new("m", vec![LlmMessage::user("hi")]);
        let b = LlmRequest::new("m", vec![LlmMessage::user("bye")]);
        assert_ne!(a.prompt_hash(), b.prompt_hash());
    }

    #[test]
    fn different_models_produce_different_hashes() {
        let messages = vec![LlmMessage::user("hi")];
        let a = LlmRequest::new("claude-haiku-4-5", messages.clone());
        let b = LlmRequest::new("claude-sonnet-4-6", messages);
        assert_ne!(a.prompt_hash(), b.prompt_hash());
    }

    #[test]
    fn hash_length_is_64_hex_chars() {
        let req = LlmRequest::new("m", vec![LlmMessage::user("x")]);
        let h = req.prompt_hash();
        assert_eq!(h.len(), 64);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
