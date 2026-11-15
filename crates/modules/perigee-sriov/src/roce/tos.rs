//! Host RDMA CM default ToS (`default_roce_tos` in rdma_cm configfs).
//!
//! The PF's RDMA device and the rdma_cm configfs tree can appear after
//! perigee's pre-network apply (mlx5_ib or rdma_cm loading late). That case
//! is reported as pending, not as an error, so the daemon can retry it
//! without holding up boot; see [`super::retry`].

use std::io;
use std::path::{Path, PathBuf};

use super::parse::parse_tos;
use super::StepError;

/// Each mlx5 PF is its own RDMA device with a single port.
const ROCE_PORT: u32 = 1;
/// errno ENODEV (same value on Linux and the BSDs; avoids a libc dependency).
const ENODEV: i32 = 19;

/// Filesystem roots used for the ToS write; injectable for tests.
#[derive(Debug, Clone)]
pub struct TosPaths {
    /// `/sys/class/net`
    pub net_class: PathBuf,
    /// `/sys/kernel/config/rdma_cm`
    pub rdma_cm: PathBuf,
}

impl TosPaths {
    pub fn system() -> Self {
        Self {
            net_class: PathBuf::from("/sys/class/net"),
            rdma_cm: PathBuf::from("/sys/kernel/config/rdma_cm"),
        }
    }
}

/// A ToS write that could not be made yet because its prerequisites were
/// missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTos {
    pub pf: String,
    pub tos: u8,
    /// `modprobe rdma_cm` already timed out: never run it again, only wait
    /// for the configfs tree (the kernel may still finish loading it).
    pub skip_modprobe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TosAttempt {
    /// The value is now in place; `changed` when it had to be written.
    Done { changed: bool },
    /// RDMA device or rdma_cm configfs not present (yet); holds why.
    Pending(String),
    /// Device present but reading or writing the value failed.
    Failed(String),
}

/// Read the current ToS and write `tos` only if it differs.
pub fn try_write_tos(
    paths: &TosPaths,
    pf: &str,
    tos: u8,
    load_rdma_cm: impl FnOnce() -> Result<(), StepError>,
) -> TosAttempt {
    match prepare(paths, pf, load_rdma_cm) {
        Ok(ibdev) => write_if_differs(paths, &ibdev, tos),
        Err(attempt) => attempt,
    }
}

/// Run `load` (modprobe) unless it timed out before; a timeout sets `skip`,
/// so a hung modprobe is never run twice.
pub(super) fn guarded_load(
    skip: &mut bool,
    load: impl FnOnce() -> Result<(), StepError>,
) -> Result<(), StepError> {
    if *skip {
        return Err(StepError::Failed(
            "`modprobe rdma_cm` timed out earlier; not run again".into(),
        ));
    }
    let res = load();
    *skip = res.as_ref().is_err_and(StepError::is_timeout);
    res
}

/// The PF's RDMA device, once it and the rdma_cm configfs tree exist (the
/// tree appears only once rdma_cm is loaded, hence `load_rdma_cm`). `Err`
/// is `Pending` with the reason.
pub(super) fn prepare(
    paths: &TosPaths,
    pf: &str,
    load_rdma_cm: impl FnOnce() -> Result<(), StepError>,
) -> Result<String, TosAttempt> {
    let ibdev = find_rdma_device(&paths.net_class, pf)
        .map_err(|e| TosAttempt::Pending(format!("waiting for the RDMA device of {pf} ({e})")))?;
    if paths.rdma_cm.is_dir() {
        return Ok(ibdev);
    }
    let why = match load_rdma_cm() {
        _ if paths.rdma_cm.is_dir() => return Ok(ibdev),
        Err(e) => e.to_string(),
        Ok(()) => "is configfs mounted?".to_string(),
    };
    Err(TosAttempt::Pending(format!(
        "waiting for {} to appear ({why})",
        paths.rdma_cm.display()
    )))
}

/// Idempotent write: a value already in place is not rewritten.
pub(super) fn write_if_differs(paths: &TosPaths, ibdev: &str, tos: u8) -> TosAttempt {
    let group = paths.rdma_cm.join(ibdev);
    let path = group
        .join("ports")
        .join(ROCE_PORT.to_string())
        .join("default_roce_tos");
    // mkdir in rdma_cm configfs creates the device group and its ports/.
    let created = if group.is_dir() {
        false
    } else {
        match classify_mkdir(std::fs::create_dir(&group), &group) {
            Ok(created) => created,
            Err(attempt) => return attempt,
        }
    };
    if !created {
        let current = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))
            .and_then(|c| parse_tos(&c));
        match current {
            Ok(t) if t == tos => return TosAttempt::Done { changed: false },
            Ok(_) => {}
            Err(e) => return TosAttempt::Failed(e),
        }
    }
    match std::fs::write(&path, tos.to_string()) {
        Ok(()) => TosAttempt::Done { changed: true },
        Err(e) => TosAttempt::Failed(format!("cannot write {}: {e}", path.display())),
    }
}

/// `Ok(true)`: group created; `Ok(false)`: someone else created it first.
/// ENODEV means rdma_cm has not attached the device yet, so it is pending.
fn classify_mkdir(res: io::Result<()>, group: &Path) -> Result<bool, TosAttempt> {
    match res {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) if e.raw_os_error() == Some(ENODEV) => Err(TosAttempt::Pending(format!(
            "waiting for rdma_cm to attach {} ({e})",
            group.display()
        ))),
        Err(e) => Err(TosAttempt::Failed(format!(
            "cannot create {}: {e}",
            group.display()
        ))),
    }
}

/// RDMA device backing the PF (e.g. mlx5_0).
fn find_rdma_device(net_class: &Path, pf: &str) -> Result<String, String> {
    let dir = net_class.join(pf).join("device").join("infiniband");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("cannot list {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
        .into_iter()
        .next()
        .ok_or_else(|| format!("no RDMA device under {}", dir.display()))
}

#[cfg(test)]
pub(crate) mod test_env {
    use super::TosPaths;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    pub const PF: &str = "enp65s0np0";
    pub const IBDEV: &str = "mlx5_0";

    /// A throwaway sysfs/configfs lookalike, removed on drop.
    pub struct FakeSys {
        pub root: PathBuf,
        pub paths: TosPaths,
    }

    impl FakeSys {
        pub fn new() -> Self {
            static SEQ: AtomicU32 = AtomicU32::new(0);
            let root = std::env::temp_dir().join(format!(
                "perigee-tos-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            let paths = TosPaths {
                net_class: root.join("net"),
                rdma_cm: root.join("rdma_cm"),
            };
            std::fs::create_dir_all(&paths.net_class).unwrap();
            Self { root, paths }
        }

        pub fn add_rdma_device(&self) {
            let dir = self.paths.net_class.join(PF).join("device/infiniband");
            std::fs::create_dir_all(dir.join(IBDEV)).unwrap();
        }

        /// rdma_cm configfs with the device group already created, holding
        /// `current` as the ToS.
        pub fn add_configfs_group(&self, current: &str) {
            let ports = self.tos_file().parent().unwrap().to_path_buf();
            std::fs::create_dir_all(&ports).unwrap();
            std::fs::write(self.tos_file(), current).unwrap();
        }

        pub fn tos_file(&self) -> PathBuf {
            self.paths
                .rdma_cm
                .join(IBDEV)
                .join("ports/1/default_roce_tos")
        }

        pub fn read_tos(&self) -> String {
            std::fs::read_to_string(self.tos_file()).unwrap()
        }

        pub fn rdma_cm(&self) -> &Path {
            &self.paths.rdma_cm
        }
    }

    impl Drop for FakeSys {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_env::*;
    use super::*;

    fn no_load() -> Result<(), StepError> {
        panic!("rdma_cm must not be loaded when configfs exists")
    }

    #[test]
    fn missing_rdma_device_is_pending() {
        let sys = FakeSys::new();
        sys.add_configfs_group("0");
        let res = try_write_tos(&sys.paths, PF, 106, no_load);
        assert!(matches!(res, TosAttempt::Pending(ref r) if r.contains("RDMA device")));
        assert_eq!(sys.read_tos(), "0");
    }

    #[test]
    fn missing_configfs_is_pending_after_trying_to_load_rdma_cm() {
        let sys = FakeSys::new();
        sys.add_rdma_device();
        let mut loads = 0;
        let res = try_write_tos(&sys.paths, PF, 106, || {
            loads += 1;
            Err(StepError::Failed("modprobe: not found".into()))
        });
        assert_eq!(loads, 1);
        assert!(matches!(res, TosAttempt::Pending(ref r) if r.contains("modprobe: not found")));
    }

    #[test]
    fn modprobe_timeout_is_pending_and_never_rerun() {
        let sys = FakeSys::new();
        sys.add_rdma_device();
        let (mut skip, mut loads) = (false, 0);
        let mut hung = || {
            loads += 1;
            Err(StepError::TimedOut("`modprobe rdma_cm` timed out".into()))
        };
        let res = try_write_tos(&sys.paths, PF, 106, || guarded_load(&mut skip, &mut hung));
        assert!(
            matches!(res, TosAttempt::Pending(ref e) if e.contains("timed out")),
            "{res:?}"
        );
        assert!(skip);
        let res = try_write_tos(&sys.paths, PF, 106, || guarded_load(&mut skip, &mut hung));
        assert!(matches!(res, TosAttempt::Pending(ref e) if e.contains("not run again")));
        assert_eq!(loads, 1);
    }

    #[test]
    fn modprobe_failure_without_timeout_may_run_again() {
        let mut skip = false;
        let _ = guarded_load(&mut skip, || Err(StepError::Failed("not found".into())));
        assert!(!skip);
    }

    #[test]
    fn mkdir_errors_are_classified() {
        let group = Path::new("/x/mlx5_0");
        assert_eq!(classify_mkdir(Ok(()), group), Ok(true));
        let exists = io::Error::from(io::ErrorKind::AlreadyExists);
        assert_eq!(classify_mkdir(Err(exists), group), Ok(false));
        let nodev = io::Error::from_raw_os_error(ENODEV);
        assert!(matches!(
            classify_mkdir(Err(nodev), group),
            Err(TosAttempt::Pending(_))
        ));
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(matches!(
            classify_mkdir(Err(denied), group),
            Err(TosAttempt::Failed(_))
        ));
    }

    #[test]
    fn matching_value_is_not_rewritten() {
        let sys = FakeSys::new();
        sys.add_rdma_device();
        sys.add_configfs_group("106\n");
        let res = try_write_tos(&sys.paths, PF, 106, no_load);
        assert_eq!(res, TosAttempt::Done { changed: false });
        assert_eq!(sys.read_tos(), "106\n");
    }

    #[test]
    fn differing_value_is_written() {
        let sys = FakeSys::new();
        sys.add_rdma_device();
        sys.add_configfs_group("0");
        let res = try_write_tos(&sys.paths, PF, 106, no_load);
        assert_eq!(res, TosAttempt::Done { changed: true });
        assert_eq!(sys.read_tos(), "106");
    }

    #[test]
    fn rejected_write_with_device_present_is_an_error() {
        // Group mkdir succeeds but no ports/1/ appears (configfs would create
        // it), so the write itself fails: a real error, not pending.
        let sys = FakeSys::new();
        sys.add_rdma_device();
        std::fs::create_dir_all(sys.rdma_cm()).unwrap();
        let res = try_write_tos(&sys.paths, PF, 106, no_load);
        assert!(matches!(res, TosAttempt::Failed(ref e) if e.contains("cannot write")));
    }

    #[test]
    fn unreadable_current_value_is_an_error() {
        let sys = FakeSys::new();
        sys.add_rdma_device();
        sys.add_configfs_group("garbage");
        let res = try_write_tos(&sys.paths, PF, 106, no_load);
        assert!(matches!(res, TosAttempt::Failed(_)));
        assert_eq!(sys.read_tos(), "garbage");
    }
}
