use std::env;
use std::os::unix::net::UnixDatagram;
use tracing::{debug, warn};

/// Send sd_notify READY=1 to systemd.
/// Safe to call when not running under systemd — silently does nothing.
pub fn sd_notify_ready() {
    if let Err(e) = try_sd_notify("READY=1") {
        warn!(error = %e, "sd_notify failed (not running under systemd?)");
    }
}

/// Send sd_notify STATUS=<msg> to systemd.
pub fn sd_notify_status(msg: &str) {
    let _ = try_sd_notify(&format!("STATUS={}", msg));
}

/// Send sd_notify STOPPING=1 to systemd.
pub fn sd_notify_stopping() {
    let _ = try_sd_notify("STOPPING=1");
}

fn try_sd_notify(state: &str) -> Result<(), String> {
    let socket_path = match env::var("NOTIFY_SOCKET") {
        Ok(p) => p,
        Err(_) => {
            debug!("NOTIFY_SOCKET not set, skipping sd_notify");
            return Ok(());
        }
    };

    let sock = UnixDatagram::unbound().map_err(|e| e.to_string())?;
    sock.send_to(state.as_bytes(), &socket_path)
        .map_err(|e| format!("send_to {}: {}", socket_path, e))?;

    debug!(state, socket = %socket_path, "sd_notify sent");
    Ok(())
}
