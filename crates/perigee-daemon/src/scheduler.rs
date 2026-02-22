use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info};

use crate::module::ModuleRegistry;

/// Run boot-time apply for all registered modules.
/// Locks and unlocks the registry for each module individually
/// so that IPC requests can be served between applies.
pub async fn boot_apply(registry: &Arc<Mutex<ModuleRegistry>>) {
    info!("starting boot-time apply");

    let module_names: Vec<String> = {
        let reg = registry.lock().await;
        reg.all().map(|m| m.name().to_string()).collect()
    };

    for name in &module_names {
        info!(module = %name, "applying module");
        let mut reg = registry.lock().await;
        if let Some(module) = reg.get_mut(name) {
            if let Err(e) = module.apply().await {
                error!(module = %name, error = %e, "module apply failed");
            }
        }
        // Lock released here — IPC can serve requests between modules
    }

    info!("boot-time apply complete");
}

/// Main daemon run loop.
pub async fn run_daemon(
    registry: Arc<Mutex<ModuleRegistry>>,
    config: Arc<Mutex<toml::Value>>,
    shutdown_tx: broadcast::Sender<()>,
) -> Result<()> {
    let server = crate::server::IpcServer::new(registry.clone(), config.clone());
    let shutdown_rx = shutdown_tx.subscribe();

    // Spawn IPC server immediately so the socket is available
    let ipc_handle = tokio::spawn(async move {
        if let Err(e) = server.run(shutdown_rx).await {
            error!(error = %e, "IPC server error");
        }
    });

    // Tell systemd we're ready — IPC is now accepting connections
    crate::notify::sd_notify_ready();
    crate::notify::sd_notify_status("applying profiles");
    info!("daemon ready, IPC listening");

    // Yield briefly so IPC task can bind the socket
    tokio::task::yield_now().await;

    // Boot-time apply runs while IPC is already serving
    // (uses spawn_blocking internally for heavy sysfs ops)
    boot_apply(&registry).await;
    crate::notify::sd_notify_status("running");

    // Wait for shutdown signal
    tokio::select! {
        _ = ipc_handle => {}
        _ = tokio::signal::ctrl_c() => {
            info!("received SIGINT, shutting down");
            let _ = shutdown_tx.send(());
        }
    }

    // Graceful shutdown
    crate::notify::sd_notify_stopping();

    let reg = registry.lock().await;
    for module in reg.all() {
        if let Err(e) = module.shutdown().await {
            error!(module = module.name(), error = %e, "module shutdown error");
        }
    }

    info!("daemon shutdown complete");
    Ok(())
}
