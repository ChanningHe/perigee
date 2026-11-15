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
rx_ring = 8192                # optional PF RX ring size; absent = unmanaged

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

### PF RX ring (`rx_ring`, optional)

`rx_ring = N` keeps the PF's RX ring at `N` entries (`ethtool -G <pf> rx N`),
e.g. when the default ring overflows under bursts (`rx_out_of_buffer`
growing). Without the key perigee never reads or writes the ring; a config
without it behaves exactly as before. `N` must be greater than 0: `0` is
reported by the ring step and never blocks VFs, but a value of the wrong type
(`-1`, a string) makes the whole `sriov.toml` fail to parse, like any other
typo, and then no profile is applied.

- **Idempotent**: perigee reads `ethtool -g <pf>` and writes only when the
  current RX value differs. A value above the device's pre-set maximum is
  reported with both numbers and not written; an unreadable ring is not
  written.
- **Rounding**: after a write perigee reads the ring back. Drivers may round
  (mlx5 rounds up to a power of two); if the device then reports another
  value, a warning names it and asks to set `rx_ring` to that value. perigee
  compares against the configured value, so a value the driver rounds is
  requested again on every apply and the live-apply warning repeats; whether
  the driver then actually rebuilds the queues is driver-specific.
- **Applied live, too**: changing the ring rebuilds the PF netdev's queues
  (about 1.5 s on a ConnectX-5, the netdev's carrier toggles once) but does
  not flap the physical link, and VFs and RDMA sessions are not affected. So
  it is applied at boot and on every live apply (TUI "Save & Apply",
  `perigee sriov retry`). A live change records a warning that traffic on the
  PF netdev (the host and guests bridged on it) was interrupted for about a
  second. It runs after the PF is brought up, before the RoCE step and VF
  creation.
- **Failures never block VFs**: a failed or timed-out ring step is reported
  and the profile shows as degraded; a timeout also skips the RoCE commands
  (`ethtool` is hung), and the RoCE ToS then goes pending as described below.
- **Removing the key does not undo it**: the ring stays at its last value
  until changed by hand or reboot.
- **PF only**: VF rings are configured inside each guest and are not
  inherited from the PF.

In the TUI, edit it on the General tab (`PF RX ring`): type a number, or clear
the field for unmanaged. `0` and values above the u32 range are refused; the
device maximum is checked at apply time. The review summary shows it when set,
and `perigee sriov show` prints it.

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

The TUI edits this section on the RoCE tab: `Enabled` creates it with the
defaults above (later toggles only flip `enabled`; the section is never
removed), and each numeric row is validated before it is stored.

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
  `dcb`/`ethtool`, a non-mlx5 PF or a rejected write is reported and the
  profile is marked degraded; VFs are still created and configured.
- **Command timeout**: every `dcb`, `ethtool`, `modprobe` and `ip` command in
  the apply path is killed after 10 s and reported as timed out (for a write,
  the error says the setting is now in an unknown state). After the first
  timeout the RoCE step issues no further commands, except restoring global
  pause once if the hung command was the PFC write; VFs are still created.
  In the per-VF `ip link set` loop the first timeout stops the remaining VFs
  (reported as an error). A timeout in the RX ring step also counts as the
  RoCE step's first timeout. A hung tool therefore adds at most about 32 s
  per profile to boot (30 s of timeouts plus a successful ring write) instead
  of blocking READY until systemd's 120 s start timeout. Profiles are applied
  one after another, so this adds up per PF: three PFs that all hang stay
  under `TimeoutStartSec=120`, four exceed it. Only external commands
  are covered: sysfs/configfs writes (`sriov_numvfs`, the rdma_cm configfs
  mkdir/write) have no timeout. A process stuck in the kernel that ignores
  SIGKILL is left behind.
- **`ip` hang at boot: the daemon exits and systemd retries**: when an `ip`
  timeout cuts VF configuration short during the boot apply (PF link-up, the
  VF link-state read, or any per-VF `ip link set`), the daemon logs it, sets
  a systemd status, and exits non-zero without ever reporting READY.
  `Restart=on-failure` starts it again after `RestartSec=5`, so the whole
  apply is retried roughly every 15 s or more (5 s plus the attempt, which
  includes at least one 10 s timeout) until `ip` responds again; networking
  waits for it as before. VFs created so far stay as they are, and guests
  that start meanwhile may see unconfigured VFs (driver MAC, no VLAN, default
  trust/spoofchk), the same as with v0.1.4 during a hang. A command that is
  merely slow but would succeed after more than 10 s is now killed and
  retried instead of awaited. RoCE and RX ring timeouts and ordinary command
  failures do not trigger this: they are reported (degraded/error) and boot
  continues. Live applies (TUI "Save & Apply", `perigee sriov retry`) never
  exit the daemon; they only report the profile as error/degraded.
- **ToS when the RDMA device appears late**: at boot the RDMA device
  (`/sys/class/net/<pf>/device/infiniband/`) or the rdma_cm configfs tree may
  not exist yet. That is not an error and does not delay boot: the profile
  gets a "RoCE ToS pending" warning and the daemon retries in the background
  every 5 s for up to 10 minutes from that apply, writing only if the value
  differs. On success it logs an info event (RDMA connections opened before
  then keep their old ToS until they reconnect); on timeout or a rejected
  write it logs an error and the profile shows as degraded. A `modprobe
  rdma_cm` that timed out is never run again (the kernel may still finish
  loading the module); the retry then only waits for the configfs tree.
  Re-applying the profile (boot, TUI "Save &
  Apply", `perigee sriov retry`) replaces the retry; a reload stops it only
  when the profile is removed or its PF or `[roce]` section changed, and
  otherwise leaves it running; shutting down stops it.
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
