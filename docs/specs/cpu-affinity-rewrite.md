# CPU Affinity Rewrite

## Core Concept

CPU affinity management computes a deterministic desired host-CPU set for each managed Proxmox VM, then reconciles that desired state with the VM's configured `affinity` value.

The core data structure is a normalized resource model: logical CPUs belong to physical cores; VMs expose an active vCPU demand; a planner produces a complete immutable plan before any `qm set` operation runs.

## Decisions

- Enabling automatic mode manages all eligible VMs by default.
- `exclude_vmids` is an explicit ownership boundary. Excluded VMs are not modified, and their current affinity is treated as external occupied capacity.
- Managed VMs may be re-planned. Existing affinity is a stability hint, not a protection boundary.
- External changes to managed VM affinity are drift and are repaired on the next reconcile.
- Excluded VM affinity is preserved.
- Active vCPUs are calculated as:
  - `vcpus`, when configured and greater than zero;
  - otherwise `sockets * cores`;
  - missing `sockets` and `cores` use Proxmox defaults of `1`.
- `sockets * cores` is retained as `max_vcpus`; planning uses active vCPUs.
- The generated affinity set contains exactly the active-vCPU count of logical CPUs.
- SMT is a placement preference, not a vCPU-count conversion. Placement prefers distinct physical cores and may use one sibling of a physical core for an odd vCPU demand.
- Missing topology facts cause capability degradation, not fabricated hardware relationships:
  - missing sibling data disables sibling-aware optimization;
  - missing cache/NUMA data disables locality optimization;
  - missing logical CPU enumeration stops planning.
- No synthetic CCDs are inferred from `core_id / 8`.

## Data Flow

```text
sysfs topology + PVE VM inventory + typed policy
                         |
                         v
                 immutable snapshot
                         |
                         v
                    pure planner
                         |
                         v
                    AffinityPlan
                         |
                         v
                  diff + apply + verify
                         |
                         v
                  ReconcileReport/status
```

## Domain Model

The rewrite is staged around these types:

- `CpuTopology`: logical CPUs, physical cores, optional cache/NUMA domains, cache-domain source, topology quality and fingerprint.
- `VmResource`: VM identity, status, CPU shape, active/max vCPU counts, current affinity and management ownership.
- `CpuSet`: sorted, deduplicated logical CPU IDs with validation and stable formatting.
- `AffinityPolicy`: selection, placement, SMT, locality, reserve and overcommit rules.
- `AffinityPlan`: complete assignments, external occupancy, reservations, conflicts and warnings.
- `ReconcileReport`: planned/changed/unchanged/skipped/failed counts and resulting module state.

## Proxmox CPU Semantics

Proxmox defines `cores` as cores per socket, `sockets` as virtual sockets, and `vcpus` as the number of CPUs plugged at VM start when CPU hotplug is used. The maximum is `sockets * cores`.

The planner must retain both `active_vcpus` and `max_vcpus`; it must not reserve `max_vcpus` merely because hotplug is possible.

## Ownership and Drift

Automatic mode partitions VMs into:

- managed: all eligible VMs not in `exclude_vmids`;
- external: excluded VMs, whose current affinity is fixed occupied capacity;
- skipped: unreadable, invalid or zero-active-vCPU VMs.

A managed VM's current affinity may be preserved when it remains valid, but it can be replaced when policy or resource constraints require it.

## Edge Cases

- A VM with `vcpus` greater than `sockets * cores` is invalid and is skipped with an error; it must not be clamped silently.
- An odd active-vCPU count may select a partial SMT core.
- If sibling mapping is unavailable, logical CPUs are treated as independent and the plan is marked degraded.
- If cache/NUMA topology is unavailable, generic deterministic placement is allowed but locality claims are not shown.
- If the available logical CPU set cannot satisfy all managed VMs under the selected overcommit policy, the plan contains explicit conflicts and does not partially invent an assignment.
- Applying a plan must be serialized and verified after each changed VM.

## Maintainer Notes

The planner must be pure and deterministic. It must not read sysfs, parse PVE files, execute `qm`, mutate module state, or write logs. TUI, CLI and daemon must consume the same planner and plan types. The daemon is the authoritative applier when it is available.

The old `CoreGroup`/strategy implementation remains only as a migration boundary until the new planner and adapters are complete. Do not extend the old algorithm with more policy branches.
