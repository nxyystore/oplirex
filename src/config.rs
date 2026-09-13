use crate::providers::Provider;

/// Proxy configuration for the OpenCode proxy daemon (Anthropic + OpenAI).
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// Address to listen on (default: 127.0.0.1:8080)
    pub listen_addr: String,
    /// Primary OpenCode Zen API base URL (default: http://localhost:3000)
    pub opencode_base_url: String,
    /// Additional upstream base URLs for round-robin load splitting
    pub extra_upstreams: Vec<String>,
    /// API key for OpenCode Zen (if required, sent upstream)
    pub opencode_api_key: Option<String>,
    /// API key clients must present (incoming `Authorization: Bearer` or `x-api-key`); None = open
    pub require_api_key: Option<String>,
    /// Maximum retry attempts after WARP reset
    pub max_retries: u32,
    /// Delay between WARP reset steps (milliseconds)
    pub warp_reset_delay_ms: u64,
    /// Optional shell hook to run on 429 (P2.9)
    pub hook_on_429: Option<String>,
    /// Upstream provider selection (P2.7)
    pub provider: Provider,
    /// Model IDs to try in order when the requested model is rate-limited
    pub fallback_models: Vec<String>,
    /// Consecutive 429s that trip the circuit breaker
    pub circuit_threshold: u32,
    /// Seconds the circuit stays open (rejecting with 503) once tripped
    pub circuit_cooldown_secs: u64,
}

impl ProxyConfig {
    /// All upstreams: primary first, then extras. Never empty.
    pub fn effective_upstreams(&self) -> Vec<String> {
        let mut out = vec![self.opencode_base_url.clone()];
        out.extend(self.extra_upstreams.iter().cloned());
        out
    }
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
            extra_upstreams: Vec::new(),
            opencode_api_key: None,
            require_api_key: None,
            max_retries: 3,
            warp_reset_delay_ms: 5000,
            hook_on_429: None,
            provider: Provider::default(),
            fallback_models: Vec::new(),
            circuit_threshold: 5,
            circuit_cooldown_secs: 60,
        }
    }
}

impl ProxyConfig {
    /// Returns the list of free models mapped to Anthropic schema.
    pub fn free_models() -> Vec<FreeModel> {
        vec![
            FreeModel {
                id: "glm-4.7-free".to_string(),
                display_name: "GLM 4.7 Free".to_string(),
            },
            FreeModel {
                id: "minimax-m2.1-free".to_string(),
                display_name: "MiniMax M2.1 Free".to_string(),
            },
            FreeModel {
                id: "kimi-k2.5-free".to_string(),
                display_name: "Kimi K2.5 Free".to_string(),
            },
            FreeModel {
                id: "qwen-2.5-72b-free".to_string(),
                display_name: "Qwen 2.5 72B Free".to_string(),
            },
            FreeModel {
                id: "llama-3.3-70b-free".to_string(),
                display_name: "Llama 3.3 70B Free".to_string(),
            },
        ]
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

fn default_provider() -> crate::providers::Provider {
    crate::providers::Provider::default()
}

fn default_circuit_threshold() -> u32 {
    5
}

fn default_circuit_cooldown_secs() -> u64 {
    60
}

fn default_preemptive_window() -> usize {
    10
}

fn default_preemptive_threshold() -> u32 {
    3
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppConfig {
    pub listen: String,
    pub upstream: String,
    #[serde(default)]
    pub extra_upstreams: Vec<String>,
    pub max_retries: u32,
    pub warp_delay: u64,
    #[serde(default = "default_provider")]
    pub provider: crate::providers::Provider,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Incoming client key requirement (None = open proxy)
    #[serde(default)]
    pub require_api_key: Option<String>,
    /// Shell hook on 429
    #[serde(default)]
    pub hook_on_429: Option<String>,
    /// Model failover list for 429s
    #[serde(default)]
    pub fallback_models: Vec<String>,
    #[serde(default = "default_circuit_threshold")]
    pub circuit_threshold: u32,
    #[serde(default = "default_circuit_cooldown_secs")]
    pub circuit_cooldown_secs: u64,
    /// Watch-mode preemptive rotation: window of recent checks...
    #[serde(default = "default_preemptive_window")]
    pub preemptive_window: usize,
    /// ...and 429s within it that trigger a preemptive WARP rotation
    #[serde(default = "default_preemptive_threshold")]
    pub preemptive_threshold: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8080".to_string(),
            upstream: "http://localhost:3000".to_string(),
            extra_upstreams: Vec::new(),
            max_retries: 3,
            warp_delay: 5000,
            provider: crate::providers::Provider::default(),
            api_key: None,
            require_api_key: None,
            hook_on_429: None,
            fallback_models: Vec::new(),
            circuit_threshold: default_circuit_threshold(),
            circuit_cooldown_secs: default_circuit_cooldown_secs(),
            preemptive_window: default_preemptive_window(),
            preemptive_threshold: default_preemptive_threshold(),
        }
    }
}

impl AppConfig {
    pub fn into_proxy_config(self) -> ProxyConfig {
        ProxyConfig {
            listen_addr: self.listen,
            opencode_base_url: self.upstream,
            extra_upstreams: self.extra_upstreams,
            opencode_api_key: self.api_key,
            require_api_key: self.require_api_key,
            max_retries: self.max_retries,
            warp_reset_delay_ms: self.warp_delay,
            hook_on_429: self.hook_on_429,
            provider: self.provider,
            fallback_models: self.fallback_models,
            circuit_threshold: self.circuit_threshold,
            circuit_cooldown_secs: self.circuit_cooldown_secs,
        }
    }
}
