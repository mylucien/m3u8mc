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

    /// 热替换 server 列表（如重新解析 master playlist 后各镜像 media playlist 地址变化，
    /// 或 token 刷新导致地址变化）。新增的 server 初始化为健康状态；已保留的 server
    /// 沿用原有的失败/退避状态；不再出现的 server 的状态条目会被清掉，
    /// 避免 token 每次变化都留下一条永远不会再用的状态而缓慢增长。
    pub fn replace_servers(&self, new_servers: Vec<String>) {
        for s in &new_servers {
            self.state.entry(s.clone()).or_default();
        }
        self.state.retain(|k, _| new_servers.contains(k));
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

    /// 轮询取下一台可用服务器（标准模式 primary 选择）。会推进轮询游标，
    /// 仅供 Downloader 实际下载时调用；只读展示场景请用 [`Self::peek_next_server`]。
    pub fn next_server(&self) -> Option<String> {
        let avail = self.available_servers();
        if avail.is_empty() {
            return None;
        }
        let idx = self.rr_cursor.fetch_add(1, Ordering::Relaxed) % avail.len();
        Some(avail[idx].clone())
    }

    /// 只读地查看"下一个将被使用的服务器"，不推进轮询游标。
    /// 供 status 等只读查询场景使用，避免频繁查询状态干扰真实下载的轮询顺序。
    pub fn peek_next_server(&self) -> Option<String> {
        let avail = self.available_servers();
        if avail.is_empty() {
            return None;
        }
        let idx = self.rr_cursor.load(Ordering::Relaxed) % avail.len();
        Some(avail[idx].clone())
    }

    pub fn report_success(&self, server: &str) {
        if let Some(mut st) = self.state.get_mut(server) {
            st.consecutive_failures = 0;
            st.backoff_idx = 0;
            // 注意：不清 paused_until。是否解除暂停只由 is_available() 里的时间比较
            // （Instant::now() >= until）自然决定，对齐 Python 版本的设计——
            // report_success 里从不触碰 available 状态，只有 report_task_failed 里
            // 起的 _recover 定时器睡够退避时长后才会真正解除暂停。
            //
            // 之前这里会直接把 paused_until 置 None：因为 available_servers() 在
            // "全部服务器都在退避中"时会兜底返回全量列表（包含仍在退避期内的服务器），
            // 这些服务器完全可能被选中重试；一旦某次重试凑巧成功，退避状态就被立刻
            // 清空，一台本应还在惩罚期的不稳定服务器瞬间"满血复活"，退避保护形同虚设。
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
