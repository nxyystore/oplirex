#!/bin/bash
# oplire unified installer - oplirex + optional WARP
# Supports: apt (debian/ubuntu/kali/mint/pop), dnf (fedora/rhel/rocky/alma), pacman (arch/manjaro), generic tarball
set -e

FORCE=false
VERBOSE=false
INSTALL_WARP=true
VERSION="latest"

while [[ $# -gt 0 ]]; do
  case $1 in
    --force|-f) FORCE=true; shift ;;
    --verbose|-v) VERBOSE=true; shift ;;
    --no-warp) INSTALL_WARP=false; shift ;;
    --version) VERSION="$2"; shift 2 ;;
    --help|-h)
      echo "Usage: $0 [--force] [--verbose] [--no-warp] [--version vX.Y.Z]"
      echo "  --force     Reinstall even if present"
      echo "  --verbose   Detailed output"
      echo "  --no-warp   Skip Cloudflare WARP install"
      echo "  --version   Install specific version (default: latest)"
      exit 0 ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

log() { echo "[oplire] $*"; }
vlog() { [[ "$VERBOSE" == true ]] && echo "[verbose] $*" || true; }

need_root() {
  if [[ "$EUID" -ne 0 ]]; then
    if command -v sudo >/dev/null 2>&1; then
      log "Requesting sudo..."
      exec sudo bash "$0" "$@"
    else
      echo "Please run as root or install sudo"; exit 1
    fi
  fi
}

# ---- install oplirex binary ----
install_oplirex() {
  if command -v oplirex >/dev/null 2>&1 && [[ "$FORCE" == false ]]; then
    log "oplirex already installed: $(oplirex --version 2>/dev/null || echo present)"
    return 0
  fi

  # Detect arch
  ARCH=$(uname -m)
  case $ARCH in
    x86_64|amd64) ARCH_TAG="linux-x86_64" ;;
    aarch64|arm64) ARCH_TAG="linux-aarch64" ;;
    *) ARCH_TAG="linux-x86_64"; log "Unknown arch $ARCH, trying x86_64" ;;
  esac

  if [[ "$VERSION" == "latest" ]]; then
    URL="https://github.com/nxyystore/oplire/releases/latest/download/oplirex-${ARCH_TAG}.tar.gz"
  else
    URL="https://github.com/nxyystore/oplire/releases/download/${VERSION}/oplirex-${ARCH_TAG}.tar.gz"
  fi

  # Try distro-native package first
  if [[ -f /etc/os-release ]]; then
    . /etc/os-release
    OS=$ID
    vlog "OS=$OS ARCH=$ARCH_TAG URL=$URL"

    case $OS in
      arch|manjaro|endeavouros|cachyos)
        if command -v yay >/dev/null 2>&1; then
          log "Installing via AUR (oplire-bin)..."
          sudo -u "${SUDO_USER:-$USER}" yay -S --noconfirm oplire-bin && return 0 || true
        elif command -v paru >/dev/null 2>&1; then
          log "Installing via AUR (paru)..."
          sudo -u "${SUDO_USER:-$USER}" paru -S --noconfirm oplire-bin && return 0 || true
        fi
        ;;
      ubuntu|debian|linuxmint|pop|kali)
        # Try .deb if available
        DEB_URL="${URL%.tar.gz}.deb"
        # fallback to tarball if deb 404 — we just try tarball directly
        ;;
      fedora|rhel|centos|rocky|alma|opensuse*)
        RPM_URL="${URL%.tar.gz}.rpm"
        ;;
    esac
  fi

  # Generic tarball fallback
  log "Downloading oplirex $VERSION ($ARCH_TAG)..."
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$TMP/oplirex.tar.gz"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$TMP/oplirex.tar.gz" "$URL"
  else
    echo "Need curl or wget"; exit 1
  fi
  tar xzf "$TMP/oplirex.tar.gz" -C "$TMP"
  # tar contains oplirex binary at root or with prefix
  BIN=$(find "$TMP" -name "oplirex" -type f | head -1)
  if [[ -z "$BIN" ]]; then echo "oplirex binary not found in tarball"; exit 1; fi
  install -Dm755 "$BIN" /usr/local/bin/oplirex
  ln -sf /usr/local/bin/oplirex /usr/local/bin/oplire 2>/dev/null || true
  ln -sf /usr/local/bin/oplirex /usr/bin/oplirex 2>/dev/null || true
  log "oplirex installed to /usr/local/bin/oplirex"
}

# ---- install WARP (optional) ----
install_warp() {
  [[ "$INSTALL_WARP" == false ]] && { log "Skipping WARP (--no-warp)"; return 0; }
  if command -v warp-cli >/dev/null 2>&1 && [[ "$FORCE" == false ]]; then
    log "warp-cli already installed"
    return 0
  fi
  if [[ ! -f /etc/os-release ]]; then log "Cannot detect OS for WARP"; return 0; fi
  . /etc/os-release
  OS=$ID
  log "Installing Cloudflare WARP on $OS..."

  case $OS in
    ubuntu|debian|linuxmint|pop|kali)
      # Fix legacy repo file from old oplire versions
      rm -f /etc/apt/sources.list.d/cloudflare-warp.list
      curl -fsSL https://pkg.cloudflareclient.com/pubkey.gpg | gpg --yes --dearmor --output /usr/share/keyrings/cloudflare-warp-archive-keyring.gpg 2>/dev/null || true
      CODENAME=$(. /etc/os-release && echo "$VERSION_CODENAME")
      if [[ "$OS" == "kali" ]]; then CODENAME="trixie"; fi  # Kali rolling compat
      if [[ -z "$CODENAME" || "$CODENAME" == "kali-rolling" ]]; then CODENAME="bookworm"; fi
      echo "deb [signed-by=/usr/share/keyrings/cloudflare-warp-archive-keyring.gpg] https://pkg.cloudflareclient.com/ $CODENAME main" > /etc/apt/sources.list.d/cloudflare-client.list
      apt-get update -qq
      apt-get install -y -qq cloudflare-warp || { echo "Failed to install cloudflare-warp"; return 1; }
      ;;
    fedora|rhel|centos|rocky|alma)
      cat > /etc/yum.repos.d/cloudflare-warp.repo <<EOF
[cloudflare-warp]
name=Cloudflare WARP
baseurl=https://pkg.cloudflare.com/epel-$VERSION_ID/
enabled=1
gpgcheck=1
gpgkey=https://pkg.cloudflare.com/pubkey.gpg
EOF
      if command -v dnf >/dev/null 2>&1; then dnf install -y -q cloudflare-warp; else yum install -y -q cloudflare-warp; fi
      ;;
    arch|manjaro|endeavouros|cachyos)
      pacman -Sy --noconfirm cloudflare-warp 2>/dev/null || yay -S --noconfirm cloudflare-warp-bin 2>/dev/null || true
      ;;
    *)
      log "Unsupported OS for auto WARP install: $OS — see https://developers.cloudflare.com/warp-client/get-started/linux/"
      ;;
  esac
}

# ---- main ----
# Need root for /usr/local/bin and apt/dnf
if [[ "$EUID" -ne 0 ]]; then
  if command -v sudo >/dev/null 2>&1; then
    log "Re-running with sudo..."
    exec sudo bash "$0" ${FORCE:+--force} ${VERBOSE:+--verbose} ${INSTALL_WARP:+} --version "$VERSION" "$@"
  else
    echo "Please run as root"; exit 1
  fi
fi

install_oplirex
install_warp

if command -v oplirex >/dev/null 2>&1; then
  log "Done: $(oplirex --version 2>/dev/null || echo oplirex)"
  echo "  oplire status        # check WARP"
  echo "  oplire connect claude-code"
else
  echo "Warning: oplirex not found on PATH — try: /usr/local/bin/oplirex --version"
fi
if command -v warp-cli >/dev/null 2>&1; then
  log "warp-cli: $(warp-cli --version 2>/dev/null || echo present)"
fi
