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

        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
          targets = pkgs.lib.optionals pkgs.stdenv.isLinux [
            "x86_64-unknown-linux-musl"
            "aarch64-unknown-linux-musl"
          ];
        };

        darwinDeps = pkgs.lib.optionals pkgs.stdenv.isDarwin [
          pkgs.darwin.libiconv
          pkgs.apple-sdk
        ];

        # Linux-only: musl cross-compilation toolchains
        x86MuslCC = pkgs.pkgsCross.musl64.stdenv.cc;
        aarch64MuslCC = pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc;

        linuxBuildDeps = pkgs.lib.optionals pkgs.stdenv.isLinux [
          x86MuslCC
          aarch64MuslCC
        ];
      in
      {
        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [
            rustToolchain
            pkgs.just
            pkgs.pkg-config
            pkgs.git
          ] ++ linuxBuildDeps;
          buildInputs = darwinDeps;

          env = {
            RUST_BACKTRACE = "1";
          } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
            CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${x86MuslCC}/bin/cc";
            CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = "${aarch64MuslCC}/bin/cc";
          };

          shellHook = ''
            echo "perigee dev shell — $(rustc --version)"
          '';
        };

        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "perigee";
          version = cargoToml.workspace.package.version;
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
