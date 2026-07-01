//! daemon 与 `m3u8mcctl` 之间通过 Unix Domain Socket 通信的协议。
//!
//! 每条消息是一行 JSON（newline-delimited JSON），便于用简单的逐行读写实现，
//! 不需要额外的帧协议。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "args", rename_all = "snake_case")]
pub enum Request {
    Status,
    Reload,
    ChannelStart { name: String },
    ChannelStop { name: String },
    ChannelRestart { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelStatus {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub running: bool,
    pub mode: String, // "standard" | "passthrough"
    pub cached_segments: usize,
    pub active_server: Option<String>,
    pub ffmpeg_pid: Option<u32>,
    pub ffmpeg_restarts: u64,
    pub overloaded: bool,
    pub last_error: Option<String>,
    pub play_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub uptime_secs: u64,
    pub channels: Vec<ChannelStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Status(DaemonStatus),
    Error { message: String },
}
