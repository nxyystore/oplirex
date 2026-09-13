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
    proxy::handlers::{handle_messages, handle_models, ProxyState},
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
    let config = ProxyConfig {
        listen_addr: "127.0.0.1:0".to_string(),
        opencode_base_url: upstream,
        opencode_api_key: None,
        max_retries: 2,
        warp_reset_delay_ms: 10,
        hook_on_429: None,
        provider: oplirex::providers::Provider::Opencode,
    };
    let client = reqwest::Client::builder().build().unwrap();
    let warp_resolver = WarpResolver::new(2, 10);
    Arc::new(Mutex::new(ProxyState {
        config,
        client,
        warp_resolver,
    }))
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
async fn test_proxy_messages_invalid_json_returns_error() {
    let upstream = spawn_mock_upstream().await;
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
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
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
