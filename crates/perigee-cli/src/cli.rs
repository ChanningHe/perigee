use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "perigee",
    about = "Proxmox VE helper tool - SR-IOV configuration & more",
    version = env!("PERIGEE_VERSION_STRING")
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run as daemon (managed by systemd)
    Daemon,

    /// SR-IOV configuration (TUI or CLI)
    Sriov {
        #[command(subcommand)]
        action: Option<SriovAction>,
    },

    /// Notify daemon to reload all configurations
    Reload,

    /// Query daemon and module status
    Status,

    /// Install perigee systemd service
    Install {
        /// Overwrite existing files without prompting
        #[arg(long, short)]
        force: bool,
    },

    /// Uninstall perigee systemd service
    Uninstall,
}

#[derive(Debug, Subcommand)]
pub enum SriovAction {
    /// List all profiles with status summary
    List,

    /// Show detailed runtime status for a profile
    Show {
        /// Profile name
        profile: String,
    },

    /// Show event log for a profile
    Events {
        /// Profile name
        profile: String,
        /// Max events to show
        #[arg(long, default_value = "20")]
        limit: usize,
    },

    /// Add a new profile interactively
    Add {
        /// Profile name
        profile: String,
    },

    /// Remove a profile
    Remove {
        /// Profile name
        profile: String,
    },

    /// Retry failed items in a profile
    Retry {
        /// Profile name
        profile: String,
    },

    /// Generate FDB hookscript (fallback mode)
    FdbHookscript,
}
