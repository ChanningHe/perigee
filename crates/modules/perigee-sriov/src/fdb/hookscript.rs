//! Hookscript fallback: let PVE add/remove FDB entries at guest start/stop.

use anyhow::Result;
use std::path::Path;
use tracing::info;

/// Generate a hookscript for FDB management (fallback mode).
pub fn generate_hookscript(output_path: &Path, pf_dev: &str) -> Result<()> {
    let script = render_hookscript(output_path, pf_dev);

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output_path, script)?;

    #[cfg(unix)]
    {
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(output_path, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    info!(path = %output_path.display(), "hookscript generated");
    Ok(())
}

/// Render the hookscript text.
fn render_hookscript(output_path: &Path, pf_dev: &str) -> String {
    format!(
        r#"#!/bin/bash
# Perigee FDB hookscript - auto-generated
# Attach to VM: qm set <vmid> --hookscript local:snippets/$(basename {output_path})

VMID="$1"
PHASE="$2"
PF_DEV="{pf_dev}"

get_vm_macs() {{
    local conf="/etc/pve/qemu-server/${{VMID}}.conf"
    [ -f "$conf" ] || conf="/etc/pve/lxc/${{VMID}}.conf"
    [ -f "$conf" ] || return
    grep -oP '(?<==)[0-9a-fA-F]{{2}}(:[0-9a-fA-F]{{2}}){{5}}' "$conf"
}}

case "$PHASE" in
    pre-start)
        for mac in $(get_vm_macs); do
            bridge fdb add "$mac" dev "$PF_DEV" 2>/dev/null
        done
        ;;
    post-stop)
        for mac in $(get_vm_macs); do
            bridge fdb del "$mac" dev "$PF_DEV" 2>/dev/null
        done
        ;;
esac
# Always succeed: PVE aborts the guest start on a failing pre-start hook.
exit 0
"#,
        output_path = output_path.display(),
        pf_dev = pf_dev,
    )
}

#[cfg(test)]
mod tests {
    use super::render_hookscript;
    use std::path::Path;

    #[test]
    fn hookscript_always_exits_zero() {
        // PVE aborts the guest start when a pre-start hook exits non-zero, and
        // `bridge fdb add` fails whenever the entry already exists.
        let script = render_hookscript(Path::new("/tmp/hook.sh"), "enp1s0f0");
        assert!(script.trim_end().ends_with("exit 0"));
    }
}
