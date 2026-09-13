use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::str::FromStr;

use crate::transform::{
    anthropic_to_opencode_request, opencode_response_to_anthropic, opencode_stream_to_anthropic,
};

/// Supported upstream providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Opencode,
    #[serde(alias = "openai")]
    OpenAI,
    Anthropic,
}

impl Default for Provider {
    fn default() -> Self {
        Self::Opencode
    }
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Provider::Opencode => write!(f, "opencode"),
            Provider::OpenAI => write!(f, "openai"),
            Provider::Anthropic => write!(f, "anthropic"),
        }
    }
}

impl FromStr for Provider {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "opencode" | "opencode-zen" | "zen" => Ok(Provider::Opencode),
            "openai" => Ok(Provider::OpenAI),
            "anthropic" => Ok(Provider::Anthropic),
            other => Err(format!(
                "unknown provider '{}' (expected opencode, openai, anthropic)",
                other
            )),
        }
    }
}

impl Provider {
    pub fn is_openai_compatible(self) -> bool {
        matches!(self, Provider::Opencode | Provider::OpenAI)
    }

    pub fn upstream_path(self) -> &'static str {
        match self {
            Provider::Anthropic => "/v1/messages",
            Provider::Opencode | Provider::OpenAI => "/v1/chat/completions",
        }
    }
}

/// Trait required by spec: translate request based on provider.
pub trait RequestTranslator {
    fn translate_request(&self, body: &Value) -> Value;
}

impl RequestTranslator for Provider {
    fn translate_request(&self, body: &Value) -> Value {
        translate_request(*self, body)
    }
}

/// Translate Anthropic-style request body to provider-specific upstream body.
///
/// - Opencode / OpenAI: OpenAI-compatible via `anthropic_to_opencode_request`
/// - Anthropic: passthrough (native Anthropic API)
pub fn translate_request(provider: Provider, body: &Value) -> Value {
    match provider {
        Provider::Opencode | Provider::OpenAI => anthropic_to_opencode_request(body),
        Provider::Anthropic => body.clone(),
    }
}

/// Translate upstream response to Anthropic response format.
pub fn translate_response(provider: Provider, body: &Value, model: &str) -> Value {
    match provider {
        Provider::Opencode | Provider::OpenAI => opencode_response_to_anthropic(body, model),
        Provider::Anthropic => body.clone(),
    }
}

/// Translate streaming chunk to Anthropic SSE format.
///
/// Returns `Some(transformed)` if chunk should be forwarded, `None` to skip.
pub fn translate_stream_chunk(provider: Provider, line: &str, model: &str) -> Option<String> {
    match provider {
        Provider::Opencode | Provider::OpenAI => opencode_stream_to_anthropic(line, model),
        Provider::Anthropic => {
            // Anthropic native is already in correct SSE format — passthrough.
            if line.is_empty() || line.starts_with(':') {
                return None;
            }
            // Forward SSE lines as-is.
            if line.starts_with("data:") || line.starts_with("event:") {
                let mut out = line.to_string();
                out.push('\n');
                return Some(out);
            }
            None
        }
    }
}
