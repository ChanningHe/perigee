use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const SOCKET_PATH: &str = "/run/perigee.sock";

// ── Request ──

#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Reload,
    ReloadModule { name: String },
    Status,
    ProfileStatus { profile: String },
    ProfileEvents { profile: String, limit: usize },
    FdbEntries { profile: String },
    Apply { profile: String },
    RetryFailed { profile: String },
}

// ── Response ──

#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Ok,
    Status(DaemonStatus),
    ProfileDetail(ProfileDetailStatus),
    Events(Vec<ProfileEvent>),
    FdbEntries(Vec<FdbEntryInfo>),
    Error { message: String },
}

// ── Status types ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub uptime_secs: u64,
    pub modules: Vec<ModuleStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleStatus {
    pub name: String,
    pub state: ModuleState,
    pub profiles: Vec<ProfileSummary>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModuleState {
    Running,
    Degraded,
    Error,
    Stopped,
}

impl std::fmt::Display for ModuleState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "Running"),
            Self::Degraded => write!(f, "Degraded"),
            Self::Error => write!(f, "Error"),
            Self::Stopped => write!(f, "Stopped"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub name: String,
    pub state: ProfileState,
    pub error_count: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProfileState {
    Active,
    Degraded,
    Error,
    NicOffline,
    Pending,
}

impl std::fmt::Display for ProfileState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "Active"),
            Self::Degraded => write!(f, "Degraded"),
            Self::Error => write!(f, "Error"),
            Self::NicOffline => write!(f, "NIC N/A"),
            Self::Pending => write!(f, "Pending"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileDetailStatus {
    pub name: String,
    pub state: ProfileState,
    pub pf_iface: Option<String>,
    pub pf_mac: String,
    pub last_applied: Option<DateTime<Utc>>,
    pub config_dirty: bool,
    pub vfs: Vec<VfRuntimeStatus>,
    pub fdb: FdbRuntimeStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfRuntimeStatus {
    pub index: u32,
    /// PCI address (BDF) PVE uses to pass the VF through, e.g. "0000:41:00.1".
    #[serde(default)]
    pub pci_addr: Option<String>,
    /// The VM passing this VF through (via `hostpci`), if any.
    #[serde(default)]
    pub used_by: Option<VfUser>,
    pub configured: VfSnapshot,
    pub actual: Option<VfSnapshot>,
    pub matches: bool,
    pub last_error: Option<String>,
}

/// A VM that references a VF for PCI passthrough.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VfUser {
    pub vmid: String,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VfSnapshot {
    pub mac: String,
    pub trust: bool,
    pub spoofchk: bool,
    pub vlan_id: Option<u16>,
    pub vlan_qos: Option<u8>,
}

/// A single managed FDB entry: a VM MAC the daemon injected into the PF's FDB,
/// along with the VM it came from and the bridge it is attached to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FdbEntryInfo {
    pub mac: String,
    pub vmid: String,
    pub bridge: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FdbRuntimeStatus {
    pub mode: String,
    pub bridge: Option<String>,
    pub managed_entries: u32,
    pub last_sync_secs_ago: Option<u64>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileEvent {
    pub timestamp: DateTime<Utc>,
    pub level: EventLevel,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventLevel {
    Info,
    Warn,
    Error,
}

impl std::fmt::Display for EventLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Info => write!(f, "INFO"),
            Self::Warn => write!(f, "WARN"),
            Self::Error => write!(f, "ERROR"),
        }
    }
}
