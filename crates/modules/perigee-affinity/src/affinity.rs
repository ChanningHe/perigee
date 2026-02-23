use crate::topology::{Architecture, CoreGroup, CpuTopology};
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Strategy {
    SingleCcd,
    Balanced,
    Manual,
    PCoresOnly,
    ECoresOnly,
    AllCores,
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SingleCcd => write!(f, "single-ccd"),
            Self::Balanced => write!(f, "balanced"),
            Self::Manual => write!(f, "manual"),
            Self::PCoresOnly => write!(f, "p-cores-only"),
            Self::ECoresOnly => write!(f, "e-cores-only"),
            Self::AllCores => write!(f, "all-cores"),
        }
    }
}

impl std::str::FromStr for Strategy {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "single-ccd" => Ok(Self::SingleCcd),
            "balanced" => Ok(Self::Balanced),
            "manual" => Ok(Self::Manual),
            "p-cores-only" => Ok(Self::PCoresOnly),
            "e-cores-only" => Ok(Self::ECoresOnly),
            "all-cores" => Ok(Self::AllCores),
            _ => bail!("unknown strategy: {}", s),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AffinityOption {
    pub strategy: Strategy,
    pub name: String,
    pub description: String,
    pub cpus: Vec<usize>,
    pub affinity_str: String,
    pub ccds_used: Vec<String>,
    pub available: bool,
}

#[derive(Debug, Clone)]
pub struct AffinityRequest {
    pub cores_needed: usize,
    pub include_smt: bool,
    pub topology: CpuTopology,
    pub existing_bindings: Vec<VmBinding>,
}

#[derive(Debug, Clone)]
pub struct VmBinding {
    pub vmid: u32,
    pub cpus: Vec<usize>,
}

// ── Public API ──

pub fn generate(req: &AffinityRequest) -> Result<Vec<AffinityOption>> {
    if req.cores_needed == 0 {
        bail!("cores needed must be > 0");
    }

    let physical_needed = if req.include_smt && req.topology.has_smt {
        (req.cores_needed + 1) / 2
    } else {
        req.cores_needed
    };

    if physical_needed > req.topology.total_cores {
        bail!(
            "not enough cores: need {} physical for {} vCPUs, only {} available",
            physical_needed,
            req.cores_needed,
            req.topology.total_cores
        );
    }

    match req.topology.architecture {
        Architecture::IntelHybrid => generate_intel_options(req, physical_needed),
        _ => generate_amd_options(req, physical_needed),
    }
}

pub fn generate_manual(
    req: &AffinityRequest,
    selected_ccd_indices: &[usize],
) -> Result<AffinityOption> {
    if selected_ccd_indices.is_empty() {
        bail!("no CCDs selected");
    }

    let physical_needed = if req.include_smt && req.topology.has_smt {
        (req.cores_needed + 1) / 2
    } else {
        req.cores_needed
    };

    let mut selected_physical = Vec::new();
    let mut ccd_names = Vec::new();
    for &idx in selected_ccd_indices {
        if idx >= req.topology.core_groups.len() {
            continue;
        }
        let cg = &req.topology.core_groups[idx];
        ccd_names.push(cg.name.clone());
        for &phys in &cg.physical_cpus {
            if selected_physical.len() >= physical_needed {
                break;
            }
            selected_physical.push(phys);
        }
    }

    if selected_physical.len() < physical_needed {
        bail!(
            "selected CCDs only have {} cores, need {}",
            selected_physical.len(),
            physical_needed
        );
    }

    let cpus = expand_to_vcpus(&selected_physical, req.include_smt, &req.topology);
    Ok(AffinityOption {
        strategy: Strategy::Manual,
        name: "Manual".to_string(),
        description: format!("Manually selected {} CCDs", selected_ccd_indices.len()),
        affinity_str: format_cpus(&cpus),
        cpus,
        ccds_used: ccd_names,
        available: true,
    })
}

// ── AMD strategies ──

fn generate_amd_options(req: &AffinityRequest, physical_needed: usize) -> Result<Vec<AffinityOption>> {
    let mut options = vec![
        generate_single_ccd(req, physical_needed),
        generate_balanced(req, physical_needed),
        generate_manual_placeholder(req, physical_needed),
    ];

    for opt in &mut options {
        if opt.available && opt.strategy != Strategy::Manual {
            opt.affinity_str = format_cpus(&opt.cpus);
        }
    }

    Ok(options)
}

fn generate_single_ccd(req: &AffinityRequest, physical_needed: usize) -> AffinityOption {
    for cg in &req.topology.core_groups {
        if cg.physical_cpus.len() >= physical_needed {
            let physical: Vec<usize> = cg.physical_cpus[..physical_needed].to_vec();
            let cpus = expand_to_vcpus(&physical, req.include_smt, &req.topology);
            return AffinityOption {
                strategy: Strategy::SingleCcd,
                name: "Single CCD".to_string(),
                description: "All cores from one CCD (best L3 locality)".to_string(),
                affinity_str: format_cpus(&cpus),
                cpus,
                ccds_used: vec![cg.name.clone()],
                available: true,
            };
        }
    }

    AffinityOption {
        strategy: Strategy::SingleCcd,
        name: "Single CCD".to_string(),
        description: format!(
            "Not available: no single CCD has {} cores",
            physical_needed
        ),
        cpus: Vec::new(),
        affinity_str: String::new(),
        ccds_used: Vec::new(),
        available: false,
    }
}

fn generate_balanced(req: &AffinityRequest, physical_needed: usize) -> AffinityOption {
    let ccd_load = build_ccd_load(&req.topology.core_groups, &req.existing_bindings);

    // Sort CCDs by load: least bound_cores first, then least bound_vms
    let mut ccd_order: Vec<usize> = (0..req.topology.core_groups.len()).collect();
    ccd_order.sort_by(|&a, &b| {
        let la = ccd_load.get(&a).map(|l| l.bound_cores.len()).unwrap_or(0);
        let lb = ccd_load.get(&b).map(|l| l.bound_cores.len()).unwrap_or(0);
        let va = ccd_load.get(&a).map(|l| l.bound_vms).unwrap_or(0);
        let vb = ccd_load.get(&b).map(|l| l.bound_vms).unwrap_or(0);
        la.cmp(&lb).then(va.cmp(&vb))
    });

    // Try single CCD from least loaded first
    for &idx in &ccd_order {
        let cg = &req.topology.core_groups[idx];
        if cg.physical_cpus.len() >= physical_needed {
            let load = ccd_load.get(&idx);
            let bound: HashSet<usize> = load
                .map(|l| l.bound_cores.clone())
                .unwrap_or_default();

            // Prefer unbound cores within this CCD
            let mut free: Vec<usize> = cg
                .physical_cpus
                .iter()
                .filter(|c| !bound.contains(c))
                .copied()
                .collect();
            let mut occupied: Vec<usize> = cg
                .physical_cpus
                .iter()
                .filter(|c| bound.contains(c))
                .copied()
                .collect();
            free.append(&mut occupied);

            if free.len() >= physical_needed {
                let selected: Vec<usize> = free[..physical_needed].to_vec();
                let cpus = expand_to_vcpus(&selected, req.include_smt, &req.topology);

                let busy = build_busy_ccd_names(&ccd_load, &req.topology.core_groups, idx);
                let desc = if busy.is_empty() {
                    "Least-used CCD (even VM distribution)".to_string()
                } else {
                    format!(
                        "Least-used CCD (even VM distribution)  ·  {} busy",
                        busy.join(", ")
                    )
                };

                return AffinityOption {
                    strategy: Strategy::Balanced,
                    name: "Balanced".to_string(),
                    description: desc,
                    affinity_str: format_cpus(&cpus),
                    cpus,
                    ccds_used: vec![cg.name.clone()],
                    available: true,
                };
            }
        }
    }

    // Multi-CCD: take from least loaded CCDs
    let mut selected_physical = Vec::new();
    let mut used_ccds = Vec::new();
    for &idx in &ccd_order {
        if selected_physical.len() >= physical_needed {
            break;
        }
        let cg = &req.topology.core_groups[idx];
        let load = ccd_load.get(&idx);
        let bound: HashSet<usize> = load
            .map(|l| l.bound_cores.clone())
            .unwrap_or_default();

        let mut free: Vec<usize> = cg
            .physical_cpus
            .iter()
            .filter(|c| !bound.contains(c))
            .copied()
            .collect();
        let mut occupied: Vec<usize> = cg
            .physical_cpus
            .iter()
            .filter(|c| bound.contains(c))
            .copied()
            .collect();
        free.append(&mut occupied);

        let mut took = false;
        for cpu in free {
            if selected_physical.len() >= physical_needed {
                break;
            }
            selected_physical.push(cpu);
            took = true;
        }
        if took {
            used_ccds.push(cg.name.clone());
        }
    }

    let cpus = expand_to_vcpus(&selected_physical, req.include_smt, &req.topology);
    AffinityOption {
        strategy: Strategy::Balanced,
        name: "Balanced".to_string(),
        description: "Least-used CCDs (even VM distribution)".to_string(),
        affinity_str: format_cpus(&cpus),
        cpus,
        ccds_used: used_ccds,
        available: !selected_physical.is_empty(),
    }
}

fn generate_manual_placeholder(
    req: &AffinityRequest,
    physical_needed: usize,
) -> AffinityOption {
    let cores_per_ccd = req
        .topology
        .core_groups
        .first()
        .map(|g| g.physical_cpus.len())
        .unwrap_or(8);
    let min_ccds = if cores_per_ccd > 0 {
        (physical_needed + cores_per_ccd - 1) / cores_per_ccd
    } else {
        1
    };

    AffinityOption {
        strategy: Strategy::Manual,
        name: "Manual".to_string(),
        description: format!("Select CCDs manually (min {})", min_ccds),
        cpus: Vec::new(),
        affinity_str: String::new(),
        ccds_used: Vec::new(),
        available: true,
    }
}

// ── Intel strategies ──

fn generate_intel_options(req: &AffinityRequest, physical_needed: usize) -> Result<Vec<AffinityOption>> {
    let mut options = vec![
        generate_p_cores_only(req, physical_needed),
        generate_e_cores_only(req, physical_needed),
        generate_all_cores(req, physical_needed),
        generate_manual_placeholder(req, physical_needed),
    ];

    for opt in &mut options {
        if opt.available && opt.strategy != Strategy::Manual {
            opt.affinity_str = format_cpus(&opt.cpus);
        }
    }

    Ok(options)
}

fn generate_p_cores_only(req: &AffinityRequest, physical_needed: usize) -> AffinityOption {
    let p_cores = req.topology.p_cores_physical();
    if p_cores.len() < physical_needed {
        return AffinityOption {
            strategy: Strategy::PCoresOnly,
            name: "P-Cores Only".to_string(),
            description: format!(
                "Not available: only {} P-cores, need {}",
                p_cores.len(),
                physical_needed
            ),
            cpus: Vec::new(),
            affinity_str: String::new(),
            ccds_used: Vec::new(),
            available: false,
        };
    }

    let selected = p_cores[..physical_needed].to_vec();
    let cpus = expand_to_vcpus(&selected, req.include_smt, &req.topology);
    AffinityOption {
        strategy: Strategy::PCoresOnly,
        name: "P-Cores Only".to_string(),
        description: "Performance cores only (best single-thread)".to_string(),
        affinity_str: format_cpus(&cpus),
        cpus,
        ccds_used: vec!["P-Cores".to_string()],
        available: true,
    }
}

fn generate_e_cores_only(req: &AffinityRequest, physical_needed: usize) -> AffinityOption {
    let e_cores = req.topology.e_cores_physical();
    if e_cores.len() < physical_needed {
        return AffinityOption {
            strategy: Strategy::ECoresOnly,
            name: "E-Cores Only".to_string(),
            description: format!(
                "Not available: only {} E-cores, need {}",
                e_cores.len(),
                physical_needed
            ),
            cpus: Vec::new(),
            affinity_str: String::new(),
            ccds_used: Vec::new(),
            available: false,
        };
    }

    let selected = e_cores[..physical_needed].to_vec();
    let cpus = expand_to_vcpus(&selected, req.include_smt, &req.topology);
    AffinityOption {
        strategy: Strategy::ECoresOnly,
        name: "E-Cores Only".to_string(),
        description: "Efficiency cores only (power efficient)".to_string(),
        affinity_str: format_cpus(&cpus),
        cpus,
        ccds_used: vec!["E-Cores".to_string()],
        available: true,
    }
}

fn generate_all_cores(req: &AffinityRequest, physical_needed: usize) -> AffinityOption {
    let mut all: Vec<usize> = req.topology.p_cores_physical();
    all.extend(req.topology.e_cores_physical());
    all.sort();

    if all.len() < physical_needed {
        return AffinityOption {
            strategy: Strategy::AllCores,
            name: "All Cores".to_string(),
            description: format!(
                "Not available: only {} cores total, need {}",
                all.len(),
                physical_needed
            ),
            cpus: Vec::new(),
            affinity_str: String::new(),
            ccds_used: Vec::new(),
            available: false,
        };
    }

    let selected = all[..physical_needed].to_vec();
    let cpus = expand_to_vcpus(&selected, req.include_smt, &req.topology);

    let mut used = Vec::new();
    let p_set: HashSet<usize> = req.topology.p_cores_physical().into_iter().collect();
    let e_set: HashSet<usize> = req.topology.e_cores_physical().into_iter().collect();
    if selected.iter().any(|c| p_set.contains(c)) {
        used.push("P-Cores".to_string());
    }
    if selected.iter().any(|c| e_set.contains(c)) {
        used.push("E-Cores".to_string());
    }

    AffinityOption {
        strategy: Strategy::AllCores,
        name: "All Cores".to_string(),
        description: "P-cores + E-cores (maximum throughput)".to_string(),
        affinity_str: format_cpus(&cpus),
        cpus,
        ccds_used: used,
        available: true,
    }
}

// ── Balanced helper: CCD load tracking ──

struct CcdLoad {
    bound_vms: usize,
    bound_cores: HashSet<usize>,
}

fn build_ccd_load(
    core_groups: &[CoreGroup],
    bindings: &[VmBinding],
) -> HashMap<usize, CcdLoad> {
    let mut cpu_to_ccd: HashMap<usize, usize> = HashMap::new();
    for (idx, cg) in core_groups.iter().enumerate() {
        for &cpu in &cg.all_cpus {
            cpu_to_ccd.insert(cpu, idx);
        }
    }

    let mut loads: HashMap<usize, CcdLoad> = HashMap::new();
    for binding in bindings {
        let mut touched_ccds: HashSet<usize> = HashSet::new();
        for &cpu in &binding.cpus {
            if let Some(&ccd_idx) = cpu_to_ccd.get(&cpu) {
                let load = loads.entry(ccd_idx).or_insert(CcdLoad {
                    bound_vms: 0,
                    bound_cores: HashSet::new(),
                });
                load.bound_cores.insert(cpu);
                touched_ccds.insert(ccd_idx);
            }
        }
        for ccd_idx in touched_ccds {
            loads.entry(ccd_idx).and_modify(|l| l.bound_vms += 1);
        }
    }

    loads
}

fn build_busy_ccd_names(
    ccd_load: &HashMap<usize, CcdLoad>,
    core_groups: &[CoreGroup],
    exclude_idx: usize,
) -> Vec<String> {
    let mut busy = Vec::new();
    for (&idx, load) in ccd_load {
        if idx != exclude_idx && load.bound_vms > 0 {
            if let Some(cg) = core_groups.get(idx) {
                busy.push(cg.name.clone());
            }
        }
    }
    busy.sort();
    busy
}

// ── SMT expansion ──

fn expand_to_vcpus(physical_cores: &[usize], include_smt: bool, topo: &CpuTopology) -> Vec<usize> {
    if !include_smt || !topo.has_smt {
        let mut result = physical_cores.to_vec();
        result.sort();
        return result;
    }

    let mut phys_to_siblings: HashMap<usize, Vec<usize>> = HashMap::new();
    for cg in &topo.core_groups {
        let num_physical = cg.physical_cpus.len();
        for (i, &phys) in cg.physical_cpus.iter().enumerate() {
            let mut siblings = vec![phys];
            if i + num_physical < cg.all_cpus.len() {
                siblings.push(cg.all_cpus[i + num_physical]);
            }
            phys_to_siblings.insert(phys, siblings);
        }
    }

    let mut result = Vec::with_capacity(physical_cores.len() * 2);
    for &phys in physical_cores {
        if let Some(siblings) = phys_to_siblings.get(&phys) {
            result.extend(siblings);
        } else {
            result.push(phys);
        }
    }

    result.sort();
    result.dedup();
    result
}

// ── Format ──

pub fn format_cpus(cpus: &[usize]) -> String {
    if cpus.is_empty() {
        return String::new();
    }
    let mut sorted = cpus.to_vec();
    sorted.sort();
    sorted.dedup();

    let mut parts = Vec::new();
    let mut start = sorted[0];
    let mut prev = sorted[0];

    for &current in &sorted[1..] {
        if current == prev + 1 {
            prev = current;
        } else {
            parts.push(format_range(start, prev));
            start = current;
            prev = current;
        }
    }
    parts.push(format_range(start, prev));
    parts.join(",")
}

fn format_range(start: usize, end: usize) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{}-{}", start, end)
    }
}

pub fn parse_affinity_str(s: &str) -> Vec<usize> {
    let mut result = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            if let (Ok(start), Ok(end)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>()) {
                for i in start..=end {
                    result.push(i);
                }
            }
        } else if let Ok(v) = part.parse::<usize>() {
            result.push(v);
        }
    }
    result
}

/// Map a set of CPUs to the CCD names they belong to.
pub fn cpus_to_ccd_names(cpus: &[usize], core_groups: &[CoreGroup]) -> Vec<String> {
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    for cg in core_groups {
        if seen.contains(&cg.name) {
            continue;
        }
        if cpus.iter().any(|c| cg.all_cpus.contains(c)) {
            names.push(cg.name.clone());
            seen.insert(cg.name.clone());
        }
    }
    names
}
