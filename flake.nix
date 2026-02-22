{
  description = "Perigee - Proxmox VE helper CLI tool (SR-IOV & more)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = [
            "x86_64-unknown-linux-musl"
            "aarch64-unknown-linux-musl"
          ];
        };

        darwinDeps = pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.darwin.libiconv
          pkgs.apple-sdk
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [
            rustToolchain
            pkgs.just
            pkgs.pkg-config
            pkgs.zig
            pkgs.cargo-zigbuild
          ];
          buildInputs = darwinDeps;

          env = {
            RUST_BACKTRACE = "1";
          };

          shellHook = ''
            echo "perigee dev shell — $(rustc --version)"
          '';
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "perigee";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = darwinDeps;

          meta = {
            description = "Proxmox VE helper tool - SR-IOV configuration & more";
            license = pkgs.lib.licenses.mit;
            mainProgram = "perigee";
          };
        };
      });
}
