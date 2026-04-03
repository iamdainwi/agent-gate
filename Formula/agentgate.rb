class Agentgate < Formula
  desc "AI Agent Security & Observability Gateway for MCP servers"
  homepage "https://github.com/iamdainwi/agentgate"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/iamdainwi/agentgate/releases/download/v#{version}/agentgate-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_AARCH64_DARWIN_SHA256"
    else
      url "https://github.com/iamdainwi/agentgate/releases/download/v#{version}/agentgate-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER_X86_64_DARWIN_SHA256"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/iamdainwi/agentgate/releases/download/v#{version}/agentgate-v#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "PLACEHOLDER_AARCH64_LINUX_SHA256"
    else
      url "https://github.com/iamdainwi/agentgate/releases/download/v#{version}/agentgate-v#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "PLACEHOLDER_X86_64_LINUX_SHA256"
    end
  end

  def install
    bin.install "agentgate"
    man1.install "agentgate.1" if File.exist?("agentgate.1")
  end

  def post_install
    (var/"agentgate").mkpath
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/agentgate --version")
    system bin/"agentgate", "doctor"
  end
end
