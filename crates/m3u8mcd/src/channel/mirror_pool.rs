//! MirrorPool：维护一个频道的镜像服务器列表可用性状态。
//!
//! 职责（设计文档 1.2 / 1.4）：
//! - 记录每台服务器的连续失败计数；
//! - 连续失败达到阈值后暂停该服务器，按指数退避表恢复；
//! - 提供轮询取下一台可用服务器，以及"全部可用服务器列表"（赛马模式用）。

use dashmap::DashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const FAIL_THRESHOLD: u32 = 3;
const BACKOFF_TABLE_SECS: [u64; 3] = [30, 60, 120];

#[derive(Debug, Clone)]
struct ServerState {
    consecutive_failures: u32,
    paused_until: Option<Instant>,
    backoff_idx: usize,
}

impl Default for ServerState {
    fn default() -> Self {
        ServerState {
            consecutive_failures: 0,
            paused_until: None,
            backoff_idx: 0,
        }
    }
}

pub struct MirrorPool {
    servers: RwLock<Vec<String>>,
    state: DashMap<String, ServerState>,
    rr_cursor: AtomicUsize,
}

impl MirrorPool {
    pub fn new(servers: Vec<String>) -> Arc<Self> {
        let state = DashMap::new();
        for s in &servers {
            state.insert(s.clone(), ServerState::default());
        }
        Arc::new(MirrorPool {
            servers: RwLock::new(servers),
            state,
            rr_cursor: AtomicUsize::new(0),
        })
    }

    /// 热替换 server 列表（如 token 失效重新解析 master playlist 后地址发生变化）。
    /// 新增的 server 初始化为健康状态；不再出现的 server 状态条目保留但不会再被选中。
    pub fn replace_servers(&self, new_servers: Vec<String>) {
        for s in &new_servers {
            self.state.entry(s.clone()).or_default();
        }
        *self.servers.write().unwrap() = new_servers;
    }

    fn is_available(&self, server: &str) -> bool {
        match self.state.get(server) {
            Some(st) => match st.paused_until {
                Some(until) => Instant::now() >= until,
                None => true,
            },
            None => true,
        }
    }

    /// 当前可用（未处于退避暂停期）的服务器列表，顺序与配置顺序一致。
    pub fn available_servers(&self) -> Vec<String> {
        let servers = self.servers.read().unwrap().clone();
        let avail: Vec<String> = servers.iter().filter(|s| self.is_available(s)).cloned().collect();
        if avail.is_empty() {
            // 全部服务器都在退避中时，兜底返回全量列表，避免彻底无法下载
            servers
        } else {
            avail
        }
    }

    /// 轮询取下一台可用服务器（标准模式 primary 选择）。
    pub fn next_server(&self) -> Option<String> {
        let avail = self.available_servers();
        if avail.is_empty() {
            return None;
        }
        let idx = self.rr_cursor.fetch_add(1, Ordering::Relaxed) % avail.len();
        Some(avail[idx].clone())
    }

    pub fn report_success(&self, server: &str) {
        if let Some(mut st) = self.state.get_mut(server) {
            st.consecutive_failures = 0;
            st.paused_until = None;
            st.backoff_idx = 0;
        }
    }

    pub fn report_task_failed(&self, server: &str) {
        let mut st = self.state.entry(server.to_string()).or_default();
        st.consecutive_failures += 1;
        if st.consecutive_failures >= FAIL_THRESHOLD {
            let secs = BACKOFF_TABLE_SECS[st.backoff_idx.min(BACKOFF_TABLE_SECS.len() - 1)];
            st.paused_until = Some(Instant::now() + Duration::from_secs(secs));
            st.backoff_idx = (st.backoff_idx + 1).min(BACKOFF_TABLE_SECS.len() - 1);
            st.consecutive_failures = 0; // 暂停后重新计数下一轮
            tracing::warn!(server, backoff_secs = secs, "mirror server paused after repeated failures");
        }
    }

    pub fn server_count(&self) -> usize {
        self.servers.read().unwrap().len()
    }
}
