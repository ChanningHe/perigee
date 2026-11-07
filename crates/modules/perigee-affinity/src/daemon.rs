use async_trait::async_trait;
use perigee_core::ipc::{ModuleState, ModuleStatus};
use tracing::{error, info, warn};

use crate::config::AffinityConfig;
use crate::model::{ReconcileOutcome, ReconcileReport};
use crate::reconcile::{self, PveBackend};
use crate::topology_adapter;

pub struct AffinityModule {
    config: AffinityConfig,
    last_report: Option<ReconcileReport>,
    topology_quality: Option<crate::model::TopologyQuality>,
    last_error: Option<String>,
}

impl Default for AffinityModule {
    fn default() -> Self {
        Self::new()
    }
}

impl AffinityModule {
    pub fn new() -> Self {
        Self {
            config: AffinityConfig::default(),
            last_report: None,
            topology_quality: None,
            last_error: None,
        }
    }

    fn detect_topology(&mut self) -> anyhow::Result<()> {
        let topology = topology_adapter::detect()?;
        info!(
            quality = ?topology.quality,
            cache_source = ?topology.cache_domain_source,
            logical_cpus = topology.logical_cpus.len(),
            physical_cores = topology.physical_cores.len(),
            "normalized CPU topology detected"
        );
        self.topology_quality = Some(topology.quality);
        Ok(())
    }

    fn reconcile_once(&mut self) -> anyhow::Result<ReconcileReport> {
        let backend = PveBackend;
        let report = reconcile::reconcile(
            &backend,
            &self.config.policy(),
            &self.config.auto_apply.exclude_vmids,
        )?;
        self.last_report = Some(report.clone());
        Ok(report)
    }

    fn load_config(config: &toml::Value) -> anyhow::Result<AffinityConfig> {
        let Some(affinity) = config.get("affinity") else {
            return Ok(AffinityConfig::default());
        };
        let typed: AffinityConfig = affinity.clone().try_into()?;
        Ok(typed)
    }
}

#[async_trait]
impl perigee_daemon::module::Module for AffinityModule {
    fn name(&self) -> &str {
        "affinity"
    }

    async fn init(&mut self, config: &toml::Value) -> anyhow::Result<()> {
        self.config = Self::load_config(config)?;
        self.detect_topology().map_err(|error| {
            warn!(%error, "CPU topology detection failed during affinity init");
            error
        })?;
        Ok(())
    }

    async fn apply(&mut self) -> anyhow::Result<()> {
        if !self.config.enabled || !self.config.auto_apply.enabled {
            return Ok(());
        }

        let report = tokio::task::block_in_place(|| self.reconcile_once())?;
        match report.outcome {
            ReconcileOutcome::Healthy => {
                self.last_error = None;
                info!(
                    planned = report.planned,
                    changed = report.changed,
                    unchanged = report.unchanged,
                    "affinity reconcile complete"
                );
            }
            ReconcileOutcome::Degraded => {
                self.last_error = Some(report.warnings.join("; "));
                warn!(
                    failed = report.failed,
                    warnings = report.warnings.len(),
                    "affinity reconcile degraded"
                );
            }
            ReconcileOutcome::Failed => {
                self.last_error = Some(report.warnings.join("; "));
                error!(failed = report.failed, "affinity reconcile failed");
            }
        }
        Ok(())
    }

    async fn reload(&mut self, config: &toml::Value) -> anyhow::Result<()> {
        self.config = Self::load_config(config)?;
        self.detect_topology()?;
        if self.config.enabled && self.config.auto_apply.enabled {
            self.apply().await?;
        }
        Ok(())
    }

    async fn shutdown(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn status(&self) -> ModuleStatus {
        let state = if !self.config.enabled {
            ModuleState::Stopped
        } else if self.last_error.is_some() {
            ModuleState::Degraded
        } else if matches!(
            self.topology_quality,
            Some(crate::model::TopologyQuality::Invalid)
        ) {
            ModuleState::Error
        } else {
            ModuleState::Running
        };

        ModuleStatus {
            name: "affinity".to_string(),
            state,
            profiles: Vec::new(),
        }
    }
}
