use std::fs;
use std::path::PathBuf;
use std::process::Command;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------
fn current_exe_string() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "oplirex".to_string())
}

fn systemd_unit_content(exe: &str) -> String {
    format!(
        r#"[Unit]
Description=Oplirex Proxy Bridge (oplire)
After=network.target

[Service]
ExecStart={exe} proxy --listen 127.0.0.1:8080 --upstream http://localhost:3000
Restart=always
RestartSec=5
Environment=RUST_LOG=info

[Install]
WantedBy=multi-user.target
"#
    )
}

fn launchd_plist_content(exe: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.nxyy.oplirex</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>proxy</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/tmp/oplirex.log</string>
    <key>StandardErrorPath</key>
    <string>/tmp/oplirex-err.log</string>
</dict>
</plist>
"#
    )
}

// ---------------------------------------------------------------------------
// public API required by P0.1
// ---------------------------------------------------------------------------

/// Daemonize via background spawn. Minimal viable: spawn current exe with `proxy`
/// detached from current terminal. Returns after spawn.
pub fn daemon_start() -> Result<String, String> {
    let exe = current_exe_string();
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW = 0x08000000, DETACHED_PROCESS = 0x00000008
        let mut cmd = Command::new(&exe);
        cmd.arg("proxy")
            .creation_flags(0x00000008 | 0x08000000)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match cmd.spawn() {
            Ok(child) => Ok(format!(
                "daemon spawned pid={} (windows detached)",
                child.id()
            )),
            Err(e) => Err(format!("failed to spawn daemon: {e}")),
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = Command::new(&exe);
        cmd.arg("proxy")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match cmd.spawn() {
            Ok(child) => Ok(format!("daemon spawned pid={}", child.id())),
            Err(e) => Err(format!("failed to spawn daemon: {e}")),
        }
    }
}

/// Variant that accepts a ProxyConfig for callers that want to pass explicit
/// listen/upstream. Forwards the full runtime config so the background daemon
/// behaves exactly like the foreground `proxy` invocation.
pub fn daemon_start_with_config(cfg: &crate::config::ProxyConfig) -> Result<String, String> {
    let exe = current_exe_string();
    let mut cmd = Command::new(&exe);
    cmd.arg("proxy")
        .arg("--listen")
        .arg(&cfg.listen_addr)
        .arg("--upstream")
        .arg(&cfg.opencode_base_url)
        .arg("--max-retries")
        .arg(cfg.max_retries.to_string())
        .arg("--warp-delay")
        .arg(cfg.warp_reset_delay_ms.to_string())
        .arg("--provider")
        .arg(cfg.provider.to_string())
        .arg("--circuit-threshold")
        .arg(cfg.circuit_threshold.to_string())
        .arg("--circuit-cooldown")
        .arg(cfg.circuit_cooldown_secs.to_string());
    if let Some(key) = &cfg.opencode_api_key {
        cmd.arg("--api-key").arg(key);
    }
    if let Some(hook) = &cfg.hook_on_429 {
        cmd.arg("--on-429").arg(hook);
    }
    if let Some(key) = &cfg.require_api_key {
        cmd.arg("--require-key").arg(key);
    }
    for upstream in &cfg.extra_upstreams {
        cmd.arg("--extra-upstream").arg(upstream);
    }
    for model in &cfg.fallback_models {
        cmd.arg("--fallback-model").arg(model);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x00000008 | 0x08000000);
    }
    match cmd.spawn() {
        Ok(child) => Ok(format!(
            "daemon spawned pid={} (config: {})",
            child.id(),
            cfg.listen_addr
        )),
        Err(e) => Err(format!("failed to spawn daemon: {e}")),
    }
}

/// Install system service for current platform.
/// Linux: write /etc/systemd/system/oplirex.service
/// macOS: write ~/Library/LaunchAgents/com.nxyy.oplirex.plist
/// Windows: sc create / nssm fallback / scheduled task
pub fn install_service() -> Result<String, String> {
    let exe = current_exe_string();
    if cfg!(target_os = "linux") {
        let unit = systemd_unit_content(&exe);
        let dest = PathBuf::from("/etc/systemd/system/oplirex.service");
        // try direct write, fallback to sudo tee
        let write_res = fs::write(&dest, &unit);
        if let Err(e) = write_res {
            // fallback: try via shell with sudo
            let _escaped = unit.replace('"', "\\\"");
            // simpler: attempt sudo sh -c
            let sh_cmd = format!(
                "cat > {} <<'__OPLIREX_EOF__'\n{}__OPLIREX_EOF__\n",
                dest.display(),
                unit
            );
            let out = Command::new("sh")
                .arg("-c")
                .arg(format!(
                    "echo \"{}\" | sudo tee {} > /dev/null",
                    unit.replace('"', "\\\""),
                    dest.display()
                ))
                .output();
            // if still fails, return guidance error
            if out.is_err() || !out.unwrap().status.success() {
                let _ = Command::new("sudo")
                    .arg("sh")
                    .arg("-c")
                    .arg(&sh_cmd)
                    .output();
                if !dest.exists() {
                    return Err(format!(
                        "failed to write {} (need sudo): {}. Unit content:\n{}",
                        dest.display(),
                        e,
                        unit
                    ));
                }
            }
        }
        // reload + enable (best effort)
        let _ = Command::new("systemctl").arg("daemon-reload").output();
        let _ = Command::new("sudo")
            .arg("systemctl")
            .arg("daemon-reload")
            .output();
        let _ = Command::new("systemctl")
            .args(["enable", "oplirex"])
            .output();
        let _ = Command::new("sudo")
            .args(["systemctl", "enable", "oplirex"])
            .output();
        Ok(format!("installed systemd unit at {}", dest.display()))
    } else if cfg!(target_os = "macos") {
        let plist = launchd_plist_content(&exe);
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let dest = PathBuf::from(home).join("Library/LaunchAgents/com.nxyy.oplirex.plist");
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&dest, &plist).map_err(|e| e.to_string())?;
        let _ = Command::new("launchctl")
            .args(["load", "-w"])
            .arg(&dest)
            .output();
        Ok(format!("installed launchd plist at {}", dest.display()))
    } else if cfg!(target_os = "windows") {
        // Try sc create first
        let bin_path = format!("\"{}\" proxy", exe);
        let sc_out = Command::new("sc")
            .args([
                "create",
                "oplirex",
                &format!("binPath= {}", bin_path),
                "start= auto",
            ])
            .output();
        if let Ok(o) = sc_out {
            if o.status.success() {
                let _ = Command::new("sc").args(["start", "oplirex"]).output();
                return Ok("installed Windows Service via sc create".to_string());
            }
        }
        // fallback to nssm
        let nssm_out = Command::new("nssm")
            .args(["install", "oplirex", &exe, "proxy"])
            .output();
        if let Ok(o) = nssm_out {
            if o.status.success() {
                let _ = Command::new("nssm").args(["start", "oplirex"]).output();
                return Ok("installed Windows Service via nssm".to_string());
            }
        }
        // fallback to scheduled task
        let task_out = Command::new("schtasks")
            .args([
                "/Create",
                "/SC",
                "ONLOGON",
                "/TN",
                "oplirex",
                "/TR",
                &format!("\"{}\" proxy", exe),
                "/F",
            ])
            .output();
        if let Ok(o) = task_out {
            if o.status.success() {
                return Ok("installed scheduled task oplirex".to_string());
            } else {
                return Err(format!(
                    "schtasks failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                ));
            }
        }
        Err("Windows service install failed: sc, nssm and schtasks all failed".to_string())
    } else {
        Err("unsupported OS for install_service".to_string())
    }
}

pub fn uninstall_service() -> Result<String, String> {
    if cfg!(target_os = "linux") {
        let dest = PathBuf::from("/etc/systemd/system/oplirex.service");
        let _ = Command::new("systemctl").args(["stop", "oplirex"]).output();
        let _ = Command::new("sudo")
            .args(["systemctl", "stop", "oplirex"])
            .output();
        let _ = Command::new("systemctl")
            .args(["disable", "oplirex"])
            .output();
        let _ = Command::new("sudo")
            .args(["systemctl", "disable", "oplirex"])
            .output();
        if dest.exists() {
            let rm = fs::remove_file(&dest);
            if rm.is_err() {
                let _ = Command::new("sudo").args(["rm", "-f"]).arg(&dest).output();
                if dest.exists() {
                    return Err(format!("failed to remove {} (need sudo)", dest.display()));
                }
            }
        }
        let _ = Command::new("systemctl").arg("daemon-reload").output();
        let _ = Command::new("sudo")
            .arg("systemctl")
            .arg("daemon-reload")
            .output();
        Ok("uninstalled systemd service oplirex".to_string())
    } else if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let dest = PathBuf::from(home).join("Library/LaunchAgents/com.nxyy.oplirex.plist");
        let _ = Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&dest)
            .output();
        if dest.exists() {
            fs::remove_file(&dest).map_err(|e| e.to_string())?;
        }
        Ok(format!("uninstalled launchd plist {}", dest.display()))
    } else if cfg!(target_os = "windows") {
        let _ = Command::new("sc").args(["stop", "oplirex"]).output();
        let sc_out = Command::new("sc").args(["delete", "oplirex"]).output();
        if let Ok(o) = sc_out {
            if o.status.success() {
                return Ok("removed Windows Service oplirex via sc delete".to_string());
            }
        }
        let nssm_out = Command::new("nssm")
            .args(["remove", "oplirex", "confirm"])
            .output();
        if let Ok(o) = nssm_out {
            if o.status.success() {
                return Ok("removed Windows Service via nssm".to_string());
            }
        }
        let _ = Command::new("schtasks")
            .args(["/Delete", "/TN", "oplirex", "/F"])
            .output();
        Ok("removed scheduled task oplirex (if existed)".to_string())
    } else {
        Err("unsupported OS for uninstall_service".to_string())
    }
}

pub fn service_status() -> Result<String, String> {
    if cfg!(target_os = "linux") {
        let out = Command::new("systemctl")
            .args(["is-active", "oplirex"])
            .output()
            .map_err(|e| e.to_string())?;
        let state = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let dest_exists = PathBuf::from("/etc/systemd/system/oplirex.service").exists();
        Ok(format!(
            "systemd oplirex: {} (unit exists: {})",
            if state.is_empty() {
                "unknown".to_string()
            } else {
                state
            },
            dest_exists
        ))
    } else if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let dest = PathBuf::from(home).join("Library/LaunchAgents/com.nxyy.oplirex.plist");
        let exists = dest.exists();
        let out = Command::new("launchctl")
            .args(["list", "com.nxyy.oplirex"])
            .output();
        let loaded = out.map(|o| o.status.success()).unwrap_or(false);
        Ok(format!(
            "launchd com.nxyy.oplirex: exists={} loaded={}",
            exists, loaded
        ))
    } else if cfg!(target_os = "windows") {
        let sc_out = Command::new("sc")
            .args(["query", "oplirex"])
            .output()
            .map_err(|e| e.to_string())?;
        let txt = String::from_utf8_lossy(&sc_out.stdout).to_string();
        if txt.contains("RUNNING") {
            Ok("Windows Service oplirex: RUNNING".to_string())
        } else if txt.contains("STOPPED") {
            Ok("Windows Service oplirex: STOPPED".to_string())
        } else if sc_out.status.success() {
            Ok(format!("Windows Service oplirex: {}", txt.trim()))
        } else {
            // check scheduled task
            let st = Command::new("schtasks")
                .args(["/Query", "/TN", "oplirex"])
                .output();
            if let Ok(o) = st {
                if o.status.success() {
                    return Ok("scheduled task oplirex: exists".to_string());
                }
            }
            Ok("Windows Service oplirex: not installed".to_string())
        }
    } else {
        Err("unsupported OS for service_status".to_string())
    }
}
