```
  ██████  ███████ ██████  ██  ██████  ███████ ███████
  ██   ██ ██      ██   ██ ██ ██       ██      ██
  ██████  █████   ██████  ██ ██   ███ █████   █████
  ██      ██      ██   ██ ██ ██    ██ ██      ██
  ██      ███████ ██   ██ ██  ██████  ███████ ███████
```

Proxmox VE helper toolkit - multi-module TUI for advanced configuration management.

---

## Common Features

- Interactive TUI for each module
- CLI interface for scripting and automation
- Daemon for background monitoring and self-healing
- Single static binary, zero runtime dependencies
- 
## Modules

**SR-IOV** - Virtual function provisioning and management

- Automatic VF provisioning with vendor-aware drivers (Intel / Mellanox)
- Deterministic MAC assignment strategies (sequential, hash-based, OUI-prefix)
- FDB bridge forwarding database management
- Runtime drift detection and self-healing

**Affinity** - CPU topology detection and core pinning

- Hardware topology detection (NUMA, L3 cache, SMT)
- Multi-strategy affinity generation (balanced, packed, spread)
- Automatically apply optimal CPU bindings (e.g., AMD EPYC) to all VMs
- Conflict-free allocation across existing VMs

## Installation

Releases ship a single static binary per architecture (no archive). Download
the one matching your CPU and make it executable:

```bash
ARCH=$(uname -m)   # x86_64 or aarch64
VERSION=$(curl -fsSL https://api.github.com/repos/channinghe/perigee/releases/latest \
  | grep -oP '"tag_name":\s*"\K[^"]+')
curl -fL -o perigee \
  "https://github.com/channinghe/perigee/releases/download/${VERSION}/perigee-${VERSION#v}-linux-${ARCH}-musl"
chmod +x perigee
```

Run the TUI directly:
```bash
./perigee
```

To install it system-wide and set up the background daemon (systemd):
```bash
sudo ./perigee install
```

Once installed, upgrade in place to the latest release at any time:
```bash
sudo perigee update
```

Or build from source (see [Dev.md](./Dev.md) below).

## Usage

```bash
# Launch main TUI menu
perigee

# SR-IOV module
perigee sriov              # Interactive TUI
perigee sriov list         # List profiles
perigee sriov show <name>  # Show profile details

# Affinity module
perigee affinity           # Interactive TUI
perigee affinity topology  # Show CPU topology
perigee affinity generate <cores>  # Generate affinity string

# Daemon management
perigee status             # Query status
perigee reload             # Reload configs

# Lifecycle
sudo perigee install       # Install binary + systemd service
sudo perigee update        # Upgrade to the latest GitHub release
sudo perigee uninstall     # Remove service and binary
```

## Project structure

```
crates/
  perigee-cli/       CLI + TUI entry point
  perigee-core/      System interfaces (PCI, sysfs, IPC, MAC)
  perigee-daemon/    Systemd daemon, scheduler, IPC server
  perigee-tui/       TUI components (ratatui)
  modules/
    perigee-sriov/   SR-IOV module
    perigee-affinity/ CPU affinity module
```

## License

MIT
