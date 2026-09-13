use std::collections::HashMap;
use tracing::{info, warn};

/// Configuration for hook commands triggered on events like 429.
#[derive(Debug, Clone, Default)]
pub struct HookConfig {
    pub on_429: Option<String>,
}

/// Environment variables passed to the hook shell command.
#[derive(Debug, Clone, Default)]
pub struct HookEnv {
    /// Current WARP IP if known (e.g. from warp-cli status or resolver)
    pub warp_ip: Option<String>,
    /// Retry count (attempt number) for the current 429 handling
    pub retry_count: u32,
    /// Model id associated with the request (if available)
    pub model: Option<String>,
    /// Upstream base URL
    pub upstream: Option<String>,
    /// Additional custom vars
    pub extra: HashMap<String, String>,
}

impl HookEnv {
    pub fn new(retry_count: u32) -> Self {
        Self {
            retry_count,
            ..Default::default()
        }
    }

    fn to_env_map(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        if let Some(ip) = &self.warp_ip {
            m.insert("WARP_IP".to_string(), ip.clone());
        }
        m.insert("RETRY_COUNT".to_string(), self.retry_count.to_string());
        if let Some(model) = &self.model {
            m.insert("MODEL".to_string(), model.clone());
        }
        if let Some(upstream) = &self.upstream {
            m.insert("UPSTREAM".to_string(), upstream.clone());
        }
        for (k, v) in &self.extra {
            m.insert(k.clone(), v.clone());
        }
        m
    }
}

/// Spawn a shell command with the given hook environment non-blocking.
/// Uses `sh -c` on unix and `cmd /C` on windows. Returns immediately after spawn.
pub fn run_hook(cmd: &str, env_vars: HookEnv) {
    if cmd.trim().is_empty() {
        return;
    }
    let env_map = env_vars.to_env_map();
    let cmd_owned = cmd.to_string();
    info!("Running 429 hook: {}", cmd_owned);

    // Spawn detached task so caller is not blocked.
    tokio::spawn(async move {
        let result = spawn_shell(&cmd_owned, &env_map).await;
        match result {
            Ok(()) => info!("Hook completed: {}", cmd_owned),
            Err(e) => warn!("Hook failed ({}): {}", cmd_owned, e),
        }
    });
}

/// Sync/non-async fire-and-forget for use in non-tokio contexts if needed.
pub fn run_hook_blocking(cmd: &str, env_vars: HookEnv) {
    if cmd.trim().is_empty() {
        return;
    }
    let env_map = env_vars.to_env_map();
    let cmd_owned = cmd.to_string();
    // Use std::process::Command detached
    let mut command: std::process::Command;
    #[cfg(target_os = "windows")]
    {
        command = std::process::Command::new("cmd");
        command.arg("/C").arg(&cmd_owned);
    }
    #[cfg(not(target_os = "windows"))]
    {
        command = std::process::Command::new("sh");
        command.arg("-c").arg(&cmd_owned);
    }
    for (k, v) in env_map {
        command.env(k, v);
    }
    // Don't wait, just spawn
    match command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(_) => info!("Hook spawned (blocking): {}", cmd_owned),
        Err(e) => warn!("Hook spawn failed ({}): {}", cmd_owned, e),
    }
}

async fn spawn_shell(cmd: &str, env_map: &HashMap<String, String>) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let child = tokio::process::Command::new("cmd")
            .arg("/C")
            .arg(cmd)
            .envs(env_map)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        // Don't wait for completion in blocking sense; but await to capture spawn errors?
        // We do not block caller beyond spawn, but within the detached task we can wait briefly without blocking main flow.
        // Use try_wait to avoid long blocking; just ensure it started.
        // Optionally wait with timeout not needed.
        let _ = child.id();
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .envs(env_map)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let _ = child.id();
        Ok(())
    }
}
