# typed: false
# frozen_string_literal: true

# Homebrew formula for nexo-rs.
#
# Lives under packaging/homebrew/ in the main repo as the source of
# truth. The release workflow (Phase 27.2) mirrors this file (with the
# `version` + `url` + `sha256` fields rewritten per release) into the
# tap repo at https://github.com/lordmacu/homebrew-nexo, where Homebrew
# actually consumes it.
#
# Install one-liner:
#   brew tap lordmacu/nexo && brew install nexo-rs
#
# Bottles (pre-built binaries for arm64_sequoia / monterey / ventura)
# land in a follow-up — for now `brew install nexo-rs` builds from source.

class NexoRs < Formula
  desc "Multi-agent Rust framework with NATS, MCP, and channel plugins"
  homepage "https://lordmacu.github.io/nexo-rs/"
  license any_of: ["MIT", "Apache-2.0"]
  head "https://github.com/lordmacu/nexo-rs.git", branch: "main"

  # Release-pinned source. The workflow rewrites these three lines on
  # every `v*` tag so `brew upgrade nexo-rs` always pulls the latest.
  url "https://github.com/lordmacu/nexo-rs/archive/refs/tags/v0.1.1.tar.gz"
  version "0.1.1"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  depends_on "rust" => :build
  depends_on "pkg-config" => :build
  depends_on "openssl@3"
  depends_on "sqlite"

  # Optional runtime tools the channel plugins shell out to. Not
  # `depends_on` because Homebrew users can opt into / out of each
  # channel; the binary runs without them.
  #
  #   brew install ffmpeg tesseract yt-dlp git cloudflare/cloudflare/cloudflared
  #
  # Chrome / Chromium is intentionally not listed — Homebrew users
  # tend to install Chrome via the official .pkg or `brew install
  # --cask google-chrome`, not via formula.

  def install
    # Build the renamed `nexo` bin (legacy `agent` was retired in
    # commit 4bccdc3).
    system "cargo", "install", *std_cargo_args(path: ".", root: bin), "--bin", "nexo"

    # Drop the docs alongside the binary so `brew info nexo-rs`
    # surfaces them.
    doc.install "README.md"
    doc.install "LICENSE-APACHE" => "copyright-apache"
    doc.install "LICENSE-MIT"    => "copyright-mit"
  end

  def caveats
    <<~EOS
      nexo-rs is installed.

      First run:
        nexo setup            # interactive config wizard
        nexo doctor           # verify dependencies + capabilities
        nexo --help           # CLI overview

      Optional runtime tools:
        brew install ffmpeg tesseract yt-dlp git
        brew install --cask google-chrome    # for the browser plugin

      Docs: https://lordmacu.github.io/nexo-rs/
    EOS
  end

  test do
    # Smoke: `nexo --version` should succeed and print the version
    # we just installed.
    assert_match version.to_s, shell_output("#{bin}/nexo --version")

    # Smoke: `nexo --help` should not error and should mention the
    # `setup` subcommand (proves the CLI surface is wired).
    output = shell_output("#{bin}/nexo --help")
    assert_match "setup", output
  end
end
