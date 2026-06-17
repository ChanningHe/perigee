use thiserror::Error;

#[derive(Debug, Error)]
pub enum PerigeeError {
    #[error("sysfs error: {0}")]
    Sysfs(String),

    #[error("no interface found with MAC {0}")]
    InterfaceNotFound(String),

    #[error("PCI device error: {0}")]
    Pci(String),

    #[error("MAC address error: {0}")]
    Mac(String),

    #[error("IOMMU not enabled: {0}")]
    Iommu(String),

    #[error("SR-IOV error: {0}")]
    Sriov(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("permission denied: {0}")]
    Permission(String),

    #[error("vendor-specific error: {0}")]
    Vendor(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, PerigeeError>;
