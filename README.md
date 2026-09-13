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
[![AUR](https://img.shields.io/badge/AUR-1.0.1-blue?style=flat-square)](https://aur.archlinux.org/packages/oplirex)

## What is oplirex?

**oplirex** is a dual-purpose tool:

1. **WARP Rate Limit Reset** - Rotates your IP via Cloudflare WARP to reset OpenCode rate limits
2. **OpenCode Proxy Daemon** - Reverse proxy in front of OpenCode with automatic rate limit recovery, serving both API dialects:
   - `POST /v1/messages` (Anthropic — Claude Code)
   - `POST /v1/chat/completions` (OpenAI — opencode CLI, OpenAI SDKs, passed through untouched)

### How It Works

#### WARP Reset Mode

OpenCode tracks users by IP. When you hit the rate limit:

1. **Stops** the current WARP tunnel
2. **Clears** cached session data
3. **Creates** a new tunnel registration (new IP)
4. **Restarts** WARP with a fresh IP

#### Proxy Bridge Mode

Any client → oplire proxy (127.0.0.1:8080) → OpenCode Zen

- Anthropic clients (`/v1/messages`): translates Anthropic API format to OpenAI format, streams SSE responses in real-time
- OpenAI clients (`/v1/chat/completions`): passed through untouched (native opencode / OpenAI-SDK shape, streaming relayed as-is)
- Exposes free models via `/v1/models` endpoint
- **Auto-resets WARP** on 429 rate limits — transparently, for both dialects

## Installation

```bash
npm i -g oplirex@latest
brew install nxyystore/oplirex/oplirex

```

### From Source

```bash
git clone https://github.com/nxyystore/oplirex.git
cd oplirex
cargo build --release
sudo cp target/release/oplirex /usr/bin/oplirex
```

### Release versioning

```bash
python3 scripts/sync_release_version.py 1.0.0
# or update the current Cargo version and sync all packaging metadata
python3 scripts/sync_release_version.py
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
oplirex proxy                          # Start proxy daemon on :8080 (both API dialects)
oplirex proxy --listen 0.0.0.0:9000    # Custom listen address
oplirex proxy --extra-upstream http://host2:3000   # Round-robin across upstreams (repeatable)
oplirex proxy --fallback-model kimi-k2.5-free      # Try this model after 429s (repeatable)
oplirex proxy --require-key s3cret                 # Require a client API key
oplirex daemon                         # Background daemon mode
oplirex watch                          # Monitor OpenCode, auto-reset on 429
```

### Reliability: failover, circuit breaker, preemptive rotation

```bash
# Failover: when the requested model 429s past its retries, the proxy tries
# each --fallback-model in order (also settable via config file).
oplirex proxy --fallback-model kimi-k2.5-free --fallback-model glm-4.7-free

# Circuit breaker: after N consecutive 429s the proxy returns 503 for a
# cooldown instead of hammering the upstream. /health reports open/degraded.
oplirex proxy --circuit-threshold 5 --circuit-cooldown 60

# Preemptive rotation: `watch` polls the upstream and rotates WARP early when
# it sees low rate-limit headers or a trend of 429s — before traffic fails.
oplirex watch --preemptive-window 10 --preemptive-threshold 3

# Observability: open http://127.0.0.1:8080/dashboard (token usage per model,
# recent requests, WARP history) or GET /_oplire/metrics. Both are key-gated
# when --require-key is set (pass ?key=... in the browser).

# Hot-reload the proxy after `config set` — no restart needed:
curl -X POST http://127.0.0.1:8080/_oplire/reload
# Daemon also reloads on SIGHUP:  kill -HUP $(pgrep oplirex)
```

### Direct opencode Usage

```bash
# One command: starts proxy daemon + launches opencode routed through it
oplirex connect opencode

# With specific model (+ extra args passed to opencode after --)
oplirex connect opencode --model glm-4.7-free -- --print "hello"

# Or point any OpenAI-compatible client at the daemon manually:
oplirex daemon &
export OPENAI_BASE_URL=http://127.0.0.1:8080/v1
export OPENAI_API_KEY=oplire-proxy-key
opencode
```

### opencode provider snippet

`connect opencode` can write the `provider.oplire` subtree for your
`opencode.json` so the model is selectable inside opencode directly:

```bash
oplirex connect opencode --model glm-4.7-free \
  --provider-config ./oplire-provider.json
# then merge provider.oplire from that file into your opencode.json
```

### Configuration

```bash
oplirex config show    # Show current settings
oplirex config set     # Save configuration
oplirex config reset   # Reset to defaults

# config set persists the proxy options too (used as defaults by every command):
oplirex config set --upstream http://localhost:3000 \
  --extra-upstream http://host2:3000 \
  --fallback-model kimi-k2.5-free \
  --require-key s3cret \
  --circuit-threshold 5 --circuit-cooldown 60 \
  --hook "notify-me.sh" \
  --preemptive-window 10 --preemptive-threshold 3
# (empty string clears --require-key / --hook; the proxy picks up changes via
# POST /_oplire/reload or SIGHUP — no restart needed)
```

### Diagnostics

```bash
oplirex doctor         # Check WARP, Claude Code, OpenCode setup
oplirex about          # Show version and info
```

## Claude Code Integration

### Method 1: One-liner (Recommended)

```bash
oplirex connect claude-code
```

### Method 2: Manual Environment

```bash
# Start proxy in background
oplirex daemon &

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
Version: 1.0.1
Language: Rust
Purpose: OpenCode rate limit reset + Anthropic proxy
Infrastructure: Cloudflare WARP + Axum HTTP
Author: nxyy
GitHub: https://github.com/nxyystore/oplirex
```

## License

MIT License. See [LICENSE](LICENSE) for details.

---

Made by [Berke Oruc](https://github.com/BerkeOruc)
Improved by [nxyy](https://nxyy.codes)
