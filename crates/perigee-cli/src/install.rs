use anyhow::{bail, Context, Result};
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

const SERVICE_NAME: &str = "perigee.service";
const SERVICE_PATH: &str = "/etc/systemd/system/perigee.service";
const BINARY_INSTALL_PATH: &str = "/usr/local/bin/perigee";
const REPO: &str = "channinghe/perigee";

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

/// Download the latest release binary from GitHub and swap it into place.
/// Trusts the GitHub release for `REPO` over HTTPS (same trust model as any
/// self-updater); curl is used since it is always present on PVE.
pub fn update(force: bool) -> Result<()> {
    check_root()?;

    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("unsupported architecture for self-update: {}", other),
    };
    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: {}", current);

    // Resolve the latest release tag via the GitHub API.
    let api = format!("https://api.github.com/repos/{}/releases/latest", REPO);
    let json = curl_capture(&[
        "-fsSL",
        "-A",
        "perigee-updater",
        "-H",
        "Accept: application/vnd.github+json",
        api.as_str(),
    ])
    .context("failed to query GitHub releases (is curl installed and the network up?)")?;
    let parsed: serde_json::Value =
        serde_json::from_str(&json).context("unexpected GitHub API response")?;
    let tag = parsed["tag_name"]
        .as_str()
        .context("no tag_name in GitHub API response")?;
    let latest = tag.strip_prefix('v').unwrap_or(tag);
    println!("Latest release:  {}", latest);

    if !force && !is_newer(current, latest) {
        println!("Already up to date.");
        return Ok(());
    }

    // Download next to the install path so the final move is an atomic rename.
    let asset = format!("perigee-{}-linux-{}-musl", latest, arch);
    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        REPO, tag, asset
    );
    let dst = Path::new(BINARY_INSTALL_PATH);
    let tmp = dst.with_extension("update");
    println!("Downloading {} ...", asset);
    curl_download(&url, &tmp).with_context(|| format!("failed to download {}", url))?;

    // Verify the published SHA-256 before trusting (or executing) the binary.
    let sums = curl_capture(&["-fsSL", "-A", "perigee-updater", &format!("{}.sha256", url)])
        .context("failed to download checksum (.sha256)")?;
    let expected = sums
        .split_whitespace()
        .next()
        .context("empty checksum file")?
        .to_lowercase();
    let actual = sha256_hex(&tmp)?;
    if actual != expected {
        let _ = std::fs::remove_file(&tmp);
        bail!(
            "checksum mismatch (expected {}, got {}); aborting update",
            expected,
            actual
        );
    }
    println!("Checksum verified.");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }

    // Sanity-check that the verified binary actually runs on this host.
    let runs = Command::new(&tmp)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !runs {
        let _ = std::fs::remove_file(&tmp);
        bail!("downloaded binary failed to run; aborting update");
    }

    // Replace the binary, stopping the service if it holds the old inode.
    let was_active = service_is_active();
    if was_active {
        let _ = run_systemctl(&["stop", SERVICE_NAME]);
        println!("Stopped {} for upgrade.", SERVICE_NAME);
    }
    std::fs::rename(&tmp, dst)
        .with_context(|| format!("failed to install binary to {}", dst.display()))?;
    println!("Updated {} -> {}", current, latest);

    if was_active {
        run_systemctl(&["restart", SERVICE_NAME])?;
        println!("Service {} restarted.", SERVICE_NAME);
    }
    Ok(())
}

/// True when `latest` is a strictly higher semver than `current`. Falls back to
/// a plain inequality when either side is not parseable, so unusual tags still
/// allow an update rather than silently refusing.
fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => latest != current,
    }
}

fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    Some((a, b, c))
}

fn sha256_hex(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|b| format!("{:02x}", b)).collect())
}

fn curl_capture(args: &[&str]) -> Result<String> {
    let out = Command::new("curl")
        .args(args)
        .output()
        .context("failed to run curl")?;
    if !out.status.success() {
        bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("curl output was not UTF-8")
}

fn curl_download(url: &str, dst: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args(["-fL", "--retry", "2", "-o"])
        .arg(dst)
        .arg(url)
        .status()
        .context("failed to run curl")?;
    if !status.success() {
        bail!("download failed (HTTP error or network issue)");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions_are_detected() {
        assert!(is_newer("0.0.5", "0.0.6"));
        assert!(is_newer("0.0.5", "0.1.0"));
        assert!(is_newer("0.9.9", "1.0.0"));
    }

    #[test]
    fn same_or_older_is_not_newer() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("1.0.0", "0.9.9"));
    }

    #[test]
    fn unparseable_falls_back_to_inequality() {
        // A non-semver tag still allows an update when it differs.
        assert!(is_newer("0.1.0", "nightly"));
        assert!(!is_newer("nightly", "nightly"));
    }

    #[test]
    fn sha256_matches_known_vector() {
        let path = std::env::temp_dir().join("perigee_sha256_test.bin");
        std::fs::write(&path, b"hello").unwrap();
        let hex = sha256_hex(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            hex,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }
}
