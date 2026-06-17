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

# ── Nix build ──

# Build static binary via nix: just nix-build [x86_64 | aarch64]
# Auto-detects build system: native Linux uses local, macOS delegates to linux remote builder.
nix-build arch="x86_64":
    #!/usr/bin/env bash
    set -euo pipefail
    HOST_ARCH=$(uname -m)
    [ "$HOST_ARCH" = "arm64" ] && HOST_ARCH="aarch64"

    if [ "$(uname -s)" = "Linux" ] && [ "$HOST_ARCH" = "{{arch}}" ]; then
        SYSTEM="${HOST_ARCH}-linux"
    else
        SYSTEM="x86_64-linux"
    fi

    echo "Building perigee v{{version}} via nix → perigee-{{arch}} (${SYSTEM})..."
    nix build ".#packages.${SYSTEM}.perigee-{{arch}}" -o "result-{{arch}}"
    if [ -L "result-{{arch}}" ]; then
        BIN="result-{{arch}}/bin/perigee"
        echo "  ${BIN}  ($(ls -lh "${BIN}" | awk '{print $5}'))"
    fi

# Nix build for all architectures (both via x86_64-linux)
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

# ── Release ──

# Bump version, commit, and tag: just release 0.2.0
# Cargo.toml stays the single source of truth; the tag is derived from it.
release new_version:
    #!/usr/bin/env bash
    set -euo pipefail
    NEW="{{new_version}}"
    if ! printf '%s' "$NEW" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
        echo "error: version must be X.Y.Z (got '$NEW')" >&2
        exit 1
    fi
    if [ -n "$(git status --porcelain)" ]; then
        echo "error: working tree is dirty; commit or stash first" >&2
        exit 1
    fi
    TAG="v${NEW}"
    if git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null; then
        echo "error: tag ${TAG} already exists" >&2
        exit 1
    fi
    CUR=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
    echo "Bumping ${CUR} → ${NEW}"
    # Only [workspace.package] starts a line with `version = `; crate members use
    # `version.workspace = true`, so this single substitution is unambiguous.
    sed -i.bak 's/^version = ".*"/version = "'"${NEW}"'"/' Cargo.toml
    rm -f Cargo.toml.bak
    # Sync the workspace crate versions in the lockfile (no network needed).
    cargo update --workspace --offline
    git add Cargo.toml Cargo.lock
    git commit -m "chore(release): ${TAG}"
    git tag -a "${TAG}" -m "Release ${TAG}"
    echo ""
    echo "Tagged ${TAG} on $(git rev-parse --short HEAD). Push to trigger the release:"
    echo "  git push origin HEAD ${TAG}"
