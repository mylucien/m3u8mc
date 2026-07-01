//! 控制接口（设计文档 1.1 / 3.2）：Unix Domain Socket，
//! 对外提供 reload / status / channel start/stop/restart 等指令，
//! 协议见 [`m3u8mc_common::protocol`]，供 `m3u8mcctl` 和 init 脚本调用。

use crate::manager::ChannelManager;
use m3u8mc_common::protocol::{Request, Response};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

pub async fn serve(socket_path: &str, manager: Arc<ChannelManager>) -> anyhow::Result<()> {
    // 旧 socket 文件残留会导致 bind 失败，daemon 重启场景下需要先清理
    let _ = std::fs::remove_file(socket_path);
    if let Some(parent) = std::path::Path::new(socket_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let listener = UnixListener::bind(socket_path)?;
    tracing::info!(socket_path, "control socket listening");

    loop {
        let (stream, _addr) = listener.accept().await?;
        let manager = Arc::clone(&manager);
        tokio::spawn(async move {
            if let Err(e) = handle_conn(stream, manager).await {
                tracing::debug!(error = %e, "control connection ended with error");
            }
        });
    }
}

async fn handle_conn(stream: tokio::net::UnixStream, manager: Arc<ChannelManager>) -> anyhow::Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();

    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(req) => dispatch(req, &manager).await,
            Err(e) => Response::Error {
                message: format!("invalid request: {e}"),
            },
        };
        let mut payload = serde_json::to_string(&response)?;
        payload.push('\n');
        write_half.write_all(payload.as_bytes()).await?;
    }
    Ok(())
}

async fn dispatch(req: Request, manager: &Arc<ChannelManager>) -> Response {
    match req {
        Request::Status => Response::Status(manager.status().await),
        Request::Reload => match manager.reload().await {
            Ok(()) => Response::Ok,
            Err(e) => Response::Error { message: e.to_string() },
        },
        Request::ChannelStart { name } => match manager.channel_start(&name).await {
            Ok(()) => Response::Ok,
            Err(e) => Response::Error { message: e.to_string() },
        },
        Request::ChannelStop { name } => match manager.channel_stop(&name).await {
            Ok(()) => Response::Ok,
            Err(e) => Response::Error { message: e.to_string() },
        },
        Request::ChannelRestart { name } => match manager.channel_restart(&name).await {
            Ok(()) => Response::Ok,
            Err(e) => Response::Error { message: e.to_string() },
        },
    }
}
