```
   ___      ___    _       ___     ___     ___
  / _ \    | _ \  | |     |_ _|   | _ \   | __|
 | (_) |   |  _/  | |__    | |    |   /   | _|
  \___/   _|_|_   |____|  |___|   |_|_\   |___|
_|"""""|_| """ |_|"""""|_|"""""|_|"""""|_|"""""|
"`-0-0-'"`-0-0-'"`-0-0-'"`-0-0-'"`-0-0-'"`-0-0-'
     OpenCode Limit Reset + Proxy
     by nxyy
```

[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange?logo=rust)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-red)](LICENSE)
<!-- [![AUR](https://img.shields.io/badge/AUR-1.0.0-blue?style=flat-square)](https://aur.archlinux.org/packages/oplirex) -->

## What is oplirex?

**oplirex** is a dual-purpose tool:

1. **WARP Rate Limit Reset** - Rotates your IP via Cloudflare WARP to reset OpenCode rate limits
2. **API Proxy Bridge** - Connects Claude Code and OpenCode CLI to an OpenAI-compatible upstream with automatic rate limit recovery

### How It Works

#### WARP Reset Mode
OpenCode tracks users by IP. When you hit the rate limit:
1. **Stops** the current WARP tunnel
2. **Clears** cached session data
3. **Creates** a new tunnel registration (new IP)
4. **Restarts** WARP with a fresh IP

#### Proxy Bridge Mode
Claude Code or OpenCode CLI → oplirex proxy (127.0.0.1:8080) → OpenAI-compatible upstream
- Translates Anthropic API format to OpenAI chat completions
- Streams SSE responses in real-time
- Exposes free models via `/v1/models` endpoint
- **Auto-resets WARP** on 429 rate limits — transparently

## Installation

### Linux (AUR)
```bash
yay -S oplirex
```

### Linux / macOS (one-liner install script)
```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/nxyystore/oplirex/releases/download/v1.0.0/oplirex-installer.sh | sh
```

### Windows (winget)
```powershell
winget install nxyystore.oplirex
```

### Windows (PowerShell one-liner)
```powershell
irm https://github.com/nxyystore/oplirex/releases/download/v1.0.0/oplirex-installer.ps1 | iex
```

### From Source
```bash
git clone https://github.com/nxyystore/oplirex.git
cd oplirex
cargo build --release
sudo cp target/release/oplirex /usr/bin/oplirex
```

## Usage

### Quick Start — Claude Code + Proxy
```bash
# One command: starts proxy + launches Claude Code with correct env vars
oplirex connect claude-code

# With specific model
oplirex connect claude-code --model glm-4.7-free

# With custom upstream
oplirex connect claude-code --upstream http://my-opencode-server:3000
```

### OpenCode CLI v2

Install either CLI generation; v2 is recommended and v1 remains supported:
```bash
npm install -g @opencode/cli  # v2
# or
npm install -g opencode-ai    # v1
```

Start the proxy and OpenCode CLI together:
```bash
oplirex connect opencode --upstream http://localhost:3000
```

### Proxy Authentication

The proxy is unauthenticated by default for local use. Set a separate inbound key when exposing it beyond localhost. `--api-key` remains the upstream credential.
```bash
OPLIREX_PROXY_API_KEY=replace-with-a-secret oplirex connect opencode
OPLIREX_PROXY_API_KEY=replace-with-a-secret oplirex daemon
```

The same key can be passed as the global `--proxy-api-key` option. Clients may authenticate with `Authorization: Bearer <key>` or `x-api-key: <key>`.

### WARP Reset Commands
```bash
oplirex reset          # Full WARP tunnel reset
oplirex quick-reset    # Fast IP rotation (no service restart)
oplirex status         # Check WARP connection status
oplirex stop           # Stop WARP tunnel
oplirex install        # Install Cloudflare WARP
```

### Proxy Commands
```bash
oplirex proxy                          # Start reverse proxy on :8080
oplirex proxy --listen 0.0.0.0:9000    # Custom listen address
oplirex daemon                         # Background daemon mode
oplirex watch                          # Monitor OpenCode, auto-reset on 429
```

### Configuration
```bash
oplirex config show    # Show current settings
oplirex config set     # Save configuration
oplirex config reset   # Reset to defaults
```

### Diagnostics
```bash
oplirex doctor         # Check WARP, Claude Code, OpenCode setup
oplirex about          # Show version and info
```

`/health` reports that the proxy process is running. `/readyz` returns success only while the configured upstream's `/v1/models` endpoint is reachable.

## Claude Code Integration

### Method 1: One-liner (Recommended)
```bash
oplirex connect claude-code
```

### Method 2: Manual Environment
```bash
# Start proxy in background
oplirex daemon

# Set environment and launch Claude Code
export ANTHROPIC_BASE_URL=http://127.0.0.1:8080
export ANTHROPIC_API_KEY=oplirex-proxy-key
claude
```

### Method 3: Claude Code Settings
```bash
claude
> /config
# Set API Base URL: http://127.0.0.1:8080
# Set API Key: oplirex-proxy-key
```

## Available Free Models

When connected through the proxy, these models appear in Claude Code's `/models` list:

| Model | ID |
|-------|-----|
| GLM 4.7 Free | `glm-4.7-free` |
| MiniMax M2.1 Free | `minimax-m2.1-free` |
| Kimi K2.5 Free | `kimi-k2.5-free` |
| Qwen 2.5 72B Free | `qwen-2.5-72b-free` |
| Llama 3.3 70B Free | `llama-3.3-70b-free` |

This is the built-in fallback, used when the upstream model list is unavailable. When the proxy runs, it reports the live upstream list instead. If upstream renames a model between releases, override the list without recompiling:

```bash
export OPLIREX_FREE_MODELS=glm-4.7-free,kimi-k2.5-free
```

## Options

- `--verbose` - Detailed output
- `--dry-run` - Preview changes without executing
- `--json` - JSON output (status command)

## About

```
Version: 1.0.0
Language: Rust
Purpose: OpenCode rate limit reset + Anthropic proxy
Infrastructure: Cloudflare WARP + Axum HTTP
Author: nxyy
GitHub: https://github.com/nxyystore/oplirex
```

## License

MIT License. See [LICENSE](LICENSE) for details.

---

Improved by [nxyy](https://github.com/nxyystore)
