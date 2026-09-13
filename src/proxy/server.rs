use axum::{
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

use crate::config::ProxyConfig;
use crate::metrics::METRICS;
use crate::proxy::handlers::{handle_messages, handle_model_detail, handle_models, ProxyState};
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

async fn health_handler() -> impl IntoResponse {
    let body = serde_json::json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION")
    });
    (StatusCode::OK, Json(body))
}

async fn metrics_handler() -> impl IntoResponse {
    let data = METRICS.to_json();
    // Ensure correct content-type via Json extractor
    (StatusCode::OK, Json(data))
}

async fn dashboard_handler() -> impl IntoResponse {
    // Inline HTML string with JS polling /_oplire/metrics every 2s
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>oplire dashboard</title>
<style>
body{font-family:system-ui,Arial,sans-serif;background:#0f0f0f;color:#e6e6e6;margin:0;padding:24px}
h1{margin:0 0 16px;font-size:22px}
.card{background:#1a1a1a;border:1px solid #2a2a2a;border-radius:10px;padding:16px;margin:12px 0}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(220px,1fr));gap:12px}
.k{color:#9a9a9a;font-size:12px;text-transform:uppercase;letter-spacing:.06em}
.v{font-size:20px;font-weight:600;margin-top:4px}
pre{white-space:pre-wrap;word-break:break-all;background:#111;padding:12px;border-radius:8px;max-height:240px;overflow:auto}
a{color:#6ea8fe}
</style>
</head>
<body>
<h1>oplire — metrics dashboard</h1>
<div class="grid">
<div class="card"><div class="k">requests_total</div><div class="v" id="req">-</div></div>
<div class="card"><div class="k">rate_limited_total (429s)</div><div class="v" id="r429">-</div></div>
<div class="card"><div class="k">retry_total</div><div class="v" id="retry">-</div></div>
<div class="card"><div class="k">warp_resets</div><div class="v" id="warp">-</div></div>
<div class="card"><div class="k">avg_latency_ms</div><div class="v" id="lat">-</div></div>
<div class="card"><div class="k">uptime</div><div class="v" id="uptime">-</div></div>
</div>
<div class="card"><div class="k">latency histogram (ms, last 1024)</div><pre id="hist">-</pre></div>
<div class="card"><div class="k">warp ip history</div><pre id="warp_hist">-</pre></div>
<div class="card"><div class="k">raw json</div><pre id="raw">-</pre></div>
<p><a href="/health">/health</a> · <a href="/_oplire/metrics">/_oplire/metrics</a></p>
<script>
async function poll(){
  try{
    const r = await fetch('/_oplire/metrics');
    const j = await r.json();
    document.getElementById('req').textContent = j.requests_total ?? '-';
    document.getElementById('r429').textContent = j.rate_limited_total ?? '-';
    document.getElementById('retry').textContent = j.retry_total ?? '-';
    document.getElementById('warp').textContent = j.warp_resets ?? '-';
    const avg = j.avg_latency_ms ?? (j.latency_ms && j.latency_ms.avg_ms) ?? '-';
    document.getElementById('lat').textContent = typeof avg === 'number' ? avg.toFixed(2) : avg;
    document.getElementById('uptime').textContent = j.uptime_human ?? (j.uptime_secs ? j.uptime_secs.toFixed(1)+'s' : '-');
    const hist = j.latency_ms && j.latency_ms.histogram ? j.latency_ms.histogram : j.latency_ms || [];
    document.getElementById('hist').textContent = Array.isArray(hist) ? hist.slice(-50).join(', ') : JSON.stringify(hist);
    const wh = j.warp_ip_history || [];
    document.getElementById('warp_hist').textContent = wh.length ? wh.join('\n') : '(none)';
    document.getElementById('raw').textContent = JSON.stringify(j,null,2);
  }catch(e){
    document.getElementById('raw').textContent = 'poll error: '+e;
  }
}
poll(); setInterval(poll,2000);
</script>
</body>
</html>"#;
    let mut resp = Html(html).into_response();
    // Ensure content-type is text/html; Html already sets it
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp
}

pub async fn start_proxy_server(config: ProxyConfig) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let warp_resolver =
        WarpResolver::new(config.max_retries, config.warp_reset_delay_ms).with_hook(config.hook_on_429.clone());

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
        .route("/health", get(health_handler))
        .route("/_oplire/metrics", get(metrics_handler))
        .route("/dashboard", get(dashboard_handler))
        .layer(middleware::from_fn(log_requests))
        .with_state(state);

    let addr: SocketAddr = config
        .listen_addr
        .parse()
        .unwrap_or_else(|_| "127.0.0.1:8080".parse().unwrap());

    info!("Starting proxy server on {}", addr);
    info!("OpenCode Zen upstream: {}", config.opencode_base_url);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!("Proxy server listening on http://{}", addr);

    // Graceful shutdown placeholder: handles Ctrl+C / SIGTERM
    async fn shutdown_signal() {
        let ctrl_c = async {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install signal handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
        info!("Shutdown signal received, starting graceful shutdown");
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}
