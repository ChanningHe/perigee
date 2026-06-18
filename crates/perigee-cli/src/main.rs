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
                            "\nFDB: {} | {} entries",
                            detail.fdb.mode, detail.fdb.managed_entries
                        );
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
        SriovAction::FdbHookscript => {
            let output = std::path::PathBuf::from("/var/lib/vz/snippets/perigee-bridgefix.sh");
            // Try to detect PF from existing config
            let config_path = perigee_sriov::config::sriov_config_path();
            if config_path.exists() {
                let config = perigee_sriov::config::SriovFileConfig::load(&config_path)?;
                if let Some((_name, profile)) = config.sriov.iter().next() {
                    let pf_mac = profile.mac.to_string();
                    let pf_iface =
                        perigee_core::sysfs::find_iface_by_mac(&pf_mac).map_err(|_| {
                            anyhow::anyhow!(
                                "Cannot detect PF interface for MAC {}. Is the NIC online?",
                                pf_mac
                            )
                        })?;
                    perigee_sriov::fdb::generate_hookscript(&output, &pf_iface)?;
                    println!("Hookscript generated: {}", output.display());
                    println!(
                        "Attach to VM: qm set <vmid> --hookscript local:snippets/perigee-bridgefix.sh"
                    );
                    return Ok(());
                }
            }
            println!("No SR-IOV profile found. Create one first with 'perigee sriov'.");
            Ok(())
        }
    }
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
            cores,
            strategy,
            smt,
        } => {
            let topo = perigee_affinity::topology::detect()?;
            let all_configs = perigee_affinity::pve::read_all_vm_configs();
            let existing: Vec<perigee_affinity::affinity::VmBinding> = all_configs
                .iter()
                .filter_map(|(vmid, cfg)| {
                    cfg.affinity
                        .as_ref()
                        .map(|a| perigee_affinity::affinity::VmBinding {
                            vmid: *vmid,
                            cpus: perigee_affinity::affinity::parse_affinity_str(a),
                        })
                })
                .collect();

            let req = perigee_affinity::affinity::AffinityRequest {
                cores_needed: cores,
                include_smt: smt,
                topology: topo,
                existing_bindings: existing,
            };
            let options = perigee_affinity::affinity::generate(&req)?;
            let strat: perigee_affinity::affinity::Strategy = strategy.parse()?;
            let opt = options
                .iter()
                .find(|o| o.strategy == strat && o.available)
                .or_else(|| options.iter().find(|o| o.available));
            match opt {
                Some(o) => {
                    println!("{}", o.affinity_str);
                }
                None => {
                    eprintln!("No available strategy for {} cores", cores);
                }
            }
            Ok(())
        }
        AffinityAction::Apply {
            vmid,
            cores,
            strategy,
            dry_run,
        } => {
            let topo = perigee_affinity::topology::detect()?;
            let core_count = match cores {
                Some(c) => c,
                None => {
                    let cfg = perigee_affinity::pve::read_vm_config(vmid)?;
                    if cfg.cores == 0 {
                        anyhow::bail!("VM {} has cores=0 in config, use --cores", vmid);
                    }
                    cfg.cores
                }
            };

            let all_configs = perigee_affinity::pve::read_all_vm_configs();
            let existing: Vec<perigee_affinity::affinity::VmBinding> = all_configs
                .iter()
                .filter_map(|(vid, cfg)| {
                    if *vid == vmid {
                        return None;
                    }
                    cfg.affinity
                        .as_ref()
                        .map(|a| perigee_affinity::affinity::VmBinding {
                            vmid: *vid,
                            cpus: perigee_affinity::affinity::parse_affinity_str(a),
                        })
                })
                .collect();

            let req = perigee_affinity::affinity::AffinityRequest {
                cores_needed: core_count,
                include_smt: true,
                topology: topo,
                existing_bindings: existing,
            };
            let options = perigee_affinity::affinity::generate(&req)?;
            let strat: perigee_affinity::affinity::Strategy = strategy.parse()?;
            let opt = options
                .iter()
                .find(|o| o.strategy == strat && o.available)
                .or_else(|| options.iter().find(|o| o.available));

            match opt {
                Some(o) => {
                    println!("qm set {} --affinity {}", vmid, o.affinity_str);
                    if !dry_run {
                        perigee_affinity::pve::set_affinity(vmid, &o.affinity_str, false)?;
                        println!("Applied.");
                    }
                }
                None => {
                    eprintln!("No available strategy for {} cores", core_count);
                }
            }
            Ok(())
        }
        AffinityAction::AutoApply { dry_run } => {
            let topo = perigee_affinity::topology::detect()?;
            let vms = perigee_affinity::pve::list_vms()?;
            let mut vm_entries: Vec<(u32, String, usize)> = Vec::new();
            for vm in &vms {
                let cfg = perigee_affinity::pve::read_vm_config(vm.vmid).unwrap_or_default();
                if cfg.cores > 0 {
                    vm_entries.push((vm.vmid, vm.name.clone(), cfg.cores));
                }
            }
            vm_entries.sort_by_key(|e| std::cmp::Reverse(e.2));

            let mut bindings: Vec<perigee_affinity::affinity::VmBinding> = Vec::new();
            for (vmid, name, cores) in &vm_entries {
                let req = perigee_affinity::affinity::AffinityRequest {
                    cores_needed: *cores,
                    include_smt: true,
                    topology: topo.clone(),
                    existing_bindings: bindings.clone(),
                };
                let Ok(options) = perigee_affinity::affinity::generate(&req) else {
                    eprintln!("VM {} ({}): no options", vmid, name);
                    continue;
                };
                let opt = options
                    .iter()
                    .find(|o| {
                        o.strategy == perigee_affinity::affinity::Strategy::Balanced && o.available
                    })
                    .or_else(|| options.iter().find(|o| o.available));

                if let Some(o) = opt {
                    println!(
                        "VM {} ({}): qm set {} --affinity {}",
                        vmid, name, vmid, o.affinity_str
                    );
                    if !dry_run {
                        match perigee_affinity::pve::set_affinity(*vmid, &o.affinity_str, false) {
                            Ok(()) => println!("  ✓ Applied"),
                            Err(e) => eprintln!("  ✗ {}", e),
                        }
                    }
                    bindings.push(perigee_affinity::affinity::VmBinding {
                        vmid: *vmid,
                        cpus: o.cpus.clone(),
                    });
                } else {
                    eprintln!(
                        "VM {} ({}): no available strategy for {} cores",
                        vmid, name, cores
                    );
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
