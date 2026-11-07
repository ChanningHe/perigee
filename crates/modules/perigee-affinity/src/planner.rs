//! Pure, deterministic CPU-affinity planner.
//!
//! The planner consumes normalized observations and produces a complete desired
//! plan. It does not read files, inspect sysfs, execute `qm`, or mutate VM
//! state. A caller must refuse to apply a plan containing conflicts.

use crate::model::{
    AffinityPlan, AffinityPolicy, AllocationPolicy, CoreKind, CorePreference, CpuSet, CpuTopology,
    LocalityPolicy, OvercommitPolicy, PlanConflict, PlanWarning, SmtPolicy, TopologyQuality,
    VmAffinityAssignment, VmOwnership, VmResource,
};
use std::collections::{BTreeMap, HashMap, HashSet};

pub fn plan(topology: &CpuTopology, vms: &[VmResource], policy: &AffinityPolicy) -> AffinityPlan {
    let logical_ids = topology.logical_cpu_ids();
    let mut warnings = Vec::new();
    let mut conflicts = Vec::new();

    if matches!(topology.quality, TopologyQuality::Invalid) || logical_ids.is_empty() {
        conflicts.push(PlanConflict {
            vmid: None,
            message: "CPU topology has no usable logical CPUs".to_string(),
        });
    }
    if !matches!(topology.quality, TopologyQuality::Complete) {
        warnings.push(PlanWarning {
            vmid: None,
            message: format!(
                "topology quality is {:?}; placement optimizations are limited",
                topology.quality
            ),
        });
    }
    if matches!(topology.quality, TopologyQuality::LogicalOnly) {
        warnings.push(PlanWarning {
            vmid: None,
            message: "physical-core and SMT relationships are unavailable".to_string(),
        });
    }

    let reserved_cpus = valid_subset(
        &policy.reserved_cpus,
        &logical_ids,
        None,
        &mut warnings,
        "reserved CPU",
    );
    let mut external_occupied = CpuSet::empty();

    for vm in vms
        .iter()
        .filter(|vm| vm.ownership == VmOwnership::Excluded)
    {
        if let Some(affinity) = &vm.current_affinity {
            external_occupied = union(
                &external_occupied,
                &valid_subset(
                    affinity,
                    &logical_ids,
                    Some(vm.vmid),
                    &mut warnings,
                    "excluded VM affinity CPU",
                ),
            );
        }
    }

    let mut hard_occupied = union(&reserved_cpus, &external_occupied);
    let mut assignments = Vec::new();
    let mut assigned_by_vm: HashMap<u32, CpuSet> = HashMap::new();

    let mut managed: Vec<&VmResource> = vms
        .iter()
        .filter(|vm| vm.ownership == VmOwnership::Managed)
        .collect();
    managed.sort_by(|a, b| {
        b.cpu
            .active_vcpus
            .cmp(&a.cpu.active_vcpus)
            .then(a.vmid.cmp(&b.vmid))
    });

    let core_index = build_core_index(topology);
    let domain_index = build_domain_index(topology);
    let mut domain_loads: BTreeMap<DomainKey, usize> = BTreeMap::new();

    for vm in managed {
        if vm.cpu.active_vcpus == 0 {
            warnings.push(PlanWarning {
                vmid: Some(vm.vmid),
                message: "VM has no active vCPUs and was skipped".to_string(),
            });
            continue;
        }
        if vm.cpu.active_vcpus > logical_ids.len() {
            conflicts.push(PlanConflict {
                vmid: Some(vm.vmid),
                message: format!(
                    "needs {} active vCPUs but topology exposes only {} logical CPUs",
                    vm.cpu.active_vcpus,
                    logical_ids.len()
                ),
            });
            continue;
        }

        let current = vm.current_affinity.as_ref().map(|affinity| {
            valid_subset(
                affinity,
                &logical_ids,
                Some(vm.vmid),
                &mut warnings,
                "managed VM affinity CPU",
            )
        });
        let preserved = current.as_ref().filter(|candidate| {
            candidate.len() == vm.cpu.active_vcpus
                && candidate.iter().all(|cpu| !hard_occupied.contains(*cpu))
        });

        let selected = if let Some(candidate) = preserved {
            Some(candidate.clone())
        } else {
            if current.is_some() {
                warnings.push(PlanWarning {
                    vmid: Some(vm.vmid),
                    message: "current affinity cannot be preserved under the new plan".to_string(),
                });
            }
            choose_cpus(
                &logical_ids,
                &hard_occupied,
                vm,
                policy,
                &core_index,
                &domain_index,
                &domain_loads,
            )
        };

        let Some(selected) = selected else {
            conflicts.push(PlanConflict {
                vmid: Some(vm.vmid),
                message: format!(
                    "cannot allocate {} logical CPUs without violating policy",
                    vm.cpu.active_vcpus
                ),
            });
            continue;
        };

        let changed = vm.current_affinity.as_ref() != Some(&selected);
        for cpu in selected.iter() {
            hard_occupied = union(&hard_occupied, &CpuSet::new([*cpu]));
            if let Some(domain) = domain_index.get(cpu) {
                *domain_loads.entry(*domain).or_default() += 1;
            }
        }
        assigned_by_vm.insert(vm.vmid, selected.clone());
        assignments.push(VmAffinityAssignment {
            vmid: vm.vmid,
            active_vcpus: vm.cpu.active_vcpus,
            allowed_cpus: selected,
            changed,
        });
    }

    // Keep the map in the model's deterministic output path useful to future
    // policies without allowing an accidental unused-state regression here.
    let _ = assigned_by_vm;

    AffinityPlan {
        topology_fingerprint: topology.fingerprint.clone(),
        assignments,
        reserved_cpus,
        external_occupied_cpus: external_occupied,
        warnings,
        conflicts,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum DomainKey {
    Cache(usize),
    Numa(usize),
    Package(usize),
    Unknown,
}

fn build_core_index(topology: &CpuTopology) -> HashMap<usize, &crate::model::PhysicalCore> {
    let mut index = HashMap::new();
    for core in &topology.physical_cores {
        for cpu in core.threads.iter() {
            index.insert(*cpu, core);
        }
    }
    index
}

fn build_domain_index(topology: &CpuTopology) -> HashMap<usize, DomainKey> {
    topology
        .logical_cpus
        .iter()
        .map(|cpu| {
            let domain = cpu
                .cache_domain
                .map(DomainKey::Cache)
                .or_else(|| cpu.numa_node.map(DomainKey::Numa))
                .or_else(|| cpu.package_id.map(DomainKey::Package))
                .unwrap_or(DomainKey::Unknown);
            (cpu.id, domain)
        })
        .collect()
}

fn choose_cpus(
    logical_ids: &CpuSet,
    hard_occupied: &CpuSet,
    vm: &VmResource,
    policy: &AffinityPolicy,
    core_index: &HashMap<usize, &crate::model::PhysicalCore>,
    domain_index: &HashMap<usize, DomainKey>,
    domain_loads: &BTreeMap<DomainKey, usize>,
) -> Option<CpuSet> {
    let mut candidates: Vec<usize> = logical_ids
        .iter()
        .copied()
        .filter(|cpu| !hard_occupied.contains(*cpu))
        .filter(|cpu| {
            core_index
                .get(cpu)
                .map(|core| core_matches(core.kind, policy.core_preference))
                .unwrap_or(policy.core_preference == CorePreference::Any)
        })
        .collect();

    if policy.overcommit == OvercommitPolicy::AllowShared {
        // Reserved and external CPUs remain hard boundaries. Managed CPUs are
        // still selected from the free set first; explicit sharing is a final
        // fallback only when exclusive capacity cannot satisfy the VM.
    }

    let mut selected = Vec::new();
    let mut selected_cores = HashSet::new();
    let mut selected_domains: HashMap<DomainKey, usize> = HashMap::new();

    while selected.len() < vm.cpu.active_vcpus {
        candidates.sort_by_key(|cpu| {
            let core = core_index.get(cpu).map(|core| core.id);
            let same_core = core.is_some_and(|id| selected_cores.contains(&id));
            let domain = domain_index.get(cpu).copied().unwrap_or(DomainKey::Unknown);
            let domain_load = domain_loads.get(&domain).copied().unwrap_or(0);
            let selected_domain_load = selected_domains.get(&domain).copied().unwrap_or(0);
            let sibling_rank = match policy.smt {
                SmtPolicy::PreferDistinctCores => same_core as usize,
                SmtPolicy::PreferSiblings => (!same_core) as usize,
                SmtPolicy::LogicalOnly => 0,
            };
            let domain_rank = match policy.allocation {
                AllocationPolicy::Packed => usize::MAX - domain_load,
                AllocationPolicy::Balanced | AllocationPolicy::Spread => {
                    domain_load + selected_domain_load
                }
            };
            let locality_rank = match policy.locality {
                LocalityPolicy::None => 0,
                LocalityPolicy::PreferCacheDomain | LocalityPolicy::PreferNumaNode => {
                    selected_domain_load
                }
            };
            (sibling_rank, domain_rank, locality_rank, *cpu)
        });

        let cpu = *candidates.first()?;
        candidates.remove(0);
        selected.push(cpu);
        if let Some(core) = core_index.get(&cpu) {
            selected_cores.insert(core.id);
        }
        let domain = domain_index
            .get(&cpu)
            .copied()
            .unwrap_or(DomainKey::Unknown);
        *selected_domains.entry(domain).or_default() += 1;
    }

    Some(CpuSet::new(selected))
}

fn core_matches(kind: CoreKind, preference: CorePreference) -> bool {
    match preference {
        CorePreference::Any => true,
        CorePreference::Performance => kind == CoreKind::Performance,
        CorePreference::Efficiency => kind == CoreKind::Efficiency,
    }
}

fn valid_subset(
    requested: &CpuSet,
    logical_ids: &CpuSet,
    vmid: Option<u32>,
    warnings: &mut Vec<PlanWarning>,
    label: &str,
) -> CpuSet {
    let valid: Vec<usize> = requested
        .iter()
        .copied()
        .filter(|cpu| logical_ids.contains(*cpu))
        .collect();
    if valid.len() != requested.len() {
        warnings.push(PlanWarning {
            vmid,
            message: format!("{label} contains CPUs not present in current topology"),
        });
    }
    CpuSet::new(valid)
}

fn union(left: &CpuSet, right: &CpuSet) -> CpuSet {
    CpuSet::new(left.iter().chain(right.iter()).copied())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CacheDomainSource, CoreKind, LogicalCpu, PhysicalCore, PhysicalCoreId, TopologyQuality,
        VmCpuShape, VmStatus,
    };

    fn topology() -> CpuTopology {
        let cores = (0..4)
            .map(|core_id| PhysicalCore {
                id: PhysicalCoreId {
                    package_id: Some(0),
                    core_id,
                },
                threads: CpuSet::new([core_id * 2, core_id * 2 + 1]),
                kind: if core_id < 3 {
                    CoreKind::Performance
                } else {
                    CoreKind::Efficiency
                },
                package_id: Some(0),
                numa_node: Some(0),
                cache_domain: Some(core_id / 2),
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
        CpuTopology {
            logical_cpus,
            physical_cores: cores,
            quality: TopologyQuality::Complete,
            cache_domain_source: CacheDomainSource::L3Sysfs,
            fingerprint: "test-topology".to_string(),
        }
    }

    fn vm(
        vmid: u32,
        active_vcpus: usize,
        affinity: Option<CpuSet>,
        ownership: VmOwnership,
    ) -> VmResource {
        VmResource {
            vmid,
            name: format!("vm-{vmid}"),
            status: VmStatus::Stopped,
            cpu: VmCpuShape {
                sockets: 1,
                cores_per_socket: active_vcpus,
                max_vcpus: active_vcpus,
                active_vcpus,
                cpu_hotplug: false,
            },
            current_affinity: affinity,
            ownership,
        }
    }

    #[test]
    fn allocates_exactly_active_vcpus_and_prefers_distinct_cores() {
        let plan = plan(
            &topology(),
            &[vm(100, 3, None, VmOwnership::Managed)],
            &AffinityPolicy::default(),
        );
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.assignments[0].allowed_cpus.len(), 3);
        assert_eq!(plan.assignments[0].allowed_cpus.as_slice(), &[0, 2, 4]);
    }

    #[test]
    fn odd_vcpu_demand_does_not_expand_to_a_sibling_pair() {
        let plan = plan(
            &topology(),
            &[vm(100, 5, None, VmOwnership::Managed)],
            &AffinityPolicy::default(),
        );
        assert_eq!(plan.assignments[0].allowed_cpus.len(), 5);
    }

    #[test]
    fn excluded_affinity_is_external_occupied_capacity() {
        let vms = vec![
            vm(100, 2, Some(CpuSet::new([0, 1])), VmOwnership::Excluded),
            vm(101, 2, None, VmOwnership::Managed),
        ];
        let plan = plan(&topology(), &vms, &AffinityPolicy::default());
        assert_eq!(plan.external_occupied_cpus.as_slice(), &[0, 1]);
        assert_eq!(plan.assignments[0].allowed_cpus.as_slice(), &[2, 4]);
    }

    #[test]
    fn valid_current_affinity_is_preserved() {
        let current = CpuSet::new([4, 6]);
        let plan = plan(
            &topology(),
            &[vm(100, 2, Some(current.clone()), VmOwnership::Managed)],
            &AffinityPolicy::default(),
        );
        assert_eq!(plan.assignments[0].allowed_cpus, current);
        assert!(!plan.assignments[0].changed);
    }

    #[test]
    fn reserved_cpu_is_not_allocated() {
        let mut policy = AffinityPolicy::default();
        policy.reserved_cpus = CpuSet::new([0, 1]);
        let plan = plan(
            &topology(),
            &[vm(100, 2, None, VmOwnership::Managed)],
            &policy,
        );
        assert_eq!(plan.assignments[0].allowed_cpus.as_slice(), &[2, 4]);
    }

    #[test]
    fn insufficient_exclusive_capacity_is_a_conflict() {
        let vms = vec![
            vm(100, 8, None, VmOwnership::Managed),
            vm(101, 1, None, VmOwnership::Managed),
        ];
        let plan = plan(&topology(), &vms, &AffinityPolicy::default());
        assert!(plan
            .conflicts
            .iter()
            .any(|conflict| conflict.vmid == Some(101)));
    }
}
