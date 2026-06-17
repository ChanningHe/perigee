use anyhow::{bail, Context, Result};
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

const SERVICE_NAME: &str = "perigee.service";
const SERVICE_PATH: &str = "/etc/systemd/system/perigee.service";
const BINARY_INSTALL_PATH: &str = "/usr/local/bin/perigee";

const SERVICE_CONTENT: &str = r#"[Unit]
Description=Perigee - Proxmox VE Helper Daemon
After=network-pre.target systemd-udev-settle.service
Before=network.target
DefaultDependencies=no

[Service]
Type=notify
NotifyAccess=main
# Create the control socket (and any other files) with no group/other access,
# closing the bind->chmod window on /run/perigee.sock.
UMask=0077
ExecStart=/usr/local/bin/perigee daemon
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=5s
TimeoutStartSec=30

[Install]
WantedBy=multi-user.target
"#;

pub fn install(force: bool) -> Result<()> {
    check_root()?;

    let current_exe = std::env::current_exe().context("cannot determine current binary path")?;
    let is_same_path = current_exe.to_str() == Some(BINARY_INSTALL_PATH);

    // -- Check existing binary --
    if !is_same_path && Path::new(BINARY_INSTALL_PATH).exists() {
        let existing_ver = probe_binary_version(BINARY_INSTALL_PATH);
        let current_ver = probe_binary_version(&current_exe.to_string_lossy());

        println!("Binary already exists: {}", BINARY_INSTALL_PATH);
        if let Some(v) = &existing_ver {
            print!("  installed: {}", v);
        }
        if let Some(v) = &current_ver {
            print!("  new: {}", v);
        }
        println!();

        if !force && !confirm("Overwrite the existing binary?")? {
            println!("Skipped binary install.");
        } else if service_is_active()
            && !force
            && !confirm("Service is running; stop it to replace the binary?")?
        {
            // A running daemon holds the executable inode, so an in-place copy
            // would fail with ETXTBSY. Refuse rather than half-install.
            println!("Skipped binary install (service still running).");
        } else {
            if service_is_active() {
                let _ = run_systemctl(&["stop", SERVICE_NAME]);
                println!("Stopped {} for upgrade.", SERVICE_NAME);
            }
            copy_binary(&current_exe)?;
        }
    } else if !is_same_path {
        copy_binary(&current_exe)?;
    } else {
        println!("Binary: {} (already in place)", BINARY_INSTALL_PATH);
    }

    // -- Check existing service file --
    if Path::new(SERVICE_PATH).exists() {
        let existing = std::fs::read_to_string(SERVICE_PATH).unwrap_or_default();
        if existing.trim() == SERVICE_CONTENT.trim() {
            println!("Service file: {} (unchanged)", SERVICE_PATH);
        } else {
            println!("Service file already exists: {}", SERVICE_PATH);
            println!("  The new service definition differs from the installed one.");
            if !force && !confirm("Overwrite the existing service file?")? {
                println!("Skipped service file update.");
            } else {
                write_service_file()?;
            }
        }
    } else {
        write_service_file()?;
    }

    // -- Config directory --
    let config_dir = Path::new("/etc/perigee");
    if !config_dir.exists() {
        std::fs::create_dir_all(config_dir).context("failed to create /etc/perigee")?;
        println!("Created config directory /etc/perigee/");
    }

    // -- Enable & start --
    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", SERVICE_NAME])?;
    println!("Service {} enabled.", SERVICE_NAME);

    // Restart (not start) so an upgrade picks up the freshly installed binary
    // even if the unit was left running. Done separately for a useful message.
    match run_systemctl(&["restart", SERVICE_NAME]) {
        Ok(()) => {
            println!("Service {} started.", SERVICE_NAME);
        }
        Err(_) => {
            eprintln!();
            eprintln!("Warning: service installed and enabled but failed to start.");
            eprintln!("  Check logs:  journalctl -xeu {}", SERVICE_NAME);
            eprintln!("  Check state: systemctl status {}", SERVICE_NAME);
            eprintln!();
            eprintln!("The service will attempt to start again on next boot.");
        }
    }

    Ok(())
}

pub fn uninstall() -> Result<()> {
    check_root()?;

    let _ = run_systemctl(&["stop", SERVICE_NAME]);
    let _ = run_systemctl(&["disable", SERVICE_NAME]);

    if Path::new(SERVICE_PATH).exists() {
        std::fs::remove_file(SERVICE_PATH)?;
        println!("Removed {}", SERVICE_PATH);
    }

    run_systemctl(&["daemon-reload"])?;
    println!("Service {} uninstalled.", SERVICE_NAME);

    if Path::new(BINARY_INSTALL_PATH).exists() {
        std::fs::remove_file(BINARY_INSTALL_PATH)?;
        println!("Removed {}", BINARY_INSTALL_PATH);
    }

    println!("Config directory /etc/perigee/ preserved. Remove manually if desired.");
    Ok(())
}

// ── helpers ──

fn copy_binary(src: &Path) -> Result<()> {
    // Write to a temp file then rename into place. Rename swaps the directory
    // entry atomically and never hits ETXTBSY, so it works even if a process
    // still holds the old binary's inode.
    let dst = Path::new(BINARY_INSTALL_PATH);
    let tmp = dst.with_extension("new");
    std::fs::copy(src, &tmp)
        .with_context(|| format!("failed to copy {} to {}", src.display(), tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    std::fs::rename(&tmp, dst)
        .with_context(|| format!("failed to install binary to {}", dst.display()))?;
    println!("Installed binary to {}", BINARY_INSTALL_PATH);
    Ok(())
}

fn service_is_active() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", SERVICE_NAME])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn write_service_file() -> Result<()> {
    std::fs::write(SERVICE_PATH, SERVICE_CONTENT).context("failed to write service file")?;
    println!("Wrote {}", SERVICE_PATH);
    Ok(())
}

fn probe_binary_version(path: &str) -> Option<String> {
    Command::new(path)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .map(|s| s.trim().to_string())
        })
}

fn confirm(prompt: &str) -> Result<bool> {
    print!("{} [y/N] ", prompt);
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn check_root() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("this operation requires root privileges");
    }
    Ok(())
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .args(args)
        .output()
        .context("failed to run systemctl")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("systemctl {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}
