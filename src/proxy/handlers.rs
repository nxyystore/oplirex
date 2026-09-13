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
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::config::ProxyConfig;
use crate::hooks::{run_hook, HookEnv};
use crate::metrics::METRICS;
use crate::providers::{translate_request, translate_response, translate_stream_chunk};
use crate::warp::WarpResolver;

pub struct ProxyState {
    pub config: ProxyConfig,
    pub client: Client,
    pub warp_resolver: WarpResolver,
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
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    METRICS.inc_request();
    let start = std::time::Instant::now();
    let request_body = match serde_json::from_slice::<Value>(&body) {
        Ok(v) => v,
        Err(e) => {
            error!("Failed to parse request body: {}", e);
            METRICS.record_latency(start.elapsed().as_millis() as u64);
            return error_response(&format!("Invalid JSON: {}", e));
        }
    };

    let is_stream = request_body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let model = request_body
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let (client, base_url, api_key, max_retries, reset_delay, hook_on_429, provider) = {
        let state_guard = state.lock().await;
        (
            state_guard.client.clone(),
            state_guard.config.opencode_base_url.clone(),
            state_guard.config.opencode_api_key.clone(),
            state_guard.config.max_retries,
            state_guard.config.warp_reset_delay_ms,
            state_guard.config.hook_on_429.clone(),
            state_guard.config.provider,
        )
    };

    let translated_body = translate_request(provider, &request_body);

    let mut retry_count = 0;
    let mut backoff_retries: u32 = 0;

    loop {
        let result = if is_stream {
            forward_streaming(&client, &base_url, &api_key, &translated_body, &model, provider).await
        } else {
            forward_non_streaming(&client, &base_url, &api_key, &translated_body, &model, provider).await
        };

        match result {
            Ok(response) => {
                METRICS.record_latency(start.elapsed().as_millis() as u64);
                return response;
            },
            Err(ProxyError::RateLimited) => {
                METRICS.inc_429();
                retry_count += 1;
                // hook integration P2.9: fire on_429 hook non-blocking
                if let Some(hook) = &hook_on_429 {
                    let mut env = HookEnv::new(retry_count);
                    env.model = Some(model.clone());
                    env.upstream = Some(base_url.clone());
                    run_hook(hook, env);
                }
                if retry_count > max_retries {
                    METRICS.record_latency(start.elapsed().as_millis() as u64);
                    return error_response("Rate limit exceeded after WARP resets");
                }

                info!(
                    "429 received, attempting WARP reset (attempt {}/{})",
                    retry_count, max_retries
                );

                let resolver = WarpResolver::new(max_retries, reset_delay).with_hook(hook_on_429.clone());
                if !resolver.handle_429(retry_count - 1).await {
                    METRICS.record_latency(start.elapsed().as_millis() as u64);
                    return error_response("WARP reset failed, rate limit still active");
                }
                // WARP reset succeeded; track it and retry
                METRICS.inc_warp_reset();
            }
            Err(ProxyError::Retryable(status)) => {
                METRICS.inc_retry();
                backoff_retries += 1;
                if backoff_retries > max_retries {
                    METRICS.record_latency(start.elapsed().as_millis() as u64);
                    return error_response(&format!("Upstream {} unavailable after {} retries", status, max_retries));
                }
                let backoff_ms = 200_u64.saturating_mul(2_u64.pow(backoff_retries - 1)).min(5000);
                warn!(
                    "Upstream {} retryable, backing off {}ms (attempt {}/{})",
                    status, backoff_ms, backoff_retries, max_retries
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                continue;
            }
            Err(ProxyError::RequestFailed(msg)) => {
                error!("Upstream request failed: {}", msg);
                METRICS.record_latency(start.elapsed().as_millis() as u64);
                return error_response(&format!("Upstream error: {}", msg));
            }
        }
    }
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
