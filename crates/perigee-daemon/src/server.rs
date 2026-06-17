use anyhow::Result;
use perigee_core::ipc::{DaemonStatus, Request, Response, SOCKET_PATH};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{broadcast, Mutex};
use tracing::{error, info, warn};

use crate::module::ModuleRegistry;

pub struct IpcServer {
    registry: Arc<Mutex<ModuleRegistry>>,
    start_time: Instant,
    config: Arc<Mutex<toml::Value>>,
}

impl IpcServer {
    pub fn new(registry: Arc<Mutex<ModuleRegistry>>, config: Arc<Mutex<toml::Value>>) -> Self {
        Self {
            registry,
            start_time: Instant::now(),
            config,
        }
    }

    pub async fn run(&self, mut shutdown: broadcast::Receiver<()>) -> Result<()> {
        // Clean up stale socket
        let _ = std::fs::remove_file(SOCKET_PATH);

        let listener = UnixListener::bind(SOCKET_PATH)?;
        info!(path = SOCKET_PATH, "IPC server listening");

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((stream, _)) => {
                            let registry = self.registry.clone();
                            let start = self.start_time;
                            let config = self.config.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, registry, start, config).await {
                                    warn!(error = %e, "IPC connection error");
                                }
                            });
                        }
                        Err(e) => {
                            error!(error = %e, "accept error");
                        }
                    }
                }
                _ = shutdown.recv() => {
                    info!("IPC server shutting down");
                    let _ = std::fs::remove_file(SOCKET_PATH);
                    break;
                }
            }
        }
        Ok(())
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    registry: Arc<Mutex<ModuleRegistry>>,
    start_time: Instant,
    config: Arc<Mutex<toml::Value>>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut line = String::new();

    while reader.read_line(&mut line).await? > 0 {
        let request: Request = match serde_json::from_str(line.trim()) {
            Ok(req) => req,
            Err(e) => {
                let resp = Response::Error {
                    message: format!("invalid request: {}", e),
                };
                let json = serde_json::to_string(&resp)? + "\n";
                writer.write_all(json.as_bytes()).await?;
                line.clear();
                continue;
            }
        };

        let response = process_request(request, &registry, start_time, &config).await;
        let json = serde_json::to_string(&response)? + "\n";
        writer.write_all(json.as_bytes()).await?;
        line.clear();
    }

    Ok(())
}

async fn process_request(
    request: Request,
    registry: &Arc<Mutex<ModuleRegistry>>,
    start_time: Instant,
    config: &Arc<Mutex<toml::Value>>,
) -> Response {
    match request {
        Request::Status => {
            let reg = registry.lock().await;
            let uptime = start_time.elapsed().as_secs();
            Response::Status(DaemonStatus {
                uptime_secs: uptime,
                modules: reg.statuses(),
            })
        }
        Request::Reload => match crate::config::load_all_configs() {
            Ok(new_config) => {
                let mut cfg = config.lock().await;
                *cfg = new_config.clone();
                let mut reg = registry.lock().await;
                for module in reg.all_mut() {
                    if let Err(e) = module.reload(&new_config).await {
                        return Response::Error {
                            message: format!("reload failed for {}: {}", module.name(), e),
                        };
                    }
                }
                Response::Ok
            }
            Err(e) => Response::Error {
                message: format!("config load failed: {}", e),
            },
        },
        Request::ReloadModule { name } => {
            let new_config = match crate::config::load_all_configs() {
                Ok(c) => c,
                Err(e) => {
                    return Response::Error {
                        message: format!("config load failed: {}", e),
                    };
                }
            };
            let mut reg = registry.lock().await;
            if let Some(module) = reg.get_mut(&name) {
                match module.reload(&new_config).await {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error {
                        message: format!("reload failed: {}", e),
                    },
                }
            } else {
                Response::Error {
                    message: format!("module '{}' not found", name),
                }
            }
        }
        Request::Apply { profile } => {
            let mut reg = registry.lock().await;
            if let Some(module) = reg.get_mut("sriov") {
                match module.retry_profile(&profile) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error {
                        message: e.to_string(),
                    },
                }
            } else {
                Response::Error {
                    message: "SR-IOV module not loaded".to_string(),
                }
            }
        }
        Request::RetryFailed { profile } => {
            let mut reg = registry.lock().await;
            if let Some(module) = reg.get_mut("sriov") {
                match module.retry_profile(&profile) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error {
                        message: format!("retry failed for '{}': {}", profile, e),
                    },
                }
            } else {
                Response::Error {
                    message: "SR-IOV module not loaded".to_string(),
                }
            }
        }
        Request::ProfileStatus { profile } => {
            let reg = registry.lock().await;
            if let Some(module) = reg.get("sriov") {
                match module.profile_detail(&profile) {
                    Some(detail) => Response::ProfileDetail(detail),
                    None => Response::Error {
                        message: format!("profile '{}' not found", profile),
                    },
                }
            } else {
                Response::Error {
                    message: "SR-IOV module not loaded".to_string(),
                }
            }
        }
        Request::ProfileEvents { profile, limit } => {
            let reg = registry.lock().await;
            if let Some(module) = reg.get("sriov") {
                Response::Events(module.profile_events(&profile, limit))
            } else {
                Response::Events(Vec::new())
            }
        }
    }
}
