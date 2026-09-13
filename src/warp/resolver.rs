use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::hooks::{run_hook, HookEnv};

static ACTIVE_429_COUNT: AtomicU32 = AtomicU32::new(0);
static RESET_IN_PROGRESS: Mutex<()> = Mutex::const_new(());

pub struct WarpResolver {
    pub max_retries: u32,
    pub reset_delay_ms: u64,
    pub hook_on_429: Option<String>,
}

impl WarpResolver {
    pub fn new(max_retries: u32, reset_delay_ms: u64) -> Self {
        Self {
            max_retries,
            reset_delay_ms,
            hook_on_429: None,
        }
    }

    pub fn with_hook(mut self, hook: Option<String>) -> Self {
        self.hook_on_429 = hook;
        self
    }

    pub fn set_hook(&mut self, hook: Option<String>) {
        self.hook_on_429 = hook;
    }

    pub async fn handle_429(&self, retry_count: u32) -> bool {
        if retry_count >= self.max_retries {
            warn!(
                "Max retry attempts ({}) reached, giving up",
                self.max_retries
            );
            return false;
        }

        let _guard = match RESET_IN_PROGRESS.try_lock() {
            Ok(g) => g,
            Err(_) => {
                info!("WARP reset already in progress, waiting...");
                tokio::time::sleep(Duration::from_millis(self.reset_delay_ms)).await;
                return true;
            }
        };

        let count = ACTIVE_429_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        info!(
            "HTTP 429 detected (count: {}). Initiating WARP reset...",
            count
        );

        // Fire hook non-blocking before reset (provides RETRY_COUNT, WARP_IP etc)
        if let Some(hook) = &self.hook_on_429 {
            let mut env = HookEnv::new(retry_count);
            // Try to capture current WARP IP best-effort
            if let Ok(ip) = fetch_warp_ip().await {
                env.warp_ip = Some(ip);
            }
            run_hook(hook, env);
        }

        if let Err(e) = self.reset_warp().await {
            error!("WARP reset failed: {}", e);
            return false;
        }

        info!("WARP reset complete. Waiting for connection stabilization...");
        tokio::time::sleep(Duration::from_millis(self.reset_delay_ms)).await;

        ACTIVE_429_COUNT.fetch_sub(1, Ordering::SeqCst);
        true
    }

    async fn reset_warp(&self) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            self.reset_warp_windows().await
        }
        #[cfg(target_os = "macos")]
        {
            self.reset_warp_macos().await
        }
        #[cfg(target_os = "linux")]
        {
            self.reset_warp_linux().await
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            self.reset_warp_generic().await
        }
    }

    #[cfg(target_os = "linux")]
    async fn reset_warp_linux(&self) -> Result<(), String> {
        info!("Step 1: warp-cli disconnect");
        run_warp_command(&["disconnect"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 2: systemctl stop warp-svc");
        run_privileged_command("systemctl stop warp-svc").await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        info!("Step 3: Clearing WARP cache");
        run_privileged_command("rm -rf /var/lib/cloudflare-warp/*").await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        info!("Step 4: systemctl start warp-svc");
        run_privileged_command("systemctl start warp-svc").await?;

        tokio::time::sleep(Duration::from_millis(2000)).await;

        info!("Step 5: warp-cli registration new");
        run_warp_command(&["registration", "new"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 6: warp-cli connect");
        run_warp_command(&["connect"]).await?;

        info!("WARP tunnel reset complete");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn reset_warp_macos(&self) -> Result<(), String> {
        info!("Step 1: warp-cli disconnect");
        run_warp_command(&["disconnect"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 2: launchctl unload warp daemon");
        // Try launchctl, fallback to warp-svc if custom install
        let _ = run_privileged_command(
            "launchctl unload /Library/LaunchDaemons/com.cloudflare.warp.plist 2>/dev/null || launchctl bootout system/com.cloudflare.warp 2>/dev/null || systemctl stop warp-svc 2>/dev/null || true",
        )
        .await;

        tokio::time::sleep(Duration::from_millis(500)).await;

        info!("Step 3: Clearing WARP cache (macOS)");
        run_privileged_command(
            "rm -rf /var/db/com.cloudflare.warp/* /Library/Application\\ Support/Cloudflare/warp/* 2>/dev/null; true",
        )
        .await?;

        tokio::time::sleep(Duration::from_millis(500)).await;

        info!("Step 4: launchctl load warp daemon");
        let _ = run_privileged_command(
            "launchctl load /Library/LaunchDaemons/com.cloudflare.warp.plist 2>/dev/null || launchctl bootstrap system /Library/LaunchDaemons/com.cloudflare.warp.plist 2>/dev/null || systemctl start warp-svc 2>/dev/null || true",
        )
        .await;

        tokio::time::sleep(Duration::from_millis(2000)).await;

        info!("Step 5: warp-cli registration new");
        run_warp_command(&["registration", "new"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 6: warp-cli connect");
        run_warp_command(&["connect"]).await?;

        info!("WARP tunnel reset complete");
        Ok(())
    }

    #[cfg(target_os = "windows")]
    async fn reset_warp_windows(&self) -> Result<(), String> {
        info!("Step 1: warp-cli disconnect");
        run_warp_command(&["disconnect"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 2: stopping Cloudflare WARP service");
        // sc/net are available without sudo on Windows; try both service names
        let _ = run_privileged_command(
            "net stop CloudflareWARP 2>nul & sc stop CloudflareWARP 2>nul & net stop warp-svc 2>nul & sc stop warp-svc 2>nul & timeout /t 2 >nul & exit 0",
        )
        .await;

        tokio::time::sleep(Duration::from_millis(800)).await;

        info!("Step 3: Clearing WARP cache (Windows)");
        // Best-effort: registration delete resets identity without manual file removal
        let _ = run_warp_command(&["registration", "delete"]).await;
        // Also try clearing ProgramData cache if we have permission
        let _ = run_privileged_command(
            "rmdir /s /q \"%ProgramData%\\Cloudflare\\WARP\" 2>nul & del /q \"%ProgramData%\\Cloudflare\\*.conf\" 2>nul & exit 0",
        )
        .await;

        tokio::time::sleep(Duration::from_millis(500)).await;

        info!("Step 4: starting Cloudflare WARP service");
        let _ = run_privileged_command(
            "net start CloudflareWARP 2>nul & sc start CloudflareWARP 2>nul & net start warp-svc 2>nul & sc start warp-svc 2>nul & timeout /t 2 >nul & exit 0",
        )
        .await;

        tokio::time::sleep(Duration::from_millis(2500)).await;

        info!("Step 5: warp-cli registration new");
        run_warp_command(&["registration", "new"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 6: warp-cli connect");
        run_warp_command(&["connect"]).await?;

        info!("WARP tunnel reset complete");
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    async fn reset_warp_generic(&self) -> Result<(), String> {
        info!("Step 1: warp-cli disconnect");
        run_warp_command(&["disconnect"]).await?;
        tokio::time::sleep(Duration::from_millis(1000)).await;
        info!("Step 2: warp-cli registration new");
        run_warp_command(&["registration", "new"]).await?;
        tokio::time::sleep(Duration::from_millis(1000)).await;
        info!("Step 3: warp-cli connect");
        run_warp_command(&["connect"]).await?;
        info!("WARP tunnel reset complete (generic)");
        Ok(())
    }
}

async fn run_warp_command(args: &[&str]) -> Result<(), String> {
    // On Windows the binary is warp-cli.exe and may not be on PATH — try common locations
    #[cfg(target_os = "windows")]
    let candidates = [
        "warp-cli",
        "warp-cli.exe",
        r"C:\Program Files\Cloudflare\Cloudflare WARP\warp-cli.exe",
    ];
    #[cfg(not(target_os = "windows"))]
    let candidates: &[&str] = &["warp-cli"];

    let mut last_err = String::new();
    for bin in candidates {
        let output = Command::new(bin)
            .args(args)
            .output()
            .await
            .map_err(|e| format!("Failed to execute {bin}: {e}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let msg = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else {
            stdout.trim().to_string()
        };
        last_err = format!("{bin} {args:?} failed: {msg}");
        // Only try next candidate if binary not found; if command ran but failed, return error
        #[cfg(target_os = "windows")]
        if msg.contains("Failed to execute") || msg.contains("not found") {
            continue;
        }
        #[cfg(not(target_os = "windows"))]
        break;
    }
    Err(last_err)
}

async fn run_privileged_command(cmd: &str) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        // Windows: use cmd /C (no sudo). Caller ensures `& exit 0` for best-effort steps.
        let output = Command::new("cmd")
            .arg("/C")
            .arg(cmd)
            .output()
            .await
            .map_err(|e| format!("Failed to execute cmd /C {cmd}: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let msg = if !stderr.trim().is_empty() {
                stderr.trim()
            } else {
                stdout.trim()
            };
            // Best-effort commands contain `exit 0` and should not fail; otherwise surface error
            if cmd.contains("exit 0") {
                Ok(())
            } else {
                Err(format!("cmd /C {cmd} failed: {msg}"))
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        // macOS: sudo -n (non-interactive); same as Linux but different cache paths handled by caller
        let output = Command::new("sudo")
            .arg("-n")
            .arg("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .await
            .map_err(|e| format!("Failed to execute sudo {cmd}: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("sudo {cmd} failed: {}", stderr.trim()))
        }
    }
    #[cfg(target_os = "linux")]
    {
        let output = Command::new("sudo")
            .arg("-n")
            .arg("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .await
            .map_err(|e| format!("Failed to execute sudo {cmd}: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("sudo {cmd} failed: {}", stderr.trim()))
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        let output = Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .await
            .map_err(|e| format!("Failed to execute sh -c {cmd}: {e}"))?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(format!("sh -c {cmd} failed: {}", stderr.trim()))
        }
    }
}

async fn fetch_warp_ip() -> Result<String, String> {
    let out = Command::new("warp-cli")
        .arg("status")
        .output()
        .await
        .map_err(|e| e.to_string())?;
    let txt = String::from_utf8_lossy(&out.stdout).to_string();
    // Try to parse IP from status output; fallback to empty
    for line in txt.lines() {
        let l = line.trim();
        if l.contains("IP") || l.contains("ip") {
            // heuristic: extract first token looking like ipv4
            for tok in l.split_whitespace() {
                if tok.chars().filter(|c| *c == '.').count() == 3 {
                    return Ok(tok.trim_matches(|c: char| !c.is_ascii_digit() && c != '.').to_string());
                }
            }
        }
    }
    // fallback: try curl ifconfig
    Ok(txt.lines().next().unwrap_or("").to_string())
}
