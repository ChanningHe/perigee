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
- FDB bridge forwarding database management: installs guest NIC MACs and host
  bridge/VLAN MACs into the PF's FDB so VFs can reach VMs, containers and the
  host behind a Linux bridge on the PF. Not covered: hosts reached through a
  *different* uplink of the same bridge (e.g. `bridge-ports eno1 enp1s0f0`).
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

## SR-IOV configuration (`/etc/perigee/sriov.toml`)

The TUI writes this file; it can also be edited by hand. One table per profile,
keyed by a profile name:

```toml
[sriov.enp65s0np0]
mac = "8c:2a:8e:88:9a:f6"     # PF MAC (stable PF identifier)
num_vfs = 64
mac_strategy = "sequential"   # sequential | random | custom

[sriov.enp65s0np0.defaults]   # applied to every VF
trust = true                  # default true
spoofchk = false              # default false
link_state = "enable"         # auto | enable | disable; absent = unmanaged
# vlan = { id = 100, qos = 0 }

[[sriov.enp65s0np0.vf]]       # per-VF override; unset keys use defaults
index = 20
trust = false
link_state = "auto"
vlan = { id = 200 }

[sriov.enp65s0np0.fdb]
mode = "daemon_watch"         # daemon_watch | hookscript | disabled

[sriov.enp65s0np0.roce]       # optional, see below
enabled = true
```

### VF link state

`link_state` maps to `ip link set <pf> vf <n> state <value>`. It is optional;
without it (in `defaults` and in the VF override) perigee does not manage the
VF's link state at all: it never issues `state`, never resets a value set by
hand, and the status view does not compare it. Only an explicit value is
enforced:

- `auto` - the VF's link follows the PF uplink (the kernel default). When the uplink
  drops (switch port change, cable, a PFC change that flaps the port), every
  VF loses link too, including traffic that never leaves the NIC
  (PF<->VF, VF<->VF), e.g. RDMA storage served from the host.
- `enable` - the VF reports link up regardless of the uplink, so NIC-internal
  traffic keeps flowing while the uplink is down.
- `disable` - the VF's link is held down.

perigee reads the current state first and only issues `state` for managed
VFs whose state differs. A config without `link_state` behaves exactly as
before. The TUI cycles the value unmanaged -> auto -> enable -> disable and
only writes `link_state` to the file when one is chosen.

### PF RoCE / lossless Ethernet (`roce`, opt-in, mlx5 only)

Absent section (the default) or `enabled = false`: perigee never touches the
PF's DCB, pause or RoCE settings. With `enabled = true`, each apply configures
the PF equivalently to:

```bash
dcb app add dev <pf> dscp-prio 26:3 48:6     # DSCP trust; RoCE 26 -> prio 3, CNP 48 -> prio 6
ethtool -A <pf> rx off tx off                # 802.3x global pause off (never combined with PFC)
dcb pfc set dev <pf> prio-pfc all:off 3:on   # PFC on the RoCE priority only
ethtool --set-tunable <pf> pfc-prevention-tout 100   # PFC storm prevention
echo 106 > /sys/kernel/config/rdma_cm/<ibdev>/ports/1/default_roce_tos
```

```toml
[sriov.enp65s0np0.roce]
enabled = true
dscp = 26                   # RoCE data DSCP (0-63)
priority = 3                # priority for RoCE data, the only one with PFC (0-7)
cnp_dscp = 48               # congestion notification DSCP (0-63, != dscp)
cnp_priority = 6            # (0-7)
pfc_storm_timeout_ms = 100  # 0 disables, else 80-8000
```

Only `enabled` is required; the other keys default to the values shown. The
host's RDMA CM default ToS is derived from `dscp` (`dscp << 2 | ECT(0)`, so
26 -> 106). `<ibdev>` is the PF's RDMA device (e.g. `mlx5_0`); when the ToS
has to be written and the rdma_cm configfs tree is missing, perigee runs
`modprobe rdma_cm` first.

Values inside the type range but outside the allowed range (e.g.
`pfc_storm_timeout_ms = 50`, `cnp_dscp` equal to `dscp`) make the RoCE step
fail with a message; VFs are still created. Values that do not fit the field
type at all (e.g. `dscp = 300`, a negative number, an unknown `link_state`
string) fail TOML parsing of the whole file, like a malformed `vlan` does
today, and nothing is applied.

Behaviour:

- **Order**: at boot this runs before `sriov_numvfs` is written, so the PF
  link flap caused by the PFC change happens before any VF or VM exists.
- **Idempotent**: perigee reads the current state (`dcb -j app/pfc show`,
  `ethtool -a`, `ethtool --get-tunable`, the configfs file) and writes only
  what differs. A setting it cannot read is left alone and reported. DSCP
  mappings perigee does not manage are left in place.
- **PFC only without VFs**: on mlx5, changing PFC toggles the PF link once.
  When VFs already exist (e.g. TUI "Save & Apply" on a live host), or the VF
  count cannot be read, perigee does **not** change PFC and does not turn
  global pause off either (the port is never left with neither); the profile
  is reported degraded with a message, and both take effect on the next boot.
  VFs are never recreated for this. The DSCP map, storm timeout and ToS are
  still applied live. Adding the first DSCP mapping switches the port to DSCP
  trust, which may briefly reset the PF's queues; perigee records a warning
  when that happens with VFs present.
- **No flow control gap**: if turning pause off fails, PFC is not touched.
  If PFC fails after pause was turned off, perigee restores the original
  pause setting; if that also fails, the error says the port currently has
  no flow control.
- **Failures never block VFs**: an invalid `roce` section, a missing
  `dcb`/`ethtool`, a non-mlx5 PF, no RDMA device or no rdma_cm configfs is
  reported and the profile is marked degraded; VFs are still created and
  configured.
- **Boot timing limitation**: at boot the RDMA device (`/sys/class/net/<pf>/
  device/infiniband/`) may not exist yet when perigee runs. The ToS step then
  fails (degraded, with the underlying error) and is **not retried
  automatically**; re-apply the profile later (TUI "Save & Apply").
- **Turning it off does not undo it**: `enabled = false` or removing the
  section leaves the PF as it is. Revert manually (`dcb app del ...`,
  `dcb pfc set dev <pf> prio-pfc all:off`, `ethtool -A ...`) if needed.

This covers the host PF only:

- **Each guest doing RoCE must set its own `default_roce_tos`** (inside the
  VM, under `/sys/kernel/config/rdma_cm/<ibdev>/ports/1/`) or use a ToS in its
  application. The host setting does not apply to VFs passed into guests.
- **The switch must be configured to match**: DSCP trust, the same
  DSCP -> priority mapping, PFC on the same priority, and ECN marking.
  Traffic switched inside the NIC (PF<->VF, VF<->VF) does not depend on it.

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
