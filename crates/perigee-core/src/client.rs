use anyhow::{Context, Result};
use crate::ipc::{Request, Response, SOCKET_PATH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct IpcClient;

impl IpcClient {
    pub async fn send(request: &Request) -> Result<Response> {
        let stream = UnixStream::connect(SOCKET_PATH)
            .await
            .with_context(|| {
                format!(
                    "cannot connect to daemon at {}. Is perigee daemon running?",
                    SOCKET_PATH
                )
            })?;

        let (reader, mut writer) = stream.into_split();
        let json = serde_json::to_string(request)? + "\n";
        writer.write_all(json.as_bytes()).await?;
        writer.shutdown().await?;

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await?;

        let response: Response = serde_json::from_str(line.trim())
            .context("failed to parse daemon response")?;
        Ok(response)
    }

    pub fn is_daemon_running() -> bool {
        std::path::Path::new(SOCKET_PATH).exists()
    }
}
