use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use futures::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::config::ProxyConfig;
use crate::hooks::{run_hook, HookEnv};
use crate::metrics::{RecordedRequest, METRICS};
use crate::providers::{translate_request, translate_response, translate_stream_chunk};
use crate::warp::WarpResolver;

/// Shared proxy state: HTTP client, config, and WARP resolver.
/// `rr_counter` rotates across effective upstreams; `consecutive_429` and
/// `circuit_open_until` implement the circuit breaker. All mutated under lock.
pub struct ProxyState {
    pub config: ProxyConfig,
    pub client: Client,
    pub warp_resolver: WarpResolver,
    pub rr_counter: usize,
    pub consecutive_429: u32,
    pub circuit_open_until: Option<Instant>,
}

impl ProxyState {
    pub fn new(config: ProxyConfig, client: Client, warp_resolver: WarpResolver) -> Self {
        Self {
            config,
            client,
            warp_resolver,
            rr_counter: 0,
            consecutive_429: 0,
            circuit_open_until: None,
        }
    }
}

/// Per-request context for latency + history recording.
struct ReqCtx {
    endpoint: &'static str,
    model: String,
    start: Instant,
}

impl ReqCtx {
    fn finish(&self, status: u16) {
        let latency_ms = self.start.elapsed().as_millis() as u64;
        METRICS.record_latency(latency_ms);
        let ts_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        METRICS.push_history(RecordedRequest {
            ts_millis,
            endpoint: self.endpoint.to_string(),
            model: self.model.clone(),
            status,
            latency_ms,
        });
    }
}

/// Snapshot of everything a request needs, plus round-robin upstream pick.
#[derive(Clone)]
struct Snapshot {
    client: Client,
    upstream: String,
    upstream_count: usize,
    api_key: Option<String>,
    max_retries: u32,
    reset_delay: u64,
    hook_on_429: Option<String>,
    provider: crate::providers::Provider,
    fallback_models: Vec<String>,
    circuit_threshold: u32,
    circuit_cooldown_secs: u64,
    require_api_key: Option<String>,
}

/// Admission gate: circuit breaker + config snapshot + upstream rotation.
enum Gate {
    Pass(Snapshot),
    /// Circuit open; value = remaining cooldown seconds.
    CircuitOpen(u64),
}

async fn gate(state: &Arc<Mutex<ProxyState>>) -> Gate {
    let mut guard = state.lock().await;
    if let Some(open_until) = guard.circuit_open_until {
        let now = Instant::now();
        if now < open_until {
            let remaining = open_until.duration_since(now).as_secs().max(1);
            return Gate::CircuitOpen(remaining);
        }
        // Cooldown expired: half-open, allow this request through.
        guard.circuit_open_until = None;
        guard.consecutive_429 = 0;
    }
    let upstreams = guard.config.effective_upstreams();
    let idx = guard.rr_counter % upstreams.len();
    guard.rr_counter = guard.rr_counter.wrapping_add(1);
    let snap = Snapshot {
        client: guard.client.clone(),
        upstream: upstreams[idx].clone(),
        upstream_count: upstreams.len(),
        api_key: guard.config.opencode_api_key.clone(),
        max_retries: guard.config.max_retries,
        reset_delay: guard.config.warp_reset_delay_ms,
        hook_on_429: guard.config.hook_on_429.clone(),
        provider: guard.config.provider,
        fallback_models: guard.config.fallback_models.clone(),
        circuit_threshold: guard.config.circuit_threshold,
        circuit_cooldown_secs: guard.config.circuit_cooldown_secs,
        require_api_key: guard.config.require_api_key.clone(),
    };
    Gate::Pass(snap)
}

/// Rotate to the next upstream (used when the current one is unreachable).
async fn rotate_upstream(state: &Arc<Mutex<ProxyState>>) -> String {
    let mut guard = state.lock().await;
    let upstreams = guard.config.effective_upstreams();
    let idx = guard.rr_counter % upstreams.len();
    guard.rr_counter = guard.rr_counter.wrapping_add(1);
    upstreams[idx].clone()
}

async fn note_success(state: &Arc<Mutex<ProxyState>>) {
    state.lock().await.consecutive_429 = 0;
}

/// Record a 429. Returns true when this 429 trips the circuit breaker.
async fn note_429(state: &Arc<Mutex<ProxyState>>, threshold: u32, cooldown_secs: u64) -> bool {
    let mut guard = state.lock().await;
    guard.consecutive_429 += 1;
    if guard.consecutive_429 >= threshold.max(1) {
        guard.circuit_open_until =
            Some(Instant::now() + std::time::Duration::from_secs(cooldown_secs.max(1)));
        guard.consecutive_429 = 0;
        METRICS.inc_circuit_open();
        return true;
    }
    false
}

/// Check the incoming client key when `require_api_key` is set.
/// Accepts `Authorization: Bearer <key>`, a bare `Authorization: <key>`,
/// or `x-api-key: <key>` (what Claude Code sends). `None` = open proxy.
fn check_incoming_auth(require_api_key: &Option<String>, headers: &HeaderMap) -> bool {
    let Some(expected) = require_api_key.as_deref() else {
        return true;
    };
    let present = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
                .unwrap_or(v)
                .trim()
        })
        .or_else(|| {
            headers
                .get("x-api-key")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.trim())
        });
    matches!(present, Some(got) if got == expected)
}

/// Ordered model attempts: requested first, then configured fallbacks (deduped).
fn candidate_models(requested: &str, fallbacks: &[String]) -> Vec<String> {
    let mut out = vec![requested.to_string()];
    for f in fallbacks {
        if f != requested && !out.iter().any(|m| m == f) {
            out.push(f.clone());
        }
    }
    out
}

/// Hint appended to upstream connection errors so users check `opencode serve`.
fn upstream_hint(base_url: &str) -> String {
    format!(
        " (upstream {} unreachable — is `opencode serve` running there?)",
        base_url
    )
}

/// Pull OpenAI-shaped `usage` out of an upstream body into token metrics.
/// Non-streaming responses only; streaming chunks rarely carry totals.
fn record_upstream_usage(model: &str, upstream_body: &Value) {
    let prompt = upstream_body
        .pointer("/usage/prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let completion = upstream_body
        .pointer("/usage/completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    METRICS.record_usage(model, prompt, completion);
}

/// Fire the on-429 hook with request context (best-effort, non-blocking).
fn fire_429_hook(hook: &Option<String>, retry: u32, model: &str, upstream: &str) {
    if let Some(hook) = hook {
        let mut env = HookEnv::new(retry);
        env.model = Some(model.to_string());
        env.upstream = Some(upstream.to_string());
        run_hook(hook, env);
    }
}

pub async fn handle_models(State(state): State<Arc<Mutex<ProxyState>>>) -> impl IntoResponse {
    METRICS.inc_request();
    let start = std::time::Instant::now();
    let (client, base_url, api_key) = {
        let state_guard = state.lock().await;
        (
            state_guard.client.clone(),
            state_guard.config.opencode_base_url.clone(),
            state_guard.config.opencode_api_key.clone(),
        )
    };

    let models_url = format!("{}/v1/models", base_url.trim_end_matches('/'));

    let mut request = client
        .get(&models_url)
        .header("Accept", "application/json");

    if let Some(key) = &api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    let result = match request.send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<Value>().await {
                Ok(upstream_models) => {
                    let transformed = transform_models_to_anthropic(&upstream_models);
                    (StatusCode::OK, Json(transformed))
                }
                Err(e) => {
                    error!("Failed to parse models response: {}", e);
                    (StatusCode::OK, Json(ProxyConfig::models_response()))
                }
            }
        }
        Ok(resp) => {
            warn!("Upstream /v1/models returned status: {}", resp.status());
            (StatusCode::OK, Json(ProxyConfig::models_response()))
        }
        Err(e) => {
            warn!("Failed to fetch models from upstream: {}", e);
            (StatusCode::OK, Json(ProxyConfig::models_response()))
        }
    };
    METRICS.record_latency(start.elapsed().as_millis() as u64);
    result
}

fn transform_models_to_anthropic(upstream: &Value) -> Value {
    let data = upstream.get("data").and_then(|v| v.as_array());

    let models: Vec<Value> = match data {
        Some(models_array) => models_array
            .iter()
            .filter_map(|m| {
                let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
                if id.is_empty() {
                    return None;
                }

                let display_name = match m.get("name").and_then(|v| v.as_str()) {
                    Some(name) => name.to_string(),
                    None => id.replace('-', " ")
                        .split_whitespace()
                        .map(|w| {
                            let mut chars = w.chars();
                            match chars.next() {
                                None => String::new(),
                                Some(c) => {
                                    let upper = c.to_uppercase().collect::<String>();
                                    format!("{}{}", upper, chars.as_str())
                                }
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                };

                let context_window = m
                    .get("context_window")
                    .and_then(|v| v.as_u64())
                    .or_else(|| m.get("max_tokens").and_then(|v| v.as_u64()))
                    .unwrap_or(128_000);

                let pricing_input = m
                    .get("input_price")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let pricing_output = m
                    .get("output_price")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);

                let mut model = serde_json::Map::new();
                model.insert("id".to_string(), Value::String(id.to_string()));
                model.insert("name".to_string(), Value::String(display_name.clone()));
                model.insert("type".to_string(), Value::String("model".to_string()));
                model.insert("display_name".to_string(), Value::String(display_name));
                model.insert("context_window".to_string(), Value::Number(serde_json::Number::from(context_window)));
                model.insert("input_price".to_string(), Value::Number(serde_json::Number::from_f64(pricing_input).unwrap_or(serde_json::Number::from_f64(0.0).unwrap())));
                model.insert("output_price".to_string(), Value::Number(serde_json::Number::from_f64(pricing_output).unwrap_or(serde_json::Number::from_f64(0.0).unwrap())));

                if let Some(created) = m.get("created") {
                    model.insert("created".to_string(), created.clone());
                }
                if let Some(owned_by) = m.get("owned_by") {
                    model.insert("owned_by".to_string(), owned_by.clone());
                }
                if let Some(arch) = m.get("architecture") {
                    model.insert("architecture".to_string(), arch.clone());
                }

                Some(Value::Object(model))
            })
            .collect(),
        None => {
            ProxyConfig::free_models()
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "id": m.id,
                        "name": m.display_name,
                        "type": "model",
                        "display_name": m.display_name,
                        "context_window": 128000,
                        "input_price": 0.0,
                        "output_price": 0.0
                    })
                })
                .collect()
        }
    };

    serde_json::json!({
        "data": models,
        "has_more": false,
        "first_id": models.first().and_then(|m| m.get("id").and_then(|v| v.as_str())).unwrap_or(""),
        "last_id": models.last().and_then(|m| m.get("id").and_then(|v| v.as_str())).unwrap_or("")
    })
}

pub async fn handle_model_detail(
    State(state): State<Arc<Mutex<ProxyState>>>,
    axum::extract::Path(model_id): axum::extract::Path<String>,
) -> impl IntoResponse {
    METRICS.inc_request();
    let start = std::time::Instant::now();
    let (client, base_url, api_key) = {
        let state_guard = state.lock().await;
        (
            state_guard.client.clone(),
            state_guard.config.opencode_base_url.clone(),
            state_guard.config.opencode_api_key.clone(),
        )
    };

    let model_url = format!("{}/v1/models/{}", base_url.trim_end_matches('/'), model_id);

    let mut request = client
        .get(&model_url)
        .header("Accept", "application/json");

    if let Some(key) = &api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    let resp = match request.send().await {
        Ok(resp) if resp.status().is_success() => {
            match resp.json::<Value>().await {
                Ok(model) => {
                    let mut transformed = serde_json::Map::new();
                    if let Some(id) = model.get("id").and_then(|v| v.as_str()) {
                        transformed.insert("id".to_string(), Value::String(id.to_string()));
                    }
                    if let Some(name) = model.get("name").and_then(|v| v.as_str()) {
                        transformed.insert("name".to_string(), Value::String(name.to_string()));
                        transformed.insert("display_name".to_string(), Value::String(name.to_string()));
                    }
                    transformed.insert("type".to_string(), Value::String("model".to_string()));

                    (StatusCode::OK, Json(Value::Object(transformed))).into_response()
                }
                Err(e) => {
                    error!("Failed to parse model detail: {}", e);
                    StatusCode::NOT_FOUND.into_response()
                }
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    };
    METRICS.record_latency(start.elapsed().as_millis() as u64);
    resp
}

pub async fn handle_messages(
    State(state): State<Arc<Mutex<ProxyState>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle_anthropic_messages(state, headers, body).await
}

/// OpenAI-compatible endpoint for direct opencode / OpenAI-SDK clients.
///
/// Unlike [`handle_messages`] (Anthropic `POST /v1/messages`), this passes the
/// request body through untouched and returns the upstream JSON / SSE bytes
/// untouched — no Anthropic translation. 429s still trigger the shared WARP
/// reset + retry loop, so opencode gets transparent rate-limit recovery.
pub async fn handle_chat_completions(
    State(state): State<Arc<Mutex<ProxyState>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    METRICS.inc_request();
    let start = Instant::now();
    let request_body = match serde_json::from_slice::<Value>(&body) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to parse request body: {}", e);
            ReqCtx {
                endpoint: "openai",
                model: "unknown".to_string(),
                start,
            }
            .finish(400);
            return error_response_with_status(
                &format!("Invalid JSON: {}", e),
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let requested_model = request_body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let ctx = ReqCtx {
        endpoint: "openai",
        model: requested_model.clone(),
        start,
    };
    let is_stream = request_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let snap = match gate(&state).await {
        Gate::Pass(s) => s,
        Gate::CircuitOpen(remaining) => {
            warn!(
                "Circuit open, rejecting openai request (cooldown {}s left)",
                remaining
            );
            ctx.finish(503);
            return error_response_with_status(
                &format!(
                    "Circuit breaker open after repeated 429s; retry in ~{}s",
                    remaining
                ),
                StatusCode::SERVICE_UNAVAILABLE,
            );
        }
    };

    if !check_incoming_auth(&snap.require_api_key, &headers) {
        warn!("Rejected openai request with missing/invalid API key");
        ctx.finish(401);
        return unauthorized_response();
    }

    // Native OpenAI clients already speak the upstream dialect (opencode/openai
    // providers) — forward untouched. For a native Anthropic upstream there is
    // no OpenAI→Anthropic translation; direct those clients at /v1/messages.
    if matches!(
        snap.provider,
        crate::providers::Provider::Anthropic
    ) {
        warn!("OpenAI client hit /v1/chat/completions with provider=anthropic; use /v1/messages instead");
        ctx.finish(500);
        return error_response(
            "provider=anthropic has no OpenAI translation; send Anthropic requests to /v1/messages",
        );
    }

    for attempt_model in candidate_models(&requested_model, &snap.fallback_models) {
        if attempt_model != requested_model {
            info!(
                "Failing over openai request {} -> {}",
                requested_model, attempt_model
            );
            METRICS.inc_fallback();
        }
        let mut attempt_body = request_body.clone();
        attempt_body["model"] = Value::String(attempt_model.clone());
        let mut upstream = snap.upstream.clone();
        let mut upstream_attempts_left = snap.upstream_count;

        let mut retry_count = 0;
        let mut backoff_retries: u32 = 0;

        loop {
            let result = if is_stream {
                forward_openai_streaming(&snap.client, &upstream, &snap.api_key, &attempt_body)
                    .await
            } else {
                forward_openai_non_streaming(
                    &snap.client,
                    &upstream,
                    &snap.api_key,
                    &attempt_body,
                    &attempt_model,
                )
                .await
            };

            match result {
                Ok(response) => {
                    note_success(&state).await;
                    ctx.finish(200);
                    return response;
                }
                Err(ProxyError::RateLimited) => {
                    METRICS.inc_429();
                    retry_count += 1;
                    fire_429_hook(&snap.hook_on_429, retry_count, &attempt_model, &upstream);
                    if note_429(&state, snap.circuit_threshold, snap.circuit_cooldown_secs).await
                    {
                        warn!(
                            "Circuit breaker tripped ({} consecutive 429s)",
                            snap.circuit_threshold
                        );
                        ctx.finish(503);
                        return error_response_with_status(
                            "Circuit breaker open after repeated 429s; backing off",
                            StatusCode::SERVICE_UNAVAILABLE,
                        );
                    }
                    if retry_count > snap.max_retries {
                        warn!(
                            "Model {} still limited after {} WARP resets, trying failover",
                            attempt_model, snap.max_retries
                        );
                        break;
                    }

                    info!(
                        "429 received (openai passthrough), attempting WARP reset (attempt {}/{})",
                        retry_count, snap.max_retries
                    );

                    let resolver = WarpResolver::new(snap.max_retries, snap.reset_delay)
                        .with_hook(snap.hook_on_429.clone());
                    if !resolver.handle_429(retry_count - 1).await {
                        warn!("WARP reset failed; trying failover model");
                        break;
                    }
                    METRICS.inc_warp_reset();
                }
                Err(ProxyError::Retryable(status)) => {
                    METRICS.inc_retry();
                    backoff_retries += 1;
                    if backoff_retries > snap.max_retries {
                        warn!("Upstream {} unavailable, trying failover", status);
                        break;
                    }
                    let backoff_ms = 200_u64
                        .saturating_mul(2_u64.pow(backoff_retries - 1))
                        .min(5000);
                    warn!(
                        "Upstream {} retryable, backing off {}ms (attempt {}/{})",
                        status, backoff_ms, backoff_retries, snap.max_retries
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(ProxyError::RequestFailed(msg)) => {
                    upstream_attempts_left = upstream_attempts_left.saturating_sub(1);
                    if upstream_attempts_left > 0 {
                        upstream = rotate_upstream(&state).await;
                        warn!(
                            "Upstream failed, rotating to {} ({} attempts left)",
                            upstream, upstream_attempts_left
                        );
                        continue;
                    }
                    error!("Upstream request failed: {}", msg);
                    ctx.finish(502);
                    return error_response_with_status(
                        &format!("Upstream error: {}{}", msg, upstream_hint(&upstream)),
                        StatusCode::BAD_GATEWAY,
                    );
                }
            }
        }
    }

    ctx.finish(429);
    error_response_with_status(
        "Rate limit exceeded on all models after WARP resets",
        StatusCode::TOO_MANY_REQUESTS,
    )
}

async fn handle_anthropic_messages(
    state: Arc<Mutex<ProxyState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    METRICS.inc_request();
    let start = Instant::now();
    let request_body = match serde_json::from_slice::<Value>(&body) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to parse request body: {}", e);
            ReqCtx {
                endpoint: "anthropic",
                model: "unknown".to_string(),
                start,
            }
            .finish(400);
            return error_response_with_status(
                &format!("Invalid JSON: {}", e),
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let is_stream = request_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let requested_model = request_body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let ctx = ReqCtx {
        endpoint: "anthropic",
        model: requested_model.clone(),
        start,
    };

    let snap = match gate(&state).await {
        Gate::Pass(s) => s,
        Gate::CircuitOpen(remaining) => {
            warn!(
                "Circuit open, rejecting anthropic request (cooldown {}s left)",
                remaining
            );
            ctx.finish(503);
            return error_response_with_status(
                &format!(
                    "Circuit breaker open after repeated 429s; retry in ~{}s",
                    remaining
                ),
                StatusCode::SERVICE_UNAVAILABLE,
            );
        }
    };

    if !check_incoming_auth(&snap.require_api_key, &headers) {
        warn!("Rejected anthropic request with missing/invalid API key");
        ctx.finish(401);
        return unauthorized_response();
    }

    for attempt_model in candidate_models(&requested_model, &snap.fallback_models) {
        if attempt_model != requested_model {
            info!(
                "Failing over anthropic request {} -> {}",
                requested_model, attempt_model
            );
            METRICS.inc_fallback();
        }
        let mut attempt_body = request_body.clone();
        attempt_body["model"] = Value::String(attempt_model.clone());
        let translated_body = translate_request(snap.provider, &attempt_body);
        let mut upstream = snap.upstream.clone();
        let mut upstream_attempts_left = snap.upstream_count;

        let mut retry_count = 0;
        let mut backoff_retries: u32 = 0;

        loop {
            let result = if is_stream {
                forward_streaming(
                    &snap.client,
                    &upstream,
                    &snap.api_key,
                    &translated_body,
                    &attempt_model,
                    snap.provider,
                )
                .await
            } else {
                forward_non_streaming(
                    &snap.client,
                    &upstream,
                    &snap.api_key,
                    &translated_body,
                    &attempt_model,
                    snap.provider,
                )
                .await
            };

            match result {
                Ok(response) => {
                    note_success(&state).await;
                    ctx.finish(200);
                    return response;
                }
                Err(ProxyError::RateLimited) => {
                    METRICS.inc_429();
                    retry_count += 1;
                    fire_429_hook(&snap.hook_on_429, retry_count, &attempt_model, &upstream);
                    if note_429(&state, snap.circuit_threshold, snap.circuit_cooldown_secs).await
                    {
                        warn!(
                            "Circuit breaker tripped ({} consecutive 429s)",
                            snap.circuit_threshold
                        );
                        ctx.finish(503);
                        return error_response_with_status(
                            "Circuit breaker open after repeated 429s; backing off",
                            StatusCode::SERVICE_UNAVAILABLE,
                        );
                    }
                    if retry_count > snap.max_retries {
                        warn!(
                            "Model {} still limited after {} WARP resets, trying failover",
                            attempt_model, snap.max_retries
                        );
                        break;
                    }

                    info!(
                        "429 received, attempting WARP reset (attempt {}/{})",
                        retry_count, snap.max_retries
                    );

                    let resolver = WarpResolver::new(snap.max_retries, snap.reset_delay)
                        .with_hook(snap.hook_on_429.clone());
                    if !resolver.handle_429(retry_count - 1).await {
                        warn!("WARP reset failed; trying failover model");
                        break;
                    }
                    // WARP reset succeeded; track it and retry
                    METRICS.inc_warp_reset();
                }
                Err(ProxyError::Retryable(status)) => {
                    METRICS.inc_retry();
                    backoff_retries += 1;
                    if backoff_retries > snap.max_retries {
                        warn!("Upstream {} unavailable, trying failover", status);
                        break;
                    }
                    let backoff_ms =
                        200_u64.saturating_mul(2_u64.pow(backoff_retries - 1)).min(5000);
                    warn!(
                        "Upstream {} retryable, backing off {}ms (attempt {}/{})",
                        status, backoff_ms, backoff_retries, snap.max_retries
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(ProxyError::RequestFailed(msg)) => {
                    upstream_attempts_left = upstream_attempts_left.saturating_sub(1);
                    if upstream_attempts_left > 0 {
                        upstream = rotate_upstream(&state).await;
                        warn!(
                            "Upstream failed, rotating to {} ({} attempts left)",
                            upstream, upstream_attempts_left
                        );
                        continue;
                    }
                    error!("Upstream request failed: {}", msg);
                    ctx.finish(502);
                    return error_response_with_status(
                        &format!("Upstream error: {}{}", msg, upstream_hint(&upstream)),
                        StatusCode::BAD_GATEWAY,
                    );
                }
            }
        }
    }

    ctx.finish(429);
    error_response_with_status(
        "Rate limit exceeded on all models after WARP resets",
        StatusCode::TOO_MANY_REQUESTS,
    )
}

async fn forward_streaming(
    client: &Client,
    base_url: &str,
    api_key: &Option<String>,
    body: &Value,
    model: &str,
    provider: crate::providers::Provider,
) -> Result<Response, ProxyError> {
    let path = provider.upstream_path();
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(body);

    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    let response = request
        .send()
        .await
        .map_err(|e| ProxyError::RequestFailed(e.to_string()))?;

    if response.status() == 429 {
        return Err(ProxyError::RateLimited);
    }

    if response.status() == StatusCode::BAD_GATEWAY || response.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Err(ProxyError::Retryable(response.status()));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(ProxyError::RequestFailed(format!(
            "Upstream {} : {}",
            status, body
        )));
    }

    let model_owned = model.to_string();
    // Buffer incomplete SSE lines across chunks
    let mut buffer = String::new();
    let stream = response
        .bytes_stream()
        .map(move |chunk| {
            let chunk = chunk.map_err(std::io::Error::other)?;
            let text = String::from_utf8_lossy(&chunk);
            buffer.push_str(&text);

            let mut output = String::new();
            // Drain complete lines (ending with '\n') from buffer
            while let Some(newline_idx) = buffer.find('\n') {
                let line = buffer[..newline_idx].to_string();
                // Remove consumed line + newline from buffer
                buffer.drain(..=newline_idx);
                let trimmed = line.trim_end_matches('\r');
                if trimmed.is_empty() {
                    continue;
                }
                if let Some(transformed) = translate_stream_chunk(provider, trimmed, &model_owned) {
                    output.push_str(&transformed);
                } else {
                    if trimmed.starts_with("data:") || trimmed.starts_with("event:") || trimmed.starts_with(":") {
                        // fallback passthrough for provider native streams
                        output.push_str(trimmed);
                        output.push('\n');
                    }
                }
            }

            if output.is_empty() {
                Ok::<Bytes, std::io::Error>(Bytes::new())
            } else {
                Ok::<Bytes, std::io::Error>(Bytes::from(output))
            }
        });

    let body = Body::from_stream(stream);

    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        "Content-Type",
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        "Cache-Control",
        HeaderValue::from_static("no-cache"),
    );
    response
        .headers_mut()
        .insert("Connection", HeaderValue::from_static("keep-alive"));
    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));

    Ok(response)
}

async fn forward_non_streaming(
    client: &Client,
    base_url: &str,
    api_key: &Option<String>,
    body: &Value,
    model: &str,
    provider: crate::providers::Provider,
) -> Result<Response, ProxyError> {
    let path = provider.upstream_path();
    let url = format!("{}{}", base_url.trim_end_matches('/'), path);

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(body);

    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    let response = request
        .send()
        .await
        .map_err(|e| ProxyError::RequestFailed(e.to_string()))?;

    if response.status() == 429 {
        return Err(ProxyError::RateLimited);
    }

    if response.status() == StatusCode::BAD_GATEWAY || response.status() == StatusCode::SERVICE_UNAVAILABLE {
        return Err(ProxyError::Retryable(response.status()));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(ProxyError::RequestFailed(format!(
            "Upstream {} : {}",
            status, body
        )));
    }

    let opencode_response = response
        .json::<Value>()
        .await
        .map_err(|e| ProxyError::RequestFailed(e.to_string()))?;

    record_upstream_usage(model, &opencode_response);
    let anthropic_response = translate_response(provider, &opencode_response, model);

    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Type",
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        "x-request-id",
        HeaderValue::from_str(&format!("req_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0))).unwrap_or(HeaderValue::from_static("unknown")),
    );

    Ok((StatusCode::OK, headers, Json(anthropic_response)).into_response())
}

/// Forward an OpenAI-format body untouched; return upstream JSON untouched.
async fn forward_openai_non_streaming(
    client: &Client,
    base_url: &str,
    api_key: &Option<String>,
    body: &Value,
    model: &str,
) -> Result<Response, ProxyError> {
    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .json(body);

    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    let response = request
        .send()
        .await
        .map_err(|e| ProxyError::RequestFailed(e.to_string()))?;

    if response.status() == 429 {
        return Err(ProxyError::RateLimited);
    }

    if response.status() == StatusCode::BAD_GATEWAY
        || response.status() == StatusCode::SERVICE_UNAVAILABLE
    {
        return Err(ProxyError::Retryable(response.status()));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(ProxyError::RequestFailed(format!(
            "Upstream {} : {}",
            status, body
        )));
    }

    let upstream_json = response
        .json::<Value>()
        .await
        .map_err(|e| ProxyError::RequestFailed(e.to_string()))?;

    record_upstream_usage(model, &upstream_json);
    Ok((StatusCode::OK, Json(upstream_json)).into_response())
}

/// Forward an OpenAI streaming body untouched; relay upstream SSE bytes as-is.
async fn forward_openai_streaming(
    client: &Client,
    base_url: &str,
    api_key: &Option<String>,
    body: &Value,
) -> Result<Response, ProxyError> {
    let url = format!("{}/v1/chat/completions", base_url.trim_end_matches('/'));

    let mut request = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Accept", "text/event-stream")
        .json(body);

    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    let response = request
        .send()
        .await
        .map_err(|e| ProxyError::RequestFailed(e.to_string()))?;

    if response.status() == 429 {
        return Err(ProxyError::RateLimited);
    }

    if response.status() == StatusCode::BAD_GATEWAY
        || response.status() == StatusCode::SERVICE_UNAVAILABLE
    {
        return Err(ProxyError::Retryable(response.status()));
    }

    if !response.status().is_success() {
        let status = response.status();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(ProxyError::RequestFailed(format!(
            "Upstream {} : {}",
            status, body
        )));
    }

    let stream = response.bytes_stream().map(|chunk| {
        chunk.map_err(std::io::Error::other)
    });
    let body = Body::from_stream(stream);

    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        "Content-Type",
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        "Cache-Control",
        HeaderValue::from_static("no-cache"),
    );
    response
        .headers_mut()
        .insert("Connection", HeaderValue::from_static("keep-alive"));
    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));

    Ok(response)
}

fn error_response_with_status(message: &str, status: StatusCode) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "api_error",
            "code": "internal_error"
        }
    });
    (status, Json(body)).into_response()
}

fn unauthorized_response() -> Response {
    let body = serde_json::json!({
        "error": {
            "message": "Missing or invalid API key (set Authorization: Bearer <key> or x-api-key)",
            "type": "authentication_error",
            "code": "invalid_api_key"
        }
    });
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

fn error_response(message: &str) -> Response {
    let body = serde_json::json!({
        "error": {
            "message": message,
            "type": "api_error",
            "code": "internal_error"
        }
    });
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(body),
    )
        .into_response()
}

#[derive(Debug)]
enum ProxyError {
    RateLimited,
    Retryable(StatusCode),
    RequestFailed(String),
}
