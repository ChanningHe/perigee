//! Sysfs adapter for the normalized affinity topology model.
//!
//! This adapter records only facts exposed by sysfs. It never invents CCDs or
//! classifies unknown cores from their sibling count.

use crate::model::{
    CacheDomainSource, CoreKind, CpuSet, CpuTopology, LogicalCpu, PhysicalCore, PhysicalCoreId,
    TopologyQuality,
};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_SYSFS_CPU: &str = "/sys/devices/system/cpu";
const MAX_LOGICAL_CPUS: usize = 8192;

pub fn detect() -> Result<CpuTopology> {
    detect_from(Path::new(DEFAULT_SYSFS_CPU))
}

/// Detect a topology from a sysfs-like CPU directory. This is public so tests
/// and future platform adapters can use fixtures instead of the live host.
pub fn detect_from(cpu_root: &Path) -> Result<CpuTopology> {
    if !cpu_root.is_dir() {
        bail!("CPU sysfs path not found: {}", cpu_root.display());
    }

    let cpu_ids = list_cpu_ids(cpu_root)?;
    if cpu_ids.is_empty() {
        bail!("no CPUs found in {}", cpu_root.display());
    }

    let mut logical = Vec::with_capacity(cpu_ids.len());
    let mut cores: BTreeMap<PhysicalCoreId, PhysicalCoreBuilder> = BTreeMap::new();
    let mut cache_sources = Vec::new();
    let mut all_have_siblings = true;
    let mut all_have_physical = true;

    for cpu_id in cpu_ids {
        let dir = cpu_root.join(format!("cpu{cpu_id}"));
        let package_id = read_optional_usize(&dir.join("topology/physical_package_id"))?;
        let core_id = read_optional_usize(&dir.join("topology/core_id"))?;
        let physical_core = core_id.map(|core_id| PhysicalCoreId {
            package_id,
            core_id,
        });
        all_have_physical &= physical_core.is_some();

        let siblings = read_optional_cpu_list(&dir.join("topology/thread_siblings_list"))?;
        all_have_siblings &= siblings.is_some();

        let numa_node = read_numa_node(&dir);
        let (cache_domain, cache_source) = read_cache_domain(&dir, package_id, numa_node)?;
        cache_sources.push(cache_source);

        let kind = read_core_kind(&dir);
        logical.push(LogicalCpu {
            id: cpu_id,
            physical_core,
            package_id,
            numa_node,
            cache_domain,
        });

        if let Some(core_id) = physical_core {
            let builder = cores.entry(core_id).or_insert_with(|| PhysicalCoreBuilder {
                id: core_id,
                threads: BTreeSet::new(),
                kind: CoreKind::Unknown,
                package_id,
                numa_node,
                cache_domain,
            });
            builder.threads.insert(cpu_id);
            builder.kind = merge_core_kind(builder.kind, kind);
            builder.numa_node = builder.numa_node.or(numa_node);
            builder.cache_domain = builder.cache_domain.or(cache_domain);
        }
    }

    logical.sort_by_key(|cpu| cpu.id);
    let physical_cores: Vec<PhysicalCore> = cores
        .into_values()
        .map(PhysicalCoreBuilder::finish)
        .collect();

    let cache_domain_source = best_cache_source(&cache_sources);
    let quality = topology_quality(
        !logical.is_empty(),
        all_have_physical,
        all_have_siblings,
        cache_domain_source,
    );
    let fingerprint = fingerprint(&logical, &physical_cores, quality, cache_domain_source);

    Ok(CpuTopology {
        logical_cpus: logical,
        physical_cores,
        quality,
        cache_domain_source,
        fingerprint,
    })
}

#[derive(Debug)]
struct PhysicalCoreBuilder {
    id: PhysicalCoreId,
    threads: BTreeSet<usize>,
    kind: CoreKind,
    package_id: Option<usize>,
    numa_node: Option<usize>,
    cache_domain: Option<usize>,
}

impl PhysicalCoreBuilder {
    fn finish(self) -> PhysicalCore {
        PhysicalCore {
            id: self.id,
            threads: CpuSet::new(self.threads),
            kind: self.kind,
            package_id: self.package_id,
            numa_node: self.numa_node,
            cache_domain: self.cache_domain,
        }
    }
}

fn list_cpu_ids(root: &Path) -> Result<Vec<usize>> {
    let mut ids = Vec::new();
    for entry in fs::read_dir(root).context("reading CPU sysfs directory")? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(id) = name
            .strip_prefix("cpu")
            .and_then(|value| value.parse().ok())
        else {
            continue;
        };
        if id >= MAX_LOGICAL_CPUS {
            bail!("CPU id {id} exceeds supported limit {MAX_LOGICAL_CPUS}");
        }
        if entry.path().join("topology").is_dir() {
            ids.push(id);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

fn read_optional_usize(path: &Path) -> Result<Option<usize>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(
            value
                .trim()
                .parse()
                .with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn read_optional_cpu_list(path: &Path) -> Result<Option<Vec<usize>>> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(Some(parse_cpu_list(value.trim())?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn parse_cpu_list(value: &str) -> Result<Vec<usize>> {
    Ok(CpuSet::parse(value)
        .map_err(|error| anyhow::anyhow!("invalid CPU list: {error}"))?
        .into_vec())
}

fn read_numa_node(cpu_dir: &Path) -> Option<usize> {
    fs::read_dir(cpu_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .find_map(|name| name.strip_prefix("node")?.parse().ok())
}

fn read_cache_domain(
    cpu_dir: &Path,
    package_id: Option<usize>,
    numa_node: Option<usize>,
) -> Result<(Option<usize>, CacheDomainSource)> {
    let cache_dir = cpu_dir.join("cache");
    if cache_dir.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(&cache_dir)?
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("index"))
            })
            .collect();
        entries.sort();
        for entry in entries {
            if read_optional_usize(&entry.join("level"))? == Some(3) {
                if let Some(id) = read_optional_usize(&entry.join("id"))? {
                    return Ok((Some(id), CacheDomainSource::L3Sysfs));
                }
                if let Some(shared) = read_optional_cpu_list(&entry.join("shared_cpu_list"))? {
                    return Ok((Some(stable_list_id(&shared)), CacheDomainSource::L3Sysfs));
                }
            }
        }
    }

    let topology = cpu_dir.join("topology");
    for (file, source) in [
        ("cluster_id", CacheDomainSource::ClusterSysfs),
        ("die_id", CacheDomainSource::DieSysfs),
    ] {
        if let Some(id) = read_optional_usize(&topology.join(file))? {
            return Ok((Some(id), source));
        }
    }
    if let Some(node) = numa_node {
        return Ok((Some(node), CacheDomainSource::NumaFallback));
    }
    if let Some(package) = package_id {
        return Ok((Some(package), CacheDomainSource::PackageFallback));
    }
    Ok((None, CacheDomainSource::Unknown))
}

fn stable_list_id(cpus: &[usize]) -> usize {
    // A cache-domain ID is only an internal grouping key when sysfs omits the
    // numeric ID. The list itself is canonical and therefore deterministic.
    cpus.iter().fold(0usize, |hash, cpu| {
        hash.wrapping_mul(1_000_003).wrapping_add(cpu + 1)
    })
}

fn read_core_kind(cpu_dir: &Path) -> CoreKind {
    let path = cpu_dir.join("cpu_capacity");
    let Ok(value) = fs::read_to_string(path) else {
        return CoreKind::Unknown;
    };
    let Ok(capacity) = value.trim().parse::<u32>() else {
        return CoreKind::Unknown;
    };
    if capacity >= 900 {
        CoreKind::Performance
    } else if capacity > 0 {
        CoreKind::Efficiency
    } else {
        CoreKind::Unknown
    }
}

fn merge_core_kind(old: CoreKind, new: CoreKind) -> CoreKind {
    match (old, new) {
        (CoreKind::Unknown, kind) | (kind, CoreKind::Unknown) => kind,
        (a, b) if a == b => a,
        _ => CoreKind::Unknown,
    }
}

fn best_cache_source(sources: &[CacheDomainSource]) -> CacheDomainSource {
    sources
        .iter()
        .copied()
        .min_by_key(|source| match source {
            CacheDomainSource::L3Sysfs => 0,
            CacheDomainSource::ClusterSysfs => 1,
            CacheDomainSource::DieSysfs => 2,
            CacheDomainSource::NumaFallback => 3,
            CacheDomainSource::PackageFallback => 4,
            CacheDomainSource::Unknown => 5,
        })
        .unwrap_or(CacheDomainSource::Unknown)
}

fn topology_quality(
    has_logical: bool,
    all_have_physical: bool,
    all_have_siblings: bool,
    cache_source: CacheDomainSource,
) -> TopologyQuality {
    if !has_logical {
        return TopologyQuality::Invalid;
    }
    if !all_have_physical {
        return TopologyQuality::LogicalOnly;
    }
    if all_have_siblings
        && matches!(
            cache_source,
            CacheDomainSource::L3Sysfs
                | CacheDomainSource::ClusterSysfs
                | CacheDomainSource::DieSysfs
        )
    {
        TopologyQuality::Complete
    } else {
        TopologyQuality::Partial
    }
}

fn fingerprint(
    logical: &[LogicalCpu],
    physical: &[PhysicalCore],
    quality: TopologyQuality,
    source: CacheDomainSource,
) -> String {
    let mut data = format!("quality={quality:?};cache={source:?};");
    for cpu in logical {
        data.push_str(&format!("cpu={cpu:?};"));
    }
    for core in physical {
        data.push_str(&format!("core={core:?};"));
    }
    let digest = Sha256::digest(data.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture(with_cache: bool, with_siblings: bool) -> PathBuf {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "perigee-affinity-topology-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for cpu in 0..4 {
            let cpu_dir = dir.join(format!("cpu{cpu}"));
            fs::create_dir_all(cpu_dir.join("topology")).unwrap();
            fs::write(cpu_dir.join("topology/physical_package_id"), "0\n").unwrap();
            fs::write(cpu_dir.join("topology/core_id"), format!("{}\n", cpu / 2)).unwrap();
            if with_siblings {
                fs::write(
                    cpu_dir.join("topology/thread_siblings_list"),
                    if cpu % 2 == 0 {
                        format!("{}-{}\n", cpu, cpu + 1)
                    } else {
                        format!("{}-{}\n", cpu - 1, cpu)
                    },
                )
                .unwrap();
            }
            fs::create_dir_all(cpu_dir.join("node0")).unwrap();
            if with_cache {
                fs::create_dir_all(cpu_dir.join("cache/index3")).unwrap();
                fs::write(cpu_dir.join("cache/index3/level"), "3\n").unwrap();
                fs::write(cpu_dir.join("cache/index3/id"), "7\n").unwrap();
            }
        }
        dir
    }

    #[test]
    fn detects_siblings_and_l3_domain() {
        let dir = fixture(true, true);
        let topology = detect_from(&dir).unwrap();
        let _ = fs::remove_dir_all(dir);
        assert_eq!(topology.logical_cpu_ids().as_slice(), &[0, 1, 2, 3]);
        assert_eq!(topology.physical_cores.len(), 2);
        assert_eq!(topology.quality, TopologyQuality::Complete);
        assert_eq!(topology.cache_domain_source, CacheDomainSource::L3Sysfs);
        assert_eq!(topology.physical_cores[0].threads.as_slice(), &[0, 1]);
        assert_eq!(topology.fingerprint.len(), 64);
    }

    #[test]
    fn missing_siblings_degrades_without_inventing_them() {
        let dir = fixture(true, false);
        let topology = detect_from(&dir).unwrap();
        let _ = fs::remove_dir_all(dir);
        assert_eq!(topology.quality, TopologyQuality::Partial);
        // core_id still establishes physical-core identity; the missing
        // sibling list only removes the stronger sibling-aware guarantee.
        assert!(topology
            .physical_cores
            .iter()
            .all(|core| core.threads.len() == 2));
    }

    #[test]
    fn missing_physical_identity_is_logical_only() {
        let dir = fixture(false, false);
        for cpu in 0..4 {
            fs::remove_file(dir.join(format!("cpu{cpu}/topology/core_id"))).unwrap();
            fs::remove_file(dir.join(format!("cpu{cpu}/topology/physical_package_id"))).unwrap();
        }
        let topology = detect_from(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(topology.quality, TopologyQuality::LogicalOnly);
        assert!(topology.physical_cores.is_empty());
    }

    #[test]
    fn invalid_cpu_range_is_rejected() {
        assert!(parse_cpu_list("3-1").is_err());
        assert!(parse_cpu_list("0-8192").is_err());
    }
}
