{
  description = "Nexum runtime – WASM Component Model host for web3 modules";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        inherit (pkgs) lib stdenv;

        # Pinned to match CI exactly (.github/workflows/ci.yml uses "1.94").
        # Develop with `nix develop` so the local toolchain matches CI/CD.
        rustToolchain = pkgs.rust-bin.stable."1.94.0".default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [
            # WASM host/guest targets — the runtime's primary output.
            "wasm32-wasip2"
            "wasm32-unknown-unknown"
            # Native desktop/server targets we build and ship on.
            "x86_64-unknown-linux-gnu"
            "aarch64-unknown-linux-gnu"
            "aarch64-apple-darwin"
            "x86_64-apple-darwin"
            # Mobile targets — the client library is consumed on Android/iOS.
            "aarch64-linux-android"
            "x86_64-linux-android"
            "aarch64-apple-ios"
            "aarch64-apple-ios-sim"
          ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustToolchain
            cargo-nextest
            wasm-tools
            wabt
            just
            # Search tooling the agent hooks and CI gate scripts use:
            # ripgrep for plain-text search, ast-grep for syntax-aware queries.
            ripgrep
            ast-grep
            pkg-config
            openssl
          ] ++ lib.optionals stdenv.isLinux [ mold ];

          # Link native Linux targets with mold — far faster than bfd/gold on
          # the iterate-compile-link loop. Scoped per-target so it never touches
          # wasm or cross linking, and set as env (not a committed .cargo config)
          # so CI, which has no mold installed, keeps using its default linker.
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS =
            lib.optionalString stdenv.isLinux "-Clink-arg=-fuse-ld=mold";
          CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS =
            lib.optionalString stdenv.isLinux "-Clink-arg=-fuse-ld=mold";

          shellHook = ''
            # Opt into sccache only when the host provides it. sccache is a
            # per-user tool (server, cache and binary owned in ~/.config/nixos),
            # NOT bundled here: a client must match its server's version exactly,
            # and a second copy pinned by this flake would mismatch the host
            # server and fight it for the socket. So use the host's sccache when
            # present, and fall back to a plain build (with incremental) when not.
            if command -v sccache >/dev/null; then
              export RUSTC_WRAPPER=sccache
              export CARGO_INCREMENTAL=0 # sccache and incremental are exclusive
            fi
            echo "nexum dev shell — $(rustc --version)"
            command -v sccache >/dev/null && echo "  compiler cache: $(sccache --version)"
            echo "  test runner: $(cargo nextest --version | head -n1)"
            ${lib.optionalString stdenv.isLinux ''command -v mold >/dev/null && echo "  linker (native): mold $(mold --version | head -n1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -n1)"''}
          '';
        };
      }
    );
}
