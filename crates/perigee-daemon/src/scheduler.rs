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
///
/// Every module of the phase is applied. A failed PreNetwork module then
/// makes this return `Err` (the daemon must not report READY); PostNetwork
/// failures are only logged.
pub async fn boot_apply(registry: &Arc<Mutex<ModuleRegistry>>, phase: BootPhase) -> Result<()> {
    info!(?phase, "starting boot-time apply");

    let module_names: Vec<String> = {
        let reg = registry.lock().await;
        reg.all()
            .filter(|m| m.boot_phase() == phase)
            .map(|m| m.name().to_string())
            .collect()
    };

    let mut failed = Vec::new();
    for name in &module_names {
        info!(module = %name, "applying module");
        let mut reg = registry.lock().await;
        if let Some(module) = reg.get_mut(name) {
            if let Err(e) = module.apply().await {
                error!(module = %name, error = format!("{e:#}"), "module apply failed");
                failed.push(format!("{name}: {e:#}"));
            }
        }
        // Lock released here — IPC can serve requests between modules
    }

    info!(?phase, "boot-time apply complete");
    if phase == BootPhase::PreNetwork && !failed.is_empty() {
        anyhow::bail!("pre-network apply failed ({})", failed.join("; "));
    }
    Ok(())
}

/// Pre-network apply, then `ready` (sd_notify READY). On failure `ready` is
/// never called: the caller exits non-zero and systemd restarts the daemon,
/// which retries the apply.
async fn pre_network_then_ready(
    registry: &Arc<Mutex<ModuleRegistry>>,
    ready: impl FnOnce(),
) -> Result<()> {
    crate::notify::sd_notify_status("applying pre-network modules");
    boot_apply(registry, BootPhase::PreNetwork).await?;
    ready();
    Ok(())
}

async fn shutdown_modules(registry: &Arc<Mutex<ModuleRegistry>>) {
    let reg = registry.lock().await;
    for module in reg.all() {
        if let Err(e) = module.shutdown().await {
            error!(module = module.name(), error = %e, "module shutdown error");
        }
    }
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
    if let Err(e) = boot_apply(&registry, BootPhase::PostNetwork).await {
        error!(error = format!("{e:#}"), "post-network apply failed");
    }
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
    // ordered Before=network.target, so networking waits for them. If one
    // fails (e.g. VF configuration cut short by a hung `ip`), exit without
    // READY so systemd restarts the daemon and the apply is retried.
    if let Err(e) = pre_network_then_ready(&registry, crate::notify::sd_notify_ready).await {
        error!(
            error = format!("{e:#}"),
            "exiting without READY so systemd restarts the daemon"
        );
        crate::notify::sd_notify_status(&format!("pre-network apply failed, restarting: {e:#}"));
        let _ = shutdown_tx.send(());
        // The IPC task removes its socket on shutdown; wait for it.
        let _ = ipc_handle.await;
        shutdown_modules(&registry).await;
        return Err(e);
    }
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

    shutdown_modules(&registry).await;

    info!("daemon shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use perigee_core::ipc::{ModuleState, ModuleStatus};
    use std::cell::Cell;

    struct FakeModule {
        phase: BootPhase,
        fails: bool,
    }

    #[async_trait]
    impl crate::module::Module for FakeModule {
        fn name(&self) -> &str {
            "fake"
        }
        fn boot_phase(&self) -> BootPhase {
            self.phase
        }
        async fn init(&mut self, _config: &toml::Value) -> Result<()> {
            Ok(())
        }
        async fn apply(&mut self) -> Result<()> {
            if self.fails {
                anyhow::bail!("VF configuration cut short");
            }
            Ok(())
        }
        async fn reload(&mut self, _config: &toml::Value) -> Result<()> {
            Ok(())
        }
        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }
        fn status(&self) -> ModuleStatus {
            ModuleStatus {
                name: "fake".into(),
                state: ModuleState::Running,
                profiles: Vec::new(),
            }
        }
    }

    fn registry(phase: BootPhase, fails: bool) -> Arc<Mutex<ModuleRegistry>> {
        let mut reg = ModuleRegistry::new();
        reg.register(Box::new(FakeModule { phase, fails }));
        Arc::new(Mutex::new(reg))
    }

    #[tokio::test]
    async fn failed_pre_network_module_prevents_ready() {
        let ready = Cell::new(false);
        let res =
            pre_network_then_ready(&registry(BootPhase::PreNetwork, true), || ready.set(true))
                .await;
        assert!(res.unwrap_err().to_string().contains("cut short"));
        assert!(!ready.get());
    }

    #[tokio::test]
    async fn successful_pre_network_apply_reports_ready() {
        let ready = Cell::new(false);
        pre_network_then_ready(&registry(BootPhase::PreNetwork, false), || ready.set(true))
            .await
            .unwrap();
        assert!(ready.get());
    }

    #[tokio::test]
    async fn failed_post_network_module_is_not_fatal() {
        let reg = registry(BootPhase::PostNetwork, true);
        boot_apply(&reg, BootPhase::PostNetwork).await.unwrap();
        let ready = Cell::new(false);
        pre_network_then_ready(&reg, || ready.set(true))
            .await
            .unwrap();
        assert!(ready.get(), "a post-network failure never blocks READY");
    }

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
