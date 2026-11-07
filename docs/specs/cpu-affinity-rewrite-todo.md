# CPU Affinity Rewrite TODO

This is the execution checklist for the affinity rewrite. A phase is complete only when its implementation, tests, documentation impact, and review evidence are recorded.

## Review protocol

- After each phase, run `cargo fmt --all -- --check` and the relevant tests.
- Perform an independent architecture review before moving to the next phase.
- This Pi session does not expose an Agent/Astra subagent tool. Until one is available, the review must be explicitly labeled as a local independent second-pass review and must not be presented as a subagent review.
- If a required subagent becomes available, use a fresh reviewer with no implementation context and record findings here.

## Phases

### Phase 0 — Contract and migration boundary

- [x] Record decisions for ownership, active vCPU semantics, SMT, cache/NUMA degradation, and no synthetic CCDs.
- [x] Define the new module boundary and preserve the old implementation as a migration boundary.
- [x] Write the architecture specification.
- [x] Local independent review: validate the contract against Proxmox semantics and current repository design.

### Phase 1 — Typed domain model

- [x] Add normalized `CpuSet`, physical-core, logical-CPU, topology-quality, VM CPU-shape, VM resource, policy, plan, and reconcile-report types.
- [x] Make the types deterministic, validated, and independent of sysfs, PVE files, `qm`, TUI, and daemon state.
- [x] Add unit tests for CPU-set normalization, vCPU shape calculation, invalid shapes, and stable formatting.
- [x] Local independent second-pass review completed; no Astra/subagent tool is available in this session.

#### Phase 1 review findings

- The model correctly keeps `active_vcpus` separate from `max_vcpus` and rejects `active_vcpus > max_vcpus`.
- The model is pure and has no adapter/runtime dependencies.
- `CpuSet` normalization is deterministic and avoids the old range-formatting duplication.
- Topology source metadata and filesystem fixture handling remain intentionally deferred to Phase 2.
- The implementation does not yet replace the legacy planner; that migration remains blocked until the planner consumes these types.

### Phase 2 — Topology adapter

- [x] Replace synthetic CCD fallback with explicit cache-domain/source/quality data.
- [x] Read logical CPUs, physical-core identity, siblings, package, NUMA and cache data where available.
- [x] Preserve unknown facts as unknown; never infer P/E from SMT shape.
- [x] Add fixture-driven topology tests for AMD-like SMT, Intel hybrid, generic, multi-socket, and incomplete sysfs data.
- [x] Local independent second-pass review completed; no Astra/subagent tool is available in this session.

#### Phase 2 review findings

- The new adapter reads only sysfs facts and does not use `core_id / 8` or sibling-count P/E inference.
- Missing physical identity produces `LogicalOnly`; missing sibling data produces `Partial` without fabricating sibling relationships.
- Cache-domain source is explicit and deterministic; L3, cluster, die, NUMA and package fallback are distinguishable.
- The legacy topology module remains untouched as a migration boundary for the old UI/planner.
- The new adapter is not yet wired into daemon/TUI; that is intentional until the inventory and planner consume the normalized model.

### Phase 3 — Proxmox inventory adapter

- [x] Parse `sockets`, legacy `smp`, `cores`, `vcpus`, `hotplug`, `affinity`, VM status and template state.
- [x] Calculate `max_vcpus = sockets * cores` and `active_vcpus = vcpus > 0 ? vcpus : max_vcpus`.
- [x] Treat missing Proxmox defaults correctly; never use zero as an implicit default.
- [x] Validate `vcpus <= max_vcpus` and report malformed configurations explicitly.
- [x] Add fixture tests for normal, hotplug, legacy, missing-field and invalid configurations.
- [x] Local independent second-pass review completed; no Astra/subagent tool is available in this session.

#### Phase 3 review findings

- The adapter correctly models Proxmox's distinction between maximum and active vCPUs.
- Explicit `sockets` wins over legacy `smp`; missing fields use Proxmox defaults.
- Templates are skipped, excluded VMs are marked external, and malformed affinity/CPU shape is reported.
- The legacy `pve::VmConfig` remains for the old CLI/TUI until migration in later phases.
- The parser intentionally does not treat unreadable VM config as a zero-core VM.

### Phase 4 — Pure planner

- [x] Implement deterministic planning by active vCPU count.
- [x] Separate managed VMs, excluded/external occupancy, host reservations and free capacity.
- [x] Prefer distinct physical cores; allow partial SMT use for odd vCPU counts.
- [x] Make cache/NUMA locality optional optimizations, not correctness requirements.
- [x] Preserve valid current assignments where possible to avoid churn.
- [x] Make exclusive/overcommit behavior explicit in the policy model.
- [x] Return complete plans and explicit conflicts; never silently emit partial plans.
- [x] Add synthetic topology and planner tests.
- [x] Local independent second-pass review completed; no Astra/subagent tool is available in this session.

#### Phase 4 review findings

- The new planner is pure and deterministic; all VM assignments are selected before application.
- It plans exact active-vCPU-sized logical CPU sets and does not expand odd requests to sibling pairs.
- Excluded VM affinity and host reservations are hard occupied capacity.
- Valid managed affinity is preserved to reduce churn; invalid or conflicting affinity is replanned.
- The planner emits conflicts rather than silently applying a partial result.
- The current implementation deliberately keeps `AllowShared` as a policy boundary; shared managed allocation is not silently enabled by the default exclusive policy.

### Phase 5 — Reconciler and backend

- [x] Introduce a PVE backend abstraction for list/read/apply/verify operations.
- [x] Diff desired plan against current affinity.
- [x] Apply only changed managed VMs, serially and deterministically.
- [x] Verify each change after `qm set`.
- [x] Return structured partial-failure reports and map them to Healthy/Degraded/Failed at the domain layer.
- [x] Add fake-backend tests for idempotence, drift repair, excluded VMs, failures and retries.
- [x] Local independent second-pass review completed; no Astra/subagent tool is available in this session.

#### Phase 5 review findings

- Reconcile refuses to partially apply a plan containing planner conflicts.
- Changed assignments are applied serially and verified immediately.
- A process-local mutex prevents concurrent reconcile calls from interleaving.
- The production backend is isolated behind `AffinityBackend`; tests use `MemoryBackend`.
- The current module/IPC state mapping remains to be integrated in Phase 6.

### Phase 6 — Configuration and daemon integration

- [ ] Replace string round-tripping of nested TOML with typed deserialization.
- [ ] Make configuration errors explicit; do not silently fall back to defaults.
- [ ] Add periodic/manual/config-reload reconcile triggers.
- [ ] Make daemon the authoritative applier when available.
- [ ] Preserve excluded VM affinity and treat it as external occupancy.
- [ ] Add status/reporting for topology degradation and partial reconcile failure.
- [ ] Review daemon/config behavior independently.

### Phase 7 — CLI and TUI migration

- [ ] Make CLI and TUI consume the same inventory, planner, plan and report types.
- [ ] Route mutations through daemon IPC when available.
- [ ] Keep standalone mode explicit when daemon is unavailable.
- [ ] Remove direct planner/application duplication from UI code.
- [ ] Update displays from physical cores/SMT conversion language to active vCPU and allowed CPU-set language.
- [ ] Review user-facing behavior independently.

### Phase 8 — Compatibility, documentation and removal

- [ ] Decide and test old config compatibility/migration.
- [ ] Update README, CLI help and affinity specification.
- [ ] Remove obsolete affinity planner, synthetic topology behavior and duplicated execution paths.
- [ ] Run workspace tests, formatting, clippy where available, and targeted fixture tests.
- [ ] Perform final independent review and record unresolved limitations.

## Blocked stop condition

Stop and report evidence if a phase cannot be completed without guessing Proxmox semantics, fabricating topology facts, breaking existing CLI/config behavior without a migration path, or changing unrelated modules.
