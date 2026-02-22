# Perigee — build recipes
# Works inside `nix develop` shell or standalone (auto-detects nix env).

set dotenv-load := false

version := `grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/'`
default_target := "x86_64-unknown-linux-musl"

# Resolve arch shorthand → full Rust triple
[private]
resolve-target arch:
    #!/usr/bin/env bash
    case "{{arch}}" in
        x86_64|x86_64-unknown-linux-musl)   echo "x86_64-unknown-linux-musl" ;;
        aarch64|aarch64-unknown-linux-musl)  echo "aarch64-unknown-linux-musl" ;;
        *)                                   echo "{{arch}}" ;;
    esac

# Resolve arch → short name for filenames
[private]
resolve-short arch:
    #!/usr/bin/env bash
    case "{{arch}}" in
        x86_64|x86_64-unknown-linux-musl)   echo "x86_64" ;;
        aarch64|aarch64-unknown-linux-musl)  echo "aarch64" ;;
        *)                                   echo "{{arch}}" ;;
    esac

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

# ── Static release builds (cross-compile via cargo-zigbuild) ──

# Ensure nix dev environment PATH is available
[private]
ensure-nix-env:
    #!/usr/bin/env bash
    if ! command -v cargo-zigbuild &>/dev/null; then
        echo "cargo-zigbuild not found in PATH."
        echo "Run inside 'nix develop' shell, or use 'just nix-build <arch>' instead."
        exit 1
    fi

# Build static musl binary: just build [x86_64 | aarch64]
# Must be run inside `nix develop` shell.
build arch=default_target: ensure-nix-env
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET=$(just resolve-target "{{arch}}")

    echo "Building perigee v{{version}} → ${TARGET} (static release, zigbuild)..."
    cargo zigbuild --target "${TARGET}" --release

    BIN="target/${TARGET}/release/perigee"
    if [ -f "${BIN}" ]; then
        SIZE=$(ls -lh "${BIN}" | awk '{print $5}')
        echo "  ${BIN}  (${SIZE})"
    fi
    echo "Done."

# Build outside nix shell — auto-loads nix dev env via nix print-dev-env.
nix-build arch=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    NIXPATH=$(nix print-dev-env "$(pwd)" 2>/dev/null | grep "^PATH='/nix" | head -1 | sed "s/^PATH='//" | sed "s/'$//")
    export PATH="$NIXPATH:/usr/bin:/bin:/usr/sbin:/sbin"
    TARGET=$(just resolve-target "{{arch}}")
    echo "Building perigee v{{version}} → ${TARGET} (nix-build)..."
    cargo zigbuild --target "${TARGET}" --release
    BIN="target/${TARGET}/release/perigee"
    if [ -f "${BIN}" ]; then
        SIZE=$(ls -lh "${BIN}" | awk '{print $5}')
        echo "  ${BIN}  (${SIZE})"
    fi
    echo "Done."

# Build static debug binary (inside nix develop shell)
build-debug arch=default_target: ensure-nix-env
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET=$(just resolve-target "{{arch}}")

    echo "Building perigee v{{version}} → ${TARGET} (static debug, zigbuild)..."
    cargo zigbuild --target "${TARGET}"

    echo "Done. Binary: target/${TARGET}/debug/perigee"

# Build for all supported architectures
build-all:
    just build x86_64
    just build aarch64

# ── Packaging ──

# Package release binary into tar.gz
package arch=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET=$(just resolve-target "{{arch}}")
    ARCH_SHORT=$(just resolve-short "{{arch}}")

    BIN="target/${TARGET}/release/perigee"
    if [ ! -f "${BIN}" ]; then
        echo "Binary not found. Building first..."
        just build "{{arch}}"
    fi

    OUT_DIR="dist"
    ARCHIVE="perigee-{{version}}-linux-${ARCH_SHORT}-musl.tar.gz"
    mkdir -p "${OUT_DIR}"

    cp "${BIN}" "${OUT_DIR}/perigee"
    tar -czf "${OUT_DIR}/${ARCHIVE}" -C "${OUT_DIR}" perigee
    rm -f "${OUT_DIR}/perigee"

    SIZE=$(ls -lh "${OUT_DIR}/${ARCHIVE}" | awk '{print $5}')
    echo "Package: ${OUT_DIR}/${ARCHIVE}  (${SIZE})"

# Package all architectures
package-all:
    just package x86_64
    just package aarch64

# ── Verification ──

# Verify binary is statically linked
verify arch=default_target:
    #!/usr/bin/env bash
    set -euo pipefail
    TARGET=$(just resolve-target "{{arch}}")

    BIN="target/${TARGET}/release/perigee"
    if [ ! -f "${BIN}" ]; then
        echo "Binary not found: ${BIN}"
        echo "Run: just build {{arch}}"
        exit 1
    fi

    echo "File:"
    file "${BIN}"

    if command -v ldd &>/dev/null; then
        echo ""
        echo "Dynamic deps (expect 'not a dynamic executable'):"
        ldd "${BIN}" 2>&1 || true
    fi

    SIZE=$(ls -lh "${BIN}" | awk '{print $5}')
    echo ""
    echo "Size: ${SIZE}"

# ── Utility ──

# Clean all build artifacts
clean:
    cargo clean
    rm -rf dist/

# Show workspace & toolchain info
info:
    #!/usr/bin/env bash
    echo "Perigee v{{version}}"
    echo ""
    echo "Toolchain:"
    rustc --version
    cargo --version
    cargo-zigbuild --version
    zig version
    echo ""
    echo "Workspace members:"
    cargo metadata --no-deps --format-version 1 2>/dev/null \
        | python3 -c "import sys,json; [print(f'  {p[\"name\"]} v{p[\"version\"]}') for p in json.load(sys.stdin)['packages']]" 2>/dev/null \
        || echo "  (cargo metadata unavailable)"
