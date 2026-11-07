//! Domain model for CPU affinity planning.
//!
//! This module deliberately has no dependency on sysfs, Proxmox files, `qm`,
//! the daemon, or the TUI. Adapters translate external observations into these
//! normalized types; the planner will consume them later.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

const MAX_LOGICAL_CPUS: usize = 8192;

/// A normalized, sorted, duplicate-free set of logical host CPU IDs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CpuSet(Vec<usize>);

impl CpuSet {
    /// Build a set from any iterator of logical CPU IDs.
    pub fn new<I>(cpus: I) -> Self
    where
        I: IntoIterator<Item = usize>,
    {
        let mut cpus: Vec<usize> = cpus.into_iter().collect();
        cpus.sort_unstable();
        cpus.dedup();
        Self(cpus)
    }

    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse the comma/range syntax accepted by Proxmox affinity settings.
    pub fn parse(value: &str) -> Result<Self, CpuSetParseError> {
        let mut cpus = Vec::new();
        for part in value.split(',').map(str::trim) {
            if part.is_empty() {
                return Err(CpuSetParseError::EmptyPart);
            }
            if let Some((start, end)) = part.split_once('-') {
                let start = parse_cpu_id(start)?;
                let end = parse_cpu_id(end)?;
                if start > end {
                    return Err(CpuSetParseError::InvertedRange { start, end });
                }
                cpus.extend(start..=end);
            } else {
                cpus.push(parse_cpu_id(part)?);
            }
        }
        Ok(Self::new(cpus))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn contains(&self, cpu: usize) -> bool {
        self.0.binary_search(&cpu).is_ok()
    }

    pub fn iter(&self) -> impl Iterator<Item = &usize> {
        self.0.iter()
    }

    pub fn as_slice(&self) -> &[usize] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<usize> {
        self.0
    }

    /// Format the set using the syntax accepted by Proxmox `--affinity`.
    pub fn to_range_string(&self) -> String {
        let Some(&first) = self.0.first() else {
            return String::new();
        };

        let mut ranges = Vec::new();
        let mut start = first;
        let mut end = first;

        for &cpu in &self.0[1..] {
            if end.checked_add(1) == Some(cpu) {
                end = cpu;
            } else {
                ranges.push(format_range(start, end));
                start = cpu;
                end = cpu;
            }
        }
        ranges.push(format_range(start, end));
        ranges.join(",")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CpuSetParseError {
    #[error("affinity contains an empty item")]
    EmptyPart,
    #[error("invalid CPU id '{0}'")]
    InvalidCpu(String),
    #[error("CPU id {0} exceeds supported limit {MAX_LOGICAL_CPUS}")]
    CpuOutOfRange(usize),
    #[error("affinity range {start}-{end} is inverted")]
    InvertedRange { start: usize, end: usize },
}

fn parse_cpu_id(value: &str) -> Result<usize, CpuSetParseError> {
    let id = value
        .trim()
        .parse::<usize>()
        .map_err(|_| CpuSetParseError::InvalidCpu(value.trim().to_string()))?;
    if id >= MAX_LOGICAL_CPUS {
        return Err(CpuSetParseError::CpuOutOfRange(id));
    }
    Ok(id)
}

impl From<Vec<usize>> for CpuSet {
    fn from(cpus: Vec<usize>) -> Self {
        Self::new(cpus)
    }
}

impl From<&[usize]> for CpuSet {
    fn from(cpus: &[usize]) -> Self {
        Self::new(cpus.iter().copied())
    }
}

impl IntoIterator for CpuSet {
    type Item = usize;
    type IntoIter = std::vec::IntoIter<usize>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

fn format_range(start: usize, end: usize) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

/// Stable identity of a physical core when package information is available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PhysicalCoreId {
    pub package_id: Option<usize>,
    pub core_id: usize,
}

/// Physical core classification. Unknown is retained rather than guessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreKind {
    Performance,
    Efficiency,
    Unknown,
}

/// One logical host CPU and the topology facts known about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalCpu {
    pub id: usize,
    pub physical_core: Option<PhysicalCoreId>,
    pub package_id: Option<usize>,
    pub numa_node: Option<usize>,
    pub cache_domain: Option<usize>,
}

/// One physical core and its logical sibling threads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalCore {
    pub id: PhysicalCoreId,
    pub threads: CpuSet,
    pub kind: CoreKind,
    pub package_id: Option<usize>,
    pub numa_node: Option<usize>,
    pub cache_domain: Option<usize>,
}

/// How much topology information is available to the planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopologyQuality {
    Complete,
    Partial,
    LogicalOnly,
    Invalid,
}

/// Source used to identify shared cache domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheDomainSource {
    L3Sysfs,
    ClusterSysfs,
    DieSysfs,
    NumaFallback,
    PackageFallback,
    Unknown,
}

/// Normalized host CPU topology. Adapters populate this from sysfs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuTopology {
    pub logical_cpus: Vec<LogicalCpu>,
    pub physical_cores: Vec<PhysicalCore>,
    pub quality: TopologyQuality,
    pub cache_domain_source: CacheDomainSource,
    pub fingerprint: String,
}

impl CpuTopology {
    pub fn logical_cpu_ids(&self) -> CpuSet {
        CpuSet::new(self.logical_cpus.iter().map(|cpu| cpu.id))
    }

    pub fn physical_core_ids(&self) -> Vec<PhysicalCoreId> {
        self.physical_cores.iter().map(|core| core.id).collect()
    }
}

/// Proxmox CPU shape and the number of vCPUs currently active at VM start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmCpuShape {
    pub sockets: usize,
    pub cores_per_socket: usize,
    pub max_vcpus: usize,
    pub active_vcpus: usize,
    pub cpu_hotplug: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum VmCpuShapeError {
    #[error("sockets must be greater than zero")]
    ZeroSockets,
    #[error("cores per socket must be greater than zero")]
    ZeroCores,
    #[error("sockets × cores overflowed")]
    MaxVcpuOverflow,
    #[error("active vCPUs ({active}) exceed max vCPUs ({max})")]
    ActiveExceedsMax { active: usize, max: usize },
}

impl VmCpuShape {
    /// Construct a shape from Proxmox's current CPU fields.
    ///
    /// `smp` is the legacy fallback for `sockets`; explicit `sockets` wins.
    /// Proxmox treats a missing/zero `vcpus` value as all `sockets * cores` CPUs
    /// plugged at startup.
    pub fn from_proxmox(
        sockets: Option<usize>,
        smp: Option<usize>,
        cores: Option<usize>,
        vcpus: Option<usize>,
        cpu_hotplug: bool,
    ) -> Result<Self, VmCpuShapeError> {
        let sockets = sockets.or(smp).unwrap_or(1);
        let cores_per_socket = cores.unwrap_or(1);

        if sockets == 0 {
            return Err(VmCpuShapeError::ZeroSockets);
        }
        if cores_per_socket == 0 {
            return Err(VmCpuShapeError::ZeroCores);
        }

        let max_vcpus = sockets
            .checked_mul(cores_per_socket)
            .ok_or(VmCpuShapeError::MaxVcpuOverflow)?;
        let active_vcpus = match vcpus {
            Some(value) if value > 0 => value,
            _ => max_vcpus,
        };

        if active_vcpus > max_vcpus {
            return Err(VmCpuShapeError::ActiveExceedsMax {
                active: active_vcpus,
                max: max_vcpus,
            });
        }

        Ok(Self {
            sockets,
            cores_per_socket,
            max_vcpus,
            active_vcpus,
            cpu_hotplug,
        })
    }
}

/// Current VM lifecycle state as observed from Proxmox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmStatus {
    Running,
    Stopped,
    Unknown,
}

/// Whether the affinity module owns this VM's affinity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmOwnership {
    Managed,
    Excluded,
    Skipped,
}

/// A normalized VM inventory record consumed by the planner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmResource {
    pub vmid: u32,
    pub name: String,
    pub status: VmStatus,
    pub cpu: VmCpuShape,
    pub current_affinity: Option<CpuSet>,
    pub ownership: VmOwnership,
}

/// Coarse allocation objective across managed VMs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AllocationPolicy {
    Packed,
    Balanced,
    Spread,
}

/// Preferred physical-core class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CorePreference {
    Any,
    Performance,
    Efficiency,
}

/// How the planner should treat SMT siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SmtPolicy {
    PreferDistinctCores,
    PreferSiblings,
    LogicalOnly,
}

/// Whether cache/NUMA locality is an optimization or a hard requirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalityPolicy {
    None,
    PreferCacheDomain,
    PreferNumaNode,
}

/// Whether a physical/logical CPU may be shared by managed VMs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OvercommitPolicy {
    Exclusive,
    AllowShared,
}

/// Planner input policy. The planner never reads configuration directly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffinityPolicy {
    pub allocation: AllocationPolicy,
    pub core_preference: CorePreference,
    pub smt: SmtPolicy,
    pub locality: LocalityPolicy,
    pub overcommit: OvercommitPolicy,
    pub reserved_cpus: CpuSet,
}

impl Default for AffinityPolicy {
    fn default() -> Self {
        Self {
            allocation: AllocationPolicy::Balanced,
            core_preference: CorePreference::Any,
            smt: SmtPolicy::PreferDistinctCores,
            locality: LocalityPolicy::PreferCacheDomain,
            overcommit: OvercommitPolicy::Exclusive,
            reserved_cpus: CpuSet::empty(),
        }
    }
}

/// One managed VM's desired affinity set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmAffinityAssignment {
    pub vmid: u32,
    pub active_vcpus: usize,
    pub allowed_cpus: CpuSet,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanWarning {
    pub vmid: Option<u32>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanConflict {
    pub vmid: Option<u32>,
    pub message: String,
}

/// Complete immutable output of the pure planner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffinityPlan {
    pub topology_fingerprint: String,
    pub topology_quality: TopologyQuality,
    pub topology_warning: Option<String>,
    pub assignments: Vec<VmAffinityAssignment>,
    pub reserved_cpus: CpuSet,
    pub external_occupied_cpus: CpuSet,
    pub warnings: Vec<PlanWarning>,
    pub conflicts: Vec<PlanConflict>,
}

/// Outcome of a reconcile pass, independent of daemon presentation details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileOutcome {
    Healthy,
    Degraded,
    Failed,
}

impl ReconcileOutcome {
    pub fn is_degraded(self) -> bool {
        matches!(self, Self::Degraded)
    }
}

/// Structured result of applying a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileReport {
    pub planned: usize,
    pub changed: usize,
    pub unchanged: usize,
    pub skipped: usize,
    pub failed: usize,
    pub warnings: Vec<String>,
    pub outcome: ReconcileOutcome,
}

impl fmt::Display for CpuSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_range_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_set_is_sorted_deduplicated_and_stably_formatted() {
        let cpus = CpuSet::new([8, 2, 3, 3, 0, 1, 9]);
        assert_eq!(cpus.as_slice(), &[0, 1, 2, 3, 8, 9]);
        assert_eq!(cpus.len(), 6);
        assert!(cpus.contains(8));
        assert!(!cpus.contains(7));
        assert_eq!(cpus.to_range_string(), "0-3,8-9");
        assert_eq!(cpus.to_string(), "0-3,8-9");
    }

    #[test]
    fn empty_cpu_set_formats_as_empty_string() {
        assert_eq!(CpuSet::empty().to_range_string(), "");
    }

    #[test]
    fn cpu_set_parser_rejects_malformed_ranges() {
        assert_eq!(CpuSet::parse("0-3,8").unwrap().as_slice(), &[0, 1, 2, 3, 8]);
        assert!(matches!(
            CpuSet::parse("8-4"),
            Err(CpuSetParseError::InvertedRange { .. })
        ));
        assert!(matches!(
            CpuSet::parse("0-8192"),
            Err(CpuSetParseError::CpuOutOfRange(8192))
        ));
        assert!(matches!(
            CpuSet::parse("0,,2"),
            Err(CpuSetParseError::EmptyPart)
        ));
    }

    #[test]
    fn proxmox_defaults_to_one_socket_and_one_core() {
        let shape = VmCpuShape::from_proxmox(None, None, None, None, false).unwrap();
        assert_eq!(shape.sockets, 1);
        assert_eq!(shape.cores_per_socket, 1);
        assert_eq!(shape.max_vcpus, 1);
        assert_eq!(shape.active_vcpus, 1);
    }

    #[test]
    fn explicit_sockets_win_over_legacy_smp() {
        let shape = VmCpuShape::from_proxmox(Some(2), Some(8), Some(3), None, true).unwrap();
        assert_eq!(shape.max_vcpus, 6);
        assert_eq!(shape.active_vcpus, 6);
        assert!(shape.cpu_hotplug);
    }

    #[test]
    fn legacy_smp_is_used_when_sockets_are_missing() {
        let shape = VmCpuShape::from_proxmox(None, Some(4), Some(2), None, false).unwrap();
        assert_eq!(shape.sockets, 4);
        assert_eq!(shape.max_vcpus, 8);
    }

    #[test]
    fn configured_vcpus_are_active_but_max_is_preserved() {
        let shape = VmCpuShape::from_proxmox(Some(2), None, Some(4), Some(3), true).unwrap();
        assert_eq!(shape.max_vcpus, 8);
        assert_eq!(shape.active_vcpus, 3);
    }

    #[test]
    fn zero_vcpus_means_all_max_vcpus() {
        let shape = VmCpuShape::from_proxmox(Some(2), None, Some(4), Some(0), false).unwrap();
        assert_eq!(shape.active_vcpus, 8);
    }

    #[test]
    fn invalid_vcpu_shape_is_rejected() {
        assert_eq!(
            VmCpuShape::from_proxmox(Some(2), None, Some(2), Some(5), false),
            Err(VmCpuShapeError::ActiveExceedsMax { active: 5, max: 4 })
        );
        assert_eq!(
            VmCpuShape::from_proxmox(Some(0), None, Some(2), None, false),
            Err(VmCpuShapeError::ZeroSockets)
        );
    }
}
