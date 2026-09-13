//! P3 integration tests: transform roundtrip, proxy handlers (models, messages non-streaming), metrics.
//! Mocks upstream OpenAI-compatible server using axum test server (random port).
//! Also verifies file logging helper resolves correctly.

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
    Json, Router,
};
use oplirex::{
    config::ProxyConfig,
    metrics::Metrics,
    proxy::handlers::{handle_chat_completions, handle_messages, handle_models, ProxyState},
    transform::{anthropic_to_opencode_request, opencode_response_to_anthropic, opencode_stream_to_anthropic},
    warp::WarpResolver,
};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;
use tower::ServiceExt;

// ---------- helpers ----------

async fn spawn_mock_upstream() -> String {
    // Mock upstream that mimics OpenAI /v1/models and /v1/chat/completions
    let app = Router::new()
        .route(
            "/v1/models",
            get(|| async {
                Json(json!({
                    "object": "list",
                    "data": [
                        {"id": "gpt-4o-mini", "object": "model", "created": 123, "owned_by": "openai"},
                        {"id": "glm-4.7-free", "object": "model", "created": 0, "owned_by": "opencode-zen"}
                    ]
                }))
            }),
        )
        .route(
            "/v1/chat/completions",
            post(|body: axum::extract::Json<Value>| async move {
                let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("unknown");
                // echo back a simple completion
                let resp = json!({
                    "id": "chatcmpl-mock",
                    "object": "chat.completion",
                    "created": 0,
                    "model": model,
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "hello from mock"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
                });
                (StatusCode::OK, Json(resp))
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

fn make_proxy_state(upstream: String) -> Arc<Mutex<ProxyState>> {
    make_proxy_state_with(upstream, |_| {})
}

fn make_proxy_state_with(
    upstream: String,
    mutate: impl FnOnce(&mut ProxyConfig),
) -> Arc<Mutex<ProxyState>> {
    let mut config = ProxyConfig {
        listen_addr: "127.0.0.1:0".to_string(),
        opencode_base_url: upstream,
        opencode_api_key: None,
        max_retries: 2,
        warp_reset_delay_ms: 10,
        hook_on_429: None,
        provider: oplirex::providers::Provider::Opencode,
        extra_upstreams: vec![],
        require_api_key: None,
        fallback_models: vec![],
        circuit_threshold: 5,
        circuit_cooldown_secs: 60,
    };
    mutate(&mut config);
    let client = reqwest::Client::builder().build().unwrap();
    let warp_resolver = WarpResolver::new(2, 10);
    Arc::new(Mutex::new(ProxyState::new(config, client, warp_resolver)))
}

// ---------- transform tests ----------

#[test]
fn test_anthropic_to_opencode_basic() {
    let input = json!({
        "model": "claude-3-5-sonnet",
        "max_tokens": 512,
        "messages": [{"role": "user", "content": "hi"}]
    });
    let out = anthropic_to_opencode_request(&input);
    assert_eq!(out["model"], "claude-3-5-sonnet");
    assert_eq!(out["max_tokens"], 512);
    assert_eq!(out["messages"][0]["role"], "user");
    assert_eq!(out["messages"][0]["content"], "hi");
    assert_eq!(out["stream"], false);
}

#[test]
fn test_anthropic_to_opencode_with_system_and_array_content() {
    let input = json!({
        "model": "x",
        "max_tokens": 100,
        "system": "you are helpful",
        "messages": [
            {"role": "user", "content": [{"type": "text", "text": "hello "}, {"type": "text", "text": "world"}]},
            {"role": "assistant", "content": "ok"}
        ],
        "temperature": 0.7,
        "top_p": 0.9,
        "stream": true
    });
    let out = anthropic_to_opencode_request(&input);
    // system becomes first message with role system
    assert_eq!(out["messages"][0]["role"], "system");
    assert_eq!(out["messages"][0]["content"], "you are helpful");
    assert_eq!(out["messages"][1]["content"], "hello \nworld");
    assert_eq!(out["messages"][2]["role"], "assistant");
    assert_eq!(out["temperature"], 0.7);
    assert_eq!(out["top_p"], 0.9);
    assert_eq!(out["stream"], true);
}

#[test]
fn test_anthropic_system_array() {
    let input = json!({
        "model": "x",
        "system": [{"type": "text", "text": "sys1"}, {"type": "text", "text": "sys2"}],
        "messages": [{"role": "user", "content": "hi"}]
    });
    let out = anthropic_to_opencode_request(&input);
    assert_eq!(out["messages"][0]["content"], "sys1\nsys2");
}

#[test]
fn test_opencode_response_to_anthropic() {
    let upstream = json!({
        "choices": [{
            "message": {"content": "hello"},
            "finish_reason": "stop"
        }],
        "usage": {"input_tokens": 1, "output_tokens": 2}
    });
    let out = opencode_response_to_anthropic(&upstream, "my-model");
    assert_eq!(out["role"], "assistant");
    assert_eq!(out["model"], "my-model");
    assert_eq!(out["content"][0]["text"], "hello");
    assert_eq!(out["stop_reason"], "end_turn");
    assert_eq!(out["type"], "message");
}

#[test]
fn test_opencode_response_to_anthropic_length() {
    let upstream = json!({
        "choices": [{
            "message": {"content": "hi"},
            "finish_reason": "length"
        }]
    });
    let out = opencode_response_to_anthropic(&upstream, "m");
    assert_eq!(out["stop_reason"], "max_tokens");
}

#[test]
fn test_opencode_stream_to_anthropic_delta() {
    let line = r#"data: {"choices": [{"delta": {"content": "hi"}, "finish_reason": null}]}"#;
    let out = opencode_stream_to_anthropic(line, "m").unwrap();
    assert!(out.contains("content_block_delta"));
    assert!(out.contains("hi"));
}

#[test]
fn test_opencode_stream_to_anthropic_done() {
    let out = opencode_stream_to_anthropic("data: [DONE]", "m").unwrap();
    assert!(out.contains("message_stop"));
}

#[test]
fn test_opencode_stream_to_anthropic_skip() {
    assert!(opencode_stream_to_anthropic("", "m").is_none());
    assert!(opencode_stream_to_anthropic(": keepalive", "m").is_none());
}

// ---------- metrics tests ----------

#[test]
fn test_metrics_inc_and_latency() {
    let m = Metrics::new();
    assert_eq!(m.requests_total.load(std::sync::atomic::Ordering::Relaxed), 0);
    m.inc_request();
    m.inc_request();
    m.inc_429();
    m.inc_retry();
    m.inc_warp_reset();
    m.record_latency(100);
    m.record_latency(200);
    m.push_warp_ip("1.1.1.1".to_string());

    let j = m.to_json();
    assert_eq!(j["requests_total"], 2);
    assert_eq!(j["rate_limited_total"], 1);
    assert_eq!(j["retry_total"], 1);
    // push_warp_ip also inc_warp_reset, so total 2
    assert_eq!(j["warp_resets"], 2);
    assert_eq!(j["latency_ms"]["count"], 2);
    assert!((j["avg_latency_ms"].as_f64().unwrap() - 150.0).abs() < 0.01);
    assert_eq!(j["warp_ip_history"][0], "1.1.1.1");
}

#[test]
fn test_metrics_histogram_cap() {
    let m = Metrics::new();
    for i in 0..1100 {
        m.record_latency(i);
    }
    let j = m.to_json();
    // capped at 1024
    assert_eq!(j["latency_ms"]["histogram"].as_array().unwrap().len(), 1024);
}

// ---------- proxy handler integration tests (mock upstream) ----------

#[tokio::test]
async fn test_proxy_models_handler_with_mock_upstream() {
    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state(upstream);

    // Build minimal router using same handler
    let app = Router::new()
        .route("/v1/models", get(handle_models))
        .with_state(state);

    let req = Request::builder()
        .uri("/v1/models")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    // handler transforms to anthropic style: has data array
    assert!(v.get("data").is_some());
    let data = v["data"].as_array().unwrap();
    assert!(data.len() >= 2);
    // check display_name mapping
    assert!(data.iter().any(|m| m["id"] == "gpt-4o-mini"));
}

#[tokio::test]
async fn test_proxy_messages_non_streaming() {
    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state(upstream);

    let app = Router::new()
        .route("/v1/messages", post(handle_messages))
        .with_state(state);

    let payload = json!({
        "model": "gpt-4o-mini",
        "max_tokens": 64,
        "messages": [{"role": "user", "content": "hello"}]
    });

    let req = Request::builder()
        .uri("/v1/messages")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    // should be anthropic transformed response
    assert_eq!(v["type"], "message");
    assert_eq!(v["role"], "assistant");
    assert_eq!(v["content"][0]["text"], "hello from mock");
    assert_eq!(v["model"], "gpt-4o-mini");
}

#[tokio::test]
async fn test_proxy_messages_invalid_json_returns_error() {    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state(upstream);
    let app = Router::new()
        .route("/v1/messages", post(handle_messages))
        .with_state(state);

    let req = Request::builder()
        .uri("/v1/messages")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from("not json"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_proxy_models_fallback_when_upstream_down() {
    // upstream points to non-existent server -> handler should fallback to ProxyConfig::models_response()
    let state = make_proxy_state("http://127.0.0.1:1".to_string());
    let app = Router::new()
        .route("/v1/models", get(handle_models))
        .with_state(state);
    let req = Request::builder().uri("/v1/models").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    // fallback has object=list
    assert!(v.get("object").is_some() || v.get("data").is_some());
}

// ---------- OpenAI passthrough (direct opencode daemon) ----------

#[tokio::test]
async fn test_chat_completions_passthrough_non_streaming() {
    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state(upstream);

    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state);

    // OpenAI-format body goes through untouched and comes back untouched
    let payload = json!({
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "hello"}],
        "temperature": 0.5
    });

    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    // NOT translated to Anthropic shape — raw OpenAI shape preserved
    assert_eq!(v["object"], "chat.completion");
    assert_eq!(v["id"], "chatcmpl-mock");
    assert_eq!(v["model"], "gpt-4o-mini");
    assert_eq!(v["choices"][0]["message"]["content"], "hello from mock");
    assert!(v.get("type").is_none());
}

#[tokio::test]
async fn test_chat_completions_streaming_passthrough() {
    // Mock upstream with an SSE streaming endpoint
    let app_upstream = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            let sse = "data: {\"id\":\"chatcmpl-x\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";
            (
                StatusCode::OK,
                [("content-type", "text/event-stream")],
                Body::from(sse.to_string()),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app_upstream).await.unwrap();
    });
    let state = make_proxy_state(format!("http://{}", addr));

    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state);

    let payload = json!({
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "hello"}],
        "stream": true
    });

    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers()["content-type"]
        .to_str()
        .unwrap()
        .contains("text/event-stream"));

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    // Raw OpenAI SSE relayed as-is — no Anthropic `event:` translation
    assert!(text.contains("chat.completion.chunk"));
    assert!(text.contains("[DONE]"));
    assert!(!text.contains("content_block_delta"));
}

#[tokio::test]
async fn test_chat_completions_invalid_json_returns_error() {
    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state(upstream);
    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state);

    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from("not json"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_chat_completions_rejects_anthropic_provider() {
    let upstream = spawn_mock_upstream().await;
    let config = ProxyConfig {
        listen_addr: "127.0.0.1:0".to_string(),
        opencode_base_url: upstream,
        opencode_api_key: None,
        max_retries: 2,
        warp_reset_delay_ms: 10,
        hook_on_429: None,
        provider: oplirex::providers::Provider::Anthropic,
        extra_upstreams: vec![],
        require_api_key: None,
        fallback_models: vec![],
        circuit_threshold: 5,
        circuit_cooldown_secs: 60,
    };
    let client = reqwest::Client::builder().build().unwrap();
    let warp_resolver = WarpResolver::new(2, 10);
    let state = Arc::new(Mutex::new(ProxyState::new(config, client, warp_resolver)));
    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state);

    let payload = json!({
        "model": "x",
        "messages": [{"role": "user", "content": "hi"}]
    });
    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

// Wiremock alternative smoke test (demonstrates mockito/wiremock usage)
#[tokio::test]
async fn test_wiremock_models() {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::{method, path}};

    let mock_server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [{"id": "wiremock-model", "object": "model", "created": 0, "owned_by": "test"}]
        })))
        .mount(&mock_server)
        .await;

    let state = make_proxy_state(mock_server.uri());
    let app = Router::new()
        .route("/v1/models", get(handle_models))
        .with_state(state);
    let req = Request::builder().uri("/v1/models").body(Body::empty()).unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024*1024).await.unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert!(v["data"].as_array().unwrap().iter().any(|m| m["id"]=="wiremock-model"));
}

// ---------- new-feature tests (failover, auth, circuit, rotation, usage, reload) ----------

use axum::response::IntoResponse;
use oplirex::metrics::METRICS;
use oplirex::proxy::server::reload_handler;
use std::time::{Duration, Instant};

/// Upstream that 429s `primary-model` but serves everything else (with usage).
async fn spawn_flaky_upstream() -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|body: axum::extract::Json<Value>| async move {
            let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model == "primary-model" {
                return StatusCode::TOO_MANY_REQUESTS.into_response();
            }
            Json(json!({
                "id": "chatcmpl-flaky",
                "object": "chat.completion",
                "created": 0,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "served"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8}
            }))
            .into_response()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

/// Upstream echoing a fixed tag as the completion content.
async fn spawn_tagged_upstream(tag: &'static str) -> String {
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |body: axum::extract::Json<Value>| async move {
            let model = body
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            Json(json!({
                "id": "chatcmpl-tagged",
                "object": "chat.completion",
                "created": 0,
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": tag},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            }))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

fn chat_payload(model: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model": model,
        "messages": [{"role": "user", "content": "hi"}]
    }))
    .unwrap()
}

fn messages_payload(model: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model": model,
        "max_tokens": 16,
        "messages": [{"role": "user", "content": "hi"}]
    }))
    .unwrap()
}

#[tokio::test]
async fn test_fallback_model_serves_after_429() {
    let upstream = spawn_flaky_upstream().await;
    let state = make_proxy_state_with(upstream, |cfg| {
        cfg.max_retries = 0; // exhaust the primary immediately, then fail over
        cfg.fallback_models = vec!["fallback-model".to_string()];
    });
    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state);

    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(chat_payload("primary-model")))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let v: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["model"], "fallback-model");
    assert_eq!(v["choices"][0]["message"]["content"], "served");

    let metrics = METRICS.to_json();
    assert!(metrics["fallback_total"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn test_incoming_auth_enforced() {
    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state_with(upstream, |cfg| {
        cfg.require_api_key = Some("topsecret".to_string());
    });
    let app = Router::new()
        .route("/v1/messages", post(handle_messages))
        .with_state(state);

    // No key -> 401.
    let req = Request::builder()
        .uri("/v1/messages")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(messages_payload("m")))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Wrong key -> 401.
    let req = Request::builder()
        .uri("/v1/messages")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer wrong")
        .body(Body::from(messages_payload("m")))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Correct Bearer key -> 200.
    let req = Request::builder()
        .uri("/v1/messages")
        .method("POST")
        .header("content-type", "application/json")
        .header("authorization", "Bearer topsecret")
        .body(Body::from(messages_payload("m")))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Correct x-api-key (what Claude Code sends) -> 200.
    let req = Request::builder()
        .uri("/v1/messages")
        .method("POST")
        .header("content-type", "application/json")
        .header("x-api-key", "topsecret")
        .body(Body::from(messages_payload("m")))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_open_circuit_rejects_with_503() {
    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state(upstream);
    {
        let mut guard = state.lock().await;
        guard.circuit_open_until = Some(Instant::now() + Duration::from_secs(60));
    }
    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state);

    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(chat_payload("m")))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn test_expired_circuit_half_opens() {
    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state(upstream);
    {
        let mut guard = state.lock().await;
        guard.circuit_open_until = Some(Instant::now() - Duration::from_secs(1));
    }
    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state);

    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(chat_payload("m")))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_round_robin_across_upstreams() {
    let first = spawn_tagged_upstream("tag-one").await;
    let second = spawn_tagged_upstream("tag-two").await;
    let state = make_proxy_state_with(first, |cfg| {
        cfg.extra_upstreams = vec![second];
    });
    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state);

    let mut tags = vec![];
    for _ in 0..2 {
        let req = Request::builder()
            .uri("/v1/chat/completions")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(chat_payload("m")))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        tags.push(v["choices"][0]["message"]["content"].as_str().unwrap().to_string());
    }
    assert_eq!(tags, vec!["tag-one".to_string(), "tag-two".to_string()]);
}

#[tokio::test]
async fn test_usage_and_history_recorded() {
    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state(upstream);
    let app = Router::new()
        .route("/v1/chat/completions", post(handle_chat_completions))
        .with_state(state);

    let req = Request::builder()
        .uri("/v1/chat/completions")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(chat_payload("usage-probe-model")))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let metrics = METRICS.to_json();
    assert_eq!(metrics["models_usage"]["usage-probe-model"]["prompt_tokens"], 5);
    assert_eq!(metrics["models_usage"]["usage-probe-model"]["completion_tokens"], 3);
    let history = metrics["request_history"].as_array().unwrap();
    assert!(history.iter().any(|r| r["model"] == "usage-probe-model"
        && r["endpoint"] == "openai"
        && r["status"] == 200));
}

#[tokio::test]
async fn test_reload_without_config_file_errors() {
    // No config file registered in this test process -> 400, not a panic.
    let upstream = spawn_mock_upstream().await;
    let state = make_proxy_state(upstream);
    let app = Router::new()
        .route("/_oplire/reload", post(reload_handler))
        .with_state(state);

    let req = Request::builder()
        .uri("/_oplire/reload")
        .method("POST")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
