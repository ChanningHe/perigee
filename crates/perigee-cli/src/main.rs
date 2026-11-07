mod cli;
mod install;
mod ui;

use anyhow::Result;
use clap::Parser;
use cli::{AffinityAction, Cli, Commands, SriovAction};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        None => {
            // No subcommand: launch main TUI menu
            ui::run_app().await
        }
        Some(Commands::Daemon) => run_daemon().await,
        Some(Commands::Sriov { action }) => match action {
            None => ui::run_sriov_tui().await,
            Some(action) => handle_sriov_cli(action).await,
        },
        Some(Commands::Reload) => cmd_reload().await,
        Some(Commands::Status) => cmd_status().await,
        Some(Commands::Install { force }) => {
            install::install(force)?;
            Ok(())
        }
        Some(Commands::Uninstall) => {
            install::uninstall()?;
            Ok(())
        }
        Some(Commands::Update { force }) => {
            install::update(force)?;
            Ok(())
        }
        Some(Commands::Affinity { action }) => match action {
            None => ui::run_affinity_tui().await,
            Some(action) => handle_affinity_cli(action).await,
        },
    }
}

async fn run_daemon() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();

    let config = perigee_daemon::config::load_all_configs()?;
    let config = std::sync::Arc::new(tokio::sync::Mutex::new(config));
    let registry = std::sync::Arc::new(tokio::sync::Mutex::new(
        perigee_daemon::module::ModuleRegistry::new(),
    ));

    {
        let mut reg = registry.lock().await;
        let cfg = config.lock().await;

        let mut sriov = perigee_sriov::create_module();
        sriov.init(&cfg).await?;
        reg.register(sriov);

        let mut affinity = perigee_affinity::create_module();
        affinity.init(&cfg).await?;
        reg.register(affinity);
    }

    let (shutdown_tx, _) = tokio::sync::broadcast::channel(1);
    perigee_daemon::scheduler::run_daemon(registry, config, shutdown_tx).await
}

async fn handle_sriov_cli(action: SriovAction) -> Result<()> {
    use perigee_core::ipc::{Request, Response};

    match action {
        SriovAction::List => {
            let config_path = perigee_sriov::config::sriov_config_path();
            if !config_path.exists() {
                println!("No SR-IOV profiles configured. Use 'perigee sriov' to create one.");
                return Ok(());
            }
            let config = perigee_sriov::config::SriovFileConfig::load(&config_path)?;
            println!(
                "{:<20} {:<20} {:>4}  {:<10}",
                "Profile", "PF MAC", "VFs", "Status"
            );
            println!("{}", "─".repeat(60));
            for (name, profile) in &config.sriov {
                let status = if perigee_core::client::IpcClient::is_daemon_running() {
                    match perigee_core::client::IpcClient::send(&Request::ProfileStatus {
                        profile: name.clone(),
                    })
                    .await
                    {
                        Ok(Response::ProfileDetail(detail)) => detail.state.to_string(),
                        _ => "Unknown".to_string(),
                    }
                } else {
                    "Daemon offline".to_string()
                };
                println!(
                    "{:<20} {:<20} {:>4}  {:<10}",
                    name, profile.mac, profile.num_vfs, status
                );
            }
            Ok(())
        }
        SriovAction::Show { profile } => {
            if perigee_core::client::IpcClient::is_daemon_running() {
                match perigee_core::client::IpcClient::send(&Request::ProfileStatus {
                    profile: profile.clone(),
                })
                .await
                {
                    Ok(Response::ProfileDetail(detail)) => {
                        println!("Profile:      {}", detail.name);
                        println!(
                            "PF:           {} ({})",
                            detail.pf_iface.as_deref().unwrap_or("N/A"),
                            detail.pf_mac
                        );
                        println!("State:        {}", detail.state);
                        if let Some(ts) = &detail.last_applied {
                            println!("Last Applied: {}", ts);
                        }
                        println!("\nVF Runtime Status:");
                        for vf in &detail.vfs {
                            let status = if vf.matches { "OK" } else { "MISMATCH" };
                            let vlan_str = vf
                                .configured
                                .vlan_id
                                .map(|id| id.to_string())
                                .unwrap_or_else(|| "-".into());
                            let pci = vf.pci_addr.as_deref().unwrap_or("-");
                            let used = match &vf.used_by {
                                Some(u) if u.running => format!("VM {} (running)", u.vmid),
                                Some(u) => format!("VM {} (stopped)", u.vmid),
                                None => "-".to_string(),
                            };
                            println!(
                                "  VF#{:<3} {:<14} {} trust={:<5} spoofchk={:<5} vlan={:<6} {:<10} {}",
                                vf.index,
                                pci,
                                vf.configured.mac,
                                vf.configured.trust,
                                vf.configured.spoofchk,
                                vlan_str,
                                status,
                                used
                            );
                        }
                        if detail.config_dirty {
                            println!(
                                "\n  ⚠ Config modified since last apply. Run `perigee sriov apply {}` to apply.",
                                detail.name
                            );
                        }
                        println!(
                            "\nFDB: {} | {} entries | {}",
                            detail.fdb.mode,
                            detail.fdb.managed_entries,
                            detail.fdb.sync_summary()
                        );
                        for err in &detail.fdb.errors {
                            println!("  ⚠ {}", err);
                        }
                        return Ok(());
                    }
                    Ok(Response::Error { message }) => {
                        eprintln!("Daemon error: {}", message);
                    }
                    _ => {
                        eprintln!("Unexpected response from daemon");
                    }
                }
            }

            // Fallback: config-only + sysfs info
            println!("(Daemon offline — showing config + sysfs info)\n");
            let config_path = perigee_sriov::config::sriov_config_path();
            if !config_path.exists() {
                println!("No config found at {}", config_path.display());
                return Ok(());
            }
            let config = perigee_sriov::config::SriovFileConfig::load(&config_path)?;
            if let Some(p) = config.sriov.get(&profile) {
                println!("Profile:      {}", profile);
                println!("PF MAC:       {}", p.mac);
                let pf_iface = perigee_core::sysfs::find_iface_by_mac(&p.mac.to_string()).ok();
                println!(
                    "PF Iface:     {}",
                    pf_iface.as_deref().unwrap_or("not found")
                );
                println!("VF Count:     {}", p.num_vfs);
                println!("MAC Strategy: {:?}", p.mac_strategy);
                println!(
                    "Trust:        {}",
                    if p.defaults.trust { "on" } else { "off" }
                );
                println!(
                    "SpoofChk:     {}",
                    if p.defaults.spoofchk { "on" } else { "off" }
                );
                println!("FDB Mode:     {:?}", p.fdb.mode);

                if let Some(iface) = pf_iface {
                    let current_vfs = perigee_core::sysfs::read_sriov_numvfs(&iface).unwrap_or(0);
                    let max_vfs = perigee_core::sysfs::read_sriov_totalvfs(&iface).unwrap_or(0);
                    println!("\nSysfs:");
                    println!("  Current VFs: {}", current_vfs);
                    println!("  Max VFs:     {}", max_vfs);
                }
            } else {
                println!("Profile '{}' not found in config.", profile);
            }
            Ok(())
        }
        SriovAction::Events { profile, limit } => {
            let resp =
                perigee_core::client::IpcClient::send(&Request::ProfileEvents { profile, limit })
                    .await?;
            if let Response::Events(events) = resp {
                for event in &events {
                    println!(
                        "{} [{}] {}",
                        event.timestamp.format("%H:%M:%S"),
                        event.level,
                        event.message
                    );
                }
                if events.is_empty() {
                    println!("No events.");
                }
            }
            Ok(())
        }
        SriovAction::Add { profile: _ } => {
            // TODO: interactive CLI add
            println!("Use 'perigee sriov' for interactive TUI mode.");
            Ok(())
        }
        SriovAction::Remove { profile } => {
            let config_path = perigee_sriov::config::sriov_config_path();
            if !config_path.exists() {
                println!("No config file found.");
                return Ok(());
            }
            let mut config = perigee_sriov::config::SriovFileConfig::load(&config_path)?;
            if config.sriov.remove(&profile).is_some() {
                config.save(&config_path)?;
                println!("Profile '{}' removed.", profile);
                if perigee_core::client::IpcClient::is_daemon_running() {
                    let _ = perigee_core::client::IpcClient::send(&Request::Reload).await;
                    println!("Daemon notified to reload.");
                }
            } else {
                println!("Profile '{}' not found.", profile);
            }
            Ok(())
        }
        SriovAction::Retry { profile } => {
            let resp =
                perigee_core::client::IpcClient::send(&Request::RetryFailed { profile }).await?;
            match resp {
                Response::Ok => println!("Retry initiated."),
                Response::Error { message } => eprintln!("Error: {}", message),
                _ => eprintln!("Unexpected response"),
            }
            Ok(())
        }
        SriovAction::FdbHookscript => generate_fdb_hookscripts(),
        SriovAction::FdbHook { vmid, phase } => {
            // Never fail: PVE aborts the guest start on a failing pre-start
            // hook. Errors still reach the PVE task log via stderr.
            if let Err(e) = perigee_sriov::fdb::run_hook(&vmid, &phase) {
                eprintln!("perigee fdb-hook: {e:#}");
            }
            Ok(())
        }
    }
}

/// Write the FDB hookscript to every `output_path` used by a hookscript-mode
/// profile (the default path when none is set). The script itself serves all
/// such profiles, so every copy is identical.
fn generate_fdb_hookscripts() -> Result<()> {
    use perigee_sriov::config::{sriov_config_path, FdbMode, SriovFileConfig};
    use perigee_sriov::fdb::{generate_hookscript, DEFAULT_HOOKSCRIPT_PATH};
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    let config_path = sriov_config_path();
    let profiles = if config_path.exists() {
        SriovFileConfig::load(&config_path)?.sriov
    } else {
        Default::default()
    };
    let hooked: Vec<_> = profiles
        .values()
        .filter(|p| p.fdb.mode == FdbMode::Hookscript)
        .collect();
    if hooked.is_empty() {
        println!(
            "Note: no profile uses FDB mode 'hookscript'; the hook does nothing until one does."
        );
    }
    let mut paths: BTreeSet<PathBuf> = hooked
        .iter()
        .map(|p| {
            PathBuf::from(
                p.fdb
                    .output_path
                    .as_deref()
                    .unwrap_or(DEFAULT_HOOKSCRIPT_PATH),
            )
        })
        .collect();
    if paths.is_empty() {
        paths.insert(PathBuf::from(DEFAULT_HOOKSCRIPT_PATH));
    }

    let bin = Path::new(install::BINARY_INSTALL_PATH);
    if !bin.exists() {
        eprintln!(
            "Warning: {} not found; the hook calls it, run 'perigee install' first.",
            bin.display()
        );
    }
    for path in &paths {
        generate_hookscript(path, bin)?;
        println!("Hookscript generated: {}", path.display());
    }
    println!(
        "Attach to a guest: qm set <vmid> --hookscript <storage>:snippets/<file> (pct set for CTs)"
    );
    Ok(())
}

async fn handle_affinity_cli(action: AffinityAction) -> Result<()> {
    match action {
        AffinityAction::Topology { json } => {
            let topo = perigee_affinity::topology::detect()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&topo)?);
            } else {
                println!(
                    "Architecture: {} ({})",
                    topo.architecture, topo.detect_method
                );
                println!(
                    "CPUs: {} logical / {} physical / SMT {}",
                    topo.total_cpus,
                    topo.total_cores,
                    if topo.has_smt { "yes" } else { "no" }
                );
                println!("Packages: {}\n", topo.packages.len());
                for pkg in &topo.packages {
                    println!("Package {}:", pkg.id);
                    for cg in &pkg.core_groups {
                        println!(
                            "  {:<8} L3#{:<4} {}C/{}T  {}",
                            cg.name,
                            cg.l3_cache_id,
                            cg.physical_cpus.len(),
                            cg.all_cpus.len(),
                            perigee_affinity::affinity::format_cpus(&cg.all_cpus),
                        );
                    }
                }
            }
            Ok(())
        }
        AffinityAction::Generate {
            vcpus,
            strategy,
            smt,
        } => {
            let topology = perigee_affinity::topology_adapter::detect()?;
            let inventory = perigee_affinity::inventory::collect(&[])?;
            let mut policy = perigee_affinity::config::AffinityConfig::default().policy();
            policy.smt = if smt {
                perigee_affinity::model::SmtPolicy::PreferDistinctCores
            } else {
                perigee_affinity::model::SmtPolicy::LogicalOnly
            };
            let vm = perigee_affinity::model::VmResource {
                vmid: 1,
                name: "generate".to_string(),
                status: perigee_affinity::model::VmStatus::Stopped,
                cpu: perigee_affinity::model::VmCpuShape::from_proxmox(
                    Some(1),
                    None,
                    Some(vcpus),
                    None,
                    false,
                )?,
                current_affinity: None,
                ownership: perigee_affinity::model::VmOwnership::Managed,
            };
            let plan = perigee_affinity::planner::plan(&topology, &[vm], &policy);
            if let Some(assignment) = plan.assignments.first() {
                println!("{}", assignment.allowed_cpus);
            } else {
                anyhow::bail!(
                    "no available affinity for {} vCPUs (strategy={})",
                    vcpus,
                    strategy
                );
            }
            let _ = inventory;
            Ok(())
        }
        AffinityAction::Apply {
            vmid,
            vcpus,
            strategy,
            dry_run,
        } => {
            if perigee_core::client::IpcClient::is_daemon_running() && !dry_run {
                match perigee_core::client::IpcClient::send(
                    &perigee_core::ipc::Request::ReconcileAffinity,
                )
                .await?
                {
                    perigee_core::ipc::Response::Ok => println!(
                        "Affinity reconcile requested for VM {} (strategy={}).",
                        vmid, strategy
                    ),
                    perigee_core::ipc::Response::Error { message } => {
                        anyhow::bail!("affinity reconcile failed: {message}")
                    }
                    _ => anyhow::bail!("unexpected daemon response"),
                }
                return Ok(());
            }
            let topology = perigee_affinity::topology_adapter::detect()?;
            let inventory = perigee_affinity::inventory::collect(&[])?;
            let vm = inventory
                .vms
                .iter()
                .find(|vm| vm.vmid == vmid)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("VM {} not found", vmid))?;
            let requested = vcpus.unwrap_or(vm.cpu.active_vcpus);
            let mut vm = vm;
            vm.cpu = perigee_affinity::model::VmCpuShape::from_proxmox(
                Some(1),
                None,
                Some(requested),
                None,
                false,
            )?;
            let plan = perigee_affinity::planner::plan(
                &topology,
                &[vm],
                &perigee_affinity::config::AffinityConfig::default().policy(),
            );
            let assignment = plan
                .assignments
                .first()
                .ok_or_else(|| anyhow::anyhow!("no available affinity for VM {}", vmid))?;
            println!("qm set {} --affinity {}", vmid, assignment.allowed_cpus);
            if !dry_run {
                perigee_affinity::pve::set_affinity(
                    vmid,
                    &assignment.allowed_cpus.to_range_string(),
                    false,
                )?;
                println!("Applied.");
            }
            Ok(())
        }
        AffinityAction::AutoApply { dry_run } => {
            if perigee_core::client::IpcClient::is_daemon_running() && !dry_run {
                match perigee_core::client::IpcClient::send(
                    &perigee_core::ipc::Request::ReconcileAffinity,
                )
                .await?
                {
                    perigee_core::ipc::Response::Ok => println!("Affinity reconcile requested."),
                    perigee_core::ipc::Response::Error { message } => {
                        anyhow::bail!("affinity reconcile failed: {message}")
                    }
                    _ => anyhow::bail!("unexpected daemon response"),
                }
                return Ok(());
            }
            let topology = perigee_affinity::topology_adapter::detect()?;
            let inventory = perigee_affinity::inventory::collect(&[])?;
            let plan = perigee_affinity::planner::plan(
                &topology,
                &inventory.vms,
                &perigee_affinity::config::AffinityConfig::default().policy(),
            );
            if !plan.conflicts.is_empty() {
                anyhow::bail!("affinity plan has {} conflict(s)", plan.conflicts.len());
            }
            for assignment in &plan.assignments {
                println!(
                    "VM {}: qm set {} --affinity {}",
                    assignment.vmid, assignment.vmid, assignment.allowed_cpus
                );
                if !dry_run {
                    perigee_affinity::pve::set_affinity(
                        assignment.vmid,
                        &assignment.allowed_cpus.to_range_string(),
                        false,
                    )?;
                }
            }
            Ok(())
        }
    }
}

async fn cmd_reload() -> Result<()> {
    use perigee_core::ipc::{Request, Response};

    match perigee_core::client::IpcClient::send(&Request::Reload).await {
        Ok(Response::Ok) => {
            println!("Daemon reloaded successfully.");
            Ok(())
        }
        Ok(Response::Error { message }) => {
            eprintln!("Reload failed: {}", message);
            Ok(())
        }
        Err(e) => {
            eprintln!("Cannot connect to daemon: {}", e);
            eprintln!("Is perigee daemon running? Start with: perigee install");
            Ok(())
        }
        _ => {
            eprintln!("Unexpected response from daemon");
            Ok(())
        }
    }
}

async fn cmd_status() -> Result<()> {
    use perigee_core::ipc::{Request, Response};

    match perigee_core::client::IpcClient::send(&Request::Status).await {
        Ok(Response::Status(status)) => {
            println!("Perigee Daemon");
            println!("  Uptime: {}s", status.uptime_secs);
            println!("  Modules:");
            for module in &status.modules {
                println!("    {} [{}]", module.name, module.state);
                for profile in &module.profiles {
                    let err_str = if profile.error_count > 0 {
                        format!(" ({} errors)", profile.error_count)
                    } else {
                        String::new()
                    };
                    println!("      {} {}{}", profile.name, profile.state, err_str);
                }
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Cannot connect to daemon: {}", e);
            eprintln!("Is perigee daemon running? Start with: perigee install");
            Ok(())
        }
        _ => {
            eprintln!("Unexpected response");
            Ok(())
        }
    }
}
