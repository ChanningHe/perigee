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

Download the latest release binary for your architecture:

```bash
curl -L -o perigee.tar.gz \
  https://github.com/channinghe/perigee/releases/latest/download/perigee-${VERSION}-linux-${ARCH}$-musl.tar.gz
tar xzf perigee.tar.gz && chmod +x ./perigee
```

Simply enter the TUI:
```bash
./perigee
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
