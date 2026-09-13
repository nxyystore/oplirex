use axum::{
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;
use tracing::info;

use crate::config::{AppConfig, ProxyConfig};
use crate::metrics::METRICS;
use crate::proxy::handlers::{handle_chat_completions, handle_messages, handle_model_detail, handle_models, ProxyState};
use crate::warp::WarpResolver;

/// Config file the server was booted from (`None` = explicit flags only).
/// Set once at startup; used by `POST /_oplire/reload` and SIGHUP handling.
static CONFIG_FILE: OnceLock<Option<PathBuf>> = OnceLock::new();

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

async fn health_handler(State(state): State<Arc<Mutex<ProxyState>>>) -> impl IntoResponse {
    let guard = state.lock().await;
    let circuit_open = guard.circuit_open_until.and_then(|until| {
        let now = std::time::Instant::now();
        if now < until {
            Some(until.duration_since(now).as_secs().max(1))
        } else {
            None
        }
    });
    let body = serde_json::json!({
        "status": if circuit_open.is_some() { "degraded" } else { "ok" },
        "version": env!("CARGO_PKG_VERSION"),
        "upstreams": guard.config.effective_upstreams(),
        "provider": guard.config.provider.to_string(),
        "circuit": {
            "open": circuit_open.is_some(),
            "cooldown_remaining_secs": circuit_open,
            "consecutive_429": guard.consecutive_429,
            "threshold": guard.config.circuit_threshold,
        },
    });
    (StatusCode::OK, Json(body))
}

/// Admin gate for `/_oplire/*` and `/dashboard` when `require_api_key` is set.
/// Accepts the key via header (`Authorization: Bearer` / `x-api-key`) or the
/// `?key=` query param (so the dashboard works in a plain browser visit).
fn check_admin_access(
    require_api_key: &Option<String>,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> bool {
    let Some(expected) = require_api_key.as_deref() else {
        return true;
    };
    if let Some(q) = query.get("key") {
        if q == expected {
            return true;
        }
    }
    let header_hit = headers
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
    matches!(header_hit, Some(got) if got == expected)
}

fn admin_snapshot(state: &tokio::sync::MutexGuard<'_, ProxyState>) -> Option<String> {
    state.config.require_api_key.clone()
}

async fn metrics_handler(
    State(state): State<Arc<Mutex<ProxyState>>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let require = { admin_snapshot(&state.lock().await) };
    if !check_admin_access(&require, &headers, &query) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "unauthorized"})))
            .into_response();
    }
    (StatusCode::OK, Json(METRICS.to_json())).into_response()
}

/// Re-read the boot config file and hot-swap runtime settings.
/// `listen_addr` is intentionally preserved (changing it needs a rebind).
async fn reload_from_file(state: &Arc<Mutex<ProxyState>>) -> Result<Vec<String>, String> {
    let path = CONFIG_FILE
        .get()
        .and_then(|p| p.clone())
        .ok_or_else(|| "no config file: server was started with explicit flags".to_string())?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;
    let app: AppConfig = serde_json::from_str(&raw)
        .map_err(|e| format!("cannot parse {}: {}", path.display(), e))?;
    let fresh = app.into_proxy_config();
    let mut guard = state.lock().await;
    let mut changed = Vec::new();
    if guard.config.opencode_base_url != fresh.opencode_base_url {
        changed.push(format!(
            "upstream {} -> {}",
            guard.config.opencode_base_url, fresh.opencode_base_url
        ));
        guard.config.opencode_base_url = fresh.opencode_base_url;
    }
    if guard.config.extra_upstreams != fresh.extra_upstreams {
        changed.push(format!("extra_upstreams -> {:?}", fresh.extra_upstreams));
        guard.config.extra_upstreams = fresh.extra_upstreams;
    }
    if guard.config.opencode_api_key != fresh.opencode_api_key {
        changed.push("opencode_api_key updated".to_string());
        guard.config.opencode_api_key = fresh.opencode_api_key;
    }
    if guard.config.require_api_key != fresh.require_api_key {
        changed.push("require_api_key updated".to_string());
        guard.config.require_api_key = fresh.require_api_key;
    }
    if guard.config.max_retries != fresh.max_retries {
        changed.push(format!("max_retries -> {}", fresh.max_retries));
        guard.config.max_retries = fresh.max_retries;
    }
    if guard.config.warp_reset_delay_ms != fresh.warp_reset_delay_ms {
        changed.push(format!("warp_delay -> {}", fresh.warp_reset_delay_ms));
        guard.config.warp_reset_delay_ms = fresh.warp_reset_delay_ms;
    }
    if guard.config.hook_on_429 != fresh.hook_on_429 {
        changed.push("hook_on_429 updated".to_string());
        guard.config.hook_on_429 = fresh.hook_on_429;
    }
    if guard.config.fallback_models != fresh.fallback_models {
        changed.push(format!("fallback_models -> {:?}", fresh.fallback_models));
        guard.config.fallback_models = fresh.fallback_models;
    }
    if guard.config.circuit_threshold != fresh.circuit_threshold
        || guard.config.circuit_cooldown_secs != fresh.circuit_cooldown_secs
    {
        changed.push(format!(
            "circuit -> threshold {} / cooldown {}s",
            fresh.circuit_threshold, fresh.circuit_cooldown_secs
        ));
        guard.config.circuit_threshold = fresh.circuit_threshold;
        guard.config.circuit_cooldown_secs = fresh.circuit_cooldown_secs;
    }
    if guard.config.listen_addr != fresh.listen_addr {
        changed.push(format!(
            "listen_addr change ignored ({} kept; restart to move)",
            guard.config.listen_addr
        ));
    }
    if guard.config.provider != fresh.provider {
        changed.push(format!("provider -> {}", fresh.provider));
        guard.config.provider = fresh.provider;
    }
    info!("Config reloaded from {}: {:?}", path.display(), changed);
    Ok(changed)
}

pub async fn reload_handler(
    State(state): State<Arc<Mutex<ProxyState>>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let require = { admin_snapshot(&state.lock().await) };
    if !check_admin_access(&require, &headers, &query) {
        return (StatusCode::UNAUTHORIZED, Json(serde_json::json!({"error": "unauthorized"})))
            .into_response();
    }
    match reload_from_file(&state).await {
        Ok(changed) => (
            StatusCode::OK,
            Json(serde_json::json!({"status": "reloaded", "changed": changed})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"status": "error", "message": e})),
        )
            .into_response(),
    }
}

async fn dashboard_handler(
    State(state): State<Arc<Mutex<ProxyState>>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let require = { admin_snapshot(&state.lock().await) };
    if !check_admin_access(&require, &headers, &query) {
        return (StatusCode::UNAUTHORIZED, Html("unauthorized")).into_response();
    }
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
<div class="card"><div class="k">fallbacks</div><div class="v" id="fb">-</div></div>
<div class="card"><div class="k">circuit opens</div><div class="v" id="co">-</div></div>
<div class="card"><div class="k">preemptive resets</div><div class="v" id="pre">-</div></div>
<div class="card"><div class="k">avg_latency_ms</div><div class="v" id="lat">-</div></div>
<div class="card"><div class="k">uptime</div><div class="v" id="uptime">-</div></div>
</div>
<div class="card"><div class="k">latency histogram (ms, last 1024)</div><pre id="hist">-</pre></div>
<div class="card"><div class="k">per-model token usage</div><pre id="models">-</pre></div>
<div class="card"><div class="k">recent requests (last 100)</div><pre id="reqhist">-</pre></div>
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
    document.getElementById('fb').textContent = j.fallback_total ?? '-';
    document.getElementById('co').textContent = j.circuit_opens ?? '-';
    document.getElementById('pre').textContent = j.preemptive_resets ?? '-';
    const avg = j.avg_latency_ms ?? (j.latency_ms && j.latency_ms.avg_ms) ?? '-';
    document.getElementById('lat').textContent = typeof avg === 'number' ? avg.toFixed(2) : avg;
    document.getElementById('uptime').textContent = j.uptime_human ?? (j.uptime_secs ? j.uptime_secs.toFixed(1)+'s' : '-');
    const hist = j.latency_ms && j.latency_ms.histogram ? j.latency_ms.histogram : j.latency_ms || [];
    document.getElementById('hist').textContent = Array.isArray(hist) ? hist.slice(-50).join(', ') : JSON.stringify(hist);
    const wh = j.warp_ip_history || [];
    document.getElementById('warp_hist').textContent = wh.length ? wh.join('\n') : '(none)';
    const mu = j.models_usage || {};
    document.getElementById('models').textContent = Object.keys(mu).length
      ? Object.entries(mu).map(([m,u]) => `${m}: ${u.requests} req, ${u.prompt_tokens} in / ${u.completion_tokens} out (${u.total_tokens} total)`).join('\n')
      : '(no non-streaming usage recorded yet)';
    const rh = j.request_history || [];
    document.getElementById('reqhist').textContent = rh.length
      ? rh.slice(-20).reverse().map(r => `${new Date(r.ts_millis).toLocaleTimeString()} ${r.endpoint} ${r.model} → ${r.status} (${r.latency_ms}ms)`).join('\n')
      : '(none)';
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

/// Probe all effective upstreams; Ok when at least one answers /v1/models.
/// Failing fast here beats serving confusing errors to every client later.
async fn probe_upstreams(client: &reqwest::Client, config: &ProxyConfig) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for upstream in config.effective_upstreams() {
        let url = format!("{}/v1/models", upstream.trim_end_matches('/'));
        match tokio::time::timeout(std::time::Duration::from_secs(5), client.get(&url).send())
            .await
        {
            Ok(Ok(resp)) if resp.status().is_success() => return Ok(()),
            Ok(Ok(resp)) => failures.push(format!("{} -> HTTP {}", upstream, resp.status())),
            Ok(Err(e)) => failures.push(format!("{} -> {}", upstream, e)),
            Err(_) => failures.push(format!("{} -> timeout after 5s", upstream)),
        }
    }
    anyhow::bail!(
        "no upstream reachable ({}). Is `opencode serve` running? Start it first, or point --upstream at a live server.",
        failures.join("; ")
    )
}

pub async fn start_proxy_server(config: ProxyConfig) -> anyhow::Result<()> {
    start_proxy_server_with_config_file(config, None).await
}

/// Boot the proxy, remembering `config_file` for `POST /_oplire/reload` + SIGHUP.
pub async fn start_proxy_server_with_config_file(
    config: ProxyConfig,
    config_file: Option<PathBuf>,
) -> anyhow::Result<()> {
    let _ = CONFIG_FILE.set(config_file);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    // Fail fast: refuse to boot when no upstream answers.
    probe_upstreams(&client, &config).await?;

    let warp_resolver =
        WarpResolver::new(config.max_retries, config.warp_reset_delay_ms).with_hook(config.hook_on_429.clone());

    let state = Arc::new(Mutex::new(ProxyState::new(
        config.clone(),
        client,
        warp_resolver,
    )));

    // SIGHUP hot-reload (unix only).
    #[cfg(unix)]
    {
        let reload_state = state.clone();
        tokio::spawn(async move {
            let mut hups = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .expect("failed to install SIGHUP handler");
            loop {
                hups.recv().await;
                info!("SIGHUP received, reloading config");
                match reload_from_file(&reload_state).await {
                    Ok(changed) => info!("Reloaded config: {:?}", changed),
                    Err(e) => tracing::warn!("Config reload failed: {}", e),
                }
            }
        });
    }

    let app = Router::new()
        .route("/v1/models", get(handle_models))
        .route("/v1/models/{model_id}", get(handle_model_detail))
        .route("/v1/messages", post(handle_messages))
        .route("/v1/chat/completions", post(handle_chat_completions))
        .route("/health", get(health_handler))
        .route("/_oplire/metrics", get(metrics_handler))
        .route("/_oplire/reload", post(reload_handler))
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
