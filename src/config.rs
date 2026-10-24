/// Proxy configuration for the Anthropic ↔ OpenCode Zen bridge.
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Address to listen on (default: 127.0.0.1:8080)
    pub listen_addr: String,
    /// OpenCode Zen API base URL (default: http://localhost:3000)
    pub opencode_base_url: String,
    /// API key for OpenCode Zen (if required)
    pub opencode_api_key: Option<String>,
    /// Optional API key required from clients connecting to this proxy
    pub inbound_api_key: Option<String>,
    /// Maximum retry attempts after WARP reset
    pub max_retries: u32,
    /// Delay between WARP reset steps (milliseconds)
    pub warp_reset_delay_ms: u64,
}

/// A free model available on OpenCode Zen.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FreeModel {
    /// Model identifier used by OpenCode Zen
    pub id: String,
    /// Display name shown in Claude Code
    pub display_name: String,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:8080".to_string(),
            opencode_base_url: "http://localhost:3000".to_string(),
            opencode_api_key: None,
            inbound_api_key: None,
            max_retries: 3,
            warp_reset_delay_ms: 5000,
        }
    }
}

/// Default free models exposed when upstream model discovery is unavailable.
/// A stale entry here only affects the API surface of `/v1/models` fallbacks —
/// override at runtime with `$OPLIREX_FREE_MODELS` instead of recompiling.
const FALLBACK_FREE_MODELS: &[(&str, &str)] = &[
    ("glm-4.7-free", "GLM 4.7 Free"),
    ("minimax-m2.1-free", "MiniMax M2.1 Free"),
    ("kimi-k2.5-free", "Kimi K2.5 Free"),
    ("qwen-2.5-72b-free", "Qwen 2.5 72B Free"),
    ("llama-3.3-70b-free", "Llama 3.3 70B Free"),
];

/// Parse a comma-separated model id list (e.g. `glm-4.7-free, kimi-k2.5-free`).
/// Returns `None` when nothing usable remains after trimming/blanks.
fn parse_free_model_override(raw: &str) -> Option<Vec<FreeModel>> {
    let ids: Vec<&str> = raw.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();

    if ids.is_empty() {
        return None;
    }

    Some(ids
        .iter()
        .map(|id| FreeModel {
            id: id.to_string(),
            // With no upstream metadata available, the id doubles as display name.
            display_name: id.to_string(),
        })
        .collect())
}

impl ProxyConfig {
    /// Returns the list of free models mapped to Anthropic schema.
    ///
    /// Defaults to the built-in fallback list; `$OPLIREX_FREE_MODELS`
    /// (comma-separated model ids) overrides it without recompiling.
    pub fn free_models() -> Vec<FreeModel> {
        if let Ok(raw) = std::env::var("OPLIREX_FREE_MODELS") {
            if let Some(overridden) = parse_free_model_override(&raw) {
                return overridden;
            }
        }
        FALLBACK_FREE_MODELS
            .iter()
            .map(|(id, display_name)| FreeModel {
                id: id.to_string(),
                display_name: display_name.to_string(),
            })
            .collect()
    }

    /// Build Anthropic-compatible models response JSON.
    pub fn models_response() -> serde_json::Value {
        let models: Vec<serde_json::Value> = Self::free_models()
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "object": "model",
                    "created": 0,
                    "owned_by": "opencode-zen"
                })
            })
            .collect();

        serde_json::json!({
            "object": "list",
            "data": models
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppConfig {
    pub listen: String,
    pub upstream: String,
    pub max_retries: u32,
    pub warp_delay: u64,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".to_string(),
            upstream: "http://localhost:3000".to_string(),
            max_retries: 3,
            warp_delay: 5000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comma_separated_override() {
        let models = parse_free_model_override("glm-4.7-free, minimax-m2.1-free,,").unwrap();
        assert_eq!(models.len(), 2, "empty entries are dropped");
        assert_eq!(models[0].id, "glm-4.7-free");
        assert_eq!(models[1].id, "minimax-m2.1-free");
    }

    #[test]
    fn blank_override_is_ignored() {
        assert!(parse_free_model_override("").is_none());
        assert!(parse_free_model_override("   ,  ,  ,").is_none());
    }
}
