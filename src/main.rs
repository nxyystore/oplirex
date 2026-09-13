use clap::{Parser, Subcommand};
use colored::Colorize;
use oplirex::{ProxyConfig, AppConfig};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser, Debug)]
#[command(name = "oplire")]
#[command(version = VERSION)]
#[command(about = "OpenCode Limit Reset + Anthropic Proxy Bridge", long_about = None)]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Status {
        #[arg(long)]
        json: bool,
    },
    Reset {},
    QuickReset {},
    Install {
        #[command(subcommand)]
        target: InstallTarget,
    },
    Stop {},
    About {},
    Proxy {
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: String,
        #[arg(long, default_value = "http://localhost:3000")]
        upstream: String,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long, default_value = "3")]
        max_retries: u32,
        #[arg(long, default_value = "5000")]
        warp_delay: u64,
        #[arg(long, value_name = "CMD")]
        on_429: Option<String>,
    },
    Connect {
        #[command(subcommand)]
        target: ConnectTarget,
    },
    Watch {
        #[arg(long, default_value = "http://localhost:3000")]
        upstream: String,
        #[arg(long, default_value = "3")]
        max_retries: u32,
        #[arg(long, default_value = "5000")]
        warp_delay: u64,
        #[arg(long, value_name = "CMD")]
        on_429: Option<String>,
    },
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    Doctor {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        verbose: bool,
        #[arg(long, default_value = "http://localhost:3000")]
        upstream: String,
    },
    Setup {},
    Models {
        #[arg(long, default_value = "http://localhost:3000")]
        upstream: String,
    },
    Update {
        #[arg(long)]
        check: bool,
        #[arg(long)]
        force: bool,
    },
    Hook {
        #[arg(long, value_name = "CMD")]
        on_429: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
enum InstallTarget {
    /// Install Cloudflare WARP
    Warp {},
    /// Install OpenCode desktop app
    Opencode {},
    /// Install Claude Code CLI
    ClaudeCode {},
    /// Install all: WARP + OpenCode + Claude Code
    All {},
}

#[derive(Subcommand, Debug)]
enum ConnectTarget {
    ClaudeCode {
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: String,
        #[arg(long, default_value = "http://localhost:3000")]
        upstream: String,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long, default_value = "3")]
        max_retries: u32,
        #[arg(long, default_value = "5000")]
        warp_delay: u64,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        system_prompt: Option<String>,
        #[arg(last = true)]
        claude_args: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigAction {
    Show {},
    Set {
        #[arg(long)]
        key: Option<String>,
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: String,
        #[arg(long, default_value = "http://localhost:3000")]
        upstream: String,
        #[arg(long, default_value = "3")]
        max_retries: u32,
        #[arg(long, default_value = "5000")]
        warp_delay: u64,
    },
    Reset {},
}

#[derive(Subcommand, Debug)]
enum DaemonAction {
    Start {
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: String,
        #[arg(long, default_value = "http://localhost:3000")]
        upstream: String,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long, default_value = "3")]
        max_retries: u32,
        #[arg(long, default_value = "5000")]
        warp_delay: u64,
    },
    Stop {},
    Install {},
    Uninstall {},
    Status {},
}

fn config_path() -> PathBuf {
    let mut path = dirs_config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("oplire");
    path.push("config.json");
    path
}

fn dirs_config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            let mut p = PathBuf::from(home);
            p.push("Library");
            p.push("Application Support");
            return Some(p);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(xdg));
    }
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config");
        return Some(p);
    }
    None
}

/// Resolve log directory: prefers platform data_local_dir, falls back to ~/.oplire/logs
pub fn resolve_log_dir() -> PathBuf {
    // Try platform-specific data_local_dir
    if let Some(dir) = dirs_data_local_dir() {
        let mut p = dir;
        p.push("oplire");
        p.push("logs");
        return p;
    }
    // Fallback to ~/.oplire/logs
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".oplire");
        p.push("logs");
        return p;
    }
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        let mut p = PathBuf::from(userprofile);
        p.push(".oplire");
        p.push("logs");
        return p;
    }
    PathBuf::from("./logs")
}

fn dirs_data_local_dir() -> Option<PathBuf> {
    // Windows: %LOCALAPPDATA%
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        if !local.is_empty() {
            return Some(PathBuf::from(local));
        }
    }
    // macOS: ~/Library/Application Support is also used for data, but XDG_DATA_HOME style
    if cfg!(target_os = "macos") {
        if let Ok(home) = std::env::var("HOME") {
            let mut p = PathBuf::from(home);
            p.push("Library");
            p.push("Application Support");
            return Some(p);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".local");
        p.push("share");
        return Some(p);
    }
    None
}

static LOG_GUARD: OnceLock<WorkerGuard> = OnceLock::new();

/// Initialize tracing with console + daily-rotated file appender.
/// Keeps console logging, adds file appender at `resolve_log_dir()/oplirex.log` with daily rotation.
/// Structured JSON is used for file output (A/B logging: human console + JSON file).
pub fn init_tracing(verbose: bool) {
    // Prevent double init
    if tracing::dispatcher::has_been_set() {
        return;
    }

    let env_filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| {
            if verbose {
                EnvFilter::new("debug")
            } else {
                EnvFilter::new("info")
            }
        });

    let log_dir = resolve_log_dir();
    // Best-effort create log dir
    let _ = fs::create_dir_all(&log_dir);

    // Try to create daily rolling file appender
    let file_appender_result = std::panic::catch_unwind(|| {
        tracing_appender::rolling::daily(&log_dir, "oplirex.log")
    });

    if let Ok(file_appender) = file_appender_result {
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        // Keep guard alive for program lifetime
        let _ = LOG_GUARD.set(guard);

        let console_layer = fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(true)
            .with_target(false);

        let file_layer = fmt::layer()
            .with_writer(non_blocking)
            .with_ansi(false)
            .with_target(true)
            .json();

        let subscriber = tracing_subscriber::registry()
            .with(env_filter)
            .with(console_layer)
            .with(file_layer);

        let _ = subscriber.try_init();
        tracing::info!("tracing initialized: console + file {}", log_dir.join("oplirex.log").display());
    } else {
        // Fallback: console only
        let subscriber = tracing_subscriber::registry()
            .with(env_filter)
            .with(fmt::layer().with_writer(std::io::stderr).with_ansi(true));
        let _ = subscriber.try_init();
    }
}

fn load_config() -> AppConfig {
    let path = config_path();
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(config) = serde_json::from_str(&content) {
                return config;
            }
        }
    }
    AppConfig::default()
}

fn save_config(config: &AppConfig) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let content = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(())
}

fn check_warp_installed() -> bool {
    Command::new("warp-cli").arg("--version").output().is_ok()
}

fn check_claude_installed() -> bool {
    Command::new("claude").arg("--version").output().is_ok()
}

fn check_opencode_installed() -> bool {
    Command::new("opencode").arg("--version").output().is_ok()
        || Command::new("opencode").arg("--help").output().is_ok()
}

fn check_opencode_running(base_url: &str) -> bool {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    reqwest::blocking::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn check_node_installed() -> bool {
    Command::new("node").arg("--version").output().is_ok()
}

fn run_command(cmd: &str, args: &[&str], dry_run: bool, verbose: bool) -> Result<String, String> {
    if dry_run {
        if verbose {
            eprintln!(
                "{} Would execute: {} {}",
                "[DRY-RUN]".cyan(),
                cmd,
                args.join(" ")
            );
        } else {
            eprintln!("{} {} {}", "[DRY-RUN]".cyan(), cmd, args.join(" "));
        }
        Ok(format!(
            "[DRY-RUN] Would execute: {} {}",
            cmd,
            args.join(" ")
        ))
    } else {
        let output = Command::new(cmd)
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }
}

fn run_sudo_command(cmd: &str, dry_run: bool, verbose: bool) -> Result<String, String> {
    if dry_run {
        if verbose {
            eprintln!("{} Would execute: sudo {}", "[DRY-RUN]".cyan(), cmd);
        } else {
            eprintln!("{} sudo {}", "[DRY-RUN]".cyan(), cmd);
        }
        Ok(format!("[DRY-RUN] Would execute: sudo {}", cmd))
    } else {
        let output = Command::new("sudo")
            .arg("-S")
            .arg("-n")
            .arg("-k")
            .arg("-v")
            .output()
            .map_err(|e| e.to_string())?;

        if !output.status.success() {
            return Err("sudo requires password or -n flag failed".to_string());
        }

        let output = Command::new("sudo")
            .arg("-S")
            .arg("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .map_err(|e| e.to_string())?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }
}

fn run_interactive(cmd: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| e.to_string())?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("{} exited with {}", cmd, status))
    }
}

fn fetch_models(upstream: &str) -> Result<Vec<(String, String)>, String> {
    let models_url = format!("{}/v1/models", upstream.trim_end_matches('/'));

    let resp = reqwest::blocking::Client::new()
        .get(&models_url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }

    let data = resp.json::<serde_json::Value>().map_err(|e| e.to_string())?;

    let models = data
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "No models in response".to_string())?;

    Ok(models
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(|v| v.as_str())?.to_string();
            let name = match m.get("name")
                .or_else(|| m.get("display_name"))
                .and_then(|v| v.as_str())
            {
                Some(n) => n.to_string(),
                None => id.replace('-', " ")
                    .split_whitespace()
                    .map(|w| {
                        let mut c = w.chars();
                        match c.next() {
                            None => String::new(),
                            Some(ch) => format!(
                                "{}{}",
                                ch.to_uppercase().collect::<String>(),
                                c.as_str()
                            ),
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" "),
            };
            Some((id, name))
        })
        .collect())
}

fn select_model_interactively(models: &[(String, String)]) -> Option<String> {
    use std::io::{self, Write};

    if models.is_empty() {
        return None;
    }

    println!();
    println!("{}", "Available models:".bold());
    println!();

    for (i, (id, name)) in models.iter().enumerate() {
        println!("  {} {} — {}", format!("[{}]", i + 1).dimmed(), id.bold().yellow(), name);
    }

    println!();
    print!("{} Select model (1-{}): ", "→".cyan(), models.len());
    io::stdout().flush().ok()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input).ok()?;
    let num: usize = input.trim().parse().ok()?;

    if num >= 1 && num <= models.len() {
        Some(models[num - 1].0.clone())
    } else {
        None
    }
}

fn print_banner() {
    println!(
        "{}",
        r#"
  ___      ___    _       ___     ___     ___   
 / _ \    | _ \  | |     |_ _|   | _ \   | __|  
| (_) |   |  _/  | |__    | |    |   /   | _|   
 \___/   _|_|_   |____|  |___|   |_|_\   |___|  
_|"""""|_| """ |_|"""""|_|"""""|_|"""""|_|"""""| 
"`-0-0-'"`-0-0-'"`-0-0-'"`-0-0-'"`-0-0-'"`-0-0-'
"#
        .bold()
        .cyan()
    );
}

fn print_step(num: usize, text: &str) {
    println!("  {} {}", format!("[{}/5]", num).dimmed(), text.bold());
}

fn print_success(text: &str) {
    println!("  {} {}", "✓".green().bold(), text);
}

fn print_fail(text: &str) {
    println!("  {} {}", "✗".red().bold(), text);
}

fn print_info_formatted(label: &str, desc: &str) {
    println!("  {} {} — {}", "→".cyan(), label.bold(), desc.dimmed());
}

fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    if cli.verbose {
        eprintln!("{} Verbose mode enabled", "[DEBUG]".yellow());
    }

    if cli.dry_run {
        eprintln!(
            "{} Dry run mode - no changes will be made",
            "[DRY-RUN]".cyan()
        );
    }

    let warp_installed = check_warp_installed();

    match &cli.command {
        Commands::Models { upstream } => {
            print_banner();
            println!("{}", "Available Models".bold().cyan());
            println!();
            println!("{} Fetching models from {}...", "→".cyan(), upstream.bold());
            println!();

            let models_url = format!("{}/v1/models", upstream.trim_end_matches('/'));

            match reqwest::blocking::Client::new()
                .get(&models_url)
                .timeout(std::time::Duration::from_secs(10))
                .send()
            {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<serde_json::Value>() {
                        Ok(data) => {
                            if let Some(models) = data.get("data").and_then(|v| v.as_array()) {
                                if models.is_empty() {
                                    println!("{} No models found", "[INFO]".yellow());
                                    return;
                                }

                                println!("  {}  {:<30}  {}", "#".dimmed(), "ID".bold(), "Name".bold());
                                println!("  {}", "─".repeat(60).dimmed());

                                for (i, m) in models.iter().enumerate() {
                                    let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                    let name: String = match m.get("name")
                                        .or_else(|| m.get("display_name"))
                                        .and_then(|v| v.as_str())
                                    {
                                        Some(n) => n.to_string(),
                                        None => id.replace('-', " ").split_whitespace()
                                            .map(|w| {
                                                let mut c = w.chars();
                                                match c.next() {
                                                    None => String::new(),
                                                    Some(ch) => format!("{}{}", ch.to_uppercase().collect::<String>(), c.as_str()),
                                                }
                                            })
                                            .collect::<Vec<_>>().join(" "),
                                    };

                                    println!("  {}  {:<30}  {}",
                                        format!("[{}]", i + 1).dimmed(),
                                        id.yellow(),
                                        name
                                    );
                                }

                                println!();
                                println!("{}", "Usage:".bold());
                                println!("  {}", "oplire connect claude-code --model <id>".bold().yellow());
                                println!();
                                println!("{}", "Examples:".bold());
                                for m in models.iter().take(3) {
                                    let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                    if !id.is_empty() {
                                        println!("  {}", format!("oplire connect claude-code --model {}", id).dimmed());
                                    }
                                }
                            } else {
                                println!("{} No 'data' field in response", "[ERROR]".red());
                            }
                        }
                        Err(e) => {
                            println!("{} Failed to parse response: {}", "[ERROR]".red(), e);
                        }
                    }
                }
                Ok(resp) => {
                    println!("{} Upstream returned status: {}", "[ERROR]".red(), resp.status());
                    println!("{} Is OpenCode Zen running on {}?", "Tip:".cyan(), upstream.bold());
                }
                Err(e) => {
                    println!("{} Failed to connect to {}", "[ERROR]".red(), upstream.bold());
                    println!("{} {}", "Error:".dimmed(), e.to_string().dimmed());
                    println!("{} Is OpenCode Zen running?", "Tip:".cyan());
                }
            }
        }

        Commands::Setup {} => {
            use std::io::{self, Write};
            use std::str::FromStr;
            fn prompt_with_default(prompt: &str, default: &str) -> String {
                print!("{} {} [{}]: ", "→".cyan(), prompt.bold(), default.dimmed());
                io::stdout().flush().ok();
                let mut input = String::new();
                io::stdin().read_line(&mut input).ok();
                let trimmed = input.trim().to_string();
                if trimmed.is_empty() { default.to_string() } else { trimmed }
            }
            fn validate_listen(s: &str) -> Result<(), String> {
                if s.parse::<std::net::SocketAddr>().is_ok() { return Ok(()); }
                // allow host:port without strict IP check
                let parts: Vec<&str> = s.split(':').collect();
                if parts.len() == 2 && !parts[0].is_empty() && parts[1].parse::<u16>().is_ok() {
                    // basic host validation
                    if parts[0].chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_' ) {
                        return Ok(());
                    }
                }
                Err("must be host:port like 127.0.0.1:8080".to_string())
            }
            fn validate_upstream(s: &str) -> Result<(), String> {
                if s.starts_with("http://") || s.starts_with("https://") {
                    if s.len() > 10 && !s.contains(' ') { return Ok(()); }
                }
                Err("must start with http:// or https://".to_string())
            }
            fn validate_retries(s: &str) -> Result<u32, String> {
                let v: u32 = s.parse().map_err(|_| "must be integer 0-20".to_string())?;
                if v > 20 { return Err("max 20".to_string()); }
                Ok(v)
            }
            fn validate_delay(s: &str) -> Result<u64, String> {
                let v: u64 = s.parse().map_err(|_| "must be integer milliseconds".to_string())?;
                if v > 60000 { return Err("max 60000 ms".to_string()); }
                Ok(v)
            }

            print_banner();
            println!("{}", "Welcome to oplire Setup Wizard".bold().green());
            println!();
            println!("{}", "Interactive config — press Enter to keep defaults.".dimmed());
            println!();

            // Load existing config (or defaults)
            let mut cfg = load_config();
            println!("{} Current config file: {}", "→".cyan(), config_path().display().to_string().dimmed());
            println!();

            // 1. listen address
            loop {
                let input = prompt_with_default("Listen address", &cfg.listen);
                match validate_listen(&input) {
                    Ok(()) => { cfg.listen = input; break; },
                    Err(e) => print_fail(&format!("Invalid listen: {}", e)),
                }
            }
            // 2. upstream
            loop {
                let input = prompt_with_default("Upstream URL", &cfg.upstream);
                match validate_upstream(&input) {
                    Ok(()) => { cfg.upstream = input; break; },
                    Err(e) => print_fail(&format!("Invalid upstream: {}", e)),
                }
            }
            // 3. api_key
            {
                let current = cfg.api_key.clone().unwrap_or_default();
                let display = if current.is_empty() { "(none)".to_string() } else { current.clone() };
                let input = prompt_with_default("API key (leave empty for none)", &display);
                if input == "(none)" || input.is_empty() {
                    cfg.api_key = None;
                } else if input != display {
                    cfg.api_key = Some(input);
                }
                // if user kept "(none)" and had value, they cleared it already
                if current.is_empty() && display == "(none)" {
                    // if they pressed enter, keep None
                    if cfg.api_key.is_some() && cfg.api_key.as_deref() == Some("(none)") {
                        cfg.api_key = None;
                    }
                }
            }
            // 4. max_retries
            loop {
                let def = cfg.max_retries.to_string();
                let input = prompt_with_default("Max retries (0-20)", &def);
                match validate_retries(&input) {
                    Ok(v) => { cfg.max_retries = v; break; },
                    Err(e) => print_fail(&e),
                }
            }
            // 5. warp_delay
            loop {
                let def = cfg.warp_delay.to_string();
                let input = prompt_with_default("WARP delay ms (0-60000)", &def);
                match validate_delay(&input) {
                    Ok(v) => { cfg.warp_delay = v; break; },
                    Err(e) => print_fail(&e),
                }
            }
            // 6. provider selection
            loop {
                println!();
                println!("{}", "Provider selection:".bold());
                println!("  {} opencode (default, OpenAI-compatible via Zen)", "[1]".dimmed());
                println!("  {} openai   (direct OpenAI)", "[2]".dimmed());
                println!("  {} anthropic (direct Anthropic)", "[3]".dimmed());
                let cur = cfg.provider.to_string();
                let input = prompt_with_default("Provider [1/opencode, 2/openai, 3/anthropic]", &cur);
                let parsed = match input.trim().to_lowercase().as_str() {
                    "1" | "opencode" => Ok(oplirex::providers::Provider::Opencode),
                    "2" | "openai" => Ok(oplirex::providers::Provider::OpenAI),
                    "3" | "anthropic" => Ok(oplirex::providers::Provider::Anthropic),
                    other => oplirex::providers::Provider::from_str(other),
                };
                match parsed {
                    Ok(p) => { cfg.provider = p; break; },
                    Err(e) => print_fail(&e),
                }
            }

            println!();
            match save_config(&cfg) {
                Ok(()) => {
                    print_success(&format!("Configuration saved to {}", config_path().display()));
                    println!("  {} {}", "listen:".bold(), cfg.listen);
                    println!("  {} {}", "upstream:".bold(), cfg.upstream);
                    println!("  {} {}", "provider:".bold(), cfg.provider.to_string());
                    println!("  {} {}", "api_key:".bold(), if cfg.api_key.is_some() { "***" } else { "(none)" });
                    println!("  {} {}", "max_retries:".bold(), cfg.max_retries);
                    println!("  {} {}ms", "warp_delay:".bold(), cfg.warp_delay);
                }
                Err(e) => {
                    print_fail(&format!("Failed to save config: {}", e));
                    std::process::exit(1);
                }
            }

            println!();
            // Optionally offer to install missing components
            let mut steps_needed = Vec::new();
            if !check_node_installed() { steps_needed.push("Node.js"); }
            if !warp_installed { steps_needed.push("Cloudflare WARP"); }
            if !check_opencode_installed() { steps_needed.push("OpenCode"); }
            if !check_claude_installed() { steps_needed.push("Claude Code"); }
            if steps_needed.is_empty() {
                println!("{}", "All components already installed!".green().bold());
            } else {
                println!("{} Missing: {}", "Note:".yellow().bold(), steps_needed.join(", ").dimmed());
                println!("{} Run `oplire install all` or install individually.", "Tip:".cyan());
            }
            println!();
            println!("{}", "Next steps:".bold());
            println!("  1. {}", "oplire doctor".bold().yellow());
            println!("  2. {}", "oplire proxy  (or connect claude-code)".bold().yellow());
        }

        Commands::Install { target } => match target {
            InstallTarget::Warp {} => {
                print_banner();
                println!("{}", "Installing Cloudflare WARP".bold().green());
                println!();

                if warp_installed {
                    println!("{} WARP is already installed", "[INFO]".green());
                    println!("{} Run `oplire reset` to refresh your IP", "Tip:".cyan());
                    return;
                }

                if let Ok(output) = Command::new("sh").arg("-c").arg("cat /etc/os-release").output() {
                    let os_release = String::from_utf8_lossy(&output.stdout);

                    if os_release.contains("arch") || os_release.contains("manjaro") {
                        print_step(1, "Installing cloudflare-warp-bin via yay...");
                        match run_interactive("yay", &["-S", "--noconfirm", "cloudflare-warp-bin"]) {
                            Ok(()) => print_success("WARP installed"),
                            Err(e) => print_fail(&format!("Installation failed: {}", e)),
                        }
                    } else if os_release.contains("ubuntu")
                        || os_release.contains("debian")
                        || os_release.contains("kali")
                    {
                        print_step(1, "Adding Cloudflare repository...");

                        // Remove the legacy repository file created by older oplire versions.
                        let _ = run_sudo_command(
                            "rm -f /etc/apt/sources.list.d/cloudflare-warp.list",
                            cli.dry_run,
                            cli.verbose,
                        );

                        let _ = run_sudo_command(
                            "curl -fsSL https://pkg.cloudflareclient.com/pubkey.gpg | gpg --yes --dearmor --output /usr/share/keyrings/cloudflare-warp-archive-keyring.gpg",
                            cli.dry_run, cli.verbose,
                        );

                        // Kali is rolling and reports `kali-rolling`, which is not
                        // published by Cloudflare. Use Debian stable (trixie), as
                        // recommended for third-party Debian repositories on Kali.
                        let repo_codename = if os_release.contains("kali") {
                            "trixie"
                        } else {
                            "$(. /etc/os-release && printf '%s' \"$VERSION_CODENAME\")"
                        };

                        let repo_command = format!(
                            "echo \"deb [signed-by=/usr/share/keyrings/cloudflare-warp-archive-keyring.gpg] https://pkg.cloudflareclient.com/ {} main\" > /etc/apt/sources.list.d/cloudflare-client.list",
                            repo_codename
                        );
                        let _ = run_sudo_command(
                            &repo_command,
                            cli.dry_run, cli.verbose,
                        );

                        print_step(2, "Installing cloudflare-warp...");
                        let _ = run_sudo_command("apt update -qq && apt install -y -qq cloudflare-warp", cli.dry_run, cli.verbose);
                    } else if os_release.contains("fedora") {
                        print_step(1, "Installing cloudflare-warp via dnf...");
                        let _ = run_sudo_command("dnf install -y cloudflare-warp", cli.dry_run, cli.verbose);
                    } else {
                        println!("{} Unsupported OS. Install manually:", "Warning:".yellow());
                        println!("  {}", "https://developers.cloudflare.com/warp-client/get-started/linux/".dimmed());
                        return;
                    }
                }

                if check_warp_installed() {
                    println!();
                    println!("{}", "Next steps:".bold());
                    println!("  1. {}", "warp-cli connect".bold().yellow());
                    println!("  2. {}", "warp-cli status".bold().yellow());
                    println!("  3. {}", "oplire reset".bold().yellow());
                } else {
                    println!();
                    print_fail("WARP installation may have failed. Check output above.");
                }
            }

            InstallTarget::Opencode {} => {
                print_banner();
                println!("{}", "Installing OpenCode".bold().green());
                println!();

                if check_opencode_installed() {
                    println!("{} OpenCode is already installed", "[INFO]".green());
                    return;
                }

                if !check_node_installed() {
                    println!("{} Node.js is required but not found", "[ERROR]".red());
                    println!("{} Install Node.js first: https://nodejs.org/", "Fix:".cyan());
                    std::process::exit(1);
                }

                print_step(1, "Installing OpenCode via npm...");
                match run_interactive("npm", &["install", "-g", "opencode-ai"]) {
                    Ok(()) => {
                        print_success("OpenCode installed");
                        println!();
                        println!("{}", "Next steps:".bold());
                        println!("  1. {}", "opencode".bold().yellow());
                        println!("  2. {}", "oplire doctor".bold().yellow());
                        println!("  3. {}", "oplire connect claude-code".bold().yellow());
                    }
                    Err(e) => {
                        print_fail(&format!("Installation failed: {}", e));
                        println!();
                        println!("{} Try manually:", "Fix:".cyan());
                        println!("  {}", "npm install -g opencode-ai".bold().yellow());
                    }
                }
            }

            InstallTarget::ClaudeCode {} => {
                print_banner();
                println!("{}", "Installing Claude Code".bold().green());
                println!();

                if check_claude_installed() {
                    let version = run_command("claude", &["--version"], false, false)
                        .map(|v| v.trim().to_string())
                        .unwrap_or_else(|_| "unknown".to_string());
                    println!("{} Claude Code is already installed ({})", "[INFO]".green(), version.dimmed());
                    return;
                }

                if !check_node_installed() {
                    println!("{} Node.js is required but not found", "[ERROR]".red());
                    println!("{} Install Node.js first: https://nodejs.org/", "Fix:".cyan());
                    std::process::exit(1);
                }

                print_step(1, "Installing Claude Code via npm...");
                match run_interactive("npm", &["install", "-g", "@anthropic-ai/claude-code"]) {
                    Ok(()) => {
                        print_success("Claude Code installed");
                        println!();
                        println!("{}", "Next steps:".bold());
                        println!("  1. {}", "claude --version".bold().yellow());
                        println!("  2. {}", "oplire connect claude-code".bold().yellow());
                    }
                    Err(e) => {
                        print_fail(&format!("Installation failed: {}", e));
                        println!();
                        println!("{} Try manually:", "Fix:".cyan());
                        println!("  {}", "npm install -g @anthropic-ai/claude-code".bold().yellow());
                    }
                }
            }

            InstallTarget::All {} => {
                print_banner();
                println!("{}", "Installing All Components".bold().green());
                println!();

                print_step(1, "Checking Cloudflare WARP...");
                if check_warp_installed() {
                    print_success("WARP already installed");
                } else {
                    let _ = run_interactive("oplire", &["install", "warp"]);
                }

                print_step(2, "Checking OpenCode...");
                if check_opencode_installed() {
                    print_success("OpenCode already installed");
                } else {
                    let _ = run_interactive("oplire", &["install", "opencode"]);
                }

                print_step(3, "Checking Claude Code...");
                if check_claude_installed() {
                    print_success("Claude Code already installed");
                } else {
                    let _ = run_interactive("oplire", &["install", "claude-code"]);
                }

                println!();
                println!("{}", "All components installed!".green().bold());
                println!();
                println!("{}", "Run to get started:".bold());
                println!("  {}", "oplire connect claude-code".bold().yellow());
            }
        },

        Commands::Connect {
            target: ConnectTarget::ClaudeCode {
                listen,
                upstream,
                api_key,
                max_retries,
                warp_delay,
                model,
                system_prompt,
                claude_args,
            },
        } => {
            print_banner();
            println!("{}", "Claude Code Bridge".bold().green());
            println!();

            if !check_claude_installed() {
                eprintln!("{} Claude Code not found in PATH", "[ERROR]".red());
                eprintln!("{} Install: {}", "Fix:".cyan(), "oplire install claude-code".bold().yellow());
                std::process::exit(1);
            }

            let selected_model = match model {
                Some(m) => m.clone(),
                None => {
                    println!("{} Fetching models from {}...", "→".cyan(), upstream.bold());
                    match fetch_models(upstream) {
                        Ok(models) => {
                            if models.is_empty() {
                                println!("{} No models found, using default", "[WARN]".yellow());
                                String::new()
                            } else {
                                match select_model_interactively(&models) {
                                    Some(id) => id,
                                    None => {
                                        println!("{} No selection made, using default", "[WARN]".yellow());
                                        String::new()
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            println!("{} Failed to fetch models: {}", "[WARN]".yellow(), e);
                            println!("{} Starting without model selection", "Tip:".cyan());
                            String::new()
                        }
                    }
                }
            };

            let config = ProxyConfig {
                listen_addr: listen.clone(),
                opencode_base_url: upstream.clone(),
                opencode_api_key: api_key.clone(),
                max_retries: *max_retries,
                warp_reset_delay_ms: *warp_delay,
                hook_on_429: None,
                provider: load_config().provider,
            };

            println!("{} Proxy:      {}", "→".green(), listen.bold());
            println!("{} Upstream:   {}", "→".green(), upstream.bold());
            println!("{} Auto-reset: {} (attempts: {})", "→".green(), "enabled".green().bold(), max_retries.to_string().bold());
            if !selected_model.is_empty() {
                println!("{} Model:     {}", "→".green(), selected_model.bold());
            }
            if let Some(sp) = system_prompt {
                println!("{} System:    {} chars", "→".green(), sp.len().to_string().bold());
            }
            println!();

            println!("{}", "Starting proxy server...".dimmed());

            let proxy_config = config.clone();
            let listen_clone = listen.clone();
            let model_clone = selected_model.clone();

            let proxy_handle = std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    oplirex::proxy::start_proxy_server(proxy_config).await
                })
            });

            std::thread::sleep(std::time::Duration::from_millis(1500));

            println!("{}", "Launching Claude Code...".dimmed());
            println!();

            let mut cmd = Command::new("claude");
            cmd.env("ANTHROPIC_BASE_URL", format!("http://{}", listen_clone))
                .env("ANTHROPIC_API_KEY", "oplire-proxy-key");

            if !model_clone.is_empty() {
                cmd.env("ANTHROPIC_MODEL", &model_clone);
            }

            if let Some(sp) = system_prompt {
                cmd.env("CLAUDE_CODE_SYSTEM_PROMPT", sp);
            }

            if !claude_args.is_empty() {
                cmd.args(claude_args);
            }

            let status = cmd.status().map_err(|e| e.to_string()).unwrap_or_else(|_| {
                eprintln!("{} Failed to launch Claude Code", "[ERROR]".red());
                std::process::exit(1);
            });

            println!();
            println!("{} Claude Code exited with: {}", "Info:".cyan(), status.to_string().bold());
            println!("{} Shutting down proxy...", "→".green().dimmed());

            drop(proxy_handle);
        }

        Commands::Watch {
            upstream,
            max_retries,
            warp_delay,
            on_429,
        } => {
            print_banner();
            println!("{}", "OpenCode Watch Mode".bold().yellow());
            println!();
            println!("{} Monitoring: {}", "→".green(), upstream.bold());
            println!("{} Auto-reset: {} (attempts: {})", "→".green(), "enabled".green().bold(), max_retries.to_string().bold());
            if let Some(hook) = on_429 {
                println!("{} Hook on 429: {}", "→".green(), hook.bold().yellow());
            }
            println!();
            println!("{}", "Watching for 429 rate limits...".dimmed());
            println!("{} Press Ctrl+C to stop", "Tip:".cyan());
            println!();

            let upstream_clone = upstream.clone();
            let max_retries_clone = *max_retries;
            let warp_delay_clone = *warp_delay;
            let hook_clone = on_429.clone();

            let rt = tokio::runtime::Runtime::new().unwrap();
            if let Err(e) = rt.block_on(async {
                oplirex::watch::start_watch_mode_with_hook(
                    &upstream_clone,
                    max_retries_clone,
                    warp_delay_clone,
                    hook_clone,
                )
                .await
            }) {
                eprintln!("{} Watch error: {}", "[ERROR]".red(), e);
                std::process::exit(1);
            }
        }

        Commands::Daemon { action } => match action {
            DaemonAction::Start { listen, upstream, api_key, max_retries, warp_delay } => {
                // Route to daemon.rs for background spawn, but also keep existing direct run as fallback
                print_banner();
                println!("{}", "Daemon Mode".bold().magenta());
                println!();
                println!("{} Listening:  {}", "→".green(), listen.bold());
                println!("{} Upstream:   {}", "→".green(), upstream.bold());
                println!("{} Auto-reset: {} (attempts: {})", "→".green(), "enabled".green().bold(), max_retries.to_string().bold());
                println!();
                // try daemonized spawn via daemon.rs; if verbose show action
                if cli.verbose {
                    eprintln!("{} Attempting daemonized spawn via oplirex::daemon::daemon_start()", "[DEBUG]".yellow());
                }
                let cfg = ProxyConfig {
                    listen_addr: listen.clone(),
                    opencode_base_url: upstream.clone(),
                    opencode_api_key: api_key.clone(),
                    max_retries: *max_retries,
                    warp_reset_delay_ms: *warp_delay,
                    hook_on_429: None,
                    provider: load_config().provider,
                };
                // First try background spawn helper (minimal viable). If it fails, fall back to foreground proxy.
                match oplirex::daemon::daemon_start_with_config(&cfg) {
                    Ok(msg) => {
                        println!("{} {}", "✓".green().bold(), msg);
                        println!("{}", "Proxy running in background (daemon). Use `oplire daemon status/stop` to manage.".dimmed());
                        // also optionally keep foreground if spawn indicates detached failure - do nothing
                    }
                    Err(e) => {
                        if cli.verbose { eprintln!("{} daemon_start failed ({}), falling back to foreground", "[WARN]".yellow(), e); }
                        println!("{}", "Running as foreground daemon (fallback)...".dimmed());
                        println!("{} The proxy will auto-reset rate limits silently", "Tip:".cyan());
                        println!();
                        let rt = tokio::runtime::Runtime::new().unwrap();
                        if let Err(err) = rt.block_on(async { oplirex::proxy::start_proxy_server(cfg).await }) {
                            eprintln!("{} Daemon error: {}", "[ERROR]".red(), err);
                            std::process::exit(1);
                        }
                    }
                }
            }
            DaemonAction::Stop {} => {
                println!("{}", "Stopping daemon...".bold());
                // Try to stop service if installed, otherwise hint to kill process
                match oplirex::daemon::service_status() {
                    Ok(s) => println!("{} Service status: {}", "→".cyan(), s.dimmed()),
                    Err(e) => eprintln!("{} {}", "[WARN]".yellow(), e),
                }
                // Best-effort pkill
                let _ = Command::new("pkill").args(["-f", "oplirex.*proxy"]).output();
                #[cfg(target_os = "windows")]
                { let _ = Command::new("taskkill").args(["/IM", "oplirex.exe", "/F"]).output(); }
                println!("{}", "Daemon stop signal sent (if running)".green().bold());
            }
            DaemonAction::Install {} => {
                print_banner();
                println!("{}", "Installing daemon service...".bold().green());
                match oplirex::daemon::install_service() {
                    Ok(msg) => println!("{} {}", "✓".green().bold(), msg),
                    Err(e) => { eprintln!("{} {}", "[ERROR]".red(), e); std::process::exit(1); }
                }
            }
            DaemonAction::Uninstall {} => {
                print_banner();
                println!("{}", "Uninstalling daemon service...".bold().yellow());
                match oplirex::daemon::uninstall_service() {
                    Ok(msg) => println!("{} {}", "✓".green().bold(), msg),
                    Err(e) => { eprintln!("{} {}", "[ERROR]".red(), e); std::process::exit(1); }
                }
            }
            DaemonAction::Status {} => {
                match oplirex::daemon::service_status() {
                    Ok(s) => println!("{} {}", "Daemon status:".bold(), s),
                    Err(e) => eprintln!("{} {}", "[ERROR]".red(), e),
                }
                // also show port check
                let port_available = std::net::TcpListener::bind("127.0.0.1:8080").is_ok();
                println!("{} Port 8080: {}", "→".cyan(), if port_available { "available".green().bold() } else { "IN USE".red().bold() });
            }
        }

        Commands::Doctor { json, verbose, upstream } => {
            let is_verbose = *verbose || cli.verbose;
            // Gather checks
            let warp_ok = warp_installed;
            let warp_detail = if warp_ok { "installed".to_string() } else { "NOT FOUND".to_string() };
            let warp_version = if warp_ok {
                run_command("warp-cli", &["--version"], false, false).map(|v| v.trim().to_string()).unwrap_or_else(|_| "unknown".to_string())
            } else { String::new() };

            let cfg_path = config_path();
            let cfg_exists = cfg_path.exists();
            let loaded_cfg = load_config();
            // use CLI upstream if provided different from default, else use config upstream
            let effective_upstream = if upstream != "http://localhost:3000" { upstream.clone() } else { loaded_cfg.upstream.clone() };
            let opencode_installed = check_opencode_installed();
            let opencode_running = check_opencode_running(&effective_upstream);
            let port_available = std::net::TcpListener::bind("127.0.0.1:8080").is_ok();
            let port_in_use = !port_available;

            if *json {
                let out = serde_json::json!({
                    "warp": {
                        "installed": warp_ok,
                        "detail": warp_detail,
                        "version": warp_version,
                        "ok": warp_ok
                    },
                    "opencode": {
                        "installed": opencode_installed,
                        "reachable": opencode_running,
                        "upstream": effective_upstream,
                        "ok": opencode_installed && opencode_running
                    },
                    "config": {
                        "path": cfg_path.display().to_string(),
                        "exists": cfg_exists,
                        "listen": loaded_cfg.listen,
                        "upstream": loaded_cfg.upstream,
                        "ok": true
                    },
                    "port": {
                        "port": 8080,
                        "available": port_available,
                        "in_use": port_in_use,
                        "ok": port_available
                    }
                });
                if is_verbose {
                    let mut verbose_out = out.clone();
                    verbose_out["verbose"] = serde_json::json!(true);
                    verbose_out["checks"] = serde_json::json!({
                        "warp_cli": warp_ok,
                        "opencode_installed": opencode_installed,
                        "opencode_health": opencode_running,
                        "config_exists": cfg_exists,
                        "port_available": port_available
                    });
                    println!("{}", serde_json::to_string_pretty(&verbose_out).unwrap());
                } else {
                    println!("{}", serde_json::to_string_pretty(&out).unwrap());
                }
                return;
            }

            // human readable path
            print_banner();
            println!("{}", "System Diagnostics".bold().cyan());
            println!();
            if is_verbose {
                eprintln!("{} Doctor verbose: warp={}, opencode_installed={}, opencode_running={}, config={}, port_available={}", "[DEBUG]".yellow(), warp_ok, opencode_installed, opencode_running, cfg_exists, port_available);
            }
            let mut all_ok = true;
            let mut checks: Vec<(&str, String, bool)> = Vec::new();
            if warp_ok {
                let v = if !warp_version.is_empty() { format!("installed ({})", warp_version) } else { "installed".to_string() };
                checks.push(("WARP CLI", v, true));
            } else {
                checks.push(("WARP CLI", "NOT FOUND".to_string(), false));
                all_ok = false;
            }
            if check_claude_installed() {
                let version = run_command("claude", &["--version"], false, false)
                    .map(|v| v.trim().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                checks.push(("Claude Code", format!("installed ({})", version), true));
            } else {
                checks.push(("Claude Code", "NOT FOUND".to_string(), false));
                // not fatal for doctor
            }
            if opencode_installed {
                checks.push(("OpenCode", "installed".to_string(), true));
            } else {
                checks.push(("OpenCode", "NOT FOUND".to_string(), false));
                all_ok = false;
            }
            if opencode_running {
                checks.push(("OpenCode Zen", format!("running ({})", effective_upstream), true));
            } else {
                checks.push(("OpenCode Zen", format!("NOT REACHABLE ({})", effective_upstream), false));
                all_ok = false;
            }
            if cfg_exists {
                checks.push(("Config file", format!("found ({})", cfg_path.display()), true));
            } else {
                checks.push(("Config file", "using defaults".to_string(), true));
            }
            if port_available {
                checks.push(("Port 8080", "available".to_string(), true));
            } else {
                checks.push(("Port 8080", "IN USE".to_string(), false));
                all_ok = false;
            }
            for (name, status, ok) in &checks {
                let status_str = if *ok { status.green().bold() } else { status.red().bold() };
                println!("{} {}: {}", "→".cyan(), name.bold(), status_str);
            }
            println!();
            if all_ok {
                println!("{}", "All checks passed!".green().bold());
            } else {
                println!("{}", "Some checks failed.".yellow().bold());
                println!();
                println!("{} Run `oplire setup` for guided installation", "Fix:".cyan());
            }
        }

        Commands::QuickReset {} => {
            if !warp_installed {
                println!("{} WARP is not installed", "[ERROR]".red());
                println!("{} Run `oplire install warp` first", "Tip:".cyan().bold());
                return;
            }

            println!("{}", "Quick-resetting WARP tunnel...".bold());

            if cli.verbose {
                eprintln!("{} Disconnecting...", "[DEBUG]".cyan());
            }
            let _ = run_command("warp-cli", &["disconnect"], cli.dry_run, cli.verbose);

            if cli.verbose {
                eprintln!("{} Registration new...", "[DEBUG]".cyan());
            }
            let _ = run_command(
                "warp-cli",
                &["registration", "new"],
                cli.dry_run,
                cli.verbose,
            );

            if cli.verbose {
                eprintln!("{} Connecting...", "[DEBUG]".cyan());
            }
            let _ = run_command("warp-cli", &["connect"], cli.dry_run, cli.verbose);

            println!("{}", "Quick reset complete!".green().bold());
        }

        Commands::Config { action } => match action {
            ConfigAction::Show {} => {
                let config = load_config();
                println!("{}", "Current Configuration:".bold());
                println!();
                println!("{} {}", "Listen:".bold(), config.listen);
                println!("{} {}", "Upstream:".bold(), config.upstream);
                println!("{} {}", "Provider:".bold(), config.provider.to_string());
                println!("{} {}", "API Key:".bold(), if config.api_key.is_some() { "*** set" } else { "(none)" });
                println!("{} {}", "Max Retries:".bold(), config.max_retries);
                println!("{} {}ms", "WARP Delay:".bold(), config.warp_delay);
                println!("{} {}", "Config file:".bold(), config_path().display());
            }
            ConfigAction::Set {
                key: _,
                listen,
                upstream,
                max_retries,
                warp_delay,
            } => {
                let mut config = load_config();

                config.listen = listen.clone();
                config.upstream = upstream.clone();
                config.max_retries = *max_retries;
                config.warp_delay = *warp_delay;

                match save_config(&config) {
                    Ok(()) => {
                        println!("{}", "Configuration saved!".green().bold());
                        println!("{} {}", "File:".bold(), config_path().display());
                    }
                    Err(e) => {
                        eprintln!("{} Failed to save config: {}", "[ERROR]".red(), e);
                        std::process::exit(1);
                    }
                }
            }
            ConfigAction::Reset {} => {
                let path = config_path();
                if path.exists() {
                    fs::remove_file(&path)
                        .map_err(|e| e.to_string())
                        .unwrap_or_else(|e| {
                            eprintln!("{} Failed to remove config: {}", "[ERROR]".red(), e);
                            std::process::exit(1);
                        });
                    println!("{}", "Configuration reset to defaults!".green().bold());
                } else {
                    println!("{} No configuration file found", "[INFO]".green());
                }
            }
        },

        Commands::Proxy {
            listen,
            upstream,
            api_key,
            max_retries,
            warp_delay,
            on_429,
        } => {
            let config = ProxyConfig {
                listen_addr: listen.clone(),
                opencode_base_url: upstream.clone(),
                opencode_api_key: api_key.clone(),
                max_retries: *max_retries,
                warp_reset_delay_ms: *warp_delay,
                hook_on_429: on_429.clone(),
                provider: load_config().provider,
            };

            print_banner();
            println!("{}", "Anthropic ↔ OpenCode Zen Proxy".bold().cyan());
            println!();
            println!("{} Listening on: {}", "→".green(), listen.bold());
            println!("{} Upstream:     {}", "→".green(), upstream.bold());
            println!(
                "{} Max retries:  {}",
                "→".green(),
                max_retries.to_string().bold()
            );
            println!();
            println!(
                "{} Configure Claude Code to use: {}",
                "Tip:".cyan().bold(),
                format!("http://{}", listen).yellow().bold()
            );
            println!("{} Or run: {}", "→".cyan(), "oplire connect claude-code".bold());
            println!();

            let rt = tokio::runtime::Runtime::new().unwrap();
            if let Err(e) = rt.block_on(async {
                oplirex::proxy::start_proxy_server(config).await
            }) {
                eprintln!("{} Proxy server error: {}", "[ERROR]".red(), e);
                std::process::exit(1);
            }
        }

        Commands::Status { json } => {
            if !warp_installed {
                if *json {
                    println!("{{\"connected\": false, \"tunnel_id\": null, \"error\": \"warp-cli not installed\"}}");
                } else {
                    println!("{} {}", "Tunnel:".bold(), "Not connected".red());
                    println!("{} {}", "WARP:".bold(), "Not installed".red());
                    println!(
                        "\n{} Run `oplire install warp` to install WARP",
                        "Tip:".cyan().bold()
                    );
                }
                return;
            }

            println!("{}", "Checking tunnel status...".dimmed());

            match run_command("warp-cli", &["status"], cli.dry_run, cli.verbose) {
                Ok(output) => {
                    let status_str = output.trim();
                    let connected =
                        status_str.contains("Connected") || status_str.contains("connected");

                    if *json {
                        let tunnel_id = if connected { "active" } else { "null" };
                        println!(
                            "{{\"connected\": {}, \"tunnel_id\": \"{}\"}}",
                            connected, tunnel_id
                        );
                    } else {
                        if !status_str.is_empty() {
                            println!("{}", status_str);
                        }
                        let status = if connected {
                            "Active".green()
                        } else {
                            "Disconnected".red()
                        };
                        let tunnel = if connected {
                            "Connected".green()
                        } else {
                            "Not connected".red()
                        };
                        println!("\n{} {}", "Tunnel:".bold(), tunnel);
                        println!("{} {}", "WARP:".bold(), status);
                    }
                }
                Err(e) => {
                    if cli.verbose {
                        eprintln!("{} Error: {}", "[ERROR]".red(), e);
                    }
                    if *json {
                        println!(
                            "{{\"connected\": false, \"tunnel_id\": null, \"error\": \"{}\"}}",
                            e
                        );
                    } else {
                        println!("{} {}", "Tunnel:".bold(), "Error".red());
                        println!("{} {}", "Status:".bold(), e.red());
                    }
                }
            }
        }

        Commands::Reset {} => {
            if !warp_installed {
                println!("{} WARP is not installed", "[ERROR]".red());
                println!("{} Run `oplire install warp` first", "Tip:".cyan().bold());
                return;
            }

            println!("{}", "Resetting WARP tunnel...".bold());

            if cli.verbose {
                eprintln!("{} Step 1: Disconnecting...", "[DEBUG]".cyan());
            }
            let _ = run_command("warp-cli", &["disconnect"], cli.dry_run, cli.verbose);

            if cli.verbose {
                eprintln!("{} Step 2: Stopping warp-svc...", "[DEBUG]".cyan());
            }
            let _ = run_sudo_command("systemctl stop warp-svc", cli.dry_run, cli.verbose);

            if cli.verbose {
                eprintln!("{} Step 3: Clearing cache...", "[DEBUG]".cyan());
            }
            let _ = run_sudo_command(
                "rm -rf /var/lib/cloudflare-warp/*",
                cli.dry_run,
                cli.verbose,
            );

            if cli.verbose {
                eprintln!("{} Step 4: Starting warp-svc...", "[DEBUG]".cyan());
            }
            let _ = run_sudo_command("systemctl start warp-svc", cli.dry_run, cli.verbose);

            if cli.verbose {
                eprintln!("{} Step 5: Registering new tunnel...", "[DEBUG]".cyan());
            }
            let _ = run_command(
                "warp-cli",
                &["registration", "new"],
                cli.dry_run,
                cli.verbose,
            );

            if cli.verbose {
                eprintln!("{} Step 6: Connecting...", "[DEBUG]".cyan());
            }
            let _ = run_command("warp-cli", &["connect"], cli.dry_run, cli.verbose);

            println!("{}", "WARP tunnel reset complete!".green().bold());
            println!("{} Run `oplire status` to verify", "Tip:".cyan());
        }

        Commands::Stop {} => {
            if !warp_installed {
                println!("{} WARP is not installed", "[ERROR]".red());
                println!("{} Run `oplire install warp` first", "Tip:".cyan().bold());
                return;
            }

            println!("{}", "Stopping WARP tunnel...".bold());

            if cli.verbose {
                eprintln!("{} Step 1: Disconnecting...", "[DEBUG]".cyan());
            }
            let _ = run_command("warp-cli", &["disconnect"], cli.dry_run, cli.verbose);

            if cli.verbose {
                eprintln!("{} Step 2: Stopping warp-svc...", "[DEBUG]".cyan());
            }
            let _ = run_sudo_command("systemctl stop warp-svc", cli.dry_run, cli.verbose);

            if cli.verbose {
                eprintln!("{} Step 3: Disabling warp-svc...", "[DEBUG]".cyan());
            }
            let _ = run_sudo_command("systemctl disable warp-svc", cli.dry_run, cli.verbose);

            println!("{}", "WARP tunnel stopped!".green().bold());
            println!("{} Run `oplire reset` to restart", "Tip:".cyan());
        }

        Commands::Update { check, force } => {
            if *check {
                println!("{}", "Checking for updates...".dimmed());
                let rt = tokio::runtime::Runtime::new().unwrap();
                match rt.block_on(oplirex::update::check_update()) {
                    Ok(msg) => println!("{} {}", "→".green(), msg.bold()),
                    Err(e) => {
                        eprintln!("{} {}", "[ERROR]".red(), e);
                        std::process::exit(1);
                    }
                }
            } else {
                println!("{}", "Self-updating...".bold().cyan());
                let rt = tokio::runtime::Runtime::new().unwrap();
                match rt.block_on(oplirex::update::self_update(*force)) {
                    Ok(msg) => println!("{} {}", "✓".green().bold(), msg),
                    Err(e) => {
                        eprintln!("{} {}", "[ERROR]".red(), e);
                        std::process::exit(1);
                    }
                }
            }
        }
        Commands::Hook { on_429 } => {
            if let Some(cmd) = on_429 {
                println!("{} Hook on_429: {}", "→".cyan(), cmd.bold().yellow());
                // demo run
                let env = oplirex::hooks::HookEnv {
                    warp_ip: Some("127.0.0.1".to_string()),
                    retry_count: 1,
                    ..Default::default()
                };
                oplirex::hooks::run_hook(cmd, env);
                println!("{}", "Hook triggered (non-blocking)".green());
            } else {
                println!("{}", "No hook configured".yellow());
                println!("Usage: oplire hook --on-429 \"echo 429 hit $RETRY_COUNT\"");
                println!("   or: oplire proxy --on-429 \"...\"");
                println!("       oplire watch --on-429 \"...\"");
            }
        }
        Commands::About {} => {
            print_banner();
            println!();
            println!("{} {}", "Version:".bold(), VERSION);
            println!("{} Rust", "Language:".bold());
            println!("{} OpenCode rate limit reset + Anthropic proxy", "Purpose:".bold());
            println!("{} Cloudflare WARP + Axum HTTP", "Infrastructure:".bold());
            println!("{} Berke Oruc", "Author:".bold());
            println!(
                "{} https://github.com/BerkeOruc/oplire",
                "GitHub:".bold()
            );
            println!();
            println!("{}", "Installation:".bold());
            println!("  oplire install warp          # Install Cloudflare WARP");
            println!("  oplire install opencode      # Install OpenCode");
            println!("  oplire install claude-code   # Install Claude Code CLI");
            println!("  oplire install all           # Install everything");
            println!("  oplire setup                 # Guided setup wizard");
            println!();
            println!("{}", "WARP Management:".bold());
            println!("  oplire status              # Check WARP status");
            println!("  oplire reset               # Full WARP tunnel reset");
            println!("  oplire quick-reset         # Fast WARP IP rotation");
            println!("  oplire stop                # Stop WARP tunnel");
            println!();
            println!("{}", "Proxy & Claude Code:".bold());
            println!("  oplire proxy               # Start reverse proxy");
            println!("  oplire connect claude-code # Proxy + launch Claude Code");
            println!("  oplire daemon              # Background proxy service");
            println!("  oplire watch               # Monitor OpenCode, auto-reset");
            println!();
            println!("{}", "Configuration:".bold());
            println!("  oplire models              # List available OpenCode models");
            println!("  oplire config show         # Show current config");
            println!("  oplire config set          # Save config");
            println!("  oplire config reset        # Reset to defaults");
            println!("  oplire doctor              # Diagnose system setup");
            println!();
            println!("{}", "Updates & Hooks:".bold());
            println!("  oplire update              # Self-update to latest release");
            println!("  oplire update --check      # Check for updates only");
            println!("  oplire hook --on-429 \"cmd\" # Test 429 hook");
        }
    }

    if !matches!(&cli.command, Commands::About {} | Commands::Proxy { .. } | Commands::Connect { .. } | Commands::Daemon { .. } | Commands::Watch { .. } | Commands::Doctor { .. } | Commands::Config { .. } | Commands::Setup {} | Commands::Install { .. } | Commands::Models { .. } | Commands::Update { .. } | Commands::Hook { .. }) {
        println!("\n{} v{}", "oplire".bold(), VERSION.dimmed());
    }
}
