use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Core metrics store. Uses atomics for counters and Mutex for
/// histogram / IP history. Cheap to clone via global static.
pub struct Metrics {
    pub requests_total: AtomicU64,
    pub rate_limited_total: AtomicU64,
    pub retry_total: AtomicU64,
    pub warp_resets: AtomicU64,
    start: Instant,
    /// Ring buffer of recent latencies in ms (cap 1024)
    latency_ms: Mutex<Vec<u64>>,
    /// Running sum/count for avg calculation (avoid locking for avg)
    latency_sum: AtomicU64,
    latency_count: AtomicU64,
    /// History of WARP IP resets (most recent last, cap 100)
    warp_ip_history: Mutex<Vec<String>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            rate_limited_total: AtomicU64::new(0),
            retry_total: AtomicU64::new(0),
            warp_resets: AtomicU64::new(0),
            start: Instant::now(),
            latency_ms: Mutex::new(Vec::with_capacity(1024)),
            latency_sum: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            warp_ip_history: Mutex::new(Vec::new()),
        }
    }

    pub fn inc_request(&self) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_429(&self) {
        self.rate_limited_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_retry(&self) {
        self.retry_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_warp_reset(&self) {
        self.warp_resets.fetch_add(1, Ordering::Relaxed);
    }

    /// Record latency in milliseconds. Maintains ring buffer + running avg.
    pub fn record_latency(&self, ms: u64) {
        self.latency_sum.fetch_add(ms, Ordering::Relaxed);
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut hist) = self.latency_ms.lock() {
            if hist.len() >= 1024 {
                // simple ring: drop oldest
                hist.remove(0);
            }
            hist.push(ms);
        }
    }

    /// Push a WARP IP into history (cap 100, most recent last).
    pub fn push_warp_ip(&self, ip: String) {
        if let Ok(mut h) = self.warp_ip_history.lock() {
            if h.len() >= 100 {
                h.remove(0);
            }
            h.push(ip);
        }
        self.inc_warp_reset();
    }

    /// Average latency in ms (0 if no samples).
    pub fn avg_latency_ms(&self) -> f64 {
        let count = self.latency_count.load(Ordering::Relaxed);
        if count == 0 {
            return 0.0;
        }
        let sum = self.latency_sum.load(Ordering::Relaxed);
        sum as f64 / count as f64
    }

    /// Uptime in seconds as f64.
    pub fn uptime_secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// Serialize to JSON Value for /_oplire/metrics endpoint.
    pub fn to_json(&self) -> Value {
        let requests_total = self.requests_total.load(Ordering::Relaxed);
        let rate_limited_total = self.rate_limited_total.load(Ordering::Relaxed);
        let retry_total = self.retry_total.load(Ordering::Relaxed);
        let warp_resets = self.warp_resets.load(Ordering::Relaxed);
        let uptime_secs = self.uptime_secs();
        let avg_latency_ms = self.avg_latency_ms();
        let latency_count = self.latency_count.load(Ordering::Relaxed);

        // Clone histogram under lock
        let latency_histogram: Vec<u64> = self
            .latency_ms
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();

        let warp_ip_history: Vec<String> = self
            .warp_ip_history
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default();

        // Optional: compute p95-ish max/min for quick view
        let latency_p95 = if latency_histogram.is_empty() {
            0
        } else {
            let mut sorted = latency_histogram.clone();
            sorted.sort_unstable();
            let idx = ((sorted.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
            sorted[idx.min(sorted.len() - 1)]
        };

        json!({
            "requests_total": requests_total,
            "rate_limited_total": rate_limited_total,
            "retry_total": retry_total,
            "warp_resets": warp_resets,
            "uptime_secs": uptime_secs,
            "uptime_human": format!("{:.1}s", uptime_secs),
            "latency_ms": {
                "count": latency_count,
                "avg_ms": avg_latency_ms,
                "p95_ms": latency_p95,
                "histogram": latency_histogram,
            },
            "avg_latency_ms": avg_latency_ms,
            "latency_count": latency_count,
            "warp_ip_history": warp_ip_history,
        })
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrapper that exposes Metrics methods directly on the global static
/// while still backing storage with a OnceLock. This satisfies the
/// requirement "global static METRICS via OnceLock" and allows
/// `crate::metrics::METRICS.inc_request()` style calls.
pub struct MetricsGlobal(OnceLock<Metrics>);

impl MetricsGlobal {
    const fn new() -> Self {
        Self(OnceLock::new())
    }

    fn get(&self) -> &Metrics {
        self.0.get_or_init(Metrics::new)
    }

    pub fn inc_request(&self) {
        self.get().inc_request()
    }

    pub fn inc_429(&self) {
        self.get().inc_429()
    }

    pub fn inc_retry(&self) {
        self.get().inc_retry()
    }

    pub fn inc_warp_reset(&self) {
        self.get().inc_warp_reset()
    }

    pub fn record_latency(&self, ms: u64) {
        self.get().record_latency(ms)
    }

    pub fn push_warp_ip(&self, ip: String) {
        self.get().push_warp_ip(ip)
    }

    pub fn to_json(&self) -> Value {
        self.get().to_json()
    }
}

/// Global metrics instance. Import via `use crate::metrics::METRICS;`
/// and call `METRICS.inc_request()` etc. Backed by OnceLock for lazy init.
pub static METRICS: MetricsGlobal = MetricsGlobal(OnceLock::new());

/// Helper to get raw Metrics reference (alternative access).
pub fn global() -> &'static Metrics {
    METRICS.get()
}
