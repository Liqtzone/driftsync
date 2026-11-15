class Driftsync < Formula
  desc "Multi-repo configuration drift classifier and 3-way structural sync CLI"
  homepage "https://github.com/Liqtzone/driftsync"
  version "0.1.0"
  license "Elastic-2.0"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Liqtzone/driftsync/releases/download/v#{version}/driftsync-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_SHA256_AARCH64_DARWIN"
    else
      url "https://github.com/Liqtzone/driftsync/releases/download/v#{version}/driftsync-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_SHA256_X86_64_DARWIN"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Liqtzone/driftsync/releases/download/v#{version}/driftsync-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_SHA256_AARCH64_LINUX"
    else
      url "https://github.com/Liqtzone/driftsync/releases/download/v#{version}/driftsync-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_RELEASE_SHA256_X86_64_LINUX"
    end
  end

  def install
    bin.install "driftsync"
  end

  test do
    assert_match "driftsync", shell_output("#{bin}/driftsync --version")
  end
end
