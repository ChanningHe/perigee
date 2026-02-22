use anyhow::Result;
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info};

use crate::module::ModuleRegistry;

/// Run boot-time apply for all registered modules.
pub async fn boot_apply(registry: &Arc<Mutex<ModuleRegistry>>) -> Result<()> {
    info!("starting boot-time apply");
    let mut reg = registry.lock().await;
    for module in reg.all_mut() {
        info!(module = module.name(), "applying module");
        if let Err(e) = module.apply().await {
            error!(module = module.name(), error = %e, "module apply failed");
        }
    }
    info!("boot-time apply complete");
    Ok(())
}

/// Main daemon run loop.
pub async fn run_daemon(
    registry: Arc<Mutex<ModuleRegistry>>,
    config: Arc<Mutex<toml::Value>>,
    shutdown_tx: broadcast::Sender<()>,
) -> Result<()> {
    // Boot-time apply
    boot_apply(&registry).await?;

    // Start IPC server
    let server = crate::server::IpcServer::new(registry.clone(), config.clone());
    let shutdown_rx = shutdown_tx.subscribe();

    // Tell systemd we're ready
    crate::notify::sd_notify_ready();
    crate::notify::sd_notify_status("running");
    info!("daemon ready");

    tokio::select! {
        result = server.run(shutdown_rx) => {
            if let Err(e) = result {
                error!(error = %e, "IPC server error");
            }
        }
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
