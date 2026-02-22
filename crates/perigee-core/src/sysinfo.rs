use std::fs;

#[derive(Debug, Clone, Default)]
pub struct HostInfo {
    pub hostname: String,
    pub kernel: String,
    pub pve_version: Option<String>,
    pub cpu_model: String,
    pub cpu_cores: u32,
    pub memory_total_mb: u64,
    pub memory_used_mb: u64,
}

impl HostInfo {
    pub fn gather() -> Self {
        Self {
            hostname: read_hostname(),
            kernel: read_kernel(),
            pve_version: read_pve_version(),
            cpu_model: read_cpu_model(),
            cpu_cores: read_cpu_cores(),
            memory_total_mb: read_mem_total_mb(),
            memory_used_mb: read_mem_used_mb(),
        }
    }

    pub fn memory_str(&self) -> String {
        if self.memory_total_mb >= 1024 {
            format!(
                "{:.1} / {:.1} GiB",
                self.memory_used_mb as f64 / 1024.0,
                self.memory_total_mb as f64 / 1024.0,
            )
        } else {
            format!("{} / {} MiB", self.memory_used_mb, self.memory_total_mb)
        }
    }
}

fn read_hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

fn read_kernel() -> String {
    fs::read_to_string("/proc/version")
        .ok()
        .and_then(|v| v.split_whitespace().nth(2).map(String::from))
        .unwrap_or_else(|| "unknown".into())
}

fn read_pve_version() -> Option<String> {
    // pveversion outputs: "pve-manager/8.4.16/368e3c45... (running kernel: ...)"
    if let Ok(out) = std::process::Command::new("pveversion").output() {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
            // Extract "8.4.16" from "pve-manager/8.4.16/..."
            if let Some(rest) = raw.strip_prefix("pve-manager/") {
                let ver = rest.split('/').next().unwrap_or(rest.as_ref());
                return Some(ver.to_string());
            }
            return Some(raw);
        }
    }
    if let Ok(out) = std::process::Command::new("dpkg-query")
        .args(["-W", "-f", "${Version}", "proxmox-ve"])
        .output()
    {
        if out.status.success() {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !ver.is_empty() {
                return Some(ver);
            }
        }
    }
    None
}

fn read_cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split(':').nth(1))
                .map(|s| clean_cpu_name(s.trim()))
        })
        .unwrap_or_else(|| "unknown".into())
}

fn clean_cpu_name(raw: &str) -> String {
    let mut s = raw.to_string();
    // Strip common verbose suffixes
    for noise in [
        " with Radeon Graphics",
        " with Radeon Vega Graphics",
        "(R)",
        "(TM)",
        "  ",
    ] {
        s = s.replace(noise, if noise == "  " { " " } else { "" });
    }
    s.trim().to_string()
}

fn read_cpu_cores() -> u32 {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .map(|info| {
            info.lines()
                .filter(|l| l.starts_with("processor"))
                .count() as u32
        })
        .unwrap_or(0)
}

fn read_mem_total_mb() -> u64 {
    parse_meminfo_field("MemTotal")
}

fn read_mem_used_mb() -> u64 {
    let total = parse_meminfo_field("MemTotal");
    let avail = parse_meminfo_field("MemAvailable");
    total.saturating_sub(avail)
}

fn parse_meminfo_field(field: &str) -> u64 {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .find(|l| l.starts_with(field))
                .and_then(|l| {
                    l.split_whitespace()
                        .nth(1)
                        .and_then(|v| v.parse::<u64>().ok())
                })
        })
        .unwrap_or(0)
        / 1024 // kB -> MiB
}
