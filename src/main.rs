use clap::{Parser, Subcommand};
use colored::Colorize;
use oplirex::{ProxyConfig, AppConfig};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing_subscriber::EnvFilter;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Initialize the global tracing subscriber so the `info!`/`warn!`/`error!`
/// calls throughout the library actually emit output.
///
/// Output is suppressed unless the `RUST_LOG` environment variable is set
/// (or `--verbose` is passed, which forces `oplirex=info`). This keeps the
/// default CLI output clean while making diagnostics available on demand.
fn init_tracing(verbose: bool) {
    let filter = if verbose {
        EnvFilter::new("oplirex=info")
    } else {
        EnvFilter::from_default_env().add_directive("oplirex=warn".parse().unwrap())
    };

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

#[derive(Parser, Debug)]
#[command(name = "oplirex")]
#[command(version = VERSION)]
#[command(about = "OpenCode Limit Reset + Anthropic Proxy Bridge", long_about = None)]
struct Cli {
    #[arg(short, long, global = true)]
    verbose: bool,

    #[arg(long, global = true)]
    dry_run: bool,

    #[arg(long, env = "OPLIREX_PROXY_API_KEY", global = true)]
    proxy_api_key: Option<String>,

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
    },
    Daemon {
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
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    Doctor {},
    Setup {},
    Models {
        #[arg(long, default_value = "http://localhost:3000")]
        upstream: String,
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
    Opencode {
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
        opencode_args: Vec<String>,
    },
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
        /// Setting to update: `listen`, `upstream`, `max_retries`, or `warp_delay`.
        #[arg(long)]
        key: Option<String>,
        /// New value for the setting named by `--key`.
        #[arg(long)]
        value: Option<String>,
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

fn config_path() -> PathBuf {
    let mut path = dirs_config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("oplirex");
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
    if is_windows() {
        // On Windows WARP is a GUI app, not a `warp-cli` binary. Detect it by
        // the presence of the installed executable in either Program Files.
        let candidates = warp_executable_paths();
        return candidates.iter().any(|p| Path::new(p).exists());
    }
    Command::new("warp-cli").arg("--version").output().is_ok()
}

/// Return the candidate filesystem paths where the Windows WARP client may
/// live, honoring the 32-bit Program Files (x86) location used by the
/// Cloudflare installer.
fn warp_executable_paths() -> Vec<String> {
    let mut paths = Vec::new();

    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        paths.push(format!(r"{}\Cloudflare\Cloudflare WARP.exe", pf86));
    }
    if let Ok(pf64) = std::env::var("ProgramFiles") {
        paths.push(format!(r"{}\Cloudflare\Cloudflare WARP.exe", pf64));
    }

    paths
}

fn is_windows() -> bool {
    cfg!(target_os = "windows")
}

/// Wait until the local proxy's `/health` endpoint responds, polling every
/// 100ms for up to `timeout_ms` milliseconds. Returns true on success.
///
/// This replaces the previous fixed 1.5s sleep, which could fail on slow
/// machines or waste time on fast ones.
fn wait_for_proxy_ready(listen_addr: &str, timeout_ms: u64) -> bool {
    let url = format!("http://{}/health", listen_addr);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(100))
        .timeout(std::time::Duration::from_millis(200))
        .build();

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let ok = match client.as_ref() {
            Ok(c) => match c.get(&url).send() {
                Ok(resp) => resp.status().is_success(),
                Err(_) => false,
            },
            Err(_) => false,
        };

        if ok {
            return true;
        }

        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Install Cloudflare WARP on Windows by downloading the official installer
/// and running it. Mirrors the logic of `scripts/install-windows.ps1`.
fn install_warp_windows(dry_run: bool, verbose: bool) {
    let installer_url =
        "https://1111-releases.cloudflare.com/latest/Windows%20Cloudflare%20WARP%20Setup.exe";
    let installer_path = std::env::temp_dir().join("Cloudflare-WARP-Setup.exe");
    let installer_str = installer_path.display().to_string();

    print_step(1, "Downloading Cloudflare WARP installer...");
    if dry_run {
        eprintln!(
            "{} Would download {} to {}",
            "[DRY-RUN]".cyan(),
            installer_url,
            installer_str
        );
        return;
    }

    if verbose {
        eprintln!("{} Downloading {}...", "[DEBUG]".cyan(), installer_url);
    }

    match reqwest::blocking::get(installer_url) {
        Ok(resp) if resp.status().is_success() => {
            match resp.bytes() {
                Ok(bytes) => {
                    if let Err(e) = std::fs::write(&installer_path, &bytes) {
                        print_fail(&format!("Failed to save installer: {}", e));
                        return;
                    }
                }
                Err(e) => {
                    print_fail(&format!("Failed to read installer bytes: {}", e));
                    return;
                }
            }
        }
        Ok(resp) => {
            print_fail(&format!(
                "Installer download returned HTTP {}",
                resp.status()
            ));
            return;
        }
        Err(e) => {
            print_fail(&format!("Failed to download installer: {}", e));
            return;
        }
    }

    print_step(2, "Running WARP installer (quiet)...");
    let status = Command::new(&installer_path)
        .args(["/quiet", "/install"])
        .status();

    match status {
        Ok(s) if s.success() => {
            let _ = std::fs::remove_file(&installer_path);
        }
        Ok(s) => {
            print_fail(&format!("Installer exited with {}", s));
            return;
        }
        Err(e) => {
            print_fail(&format!("Failed to run installer: {}", e));
            return;
        }
    }

    if check_warp_installed() {
        print_success("Cloudflare WARP installed");
    } else {
        print_fail("WARP installation may have failed. Check output above.");
    }
}

/// Run a WARP CLI command. On Linux/macOS this is `warp-cli <args>`; on
/// Windows there is no CLI, so we drive the GUI app directly (connect/disconnect
/// are GUI-only actions; status is reported as best-effort).
fn run_warp_cli(args: &[&str], dry_run: bool, verbose: bool) -> Result<String, String> {
    if is_windows() {
        let warp = warp_executable_paths()
            .into_iter()
            .find(|p| Path::new(p).exists())
            .unwrap_or_else(|| r"C:\Program Files (x86)\Cloudflare\Cloudflare WARP.exe".to_string());

        let mut cmd_args: Vec<String> = Vec::new();
        for arg in args {
            match *arg {
                "connect" => cmd_args.push("connect".to_string()),
                "disconnect" => cmd_args.push("disconnect".to_string()),
                "status" => {
                    // The Windows app does not print machine-readable status;
                    // report "Connected" only if the process is currently running.
                    let running = Command::new("powershell")
                        .args([
                            "-NoProfile",
                            "-Command",
                            "(Get-Process 'Cloudflare WARP' -ErrorAction SilentlyContinue) -ne $null",
                        ])
                        .output()
                        .map(|o| o.status.success())
                        .unwrap_or(false);
                    return Ok(if running {
                        "Connected".to_string()
                    } else {
                        "Disconnected".to_string()
                    });
                }
                _ => {}
            }
        }

        if cmd_args.is_empty() {
            return Ok(String::new());
        }

        let arg_strs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        run_command(&warp, &arg_strs, dry_run, verbose)
    } else {
        run_command("warp-cli", args, dry_run, verbose)
    }
}

fn check_claude_installed() -> bool {
    Command::new("claude").arg("--version").output().is_ok()
}

fn check_node_installed() -> bool {
    Command::new("node").arg("--version").output().is_ok()
}

fn opencode_command_candidates() -> Vec<&'static str> {
    vec!["opencode", "opencode-ai"]
}

fn resolve_opencode_command() -> &'static str {
    opencode_command_candidates()
        .into_iter()
        .find(|name| {
            Command::new(*name)
                .arg("--version")
                .output()
                .is_ok()
                || Command::new(*name).arg("--help").output().is_ok()
        })
        .unwrap_or("opencode")
}

fn check_opencode_installed() -> bool {
    opencode_command_candidates()
        .into_iter()
        .any(|name| {
            Command::new(name).arg("--version").output().is_ok()
                || Command::new(name).arg("--help").output().is_ok()
        })
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

/// Run `npm` with the given args. If npm is not directly on PATH, fall back to
/// a shell that sources nvm (used after auto-installing Node.js via nvm).
fn run_npm_install(dry_run: bool, verbose: bool, args: &[&str]) -> Result<(), String> {
    if Command::new("npm").arg("--version").output().is_ok() {
        return run_interactive("npm", args, dry_run, verbose);
    }

    let joined = args
        .iter()
        .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!(
        "export NVM_DIR=\"$HOME/.nvm\"; \
         [ -s \"$NVM_DIR/nvm.sh\" ] && . \"$NVM_DIR/nvm.sh\"; \
         nvm use --lts >/dev/null 2>&1; \
         npm {}",
        joined
    );
    run_interactive("sh", &["-c", &cmd], dry_run, verbose)
}

/// Attempt to install Node.js via nvm (download latest nvm, install latest LTS).
/// Returns true if a usable `node` binary is available afterwards.
fn ensure_node_installed(dry_run: bool, verbose: bool) -> bool {
    if check_node_installed() {
        return true;
    }

    println!();
    println!("{} Node.js not found — installing via nvm...", "→".cyan());

    let nvm_install_script = "https://raw.githubusercontent.com/nvm-sh/nvm/master/install.sh";

    if run_command("curl", &["-fsSL", nvm_install_script, "-o", "/tmp/nvm-install.sh"], dry_run, verbose).is_err() {
        println!("{} Failed to download nvm install script", "[ERROR]".red());
        println!("{} Install Node.js manually: https://nodejs.org/", "Fix:".cyan());
        return false;
    }

    if run_interactive("sh", &["/tmp/nvm-install.sh"], dry_run, verbose).is_err() {
        println!("{} Failed to run nvm install script", "[ERROR]".red());
        println!("{} Install Node.js manually: https://nodejs.org/", "Fix:".cyan());
        return false;
    }

    println!("{} nvm installed, installing latest LTS...", "→".cyan());

    // Source nvm in a fresh shell and install latest LTS.
    let install_cmd = "export NVM_DIR=\"$HOME/.nvm\"; [ -s \"$NVM_DIR/nvm.sh\" ] && . \"$NVM_DIR/nvm.sh\"; nvm install --lts";
    if run_interactive("sh", &["-c", install_cmd], dry_run, verbose).is_err() {
        println!("{} Failed to install Node.js via nvm", "[ERROR]".red());
        println!("{} Install Node.js manually: https://nodejs.org/", "Fix:".cyan());
        return false;
    }

    println!();
    println!("{}", "Node.js installed via nvm!".green().bold());
    println!("{} Restart your terminal to use `node`/`npm` directly", "Tip:".cyan());
    println!();

    check_node_installed()
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

fn run_interactive(cmd: &str, args: &[&str], dry_run: bool, verbose: bool) -> Result<(), String> {
    if dry_run {
        if verbose {
            eprintln!(
                "{} Would execute (interactive): {} {}",
                "[DRY-RUN]".cyan(),
                cmd,
                args.join(" ")
            );
        } else {
            eprintln!("{} {} {}", "[DRY-RUN]".cyan(), cmd, args.join(" "));
        }
        return Ok(());
    }

    if verbose {
        eprintln!("{} Running: {} {}", "[DEBUG]".cyan(), cmd, args.join(" "));
    }

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
                                println!("  {}", "oplirex connect claude-code --model <id>".bold().yellow());
                                println!();
                                println!("{}", "Examples:".bold());
                                for m in models.iter().take(3) {
                                    let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                    if !id.is_empty() {
                                        println!("  {}", format!("oplirex connect claude-code --model {}", id).dimmed());
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
            print_banner();
            println!("{}", "Welcome to oplireX Setup Wizard".bold().green());
            println!();
            println!("{}", "This will check and install all required components.".dimmed());
            println!();

            let mut steps_needed = Vec::new();

            if !check_node_installed() {
                steps_needed.push(("Node.js", "npm install -g npm", "Required for Claude Code and OpenCode"));
            }
            if !warp_installed {
                steps_needed.push(("Cloudflare WARP", "oplirex install warp", "Required for rate limit reset"));
            }
            if !check_opencode_installed() {
                steps_needed.push(("OpenCode", "oplirex install opencode", "AI coding assistant backend"));
            }
            if !check_claude_installed() {
                steps_needed.push(("Claude Code", "oplirex install claudecode", "AI coding assistant CLI"));
            }

            if steps_needed.is_empty() {
                println!("{}", "Everything is already installed!".green().bold());
                println!();
                println!("{} Run to get started:", "Tip:".cyan().bold());
                println!("  {}", "oplirex connect claude-code".bold().yellow());
                return;
            }

            println!("{}", "The following components need to be installed:".bold());
            println!();
            for (name, _, desc) in &steps_needed {
                print_info_formatted(name, desc);
            }
            println!();

            for (i, (name, _cmd, _)) in steps_needed.iter().enumerate() {
                println!();
                print_step(i + 1, &format!("Installing {}...", name));

                match *name {
                    "Node.js" => {
                        if !ensure_node_installed(cli.dry_run, cli.verbose) {
                            print_fail("Node.js installation failed");
                            println!("{} Install manually: https://nodejs.org/", "Fix:".cyan());
                        }
                    }
                    "Cloudflare WARP" => {
                        let _ = run_interactive("oplirex", &["install", "warp"], cli.dry_run, cli.verbose);
                    }
                    "OpenCode" => {
                        let _ = run_interactive("oplirex", &["install", "opencode"], cli.dry_run, cli.verbose);
                    }
                    "Claude Code" => {
                        let _ = run_interactive("oplirex", &["install", "claude-code"], cli.dry_run, cli.verbose);
                    }
                    _ => {}
                }
            }

            println!();
            println!("{}", "Setup complete!".green().bold());
            println!();
            println!("{}", "Next steps:".bold());
            println!("  1. {}", "oplirex doctor".bold().yellow());
            println!("  2. {}", "oplirex connect claude-code".bold().yellow());
        }

        Commands::Install { target } => match target {
            InstallTarget::Warp {} => {
                print_banner();
                println!("{}", "Installing Cloudflare WARP".bold().green());
                println!();

                if warp_installed {
                    println!("{} WARP is already installed", "[INFO]".green());
                    println!("{} Run `oplirex reset` to refresh your IP", "Tip:".cyan());
                    return;
                }

                if is_windows() {
                    install_warp_windows(cli.dry_run, cli.verbose);
                } else if let Ok(output) = Command::new("sh").arg("-c").arg("cat /etc/os-release").output() {
                    let os_release = String::from_utf8_lossy(&output.stdout);

                    if os_release.contains("arch") || os_release.contains("manjaro") {
                        print_step(1, "Installing cloudflare-warp-bin via yay...");
                        match run_interactive("yay", &["-S", "--noconfirm", "cloudflare-warp-bin"], cli.dry_run, cli.verbose) {
                            Ok(()) => print_success("WARP installed"),
                            Err(e) => print_fail(&format!("Installation failed: {}", e)),
                        }
                    } else if os_release.contains("ubuntu")
                        || os_release.contains("debian")
                        || os_release.contains("kali")
                    {
                        print_step(1, "Adding Cloudflare repository...");

                        // Remove the legacy repository file created by older oplirex versions.
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
                    if is_windows() {
                        println!("  1. {}", "Open the Cloudflare WARP tray app and connect".bold().yellow());
                    } else {
                        println!("  1. {}", "warp-cli connect".bold().yellow());
                        println!("  2. {}", "warp-cli status".bold().yellow());
                    }
                    println!("  3. {}", "oplirex reset".bold().yellow());
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

                if !ensure_node_installed(cli.dry_run, cli.verbose) {
                    std::process::exit(1);
                }

                print_step(1, "Installing OpenCode CLI (v2, with v1 fallback)...");
                let install_ok = run_npm_install(cli.dry_run, cli.verbose, &["install", "-g", "@opencode/cli"]).is_ok()
                    || run_npm_install(cli.dry_run, cli.verbose, &["install", "-g", "opencode-ai"]).is_ok();

                if install_ok {
                    print_success("OpenCode installed");
                    println!();
                    println!("{}", "Next steps:".bold());
                    println!("  1. {}", "opencode --version".bold().yellow());
                    println!("  2. {}", "oplirex doctor".bold().yellow());
                    println!("  3. {}", "oplirex connect opencode".bold().yellow());
                } else {
                    print_fail("OpenCode installation failed");
                    println!();
                    println!("{} Try manually:", "Fix:".cyan());
                    println!("  {}", "npm install -g @opencode/cli".bold().yellow());
                    println!("  {}", "npm install -g opencode-ai".bold().yellow());
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

                if !ensure_node_installed(cli.dry_run, cli.verbose) {
                    std::process::exit(1);
                }

                print_step(1, "Installing Claude Code via npm...");
                match run_npm_install(cli.dry_run, cli.verbose, &["install", "-g", "@anthropic-ai/claude-code"]) {
                    Ok(()) => {
                        print_success("Claude Code installed");
                        println!();
                        println!("{}", "Next steps:".bold());
                        println!("  1. {}", "claude --version".bold().yellow());
                        println!("  2. {}", "oplirex connect claude-code".bold().yellow());
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
                    let _ = run_interactive("oplirex", &["install", "warp"], cli.dry_run, cli.verbose);
                }

                print_step(2, "Checking OpenCode...");
                if check_opencode_installed() {
                    print_success("OpenCode already installed");
                } else {
                    let _ = run_interactive("oplirex", &["install", "opencode"], cli.dry_run, cli.verbose);
                }

                print_step(3, "Checking Claude Code...");
                if check_claude_installed() {
                    print_success("Claude Code already installed");
                } else {
                    let _ = run_interactive("oplirex", &["install", "claude-code"], cli.dry_run, cli.verbose);
                }

                println!();
                println!("{}", "All components installed!".green().bold());
                println!();
                println!("{}", "Run to get started:".bold());
                println!("  {}", "oplirex connect claude-code".bold().yellow());
            }
        },

        Commands::Connect { target } => match target {
            ConnectTarget::Opencode {
                listen,
                upstream,
                api_key,
                max_retries,
                warp_delay,
                model,
                system_prompt,
                opencode_args,
            } => {
                print_banner();
                println!("{}", "OpenCode v2 CLI Bridge".bold().green());
                println!();

                if !check_opencode_installed() {
                    eprintln!("{} OpenCode CLI not found in PATH", "[ERROR]".red());
                    eprintln!("{} Install: {} or {}", "Fix:".cyan(), "npm install -g @opencode/cli (v2)".bold().yellow(), "npm install -g opencode-ai (v1)".bold().yellow());
                    std::process::exit(1);
                }

                let selected_model = model.clone().unwrap_or_else(|| {
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
                });

                let config = ProxyConfig {
                    listen_addr: listen.clone(),
                    opencode_base_url: upstream.clone(),
                    opencode_api_key: api_key.clone(),
                    inbound_api_key: cli.proxy_api_key.clone(),
                    max_retries: *max_retries,
                    warp_reset_delay_ms: *warp_delay,
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

                if !wait_for_proxy_ready(&listen, 5000) {
                    eprintln!("{} Proxy did not become ready in time", "[ERROR]".red());
                    std::process::exit(1);
                }

                println!("{}", "Launching OpenCode CLI...".dimmed());
                println!();

                let binary = resolve_opencode_command();
                let mut cmd = Command::new(binary);
                let proxy_key = cli.proxy_api_key.clone().unwrap_or_else(|| "oplirex-proxy-key".to_string());
                cmd.env("ANTHROPIC_BASE_URL", format!("http://{}", listen_clone))
                    .env("ANTHROPIC_API_KEY", &proxy_key)
                    .env("OPENAI_BASE_URL", format!("http://{}", listen_clone))
                    .env("OPENAI_API_KEY", &proxy_key)
                    .env("OPENCODE_BASE_URL", format!("http://{}", listen_clone))
                    .env("OPENCODE_API_KEY", &proxy_key);

                if !model_clone.is_empty() {
                    cmd.env("ANTHROPIC_MODEL", &model_clone)
                        .env("OPENAI_MODEL", &model_clone)
                        .env("OPENCODE_MODEL", &model_clone);
                }

                if let Some(sp) = system_prompt {
                    cmd.env("CLAUDE_CODE_SYSTEM_PROMPT", sp)
                        .env("OPENCODE_SYSTEM_PROMPT", sp);
                }

                if !opencode_args.is_empty() {
                    cmd.args(opencode_args);
                }

                let status = cmd.status().map_err(|e| e.to_string()).unwrap_or_else(|_| {
                    eprintln!("{} Failed to launch OpenCode CLI", "[ERROR]".red());
                    std::process::exit(1);
                });

                println!();
                println!("{} OpenCode CLI exited with: {}", "Info:".cyan(), status.to_string().bold());
                println!("{} Shutting down proxy...", "→".green().dimmed());

                drop(proxy_handle);
            }
            ConnectTarget::ClaudeCode {
                listen,
                upstream,
                api_key,
                max_retries,
                warp_delay,
                model,
                system_prompt,
                claude_args,
            } => {
                print_banner();
                println!("{}", "Claude Code Bridge".bold().green());
                println!();

                if !check_claude_installed() {
                    eprintln!("{} Claude Code not found in PATH", "[ERROR]".red());
                    eprintln!("{} Install: {}", "Fix:".cyan(), "oplirex install claude-code".bold().yellow());
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
                    inbound_api_key: cli.proxy_api_key.clone(),
                    max_retries: *max_retries,
                    warp_reset_delay_ms: *warp_delay,
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

                if !wait_for_proxy_ready(&listen, 5000) {
                    eprintln!("{} Proxy did not become ready in time", "[ERROR]".red());
                    std::process::exit(1);
                }

                println!("{}", "Launching Claude Code...".dimmed());
                println!();

            let mut cmd = Command::new("claude");
            cmd.env("ANTHROPIC_BASE_URL", format!("http://{}", listen_clone))
                .env(
                    "ANTHROPIC_API_KEY",
                    cli.proxy_api_key.as_deref().unwrap_or("oplirex-proxy-key"),
                );

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
    },

    Commands::Watch {
            upstream,
            max_retries,
            warp_delay,
        } => {
            print_banner();
            println!("{}", "OpenCode Watch Mode".bold().yellow());
            println!();
            println!("{} Monitoring: {}", "→".green(), upstream.bold());
            println!("{} Auto-reset: {} (attempts: {})", "→".green(), "enabled".green().bold(), max_retries.to_string().bold());
            println!();
            println!("{}", "Watching for 429 rate limits...".dimmed());
            println!("{} Press Ctrl+C to stop", "Tip:".cyan());
            println!();

            let upstream_clone = upstream.clone();
            let max_retries_clone = *max_retries;
            let warp_delay_clone = *warp_delay;

            let rt = tokio::runtime::Runtime::new().unwrap();
            if let Err(e) = rt.block_on(async {
                oplirex::watch::start_watch_mode(
                    &upstream_clone,
                    max_retries_clone,
                    warp_delay_clone,
                )
                .await
            }) {
                eprintln!("{} Watch error: {}", "[ERROR]".red(), e);
                std::process::exit(1);
            }
        }

        Commands::Daemon {
            listen,
            upstream,
            api_key,
            max_retries,
            warp_delay,
        } => {
            print_banner();
            println!("{}", "Daemon Mode".bold().magenta());
            println!();
            println!("{} Listening:  {}", "→".green(), listen.bold());
            println!("{} Upstream:   {}", "→".green(), upstream.bold());
            println!("{} Auto-reset: {} (attempts: {})", "→".green(), "enabled".green().bold(), max_retries.to_string().bold());
            println!();
            println!("{}", "Running as background daemon...".dimmed());
            println!("{} The proxy will auto-reset rate limits silently", "Tip:".cyan());
            println!();

            let config = ProxyConfig {
                listen_addr: listen.clone(),
                opencode_base_url: upstream.clone(),
                opencode_api_key: api_key.clone(),
                inbound_api_key: cli.proxy_api_key.clone(),
                max_retries: *max_retries,
                warp_reset_delay_ms: *warp_delay,
            };

            let rt = tokio::runtime::Runtime::new().unwrap();
            if let Err(e) = rt.block_on(async {
                oplirex::proxy::start_proxy_server(config).await
            }) {
                eprintln!("{} Daemon error: {}", "[ERROR]".red(), e);
                std::process::exit(1);
            }
        }

        Commands::Doctor {} => {
            print_banner();
            println!("{}", "System Diagnostics".bold().cyan());
            println!();

            let mut all_ok = true;
            let mut checks = Vec::new();

            if warp_installed {
                let label = if is_windows() { "WARP" } else { "WARP CLI" };
                checks.push((label, "installed".to_string(), true));
            } else {
                let label = if is_windows() { "WARP" } else { "WARP CLI" };
                checks.push((label, "NOT FOUND".to_string(), false));
                all_ok = false;
            }

            if check_claude_installed() {
                let version = run_command("claude", &["--version"], false, false)
                    .map(|v| v.trim().to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                checks.push(("Claude Code", format!("installed ({})", version), true));
            } else {
                checks.push(("Claude Code", "NOT FOUND".to_string(), false));
                all_ok = false;
            }

            if check_opencode_installed() {
                checks.push(("OpenCode", "installed".to_string(), true));
            } else {
                checks.push(("OpenCode", "NOT FOUND".to_string(), false));
                all_ok = false;
            }

            let config = load_config();
            let upstream = config.upstream.clone();
            if check_opencode_running(&upstream) {
                checks.push(("OpenCode Zen", format!("running ({})", upstream), true));
            } else {
                checks.push(("OpenCode Zen", format!("NOT REACHABLE ({})", upstream), false));
                all_ok = false;
            }

            let cfg_path = config_path();
            if cfg_path.exists() {
                checks.push(("Config file", format!("found ({})", cfg_path.display()), true));
            } else {
                checks.push(("Config file", "using defaults".to_string(), true));
            }

            if std::net::TcpListener::bind("127.0.0.1:8080").is_ok() {
                checks.push(("Port 8080", "available".to_string(), true));
            } else {
                checks.push(("Port 8080", "IN USE".to_string(), false));
                all_ok = false;
            }

            for (name, status, ok) in &checks {
                let status_str = if *ok {
                    status.green().bold()
                } else {
                    status.red().bold()
                };
                println!("{} {}: {}", "→".cyan(), name.bold(), status_str);
            }

            println!();
            if all_ok {
                println!("{}", "All checks passed!".green().bold());
            } else {
                println!("{}", "Some checks failed.".yellow().bold());
                println!();
                println!("{} Run `oplirex setup` for guided installation", "Fix:".cyan());
            }
        }

        Commands::QuickReset {} => {
            if !warp_installed {
                println!("{} WARP is not installed", "[ERROR]".red());
                println!("{} Run `oplirex install warp` first", "Tip:".cyan().bold());
                return;
            }

            println!("{}", "Quick-resetting WARP tunnel...".bold());

            if cli.verbose {
                eprintln!("{} Disconnecting...", "[DEBUG]".cyan());
            }
            let _ = run_warp_cli(&["disconnect"], cli.dry_run, cli.verbose);

            if cli.verbose {
                eprintln!("{} Registration new...", "[DEBUG]".cyan());
            }
            let _ = run_warp_cli(
                &["registration", "new"],
                cli.dry_run,
                cli.verbose,
            );

            if cli.verbose {
                eprintln!("{} Connecting...", "[DEBUG]".cyan());
            }
            let _ = run_warp_cli(&["connect"], cli.dry_run, cli.verbose);

            println!("{}", "Quick reset complete!".green().bold());
        }

        Commands::Config { action } => match action {
            ConfigAction::Show {} => {
                let config = load_config();
                println!("{}", "Current Configuration:".bold());
                println!();
                println!("{} {}", "Listen:".bold(), config.listen);
                println!("{} {}", "Upstream:".bold(), config.upstream);
                println!("{} {}", "Max Retries:".bold(), config.max_retries);
                println!("{} {}ms", "WARP Delay:".bold(), config.warp_delay);
                println!("{} {}", "Config file:".bold(), config_path().display());
            }
            ConfigAction::Set {
                key,
                value,
                listen,
                upstream,
                max_retries,
                warp_delay,
            } => {
                let mut config = load_config();

                // Apply the key/value pair first (if given), then the explicit
                // field overrides. This lets users do either:
                //   oplirex config set --key listen --value 0.0.0.0:9000
                //   oplirex config set --listen 0.0.0.0:9000
                if let Some(k) = key.as_deref() {
                    let v = value.as_deref().unwrap_or("");
                    match k {
                        "listen" => config.listen = v.to_string(),
                        "upstream" => config.upstream = v.to_string(),
                        "max_retries" => {
                            config.max_retries = v.parse::<u32>().unwrap_or(config.max_retries);
                        }
                        "warp_delay" => {
                            config.warp_delay = v.parse::<u64>().unwrap_or(config.warp_delay);
                        }
                        other => {
                            eprintln!(
                                "{} Unknown config key: {}. Valid: listen, upstream, max_retries, warp_delay",
                                "[ERROR]".red(),
                                other
                            );
                            std::process::exit(1);
                        }
                    }
                }

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
        } => {
            let config = ProxyConfig {
                listen_addr: listen.clone(),
                opencode_base_url: upstream.clone(),
                opencode_api_key: api_key.clone(),
                inbound_api_key: cli.proxy_api_key.clone(),
                max_retries: *max_retries,
                warp_reset_delay_ms: *warp_delay,
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
            println!("{} Or run: {}", "→".cyan(), "oplirex connect claude-code".bold());
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
                    println!("{{\"connected\": false, \"tunnel_id\": null, \"error\": \"WARP not installed\"}}");
                } else {
                    println!("{} {}", "Tunnel:".bold(), "Not connected".red());
                    println!("{} {}", "WARP:".bold(), "Not installed".red());
                    println!(
                        "\n{} Run `oplirex install warp` to install WARP",
                        "Tip:".cyan().bold()
                    );
                }
                return;
            }

            println!("{}", "Checking tunnel status...".dimmed());

            match run_warp_cli(&["status"], cli.dry_run, cli.verbose) {
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
                println!("{} Run `oplirex install warp` first", "Tip:".cyan().bold());
                return;
            }

            println!("{}", "Resetting WARP tunnel...".bold());

            if cli.verbose {
                eprintln!("{} Step 1: Disconnecting...", "[DEBUG]".cyan());
            }
            let _ = run_warp_cli(&["disconnect"], cli.dry_run, cli.verbose);

            if !is_windows() {
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
            }

            if cli.verbose {
                eprintln!("{} Step 5: Registering new tunnel...", "[DEBUG]".cyan());
            }
            let _ = run_warp_cli(
                &["registration", "new"],
                cli.dry_run,
                cli.verbose,
            );

            if cli.verbose {
                eprintln!("{} Step 6: Connecting...", "[DEBUG]".cyan());
            }
            let _ = run_warp_cli(&["connect"], cli.dry_run, cli.verbose);

            println!("{}", "WARP tunnel reset complete!".green().bold());
            println!("{} Run `oplirex status` to verify", "Tip:".cyan());
        }

        Commands::Stop {} => {
            if !warp_installed {
                println!("{} WARP is not installed", "[ERROR]".red());
                println!("{} Run `oplirex install warp` first", "Tip:".cyan().bold());
                return;
            }

            println!("{}", "Stopping WARP tunnel...".bold());

            if cli.verbose {
                eprintln!("{} Step 1: Disconnecting...", "[DEBUG]".cyan());
            }
            let _ = run_warp_cli(&["disconnect"], cli.dry_run, cli.verbose);

            if !is_windows() {
                if cli.verbose {
                    eprintln!("{} Step 2: Stopping warp-svc...", "[DEBUG]".cyan());
                }
                let _ = run_sudo_command("systemctl stop warp-svc", cli.dry_run, cli.verbose);

                if cli.verbose {
                    eprintln!("{} Step 3: Disabling warp-svc...", "[DEBUG]".cyan());
                }
                let _ = run_sudo_command("systemctl disable warp-svc", cli.dry_run, cli.verbose);
            }

            println!("{}", "WARP tunnel stopped!".green().bold());
            println!("{} Run `oplirex reset` to restart", "Tip:".cyan());
        }

        Commands::About {} => {
            print_banner();
            println!();
            println!("{} {}", "Version:".bold(), VERSION);
            println!("{} Rust", "Language:".bold());
            println!("{} OpenCode rate limit reset + Anthropic proxy", "Purpose:".bold());
            println!("{} Cloudflare WARP + Axum HTTP", "Infrastructure:".bold());
            println!("{} nxyy", "Author:".bold());
            println!(
                "{} https://github.com/nxyystore/oplirex",
                "GitHub:".bold()
            );
            println!();
            println!("{}", "Installation:".bold());
            println!("  oplirex install warp          # Install Cloudflare WARP");
            println!("  oplirex install opencode      # Install OpenCode");
            println!("  oplirex install claude-code   # Install Claude Code CLI");
            println!("  oplirex install all           # Install everything");
            println!("  oplirex setup                 # Guided setup wizard");
            println!();
            println!("{}", "WARP Management:".bold());
            println!("  oplirex status              # Check WARP status");
            println!("  oplirex reset               # Full WARP tunnel reset");
            println!("  oplirex quick-reset         # Fast WARP IP rotation");
            println!("  oplirex stop                # Stop WARP tunnel");
            println!();
            println!("{}", "Proxy & Claude Code:".bold());
            println!("  oplirex proxy               # Start reverse proxy");
            println!("  oplirex connect claude-code # Proxy + launch Claude Code");
            println!("  oplirex daemon              # Background proxy service");
            println!("  oplirex watch               # Monitor OpenCode, auto-reset");
            println!();
            println!("{}", "Configuration:".bold());
            println!("  oplirex models              # List available OpenCode models");
            println!("  oplirex config show         # Show current config");
            println!("  oplirex config set          # Save config");
            println!("  oplirex config reset        # Reset to defaults");
            println!("  oplirex doctor              # Diagnose system setup");
        }
    }

    if !matches!(&cli.command, Commands::About {} | Commands::Proxy { .. } | Commands::Connect { .. } | Commands::Daemon { .. } | Commands::Watch { .. } | Commands::Doctor {} | Commands::Config { .. } | Commands::Setup {} | Commands::Install { .. } | Commands::Models { .. }) {
        println!("\n{} v{}", "oplirex".bold(), VERSION.dimmed());
    }
}
