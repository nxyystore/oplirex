use std::collections::VecDeque;
use std::time::Duration;
use tokio::time::interval;
use tracing::{info, warn, error};

use crate::hooks::{run_hook, HookEnv};
use crate::metrics::METRICS;
use crate::warp::WarpResolver;

pub async fn start_watch_mode(
    upstream: &str,
    max_retries: u32,
    warp_delay_ms: u64,
) -> anyhow::Result<()> {
    start_watch_mode_with_hook(upstream, max_retries, warp_delay_ms, None).await
}

pub async fn start_watch_mode_with_hook(
    upstream: &str,
    max_retries: u32,
    warp_delay_ms: u64,
    hook_on_429: Option<String>,
) -> anyhow::Result<()> {
    start_watch_mode_full(upstream, max_retries, warp_delay_ms, hook_on_429, 10, 3).await
}

/// Watch with preemptive rotation: `preemptive_window` polls form the trend
/// window, and `preemptive_threshold` 429s inside it trigger an early WARP
/// rotation — before real traffic starts failing hard.
pub async fn start_watch_mode_full(
    upstream: &str,
    max_retries: u32,
    warp_delay_ms: u64,
    hook_on_429: Option<String>,
    preemptive_window: usize,
    preemptive_threshold: u32,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let resolver = WarpResolver::new(max_retries, warp_delay_ms).with_hook(hook_on_429.clone());
    let models_url = format!("{}/v1/models", upstream.trim_end_matches('/'));

    let window = preemptive_window.max(2);
    let threshold = preemptive_threshold.max(1);
    let mut check_interval = interval(Duration::from_secs(30));
    let mut recent: VecDeque<bool> = VecDeque::with_capacity(window);
    let mut polls_since_preempt = usize::MAX;
    let mut consecutive_429 = 0u32;

    info!(
        "Watch mode active. Checking {} every 30s (preemptive window {}/{})",
        models_url, threshold, window
    );

    loop {
        check_interval.tick().await;

        match client.get(&models_url).send().await {
            Ok(resp) => {
                let status = resp.status();

                if status.is_success() {
                    if consecutive_429 > 0 {
                        info!("Rate limit recovered (was {} consecutive 429s)", consecutive_429);
                    }
                    consecutive_429 = 0;
                    push_poll(&mut recent, window, false);
                    polls_since_preempt = polls_since_preempt.saturating_add(1);

                    // Preemptive signal 1: upstream says quota is nearly gone.
                    let remaining = resp
                        .headers()
                        .get("x-ratelimit-remaining")
                        .or_else(|| resp.headers().get("ratelimit-remaining"))
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok());
                    if let Some(n) = remaining {
                        info!("OpenCode Zen: OK (status {}), ratelimit-remaining={}", status, n);
                        if n <= threshold as u64 && polls_since_preempt >= window {
                            preemptive_reset(
                                &resolver,
                                &mut polls_since_preempt,
                                &format!("quota nearly exhausted (remaining={})", n),
                            )
                            .await;
                        }
                    } else {
                        info!("OpenCode Zen: OK (status {})", status);
                    }

                    // Preemptive signal 2: 429 trend inside the window.
                    if polls_since_preempt >= window
                        && recent.iter().filter(|&&limited| limited).count() as u32 >= threshold
                    {
                        preemptive_reset(
                            &resolver,
                            &mut polls_since_preempt,
                            &format!(
                                "429 trend ({} of last {} polls limited)",
                                recent.iter().filter(|&&l| l).count(),
                                recent.len()
                            ),
                        )
                        .await;
                        recent.clear();
                    }
                } else if status.as_u16() == 429 {
                    consecutive_429 += 1;
                    push_poll(&mut recent, window, true);
                    polls_since_preempt = polls_since_preempt.saturating_add(1);
                    if let Some(hook) = &hook_on_429 {
                        let mut env = HookEnv::new(consecutive_429);
                        env.upstream = Some(upstream.to_string());
                        run_hook(hook, env);
                    }
                    warn!(
                        "Rate limit detected (count: {}). Triggering WARP reset...",
                        consecutive_429
                    );

                    if resolver.handle_429(consecutive_429 - 1).await {
                        info!("WARP reset triggered. Rechecking...");
                        tokio::time::sleep(Duration::from_secs(5)).await;

                        match client.get(&models_url).send().await {
                            Ok(retry_resp) if retry_resp.status().is_success() => {
                                info!("Recovery confirmed. OpenCode Zen is accessible.");
                                consecutive_429 = 0;
                            }
                            _ => {
                                warn!("Recovery check failed. Will retry on next cycle.");
                            }
                        }
                    } else {
                        error!("WARP reset failed. Rate limit persists.");
                    }
                } else {
                    warn!("OpenCode Zen returned unexpected status: {}", status);
                }
            }
            Err(e) => {
                error!("Failed to reach OpenCode Zen: {}", e);
            }
        }
    }
}

fn push_poll(recent: &mut VecDeque<bool>, window: usize, limited: bool) {
    if recent.len() >= window {
        recent.pop_front();
    }
    recent.push_back(limited);
}

async fn preemptive_reset(
    resolver: &WarpResolver,
    polls_since_preempt: &mut usize,
    reason: &str,
) {
    warn!("Preemptive WARP rotation: {}.", reason);
    // retry_count 0: budget check only, never trips the resolver's max-retries.
    if resolver.handle_429(0).await {
        METRICS.inc_preemptive_reset();
        info!("Preemptive rotation done before hard rate limiting.");
    } else {
        warn!("Preemptive rotation skipped (reset already in progress or budget spent).");
    }
    *polls_since_preempt = 0;
}
