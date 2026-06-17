{ pkgs, lib, config, inputs, ... }:

let
  isLinux = pkgs.stdenv.isLinux;
in
{
  # ── Environment ──
  # On Linux: wire musl linkers directly via Nix store paths (mirrors flake.nix)
  env = {
    RUST_LOG       = "info";
    RUST_BACKTRACE = "1";
  } // lib.optionalAttrs isLinux {
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER  = "${pkgs.pkgsCross.musl64.stdenv.cc}/bin/cc";
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER = "${pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc}/bin/cc";
  };

  # ── Languages ──
  languages.rust = {
    enable  = true;
    channel = "stable";   # requires rust-overlay in devenv.yaml
    # Rust cross-compilation targets (toolchain side)
    targets = lib.optionals isLinux [
      "x86_64-unknown-linux-musl"
      "aarch64-unknown-linux-musl"
    ];
  };

  # ── Packages ──
  packages = with pkgs; [
    just        # Task runner
    pkg-config  # C library linking
  ] ++ lib.optionals isLinux [
    # musl C linkers for static cross-compilation (Linux host only)
    pkgs.pkgsCross.musl64.stdenv.cc
    pkgs.pkgsCross.aarch64-multiplatform-musl.stdenv.cc
  ];

  # ── Git hooks (pre-commit) ──
  git-hooks.hooks = {
    rustfmt.enable = true;
    clippy = {
      enable = true;
      settings.denyWarnings = true;
    };
  };

  # ── Shell greeting ──
  enterShell = ''
    echo ""
    echo "  Perigee — Proxmox VE helper toolkit"
    echo "  $(rustc --version)  |  $(cargo --version)"
    echo ""
    echo "  just check      type-check workspace"
    echo "  just test       run all tests"
    echo "  just lint       clippy -D warnings"
    echo "  just fmt        format code"
    echo "  just dev        debug build (native)"
    ${lib.optionalString isLinux ''
    echo "  just build      static musl release (x86_64)"
    echo "  just build-all  static musl release (x86_64 + aarch64)"
    ''}
    ${lib.optionalString (!isLinux) ''
    echo "  (cross-compile) just build → use Docker: compose.dev.yaml"
    ''}
    echo ""

  '';

  # ── Tests ──
  enterTest = ''
    cargo test --workspace
  '';
}
