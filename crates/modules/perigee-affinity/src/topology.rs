use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const SYSFS_BASE: &str = "/sys/devices/system/cpu";
const DEFAULT_CORES_PER_CCD: usize = 8;

/// Upper bound on a CPU index when parsing CPU/affinity lists. Guards against a
/// malformed or hostile range (e.g. `0-4294967295` from a hand-edited config)
/// expanding to billions of entries and exhausting memory. Comfortably above
/// the kernel's CONFIG_NR_CPUS ceiling (8192).
pub const MAX_LOGICAL_CPUS: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Architecture {
    Amd,
    IntelHybrid,
    Generic,
}

impl std::fmt::Display for Architecture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Amd => write!(f, "AMD"),
            Self::IntelHybrid => write!(f, "Intel Hybrid"),
            Self::Generic => write!(f, "Generic"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreType {
    Performance,
    Efficiency,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuInfo {
    pub id: usize,
    pub package_id: usize,
    pub core_id: usize,
    pub cluster_id: i32,
    pub die_id: i32,
    pub l3_cache_id: i32,
    pub thread_siblings: Vec<usize>,
    pub is_first_thread: bool,
    pub core_type: CoreType,
    pub capacity: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Package {
    pub id: usize,
    pub core_groups: Vec<CoreGroup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CoreGroup {
    pub id: usize,
    pub package_id: usize,
    pub core_type: CoreType,
    pub name: String,
    pub l3_cache_id: i32,
    pub physical_cpus: Vec<usize>,
    pub all_cpus: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CpuTopology {
    pub architecture: Architecture,
    pub total_cpus: usize,
    pub total_cores: usize,
    pub has_smt: bool,
    pub packages: Vec<Package>,
    pub core_groups: Vec<CoreGroup>,
    pub detect_method: String,
    /// Authoritative SMT sibling map: first-thread (physical) CPU id -> all
    /// thread siblings (including itself), read directly from
    /// `thread_siblings_list`. Used to expand a physical-core selection to
    /// logical vCPUs without guessing the sysfs enumeration layout.
    #[serde(skip)]
    pub thread_siblings: HashMap<usize, Vec<usize>>,
}

impl CpuTopology {
    pub fn p_cores_physical(&self) -> Vec<usize> {
        self.core_groups
            .iter()
            .filter(|g| g.core_type == CoreType::Performance)
            .flat_map(|g| g.physical_cpus.iter().copied())
            .collect()
    }

    pub fn e_cores_physical(&self) -> Vec<usize> {
        self.core_groups
            .iter()
            .filter(|g| g.core_type == CoreType::Efficiency)
            .flat_map(|g| g.physical_cpus.iter().copied())
            .collect()
    }
}

// ── Public API ──

pub fn detect() -> Result<CpuTopology> {
    let base = Path::new(SYSFS_BASE);
    if !base.is_dir() {
        bail!("sysfs CPU path not found: {}", SYSFS_BASE);
    }

    let cpu_ids = list_cpus(base)?;
    if cpu_ids.is_empty() {
        bail!("no CPUs found in sysfs");
    }

    let mut infos = Vec::with_capacity(cpu_ids.len());
    for id in &cpu_ids {
        infos.push(read_cpu_info(*id)?);
    }

    let arch = detect_architecture(&infos);
    let thread_siblings = build_sibling_map(&infos);
    let mut topo = match arch {
        Architecture::Amd => build_amd_topology(infos)?,
        Architecture::IntelHybrid => build_intel_hybrid_topology(infos)?,
        Architecture::Generic => build_generic_topology(infos)?,
    };
    topo.thread_siblings = thread_siblings;
    Ok(topo)
}

/// Map each physical (first-thread) CPU to its full set of thread siblings,
/// taken verbatim from the sysfs `thread_siblings_list`. This is the only
/// authoritative source for SMT pairing; reconstructing it from CPU index
/// arithmetic breaks on non-contiguous enumerations (e.g. AMD's cpu0/cpu1
/// pairing where physical cores are the even ids).
fn build_sibling_map(infos: &[CpuInfo]) -> HashMap<usize, Vec<usize>> {
    infos
        .iter()
        .filter(|c| c.is_first_thread)
        .map(|c| (c.id, c.thread_siblings.clone()))
        .collect()
}

// ── CPU enumeration ──

fn list_cpus(base: &Path) -> Result<Vec<usize>> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(base).context("reading sysfs cpu dir")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(num_str) = name.strip_prefix("cpu") {
            if let Ok(id) = num_str.parse::<usize>() {
                let topo_dir = entry.path().join("topology");
                if topo_dir.is_dir() {
                    ids.push(id);
                }
            }
        }
    }
    ids.sort();
    Ok(ids)
}

// ── Per-CPU info reading ──

fn read_cpu_info(cpu_id: usize) -> Result<CpuInfo> {
    let package_id = read_optional_int(&topo_path(cpu_id, "physical_package_id"), 0)?;
    let core_id = read_optional_int(&topo_path(cpu_id, "core_id"), cpu_id)?;
    let cluster_id = read_optional_int(&topo_path(cpu_id, "cluster_id"), -1i32 as usize)? as i32;
    let die_id = read_optional_int(&topo_path(cpu_id, "die_id"), -1i32 as usize)? as i32;
    let l3_cache_id = read_l3_cache_id(cpu_id).unwrap_or(-1);

    let mut siblings =
        read_optional_list(&topo_path(cpu_id, "thread_siblings_list"), vec![cpu_id])?;
    siblings.sort();
    siblings.dedup();

    let capacity = read_cpu_capacity(cpu_id);
    let core_type = detect_core_type(capacity, &siblings);

    let is_first_thread = siblings.is_empty() || cpu_id == siblings[0];

    Ok(CpuInfo {
        id: cpu_id,
        package_id,
        core_id,
        cluster_id,
        die_id,
        l3_cache_id,
        thread_siblings: siblings,
        is_first_thread,
        core_type,
        capacity,
    })
}

fn read_l3_cache_id(cpu_id: usize) -> Result<i32> {
    let cache_base = PathBuf::from(format!("{}/cpu{}/cache", SYSFS_BASE, cpu_id));
    if !cache_base.is_dir() {
        bail!("no cache dir");
    }
    for entry in std::fs::read_dir(&cache_base)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("index") {
            continue;
        }
        let level_path = entry.path().join("level");
        if let Ok(level) = read_int_file(&level_path) {
            if level == 3 {
                let id_path = entry.path().join("id");
                return read_int_file(&id_path).map(|v| v as i32);
            }
        }
    }
    bail!("L3 cache not found for cpu{}", cpu_id)
}

fn read_cpu_capacity(cpu_id: usize) -> u32 {
    let path = PathBuf::from(format!("{}/cpu{}/cpu_capacity", SYSFS_BASE, cpu_id));
    read_int_file(&path).unwrap_or(0) as u32
}

fn detect_core_type(capacity: u32, siblings: &[usize]) -> CoreType {
    if capacity >= 1000 {
        return CoreType::Performance;
    }
    if capacity > 0 && capacity < 900 {
        return CoreType::Efficiency;
    }
    // Fallback: if capacity is 0 or in 900-999 range, we can't determine
    // Intel hybrid heuristic: SMT -> P-core, no SMT -> E-core
    // This is only applied when we know we're on Intel hybrid (checked later)
    let _ = siblings;
    CoreType::Unknown
}

// ── Architecture detection ──

fn detect_architecture(cpus: &[CpuInfo]) -> Architecture {
    let vendor = read_cpu_vendor();
    match vendor.as_str() {
        "AuthenticAMD" | "AMD" => Architecture::Amd,
        "GenuineIntel" => {
            if has_hybrid_cores(cpus) {
                Architecture::IntelHybrid
            } else {
                Architecture::Generic
            }
        }
        _ => {
            if has_multiple_l3(cpus) {
                Architecture::Amd
            } else {
                Architecture::Generic
            }
        }
    }
}

fn read_cpu_vendor() -> String {
    let Ok(data) = std::fs::read_to_string("/proc/cpuinfo") else {
        return String::new();
    };
    for line in data.lines() {
        if line.starts_with("vendor_id") {
            if let Some(val) = line.split(':').nth(1) {
                return val.trim().to_string();
            }
        }
    }
    String::new()
}

fn has_hybrid_cores(cpus: &[CpuInfo]) -> bool {
    let mut caps = std::collections::HashSet::new();
    for cpu in cpus {
        if cpu.capacity > 0 {
            caps.insert(cpu.capacity);
        }
    }
    caps.len() > 1
}

fn has_multiple_l3(cpus: &[CpuInfo]) -> bool {
    let mut l3s = std::collections::HashSet::new();
    for cpu in cpus {
        if cpu.l3_cache_id >= 0 {
            l3s.insert(cpu.l3_cache_id);
        }
    }
    l3s.len() > 1
}

// ── CCD detection method ──

fn detect_ccd_method(cpus: &[CpuInfo]) -> &'static str {
    if cpus.is_empty() {
        return "inferred";
    }

    let all_have_l3 = cpus.iter().all(|c| c.l3_cache_id >= 0);
    if all_have_l3 {
        let unique: std::collections::HashSet<i32> = cpus.iter().map(|c| c.l3_cache_id).collect();
        if unique.len() > 1 {
            return "l3_cache";
        }
    }

    if cpus.iter().all(|c| c.cluster_id >= 0) {
        return "cluster_id";
    }

    if cpus.iter().all(|c| c.die_id >= 0) {
        return "die_id";
    }

    "inferred"
}

// ── Topology builders ──

fn build_amd_topology(infos: Vec<CpuInfo>) -> Result<CpuTopology> {
    let total_cpus = infos.len();
    let total_cores = infos.iter().filter(|c| c.is_first_thread).count();
    let has_smt = total_cpus > total_cores;

    let method = detect_ccd_method(&infos);
    let core_groups = group_by_ccd(&infos, method);

    let packages = build_packages(&core_groups);

    Ok(CpuTopology {
        architecture: Architecture::Amd,
        total_cpus,
        total_cores,
        has_smt,
        packages,
        core_groups,
        detect_method: method.to_string(),
        thread_siblings: HashMap::new(),
    })
}

fn build_intel_hybrid_topology(infos: Vec<CpuInfo>) -> Result<CpuTopology> {
    let total_cpus = infos.len();
    let total_cores = infos.iter().filter(|c| c.is_first_thread).count();
    let has_smt = total_cpus > total_cores;

    let core_groups = group_by_intel_core_type(&infos);
    let packages = build_packages(&core_groups);

    Ok(CpuTopology {
        architecture: Architecture::IntelHybrid,
        total_cpus,
        total_cores,
        has_smt,
        packages,
        core_groups,
        detect_method: "intel_hybrid".to_string(),
        thread_siblings: HashMap::new(),
    })
}

fn build_generic_topology(infos: Vec<CpuInfo>) -> Result<CpuTopology> {
    let total_cpus = infos.len();
    let total_cores = infos.iter().filter(|c| c.is_first_thread).count();
    let has_smt = total_cpus > total_cores;

    let mut all_cpus: Vec<usize> = infos.iter().map(|c| c.id).collect();
    all_cpus.sort();
    let mut physical_cpus: Vec<usize> = infos
        .iter()
        .filter(|c| c.is_first_thread)
        .map(|c| c.id)
        .collect();
    physical_cpus.sort();

    let cg = CoreGroup {
        id: 0,
        package_id: 0,
        core_type: CoreType::Unknown,
        name: "All Cores".to_string(),
        l3_cache_id: -1,
        physical_cpus,
        all_cpus,
    };

    let core_groups = vec![cg];
    let packages = vec![Package {
        id: 0,
        core_groups: core_groups.clone(),
    }];

    Ok(CpuTopology {
        architecture: Architecture::Generic,
        total_cpus,
        total_cores,
        has_smt,
        packages,
        core_groups,
        detect_method: "generic".to_string(),
        thread_siblings: HashMap::new(),
    })
}

fn group_by_ccd(cpus: &[CpuInfo], method: &str) -> Vec<CoreGroup> {
    #[derive(Hash, Eq, PartialEq, Clone)]
    struct Key {
        pkg: usize,
        ccd: i32,
    }

    let mut groups: HashMap<Key, CoreGroup> = HashMap::new();

    for cpu in cpus {
        let ccd_id: i32 = match method {
            "l3_cache" => cpu.l3_cache_id,
            "cluster_id" => cpu.cluster_id,
            "die_id" => cpu.die_id,
            _ => (cpu.core_id / DEFAULT_CORES_PER_CCD) as i32,
        };

        let key = Key {
            pkg: cpu.package_id,
            ccd: ccd_id,
        };

        let cg = groups.entry(key.clone()).or_insert_with(|| CoreGroup {
            id: ccd_id as usize,
            package_id: cpu.package_id,
            core_type: CoreType::Unknown,
            name: format!("CCD {}", ccd_id),
            l3_cache_id: cpu.l3_cache_id,
            physical_cpus: Vec::new(),
            all_cpus: Vec::new(),
        });

        cg.all_cpus.push(cpu.id);
        if cpu.is_first_thread {
            cg.physical_cpus.push(cpu.id);
        }
    }

    let mut list: Vec<CoreGroup> = groups.into_values().collect();
    for cg in &mut list {
        cg.all_cpus.sort();
        cg.all_cpus.dedup();
        cg.physical_cpus.sort();
        cg.physical_cpus.dedup();
    }

    list.sort_by(|a, b| a.package_id.cmp(&b.package_id).then(a.id.cmp(&b.id)));

    // On a multi-socket host, CCD ids restart per package, so a bare "CCD 0"
    // is ambiguous and dedup-by-name would merge both sockets' CCD 0. Prefix
    // with the package when there is more than one.
    let multi_socket = list
        .iter()
        .map(|cg| cg.package_id)
        .collect::<std::collections::HashSet<_>>()
        .len()
        > 1;

    // Renumber CCD IDs per package
    let mut pkg_count: HashMap<usize, usize> = HashMap::new();
    for cg in &mut list {
        let idx = pkg_count.entry(cg.package_id).or_insert(0);
        cg.id = *idx;
        cg.name = if multi_socket {
            format!("P{} CCD{}", cg.package_id, *idx)
        } else {
            format!("CCD {}", *idx)
        };
        *idx += 1;
    }

    list
}

fn group_by_intel_core_type(cpus: &[CpuInfo]) -> Vec<CoreGroup> {
    let mut p_cores = CoreGroup {
        id: 0,
        package_id: 0,
        core_type: CoreType::Performance,
        name: "P-Cores".to_string(),
        l3_cache_id: -1,
        physical_cpus: Vec::new(),
        all_cpus: Vec::new(),
    };
    let mut e_cores = CoreGroup {
        id: 1,
        package_id: 0,
        core_type: CoreType::Efficiency,
        name: "E-Cores".to_string(),
        l3_cache_id: -1,
        physical_cpus: Vec::new(),
        all_cpus: Vec::new(),
    };

    for cpu in cpus {
        match cpu.core_type {
            CoreType::Performance => {
                p_cores.all_cpus.push(cpu.id);
                if cpu.is_first_thread {
                    p_cores.physical_cpus.push(cpu.id);
                }
            }
            CoreType::Efficiency => {
                e_cores.all_cpus.push(cpu.id);
                if cpu.is_first_thread {
                    e_cores.physical_cpus.push(cpu.id);
                }
            }
            CoreType::Unknown => {
                // Heuristic: SMT-capable -> P-core, single-thread -> E-core
                if cpu.thread_siblings.len() > 1 {
                    p_cores.all_cpus.push(cpu.id);
                    if cpu.is_first_thread {
                        p_cores.physical_cpus.push(cpu.id);
                    }
                } else {
                    e_cores.all_cpus.push(cpu.id);
                    if cpu.is_first_thread {
                        e_cores.physical_cpus.push(cpu.id);
                    }
                }
            }
        }
    }

    for g in [&mut p_cores, &mut e_cores] {
        g.all_cpus.sort();
        g.physical_cpus.sort();
    }

    let mut groups = Vec::new();
    if !p_cores.physical_cpus.is_empty() {
        groups.push(p_cores);
    }
    if !e_cores.physical_cpus.is_empty() {
        groups.push(e_cores);
    }
    groups
}

fn build_packages(core_groups: &[CoreGroup]) -> Vec<Package> {
    let mut pkg_map: HashMap<usize, Vec<CoreGroup>> = HashMap::new();
    for cg in core_groups {
        pkg_map.entry(cg.package_id).or_default().push(cg.clone());
    }

    let mut pkg_ids: Vec<usize> = pkg_map.keys().copied().collect();
    pkg_ids.sort();

    pkg_ids
        .into_iter()
        .map(|id| {
            let mut groups = pkg_map.remove(&id).unwrap();
            groups.sort_by_key(|g| g.id);
            Package {
                id,
                core_groups: groups,
            }
        })
        .collect()
}

// ── Sysfs helpers ──

fn topo_path(cpu_id: usize, element: &str) -> PathBuf {
    PathBuf::from(format!("{}/cpu{}/topology/{}", SYSFS_BASE, cpu_id, element))
}

fn read_int_file(path: &Path) -> Result<usize> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    content
        .trim()
        .parse::<usize>()
        .with_context(|| format!("parsing {} from {}", content.trim(), path.display()))
}

fn read_optional_int(path: &Path, default: usize) -> Result<usize> {
    match read_int_file(path) {
        Ok(v) => Ok(v),
        Err(_) if !path.exists() => Ok(default),
        Err(e) => Err(e),
    }
}

fn read_optional_list(path: &Path, default: Vec<usize>) -> Result<Vec<usize>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) if !path.exists() => return Ok(default),
        Err(e) => return Err(e.into()),
    };
    parse_cpu_list(content.trim())
}

fn parse_cpu_list(s: &str) -> Result<Vec<usize>> {
    let mut result = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((start, end)) = part.split_once('-') {
            let start: usize = start.trim().parse()?;
            let end: usize = end.trim().parse()?;
            if start > end || end >= MAX_LOGICAL_CPUS {
                bail!("invalid CPU range {}-{}", start, end);
            }
            for i in start..=end {
                result.push(i);
            }
        } else {
            result.push(part.parse()?);
        }
    }
    Ok(result)
}
