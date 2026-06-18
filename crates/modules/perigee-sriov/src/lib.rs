pub mod config;
pub mod daemon;
pub mod detect;
pub mod fdb;
pub mod mac_strategy;
pub mod ui;
pub mod vendor;
pub mod vf;
pub mod vm_usage;

pub fn module_info() -> (&'static str, &'static str) {
    ("SR-IOV", "Configure SR-IOV virtual functions")
}

pub fn create_module() -> Box<dyn perigee_daemon::module::Module> {
    Box::new(daemon::SriovModule::new())
}
