```
            ___         _  __
 ___  ___  / (_)______ | |/_/
/ _ \/ _ \/ / / __/ -_)>  <
\___/ .__/_/_/_/  \__/_/|_|
   /_/
     opencode.ai Ratelimit Reset & Proxy
```

[![Rust](https://img.shields.io/badge/rust-1.75%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![AUR](https://img.shields.io/badge/AUR-2.4.1-blue?style=flat-square)](https://aur.archlinux.org/packages/oplire)

## What is oplire?

**oplire** is a dual-purpose tool:

1. **WARP Rate Limit Reset** - Rotates your IP via Cloudflare WARP to reset OpenCode rate limits
2. **Anthropic Proxy Bridge** - Reverse proxy that connects Claude Code to OpenCode Zen's free models with automatic rate limit recovery

### How It Works

#### WARP Reset Mode

OpenCode tracks users by IP. When you hit the rate limit:

1. **Stops** the current WARP tunnel
2. **Clears** cached session data
3. **Creates** a new tunnel registration (new IP)
4. **Restarts** WARP with a fresh IP

#### Proxy Bridge Mode

Claude Code → oplire proxy (127.0.0.1:8080) → OpenCode Zen

- Translates Anthropic API format to OpenAI format
- Streams SSE responses in real-time
- Exposes free models via `/v1/models` endpoint
- **Auto-resets WARP** on 429 rate limits — transparently

## Installation

### Linux (AUR)

```bash
yay -S oplirex
```

### Windows (winget)

```powershell
winget install nxyy.oplirex
```

### macOS

```bash
brew install berkeoruc/oplire/oplire
```

### From Source

```bash
git clone https://github.com/BerkeOruc/oplire.git
cd oplire
cargo build --release
sudo cp target/release/oplire /usr/bin/oplire
```

### Release versioning

```bash
python3 scripts/sync_release_version.py 2.4.1
# or update the current Cargo version and sync all packaging metadata
python3 scripts/sync_release_version.py
```

## Usage

### Quick Start — Claude Code + Proxy

```bash
# One command: starts proxy + launches Claude Code with correct env vars
oplire connect claude-code

# With specific model
oplire connect claude-code --model glm-4.7-free

# With custom upstream
oplire connect claude-code --upstream http://my-opencode-server:3000
```

### WARP Reset Commands

```bash
oplire reset          # Full WARP tunnel reset
oplire quick-reset    # Fast IP rotation (no service restart)
oplire status         # Check WARP connection status
oplire stop           # Stop WARP tunnel
oplire install        # Install Cloudflare WARP
```

### Proxy Commands

```bash
oplire proxy                          # Start reverse proxy on :8080
oplire proxy --listen 0.0.0.0:9000    # Custom listen address
oplire daemon                         # Background daemon mode
oplire watch                          # Monitor OpenCode, auto-reset on 429
```

### Configuration

```bash
oplire config show    # Show current settings
oplire config set     # Save configuration
oplire config reset   # Reset to defaults
```

### Diagnostics

```bash
oplire doctor         # Check WARP, Claude Code, OpenCode setup
oplire about          # Show version and info
```

## Claude Code Integration

### Method 1: One-liner (Recommended)

```bash
oplire connect claude-code
```

### Method 2: Manual Environment

```bash
# Start proxy in background
oplire daemon &

# Set environment and launch Claude Code
export ANTHROPIC_BASE_URL=http://127.0.0.1:8080
export ANTHROPIC_API_KEY=oplire-proxy-key
claude
```

### Method 3: Claude Code Settings

```bash
claude
> /config
# Set API Base URL: http://127.0.0.1:8080
# Set API Key: oplire-proxy-key
```

## Available Free Models

When connected through the proxy, these models appear in Claude Code's `/models` list:

| Model              | ID                   |
| ------------------ | -------------------- |
| GLM 4.7 Free       | `glm-4.7-free`       |
| MiniMax M2.1 Free  | `minimax-m2.1-free`  |
| Kimi K2.5 Free     | `kimi-k2.5-free`     |
| Qwen 2.5 72B Free  | `qwen-2.5-72b-free`  |
| Llama 3.3 70B Free | `llama-3.3-70b-free` |

## Options

- `--verbose` - Detailed output
- `--dry-run` - Preview changes without executing
- `--json` - JSON output (status command)

## About

```
Version: 2.4.1
Language: Rust
Purpose: OpenCode rate limit reset + Anthropic proxy
Infrastructure: Cloudflare WARP + Axum HTTP
Author: Berke Oruc
GitHub: https://github.com/nxyystore/oplire
```

## License

MIT License. See [LICENSE](LICENSE) for details.

---

Made by [Berke Oruc](https://github.com/BerkeOruc)
Improved by [nxyy](https://nxyy.codes)
