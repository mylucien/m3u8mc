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

        let mirror_pool = MirrorPool::new(Vec::new());
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
        let race_host = config
            .urls
            .first()
            .and_then(|u| url::Url::parse(u).ok())
            .and_then(|u| u.host_str().map(|s| s.to_string()));

        let downloader = Downloader::new(
            Arc::clone(&mirror_pool),
            Arc::clone(&cache),
            strategy,
            config.threads,
            config.race_ips.clone(),
            race_host,
        );

        let poller = Poller::new(Arc::clone(&mirror_pool), Arc::clone(&downloader), config.urls.clone());
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
        let active_server = self.mirror_pool.as_ref().and_then(|p| p.next_server());
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

fn normalize_path(p: &str) -> String {
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}
