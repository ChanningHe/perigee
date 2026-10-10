use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, warn};

use crate::module::{BootPhase, ModuleRegistry};

/// pmxcfs mountpoint; present (empty) on PVE hosts even before it mounts.
const PVE_MOUNTPOINT: &str = "/etc/pve";
/// Symlink pmxcfs creates to this node's directory; exists once mounted.
const PMXCFS_READY_PATH: &str = "/etc/pve/local";
const PMXCFS_POLL_INTERVAL: Duration = Duration::from_secs(2);
const PMXCFS_WAIT_TIMEOUT: Duration = Duration::from_secs(300);

/// Reload configuration and propagate it to every module. Shared by the IPC
/// `Reload` request and the SIGHUP handler so the two cannot diverge.
pub async fn reload_all(
    registry: &Arc<Mutex<ModuleRegistry>>,
    config: &Arc<Mutex<toml::Value>>,
) -> Result<()> {
    let new_config = crate::config::load_all_configs().context("config load failed")?;
    {
        let mut cfg = config.lock().await;
        *cfg = new_config.clone();
    }
    let mut reg = registry.lock().await;
    for module in reg.all_mut() {
        module
            .reload(&new_config)
            .await
            .with_context(|| format!("reload failed for {}", module.name()))?;
    }
    Ok(())
}

/// Run boot-time apply for the modules of one boot phase.
/// Locks and unlocks the registry for each module individually
/// so that IPC requests can be served between applies.
pub async fn boot_apply(registry: &Arc<Mutex<ModuleRegistry>>, phase: BootPhase) {
    info!(?phase, "starting boot-time apply");

    let module_names: Vec<String> = {
        let reg = registry.lock().await;
        reg.all()
            .filter(|m| m.boot_phase() == phase)
            .map(|m| m.name().to_string())
            .collect()
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

    info!(?phase, "boot-time apply complete");
}

/// Wait until `ready` exists, polling every `poll`, for at most `timeout`.
/// Returns whether it appeared.
async fn wait_for_path(ready: &Path, poll: Duration, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if ready.exists() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(poll).await;
    }
}

/// Block until pmxcfs has mounted /etc/pve. Hosts without PVE (no /etc/pve
/// mountpoint at all) do not wait; on timeout the caller proceeds anyway and
/// modules report their own errors.
async fn wait_for_pmxcfs() {
    if !Path::new(PVE_MOUNTPOINT).is_dir() {
        return;
    }
    if !wait_for_path(
        Path::new(PMXCFS_READY_PATH),
        PMXCFS_POLL_INTERVAL,
        PMXCFS_WAIT_TIMEOUT,
    )
    .await
    {
        warn!(
            timeout_secs = PMXCFS_WAIT_TIMEOUT.as_secs(),
            "pmxcfs not mounted in time; applying post-network modules anyway"
        );
    }
}

/// Apply the post-network modules once PVE's cluster filesystem is up.
async fn post_network_apply(registry: Arc<Mutex<ModuleRegistry>>) {
    crate::notify::sd_notify_status("waiting for pmxcfs");
    wait_for_pmxcfs().await;
    crate::notify::sd_notify_status("applying post-network modules");
    boot_apply(&registry, BootPhase::PostNetwork).await;
    crate::notify::sd_notify_status("running");
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

    // SIGTERM (systemctl stop) and SIGHUP (systemctl reload via ExecReload)
    // must be handled explicitly; their default disposition would kill the
    // process and bypass graceful module shutdown. Registered before boot apply
    // so a signal arriving meanwhile is queued, not fatal.
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;

    // Yield briefly so IPC task can bind the socket
    tokio::task::yield_now().await;

    // Pre-network modules (SR-IOV VFs) finish before READY: the unit is
    // ordered Before=network.target, so networking waits for them.
    crate::notify::sd_notify_status("applying pre-network modules");
    boot_apply(&registry, BootPhase::PreNetwork).await;
    crate::notify::sd_notify_ready();
    info!("daemon ready, IPC listening");

    // Post-network modules need pmxcfs, which starts after networking; run
    // them concurrently so signals and IPC keep being served meanwhile.
    let post_network = tokio::spawn(post_network_apply(registry.clone()));

    // Wait for a terminating signal, reloading in place on SIGHUP.
    let mut ipc_handle = ipc_handle;
    loop {
        tokio::select! {
            _ = &mut ipc_handle => break,
            _ = tokio::signal::ctrl_c() => {
                info!("received SIGINT, shutting down");
                break;
            }
            _ = sigterm.recv() => {
                info!("received SIGTERM, shutting down");
                break;
            }
            _ = sighup.recv() => {
                info!("received SIGHUP, reloading config");
                if let Err(e) = reload_all(&registry, &config).await {
                    error!(error = format!("{:#}", e), "config reload failed");
                }
            }
        }
    }
    let _ = shutdown_tx.send(());

    // Graceful shutdown
    crate::notify::sd_notify_stopping();
    post_network.abort();

    let reg = registry.lock().await;
    for module in reg.all() {
        if let Err(e) = module.shutdown().await {
            error!(module = module.name(), error = %e, "module shutdown error");
        }
    }

    info!("daemon shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::wait_for_path;
    use std::time::Duration;

    #[tokio::test]
    async fn wait_for_path_sees_late_arrival() {
        let dir = std::env::temp_dir().join(format!("perigee-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ready = dir.join("local");
        let creator = {
            let ready = ready.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                std::fs::create_dir_all(ready).unwrap();
            })
        };
        assert!(wait_for_path(&ready, Duration::from_millis(5), Duration::from_secs(5)).await);
        creator.await.unwrap();
    }

    #[tokio::test]
    async fn wait_for_path_gives_up_after_timeout() {
        let missing = std::env::temp_dir().join("perigee-wait-never-created");
        assert!(
            !wait_for_path(
                &missing,
                Duration::from_millis(5),
                Duration::from_millis(30)
            )
            .await
        );
    }
}
