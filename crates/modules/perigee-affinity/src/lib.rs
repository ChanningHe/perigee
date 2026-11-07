pub mod affinity;
pub mod config;
pub mod daemon;
pub mod inventory;
pub mod model;
pub mod planner;
pub mod pve;
pub mod reconcile;
pub mod topology;
pub mod topology_adapter;
pub mod ui;

pub fn module_info() -> (&'static str, &'static str) {
    ("CPU Affinity", "CPU core pinning & CCD topology")
}

pub fn create_module() -> Box<dyn perigee_daemon::module::Module> {
    Box::new(daemon::AffinityModule::new())
}
