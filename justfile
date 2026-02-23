# Perigee — build recipes
# Requires `nix develop` shell on Linux for static musl builds.

set dotenv-load := false

version := `grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'`

# ── Dev workflow ──

default: check

# Type-check the whole workspace
check:
    cargo check --workspace

# Run all tests
test:
    cargo test --workspace

# Run clippy lints
lint:
    cargo clippy --workspace -- -D warnings

# Format code
fmt:
    cargo fmt --all

# Format check (CI)
fmt-check:
    cargo fmt --all -- --check

# Dev build (native, debug)
dev:
    cargo build

# ── Static release builds (Linux musl, requires nix develop on Linux) ──

# Build static musl binary: just build [x86_64 | aarch64]
build arch="x86_64":
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET="{{ if arch == "aarch64" { "aarch64-unknown-linux-musl" } else { "x86_64-unknown-linux-musl" } }}"
    echo "Building perigee v{{version}} → ${TARGET} (static release)..."
    cargo build --target "${TARGET}" --release
    BIN="target/${TARGET}/release/perigee"
    if [ -f "${BIN}" ]; then
        echo "  ${BIN}  ($(ls -lh "${BIN}" | awk '{print $5}'))"
    fi

# Build for all supported architectures
build-all:
    just build x86_64
    just build aarch64

# ── Packaging ──

# Package release binary into tar.gz
package arch="x86_64":
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET="{{ if arch == "aarch64" { "aarch64-unknown-linux-musl" } else { "x86_64-unknown-linux-musl" } }}"
    BIN="target/${TARGET}/release/perigee"
    if [ ! -f "${BIN}" ]; then
        echo "Binary not found. Building first..."
        just build "{{arch}}"
    fi
    mkdir -p dist
    ARCHIVE="perigee-{{version}}-linux-{{arch}}-musl.tar.gz"
    cp "${BIN}" dist/perigee
    tar -czf "dist/${ARCHIVE}" -C dist perigee
    rm -f dist/perigee
    echo "Package: dist/${ARCHIVE}  ($(ls -lh "dist/${ARCHIVE}" | awk '{print $5}'))"

# Package all architectures
package-all:
    just package x86_64
    just package aarch64

# ── Nix build (works from any host, including macOS) ──

# Build via nix for a target system: just nix-build [x86_64 | aarch64]
nix-build arch="x86_64":
    #!/usr/bin/env bash
    set -euo pipefail
    SYSTEM="{{ if arch == "aarch64" { "aarch64-linux" } else { "x86_64-linux" } }}"
    echo "Building perigee v{{version}} via nix → ${SYSTEM}..."
    nix build ".#packages.${SYSTEM}.default" -o "result-${SYSTEM}"
    if [ -L "result-${SYSTEM}" ]; then
        BIN="result-${SYSTEM}/bin/perigee"
        echo "  ${BIN}  ($(ls -lh "${BIN}" | awk '{print $5}'))"
    fi

# Nix build for all architectures
nix-build-all:
    just nix-build x86_64
    just nix-build aarch64

# ── Utility ──

# Clean all build artifacts
clean:
    cargo clean
    rm -rf dist/

# Show version info
info:
    @echo "Perigee v{{version}}"
    @rustc --version
    @cargo --version
