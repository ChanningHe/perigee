//! Persist FDB ownership across daemon restarts.
//!
//! The file lives under `/run`: kernel FDB entries do not survive a reboot and
//! neither should the record of owning them.

use anyhow::{Context, Result};
use std::path::Path;
use tracing::warn;

use super::reconcile::OwnedMap;

pub const OWNED_PATH: &str = "/run/perigee/fdb-owned.json";

/// Load the ownership map. A missing file is a fresh boot; an unreadable or
/// corrupt one is logged and treated as empty, which can only leak entries,
/// never delete foreign ones.
pub fn load(path: &Path) -> OwnedMap {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return OwnedMap::new(),
        Err(e) => {
            warn!(path = %path.display(), error = %e, "cannot read FDB ownership");
            return OwnedMap::new();
        }
    };
    serde_json::from_str(&content).unwrap_or_else(|e| {
        warn!(path = %path.display(), error = %e, "corrupt FDB ownership, starting empty");
        OwnedMap::new()
    })
}

/// Atomically replace the ownership file (write temp + rename).
pub fn save(path: &Path, owned: &OwnedMap) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(owned)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fdb::reconcile::Owned;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("perigee-fdb-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("owned.json")
    }

    #[test]
    fn round_trips() {
        let path = scratch("roundtrip");
        let owned = OwnedMap::from([(
            "p".to_string(),
            Owned {
                pf_dev: "enp1s0f0".to_string(),
                macs: ["52:54:00:00:00:0a".to_string()].into(),
            },
        )]);
        save(&path, &owned).unwrap();
        assert_eq!(load(&path), owned);
    }

    #[test]
    fn missing_or_corrupt_file_loads_empty() {
        let path = scratch("corrupt");
        assert!(load(&path).is_empty());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_empty());
    }
}
