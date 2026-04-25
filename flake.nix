{
  description = "Nexo — multi-agent Rust framework with NATS, MCP, and channel plugins";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Match the workspace MSRV declared in `[workspace.package].rust-version`.
        # Bump in lockstep when the MSRV changes.
        rustToolchain = pkgs.rust-bin.stable."1.80.0".default.override {
          extensions = [ "rust-src" "clippy" "rustfmt" ];
        };

        # Native deps the build needs at link time. Runtime deps (chrome,
        # cloudflared, ffmpeg, tesseract) are documented in
        # docs/src/getting-started/install-native.md — Nix users wire
        # those at the system level, not via the flake.
        commonBuildInputs = with pkgs; [
          openssl
          sqlite
          pkg-config
        ];

        # Cargo invokes `git2` and `notify` which need libgit2/inotify
        # at compile time on Linux but not on Darwin.
        linuxOnlyInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [
          pkgs.libgit2
        ];

        nexo-rs = pkgs.rustPlatform.buildRustPackage {
          pname = "nexo-rs";
          version = "0.1.1";
          src = pkgs.lib.cleanSource ./.;

          cargoLock = {
            lockFile = ./Cargo.lock;
          };

          nativeBuildInputs = [ rustToolchain pkgs.pkg-config ];
          buildInputs = commonBuildInputs ++ linuxOnlyInputs;

          # Tests hit the network (wiremock, NATS, sqlx) and assume a
          # writable HOME — neither is available in the Nix build sandbox.
          # CI runs the full suite separately; here we only verify the
          # binary builds.
          doCheck = false;

          # Build the renamed `nexo` bin (legacy `agent` was retired in
          # commit 4bccdc3).
          cargoBuildFlags = [ "--bin" "nexo" ];

          meta = with pkgs.lib; {
            description = "Multi-agent Rust framework with NATS event bus, pluggable LLM providers, and channel plugins";
            homepage = "https://lordmacu.github.io/nexo-rs/";
            license = with licenses; [ mit asl20 ];
            maintainers = [ ];
            mainProgram = "nexo";
            platforms = platforms.unix;
          };
        };
      in
      {
        # `nix build` → ./result/bin/nexo built from the current tree.
        packages = {
          default = nexo-rs;
          nexo-rs = nexo-rs;
        };

        # `nix run github:lordmacu/nexo-rs -- --help`
        apps = {
          default = {
            type = "app";
            program = "${nexo-rs}/bin/nexo";
          };
          nexo-rs = self.apps.${system}.default;
        };

        # `nix develop` → contributor shell with the toolchain pinned.
        devShells.default = pkgs.mkShell {
          name = "nexo-rs-dev";
          buildInputs = commonBuildInputs ++ linuxOnlyInputs ++ (with pkgs; [
            rustToolchain
            cargo-edit
            cargo-watch
            cargo-nextest
            cargo-deny
            mdbook
            mdbook-mermaid
            git
            sqlite
          ]);
          shellHook = ''
            echo "nexo-rs dev shell — rustc $(rustc --version | cut -d' ' -f2)"
            export RUST_LOG=info
          '';
        };

        # Formatter `nix fmt`
        formatter = pkgs.nixpkgs-fmt;
      });
}
