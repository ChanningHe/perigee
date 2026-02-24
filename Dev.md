
# Development

### Prerequisites

- [Nix](https://nixos.org/) with flakes enabled (recommended)
- Or: Rust stable toolchain + `just`

### Quick start

```bash
# Enter dev shell (installs Rust, just, and build tools automatically)
nix develop

# Common commands
just check       # Type-check workspace
just test        # Run tests
just lint        # Clippy lints
just fmt         # Format code
just dev         # Debug build
```

### Building static Linux binaries

On a Linux machine inside `nix develop`:

```bash
just build x86_64          # Static musl binary (x86_64)
just build aarch64         # Static musl binary (aarch64, cross-compiled)
just build-all             # Both architectures
just package x86_64        # Package as tar.gz
```

On macOS via Docker:

```bash
docker compose -f compose.dev.yaml run build-amd64   # x86_64 binary
docker compose -f compose.dev.yaml run build-arm64   # aarch64 binary
```

On macOS via Nix remote builder:

```bash
nix build --builders 'ssh://linux-host x86_64-linux'
```
