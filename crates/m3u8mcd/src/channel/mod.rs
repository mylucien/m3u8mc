pub mod cache;
pub mod downloader;
pub mod ffmpeg_supervisor;
pub mod http_server;
pub mod iface;
pub mod m3u8;
pub mod mirror_pool;
pub mod poller;

use cache::SegmentCache;
use downloader::{DownloadStrategy, Downloader};
use ffmpeg_supervisor::FfmpegSupervisor;
use m3u8mc_common::config::ChannelConfig;
use m3u8mc_common::protocol::ChannelStatus;
use mirror_pool::MirrorPool;
use poller::Poller;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// 单个频道的运行时句柄：持有 stop 信号发送端及各子模块共享状态，
/// 供 ChannelManager 增删/重启/查询状态。
pub struct ChannelHandle {
    pub config: ChannelConfig,
    stop_tx: watch::Sender<bool>,
    ffmpeg_status: Arc<ffmpeg_supervisor::FfmpegStatus>,
    cache: Option<Arc<SegmentCache>>,
    mirror_pool: Option<Arc<MirrorPool>>,
    downloader: Option<Arc<Downloader>>,
    /// 频道内各子任务句柄，stop_and_wait 会等待它们全部退出
    join_handles: Vec<JoinHandle<()>>,
}

/// 每个子任务等待退出的最大时长，超时后直接 detach
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

impl ChannelHandle {
    pub fn spawn(config: ChannelConfig, ffmpeg_bin: String) -> Self {
        if config.use_downloader {
            Self::spawn_standard(config, ffmpeg_bin)
        } else {
            Self::spawn_passthrough(config, ffmpeg_bin)
        }
    }

    fn spawn_standard(config: ChannelConfig, ffmpeg_bin: String) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);

        // 问题2 修复：mirror_pool（下载 base 池）直接、且唯一地来自 config.urls，
        // 不再由 Poller 解析 media playlist 目录后动态派生/替换。
        // list_url 与 url 职责彻底分离：list_url 只喂给 Poller 拉列表，
        // url（=mirror_pool 的初始内容）才是分片下载真正使用的 base。
        // 只填 list_url、不填 url 时，这里就是空列表——相对路径分片会在
        // Downloader::download_round_robin 里报错（无下载 base），绝对路径分片
        // 则会直接使用分片自带的绝对地址下载，不需要 base。
        let mirror_pool = MirrorPool::new(config.urls.clone());
        // 初始窗口用 target_duration 的保守估计（6s）× cache_window，
        // Poller 拿到真实 target_duration 后该估计可能略有偏差，但足以满足设计文档
        // 2.2 节 "cache_window 决定的时间窗口应大于等于 hls_segments 覆盖时长" 的约束。
        let window = Duration::from_secs_f32(6.0 * config.cache_window as f32);
        let cache = SegmentCache::new(config.max_segments, window);

        let strategy = if config.race_mode {
            DownloadStrategy::Race
        } else {
            DownloadStrategy::RoundRobin
        };
        let downloader = Downloader::new(
            Arc::clone(&mirror_pool),
            Arc::clone(&cache),
            strategy,
            config.threads,
            config.race_ips.clone(),
            &config.race_host,
            config.effective_downloader_headers(),
            &config.download_proxy,
        );

        let poller = Poller::new(
            Arc::clone(&downloader),
            Arc::clone(&cache),
            config.cache_window as f32,
            config.effective_list_urls().to_vec(),
            config.effective_poller_headers(),
            config.list_poll_interval_secs,
            &config.list_proxy,
        );
        let poller_handle = tokio::spawn(poller.run(stop_rx.clone()));

        let http_port = config.http_port.expect("standard mode requires http_port to be set");
        let http_path = config.http_path.clone();
        let http_handle = {
            let cache = Arc::clone(&cache);
            let http_path_clone = http_path.clone();
            let hls_segments = config.hls_segments;
            let stop_rx = stop_rx.clone();
            let channel_id = config.id.clone();
            tokio::spawn(async move {
                if let Err(e) = http_server::serve(cache, http_path_clone, hls_segments, http_port, stop_rx).await {
                    tracing::error!(channel = %channel_id, error = %e, "internal http server exited with error");
                }
            })
        };

        let input_url = format!(
            "http://127.0.0.1:{}{}/playlist.m3u8",
            http_port,
            normalize_path(&http_path)
        );
        let supervisor = FfmpegSupervisor::new(config.clone(), input_url, false, ffmpeg_bin);
        let ffmpeg_status = Arc::clone(&supervisor.status);
        let supervisor_handle = tokio::spawn(Arc::clone(&supervisor).run(stop_rx.clone()));

        ChannelHandle {
            config,
            stop_tx,
            ffmpeg_status,
            cache: Some(cache),
            mirror_pool: Some(mirror_pool),
            downloader: Some(downloader),
            join_handles: vec![poller_handle, http_handle, supervisor_handle],
        }
    }

    fn spawn_passthrough(config: ChannelConfig, ffmpeg_bin: String) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);
        let input_url = config.urls.first().cloned().unwrap_or_default();
        let supervisor = FfmpegSupervisor::new(config.clone(), input_url, true, ffmpeg_bin);
        let ffmpeg_status = Arc::clone(&supervisor.status);
        let supervisor_handle = tokio::spawn(Arc::clone(&supervisor).run(stop_rx.clone()));

        ChannelHandle {
            config,
            stop_tx,
            ffmpeg_status,
            cache: None,
            mirror_pool: None,
            downloader: None,
            join_handles: vec![supervisor_handle],
        }
    }

    pub async fn stop_and_wait(&mut self) {
        let _ = self.stop_tx.send(true);
        if self.join_handles.is_empty() {
            return;
        }
        // 给每个子任务最多 SHUTDOWN_TIMEOUT 时长完成退出
        for handle in &mut self.join_handles {
            let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, handle).await;
        }
    }

    pub fn play_url(&self) -> String {
        let scheme = match self.config.protocol {
            m3u8mc_common::config::Protocol::Rtp => "rtp",
            m3u8mc_common::config::Protocol::Udp => "udp",
        };
        format!("{scheme}://@{}:{}", self.config.addr, self.config.port)
    }

    pub fn status(&self) -> ChannelStatus {
        let mode = if self.config.use_downloader { "standard" } else { "passthrough" };
        let cached_segments = self.cache.as_ref().map(|c| c.len()).unwrap_or(0);
        let active_server = self.mirror_pool.as_ref().and_then(|p| p.peek_next_server());
        let overloaded = self.downloader.as_ref().map(|d| d.is_overloaded()).unwrap_or(false);
        let running = self.ffmpeg_status.running.load(Ordering::Relaxed);
        let pid = self.ffmpeg_status.pid.load(Ordering::Relaxed);

        ChannelStatus {
            id: self.config.id.clone(),
            name: self.config.name.clone(),
            enabled: self.config.enabled,
            running,
            mode: mode.to_string(),
            cached_segments,
            active_server,
            ffmpeg_pid: if pid == 0 { None } else { Some(pid) },
            ffmpeg_restarts: self.ffmpeg_status.restarts.load(Ordering::Relaxed),
            overloaded,
            last_error: None,
            play_url: self.play_url(),
        }
    }
}

/// 统一 http_path 归一化：保证有且仅有一个前导 '/'，且没有尾部 '/'，
/// 与 http_server.rs 里注册路由时的 trim 逻辑保持一致，
/// 避免 ffmpeg 实际请求的 URL 与内部 HTTP server 注册的路由不匹配（尤其是配置了
/// 带尾斜杠的 http_path，如 '/live/'，导致 playlist.m3u8 前出现双斜杠）。
fn normalize_path(p: &str) -> String {
    format!("/{}", p.trim_matches('/'))
}
