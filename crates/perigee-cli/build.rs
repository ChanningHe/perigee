fn main() {
    println!("cargo::rerun-if-changed=../../.git/HEAD");
    println!("cargo::rerun-if-changed=../../.git/refs");

    let cargo_ver = std::env::var("CARGO_PKG_VERSION").unwrap();

    let git_desc = std::process::Command::new("git")
        .args(["describe", "--always", "--dirty", "--tags"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    let is_release = std::process::Command::new("git")
        .args(["describe", "--exact-match", "--tags", "HEAD"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let version_string = if is_release {
        cargo_ver
    } else {
        format!("{}-dev ({})", cargo_ver, git_desc)
    };

    println!("cargo::rustc-env=PERIGEE_VERSION_STRING={}", version_string);
}
