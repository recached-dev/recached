class Recached < Formula
  desc "Blazing fast, multi-core drop-in replacement for Redis"
  homepage "https://github.com/thinkgrid-labs/recached"
  version "0.1.8"
  license "MIT"

  on_macos do
    on_intel do
      url "https://github.com/thinkgrid-labs/recached/releases/download/v0.1.8/recached-macos-x86_64"
      # shasum -a 256 of the binary in target/dist/recached-macos-x86_64;
      # recompute if the release binary is rebuilt before uploading.
      sha256 "227fca7d7ff5c9511f9482863024e222c0c66f93ad40288581ca1fb32a9f20bd"
    end
    on_arm do
      url "https://github.com/thinkgrid-labs/recached/releases/download/v0.1.8/recached-macos-arm64"
      # TODO: build on Apple Silicon (or cross-compile: cargo build --release
      # --target aarch64-apple-darwin), upload, then: shasum -a 256 recached-macos-arm64
      sha256 "REPLACE_WITH_ARM64_SHA256"
    end
  end

  def install
    # Rename the downloaded binary to 'recached-server' and install it into the Homebrew bin
    binary = Hardware::CPU.arm? ? "recached-macos-arm64" : "recached-macos-x86_64"
    bin.install binary => "recached-server"
  end

  # This allows users to run `brew services start recached` to run it automatically in the background
  service do
    run opt_bin/"recached-server"
    keep_alive true
    error_log_path var/"log/recached.error.log"
    log_path var/"log/recached.log"
  end

  test do
    # Simple test to verify the binary installed correctly
    system "#{bin}/recached-server", "--version"
  end
end
