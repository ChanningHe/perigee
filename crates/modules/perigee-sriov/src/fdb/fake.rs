//! In-memory [`FdbHost`] shared by the FDB unit tests.

use anyhow::Result;
use std::cell::RefCell;
use std::collections::BTreeSet;

use super::reconcile::{FdbHost, FdbTarget};
use super::scan::GuestNic;
use crate::config::FdbMode;

pub const PF_MAC: &str = "b8:ce:f6:00:00:01";
pub const PF: &str = "enp1s0f0";
pub const BR: &str = "vmbr0";
pub const A: &str = "52:54:00:00:00:0a";
pub const B: &str = "52:54:00:00:00:0b";

/// One PF with a self FDB.
pub struct FakeHost {
    pub fdb: RefCell<BTreeSet<String>>,
    pub bridges: BTreeSet<String>,
    pub pf_online: bool,
    pub failing: BTreeSet<String>,
}

impl FakeHost {
    pub fn new(present: &[&str]) -> Self {
        Self {
            fdb: RefCell::new(present.iter().map(|s| s.to_string()).collect()),
            bridges: [BR.to_string()].into(),
            pf_online: true,
            failing: BTreeSet::new(),
        }
    }

    pub fn fdb(&self) -> Vec<String> {
        self.fdb.borrow().iter().cloned().collect()
    }

    fn check(&self, mac: &str) -> Result<()> {
        if self.failing.contains(mac) {
            anyhow::bail!("injected failure");
        }
        Ok(())
    }
}

impl FdbHost for FakeHost {
    fn resolve_pf(&self, pf_mac: &str) -> Result<String> {
        if self.pf_online && pf_mac == PF_MAC {
            Ok(PF.to_string())
        } else {
            anyhow::bail!("no such interface")
        }
    }
    fn bridges_of(&self, _pf_dev: &str) -> BTreeSet<String> {
        self.bridges.clone()
    }
    fn present_macs(&self, _pf_dev: &str) -> Result<BTreeSet<String>> {
        Ok(self.fdb.borrow().clone())
    }
    fn add(&self, mac: &str, _pf_dev: &str) -> Result<()> {
        self.check(mac)?;
        self.fdb.borrow_mut().insert(mac.to_string());
        Ok(())
    }
    fn del(&self, mac: &str, _pf_dev: &str) -> Result<()> {
        self.check(mac)?;
        self.fdb.borrow_mut().remove(mac);
        Ok(())
    }
}

/// Profile "p" on the fake PF.
pub fn target(mode: FdbMode) -> FdbTarget {
    FdbTarget {
        profile: "p".to_string(),
        pf_mac: PF_MAC.to_string(),
        mode,
    }
}

pub fn nic(vmid: &str, mac: &str, bridge: &str) -> GuestNic {
    GuestNic {
        vmid: vmid.to_string(),
        mac: mac.to_string(),
        bridge: bridge.to_string(),
    }
}
