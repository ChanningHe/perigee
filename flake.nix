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
    let
      cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
      version = cargoToml.workspace.package.version;

      # Build a static musl binary using pkgsCross.
      # pkgsCross handles compiler + linker + libc automatically:
      #   buildPlatform = the machine doing the compilation
      #   hostPlatform  = the target (e.g. x86_64-unknown-linux-musl)
      mkPerigee = pkgs: pkgs.rustPlatform.buildRustPackage {
        pname = "perigee";
        inherit version;
        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;
        nativeBuildInputs = [ pkgs.pkg-config ];
        preBuild = ''
          export RUSTFLAGS="-C target-feature=+crt-static -C link-arg=-static"
        '';
        meta = {
          description = "Proxmox VE helper tool - SR-IOV configuration & more";
          license = pkgs.lib.licenses.mit;
          mainProgram = "perigee";
        };
      };
    in

    # ── DevShells (all platforms including Darwin) ──
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

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
      }
    )

    //

    # ── Packages (Linux only, static musl binaries) ──
    #
    # packages.x86_64-linux
    #   ├── default        → perigee-x86_64
    #   ├── perigee-x86_64    (native x86_64-musl)
    #   └── perigee-aarch64   (cross-compiled aarch64-musl)
    #
    # packages.aarch64-linux
    #   ├── default        → perigee-aarch64
    #   └── perigee-aarch64   (native aarch64-musl)
    {
      packages.x86_64-linux = let
        pkgs = import nixpkgs { system = "x86_64-linux"; };
        perigee-x86_64  = mkPerigee pkgs.pkgsCross.musl64;
        perigee-aarch64 = mkPerigee pkgs.pkgsCross.aarch64-multiplatform-musl;
      in {
        inherit perigee-x86_64 perigee-aarch64;
        default = perigee-x86_64;
      };

      packages.aarch64-linux = let
        pkgs = import nixpkgs { system = "aarch64-linux"; };
        perigee-aarch64 = mkPerigee pkgs.pkgsCross.aarch64-multiplatform-musl;
      in {
        inherit perigee-aarch64;
        default = perigee-aarch64;
      };
    };
}
