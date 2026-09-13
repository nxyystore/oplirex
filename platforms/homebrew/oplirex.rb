class Oplirex < Formula
  desc "OpenCode Limit Reset + Anthropic Proxy Bridge"
  homepage "https://github.com/nxyystore/oplirex"
  version "2.4.1"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/nxyystore/oplirex/releases/download/v2.4.1/oplirex-macos-arm64.tar.gz"
      sha256 "REPLACEME_ARM64"
    else
      url "https://github.com/nxyystore/oplirex/releases/download/v2.4.1/oplirex-macos-x86_64.tar.gz"
      sha256 "REPLACEME_X86_64"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/nxyystore/oplirex/releases/download/v2.4.1/oplirex-linux-aarch64.tar.gz"
      sha256 "REPLACEME_LINUX_ARM64"
    else
      url "https://github.com/nxyystore/oplirex/releases/download/v2.4.1/oplirex-linux-x86_64.tar.gz"
      sha256 "REPLACEME_LINUX_X86_64"
    end
  end

  def install
    bin.install "oplirex"
    bin.install_symlink bin/"oplirex" => "oplire"
  end

  test do
    assert_match "oplirex", shell_output("#{bin}/oplirex --version")
  end
end
