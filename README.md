```
  ██████  ███████ ██████  ██  ██████  ███████ ███████
  ██   ██ ██      ██   ██ ██ ██       ██      ██
  ██████  █████   ██████  ██ ██   ███ █████   █████
  ██      ██      ██   ██ ██ ██    ██ ██      ██
  ██      ███████ ██   ██ ██  ██████  ███████ ███████
```

Proxmox VE helper toolkit -- SR-IOV configuration & more.

Perigee runs as a systemd daemon on Proxmox VE hosts, automating SR-IOV virtual function provisioning, MAC assignment, FDB bridge forwarding, and runtime drift detection. It ships as a single static binary with a built-in TUI for interactive setup.

## Features

- Automatic SR-IOV VF provisioning with vendor-aware drivers (Intel / Mellanox)
- Deterministic MAC address assignment strategies (sequential, hash-based, OUI-prefix)
- FDB bridge forwarding database management
- Runtime drift detection and self-healing via daemon scheduler
- Interactive TUI for profile creation and status monitoring
- CLI interface for scripting and automation
- Single static binary, zero runtime dependencies

## Installation

Download the latest release binary for your architecture:

```bash
# x86_64
curl -L -o perigee.tar.gz \
  https://github.com/OWNER/perigee/releases/latest/download/perigee-VERSION-linux-x86_64-musl.tar.gz
tar xzf perigee.tar.gz

# Install as systemd service
sudo ./perigee install
```

Or build from source (see Development below).

## Usage

```bash
# Launch interactive TUI
perigee

# SR-IOV management
perigee sriov              # Interactive TUI for SR-IOV setup
perigee sriov list         # List all profiles
perigee sriov show <name>  # Show profile details
perigee sriov add <name>   # Add a new profile
perigee sriov remove <name>

# Daemon management
perigee status             # Query daemon status
perigee reload             # Reload configuration
perigee install            # Install systemd service
perigee uninstall          # Remove systemd service
```

## Development

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

### Project structure

```
crates/
  perigee-cli/       CLI + TUI entry point
  perigee-core/      System interfaces (PCI, sysfs, IPC, MAC)
  perigee-daemon/    Systemd daemon, scheduler, IPC server
  perigee-tui/       TUI components (ratatui)
  modules/
    perigee-sriov/   SR-IOV module (detection, config, vendor drivers)
```

## License

MIT
