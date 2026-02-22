use async_trait::async_trait;
use perigee_core::ipc::{ModuleStatus, ProfileDetailStatus, ProfileEvent};
use std::collections::HashMap;

#[async_trait]
pub trait Module: Send + Sync {
    fn name(&self) -> &str;
    async fn init(&mut self, config: &toml::Value) -> anyhow::Result<()>;
    async fn apply(&mut self) -> anyhow::Result<()>;
    async fn reload(&mut self, config: &toml::Value) -> anyhow::Result<()>;
    async fn shutdown(&self) -> anyhow::Result<()>;
    fn status(&self) -> ModuleStatus;

    fn profile_detail(&self, _profile: &str) -> Option<ProfileDetailStatus> {
        None
    }
    fn profile_events(&self, _profile: &str, _limit: usize) -> Vec<ProfileEvent> {
        Vec::new()
    }
    fn retry_profile(&mut self, _profile: &str) -> anyhow::Result<()> {
        anyhow::bail!("retry not supported")
    }
}

pub struct ModuleRegistry {
    modules: HashMap<String, Box<dyn Module>>,
}

impl ModuleRegistry {
    pub fn new() -> Self {
        Self {
            modules: HashMap::new(),
        }
    }

    pub fn register(&mut self, module: Box<dyn Module>) {
        let name = module.name().to_string();
        self.modules.insert(name, module);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Module> {
        self.modules.get(name).map(|m| m.as_ref())
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Box<dyn Module>> {
        self.modules.get_mut(name)
    }

    pub fn all(&self) -> impl Iterator<Item = &dyn Module> {
        self.modules.values().map(|m| m.as_ref())
    }

    pub fn all_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Module>> {
        self.modules.values_mut()
    }

    pub fn statuses(&self) -> Vec<ModuleStatus> {
        self.modules.values().map(|m| m.status()).collect()
    }
}

impl Default for ModuleRegistry {
    fn default() -> Self {
        Self::new()
    }
}
