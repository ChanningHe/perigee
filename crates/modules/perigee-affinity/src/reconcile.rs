//! Desired-state reconciliation for VM CPU affinity.

use crate::inventory::{self, InventorySnapshot};
use crate::model::{AffinityPlan, AffinityPolicy, CpuSet, ReconcileOutcome, ReconcileReport};
use crate::planner;
use crate::pve;
use crate::topology_adapter;
use anyhow::{Context, Result};
use std::sync::Mutex;

/// Backend boundary for applying and verifying affinity changes.
pub trait AffinityBackend: Send + Sync {
    fn inventory(&self, exclude_vmids: &[u32]) -> Result<InventorySnapshot>;
    fn apply(&self, vmid: u32, affinity: &CpuSet) -> Result<()>;
    fn read_affinity(&self, vmid: u32) -> Result<Option<CpuSet>>;
}

/// Production backend using the existing Proxmox command/config interfaces.
#[derive(Debug, Default)]
pub struct PveBackend;

impl AffinityBackend for PveBackend {
    fn inventory(&self, exclude_vmids: &[u32]) -> Result<InventorySnapshot> {
        inventory::collect(exclude_vmids)
    }

    fn apply(&self, vmid: u32, affinity: &CpuSet) -> Result<()> {
        pve::set_affinity(vmid, &affinity.to_range_string(), false)
    }

    fn read_affinity(&self, vmid: u32) -> Result<Option<CpuSet>> {
        pve::read_vm_affinity(vmid)
    }
}

/// Serialize reconciles within this process. The daemon has one module
/// instance, but this also protects direct callers and future timer/IPC paths.
static RECONCILE_LOCK: Mutex<()> = Mutex::new(());

/// Build a plan from one consistent topology and inventory snapshot.
pub fn make_plan(
    topology: &crate::model::CpuTopology,
    inventory: &InventorySnapshot,
    policy: &AffinityPolicy,
) -> AffinityPlan {
    planner::plan(topology, &inventory.vms, policy)
}

/// Reconcile one complete snapshot. Plans containing conflicts are never
/// partially applied. Each changed VM is verified immediately after `qm set`.
pub fn reconcile(
    backend: &dyn AffinityBackend,
    policy: &AffinityPolicy,
    exclude_vmids: &[u32],
) -> Result<ReconcileReport> {
    let _guard = RECONCILE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("affinity reconcile lock is poisoned"))?;
    let topology = topology_adapter::detect().context("detecting CPU topology")?;
    let inventory = backend
        .inventory(exclude_vmids)
        .context("reading VM inventory")?;
    let plan = make_plan(&topology, &inventory, policy);

    if !plan.conflicts.is_empty() {
        return Ok(ReconcileReport {
            planned: plan.assignments.len(),
            changed: 0,
            unchanged: plan
                .assignments
                .iter()
                .filter(|assignment| !assignment.changed)
                .count(),
            skipped: inventory.warnings.len(),
            failed: plan.conflicts.len(),
            warnings: plan
                .warnings
                .iter()
                .map(|warning| warning.message.clone())
                .chain(
                    plan.conflicts
                        .iter()
                        .map(|conflict| conflict.message.clone()),
                )
                .collect(),
            outcome: ReconcileOutcome::Failed,
        });
    }

    let mut changed = 0;
    let mut unchanged = 0;
    let mut failed = 0;
    let mut warnings: Vec<String> = inventory
        .warnings
        .iter()
        .map(|warning| format!("VM {}: {}", warning.vmid, warning.message))
        .chain(plan.warnings.iter().map(|warning| warning.message.clone()))
        .collect();

    for assignment in &plan.assignments {
        if !assignment.changed {
            unchanged += 1;
            continue;
        }

        if let Err(error) = backend.apply(assignment.vmid, &assignment.allowed_cpus) {
            failed += 1;
            warnings.push(format!("VM {} apply failed: {error}", assignment.vmid));
            continue;
        }

        match backend.read_affinity(assignment.vmid) {
            Ok(Some(actual)) if actual == assignment.allowed_cpus => changed += 1,
            Ok(Some(actual)) => {
                failed += 1;
                warnings.push(format!(
                    "VM {} verification mismatch: expected {}, got {}",
                    assignment.vmid, assignment.allowed_cpus, actual
                ));
            }
            Ok(None) => {
                failed += 1;
                warnings.push(format!(
                    "VM {} verification found no affinity",
                    assignment.vmid
                ));
            }
            Err(error) => {
                failed += 1;
                warnings.push(format!(
                    "VM {} verification failed: {error}",
                    assignment.vmid
                ));
            }
        }
    }

    let outcome = if failed == 0 && warnings.is_empty() {
        ReconcileOutcome::Healthy
    } else if changed > 0 || unchanged > 0 {
        ReconcileOutcome::Degraded
    } else {
        ReconcileOutcome::Failed
    };

    Ok(ReconcileReport {
        planned: plan.assignments.len(),
        changed,
        unchanged,
        skipped: inventory.warnings.len(),
        failed,
        warnings,
        outcome,
    })
}

/// Test backend with mutable in-memory affinity state and optional failures.
#[derive(Debug, Default)]
pub struct MemoryBackend {
    pub inventory: InventorySnapshot,
    pub applied: Mutex<Vec<(u32, CpuSet)>>,
    pub fail_apply: Mutex<Vec<u32>>,
}

impl MemoryBackend {
    pub fn new(inventory: InventorySnapshot) -> Self {
        Self {
            inventory,
            applied: Mutex::new(Vec::new()),
            fail_apply: Mutex::new(Vec::new()),
        }
    }
}

impl AffinityBackend for MemoryBackend {
    fn inventory(&self, _exclude_vmids: &[u32]) -> Result<InventorySnapshot> {
        Ok(self.inventory.clone())
    }

    fn apply(&self, vmid: u32, affinity: &CpuSet) -> Result<()> {
        if self
            .fail_apply
            .lock()
            .map_err(|_| anyhow::anyhow!("failure list lock is poisoned"))?
            .contains(&vmid)
        {
            anyhow::bail!("injected apply failure")
        }
        self.applied
            .lock()
            .map_err(|_| anyhow::anyhow!("applied list lock is poisoned"))?
            .push((vmid, affinity.clone()));
        Ok(())
    }

    fn read_affinity(&self, vmid: u32) -> Result<Option<CpuSet>> {
        let applied = self
            .applied
            .lock()
            .map_err(|_| anyhow::anyhow!("applied list lock is poisoned"))?;
        if let Some((_, affinity)) = applied.iter().rev().find(|(id, _)| *id == vmid) {
            return Ok(Some(affinity.clone()));
        }
        Ok(self
            .inventory
            .vms
            .iter()
            .find(|vm| vm.vmid == vmid)
            .and_then(|vm| vm.current_affinity.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CacheDomainSource, CoreKind, LogicalCpu, PhysicalCore, PhysicalCoreId, TopologyQuality,
        VmCpuShape, VmOwnership, VmResource, VmStatus,
    };

    fn topology() -> crate::model::CpuTopology {
        let physical_cores = (0..4)
            .map(|core| PhysicalCore {
                id: PhysicalCoreId {
                    package_id: Some(0),
                    core_id: core,
                },
                threads: CpuSet::new([core * 2, core * 2 + 1]),
                kind: CoreKind::Performance,
                package_id: Some(0),
                numa_node: Some(0),
                cache_domain: Some(core / 2),
            })
            .collect();
        let logical_cpus = (0..8)
            .map(|id| LogicalCpu {
                id,
                physical_core: Some(PhysicalCoreId {
                    package_id: Some(0),
                    core_id: id / 2,
                }),
                package_id: Some(0),
                numa_node: Some(0),
                cache_domain: Some((id / 2) / 2),
            })
            .collect();
        crate::model::CpuTopology {
            logical_cpus,
            physical_cores,
            quality: TopologyQuality::Complete,
            cache_domain_source: CacheDomainSource::L3Sysfs,
            fingerprint: "test".to_string(),
        }
    }

    fn inventory(affinity: Option<CpuSet>) -> InventorySnapshot {
        InventorySnapshot {
            vms: vec![VmResource {
                vmid: 100,
                name: "test".to_string(),
                status: VmStatus::Stopped,
                cpu: VmCpuShape {
                    sockets: 1,
                    cores_per_socket: 2,
                    max_vcpus: 2,
                    active_vcpus: 2,
                    cpu_hotplug: false,
                },
                current_affinity: affinity,
                ownership: VmOwnership::Managed,
            }],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn plan_is_idempotent_when_current_affinity_is_valid() {
        let snapshot = inventory(Some(CpuSet::new([0, 2])));
        let plan = planner::plan(&topology(), &snapshot.vms, &AffinityPolicy::default());
        assert!(!plan.assignments[0].changed);
    }

    #[test]
    fn reconcile_applies_and_verifies_changed_vm() {
        let backend = MemoryBackend::new(inventory(None));
        let report = reconcile(&backend, &AffinityPolicy::default(), &[]).unwrap();
        assert_eq!(report.changed, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.outcome, ReconcileOutcome::Healthy);
    }

    #[test]
    fn reconcile_reports_partial_failure() {
        let backend = MemoryBackend::new(InventorySnapshot {
            vms: vec![
                inventory(None).vms[0].clone(),
                VmResource {
                    vmid: 101,
                    name: "second".to_string(),
                    status: VmStatus::Stopped,
                    cpu: VmCpuShape {
                        sockets: 1,
                        cores_per_socket: 1,
                        max_vcpus: 1,
                        active_vcpus: 1,
                        cpu_hotplug: false,
                    },
                    current_affinity: None,
                    ownership: VmOwnership::Managed,
                },
            ],
            warnings: Vec::new(),
        });
        backend.fail_apply.lock().unwrap().push(101);
        let report = reconcile(&backend, &AffinityPolicy::default(), &[]).unwrap();
        assert_eq!(report.failed, 1);
        assert_eq!(report.outcome, ReconcileOutcome::Degraded);
    }
}
