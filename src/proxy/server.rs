use axum::{
    extract::State,
    http::{header, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::config::ProxyConfig;
use crate::proxy::handlers::{
    handle_messages, handle_model_detail, handle_models, handle_readiness, ProxyState,
};
use crate::warp::WarpResolver;

async fn log_requests(
    request: axum::http::Request<axum::body::Body>,
    next: Next,
) -> axum::response::Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    info!("→ {} {}", method, uri);
    let response = next.run(request).await;
    info!("← {} {} {}", method, uri, response.status());
    response
}

async fn authenticate_requests(
    State(expected_key): State<Option<String>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let Some(expected_key) = expected_key else {
        return next.run(request).await;
    };

    let headers = request.headers();
    let bearer_key = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let api_key = headers.get("x-api-key").and_then(|value| value.to_str().ok());

    if bearer_key == Some(expected_key.as_str()) || api_key == Some(expected_key.as_str()) {
        next.run(request).await
    } else {
        (StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
    }
}

pub fn build_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .read_timeout(std::time::Duration::from_secs(300))
        .build()
}

pub async fn start_proxy_server(config: ProxyConfig) -> anyhow::Result<()> {
    let client = build_client()?;

    let warp_resolver =
        WarpResolver::new(config.max_retries, config.warp_reset_delay_ms);

    let state = Arc::new(Mutex::new(ProxyState {
        config: config.clone(),
        client,
        warp_resolver,
    }));

    let app = Router::new()
        .route("/v1/models", get(handle_models))
        .route("/v1/models/{model_id}", get(handle_model_detail))
        .route("/v1/messages", post(handle_messages))
        .route("/v1/chat/completions", post(handle_messages))
        .route("/health", get(|| async { "OK" }))
        .route("/readyz", get(handle_readiness))
        .layer(middleware::from_fn_with_state(
            config.inbound_api_key.clone(),
            authenticate_requests,
        ))
        .layer(middleware::from_fn(log_requests))
        .with_state(state);

    let addr: SocketAddr = config
        .listen_addr
        .parse()
        .unwrap_or_else(|_| "127.0.0.1:8080".parse().unwrap());

    if !addr.ip().is_loopback() && config.inbound_api_key.is_none() {
        tracing::warn!(
            "Proxy is listening on a non-loopback address without client authentication"
        );
    }

    info!("Starting proxy server on {}", addr);
    info!("OpenCode Zen upstream: {}", config.opencode_base_url);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Proxy server listening on http://{}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get};
    use tower::ServiceExt;

    fn auth_app() -> Router {
        Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(middleware::from_fn_with_state(
                Some("proxy-secret".to_string()),
                authenticate_requests,
            ))
    }

    #[tokio::test]
    async fn rejects_requests_without_a_proxy_key() {
        let response = auth_app()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accepts_bearer_and_anthropic_api_key_headers() {
        for header in [
            ("authorization", "Bearer proxy-secret"),
            ("x-api-key", "proxy-secret"),
        ] {
            let response = auth_app()
                .oneshot(
                    Request::builder()
                        .uri("/")
                        .header(header.0, header.1)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
        }
    }
}
