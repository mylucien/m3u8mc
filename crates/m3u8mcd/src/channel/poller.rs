//! Poller（设计文档 1.2 / 1.7）。
//!
//! - 启动时做二级播放列表解析（master → 按带宽选最优 variant → 替换各镜像 host）；
//! - 此后按 `target_duration/2` 轮询 media url，用全局 media sequence 去重；
//! - 检测 sequence 异常回退（上游重启）并重置追踪基线；
//! - media url 连续失败达到阈值后判定 token 失效，触发重新解析 master playlist。

use super::cache::SegmentCache;
use super::downloader::{apply_proxy, parse_headers, Downloader, SegmentTask};
use super::m3u8::{self, ParsedPlaylist};
use super::mirror_pool::MirrorPool;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use url::Url;

const MEDIA_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const TOKEN_INVALID_THRESHOLD: u32 = 5;
/// sequence 回退超过该幅度才判定为"上游服务器重启"，而非普通抖动

pub struct Poller {
    client: reqwest::Client,
    mirror_pool: Arc<MirrorPool>,
    downloader: Arc<Downloader>,
    /// 用于在拿到上游真实 target_duration 后动态刷新缓存淘汰窗口（见
    /// `SegmentCache::set_window` 的说明），避免频道启动时基于固定 6s/分片估计出的
    /// 窗口跟真实分片时长偏差过大，导致分片还没被 ffmpeg 取走就被提前淘汰。
    cache: Arc<SegmentCache>,
    /// = 配置里的 cache_window，即窗口时长相对分片时长的倍数
    cache_window_multiplier: f32,
    /// 配置里的原始镜像 url 列表（master/media playlist 地址，每个镜像一条）
    origin_urls: Vec<String>,
    /// 解析出的各镜像 media playlist 完整地址，按 origin_urls 顺序对应
    media_urls: Mutex<Vec<String>>,
    last_target_duration: Mutex<f32>,
    last_max_global_seq: AtomicU64,
    has_baseline: AtomicBool,
    /// 用户显式配置的轮询间隔（秒）。> 0 时固定使用该值；
    /// <= 0（未配置）时沿用旧的自适应逻辑：target_duration/2。
    poll_interval_override: Option<Duration>,
}

impl Poller {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mirror_pool: Arc<MirrorPool>,
        downloader: Arc<Downloader>,
        cache: Arc<SegmentCache>,
        cache_window_multiplier: f32,
        origin_urls: Vec<String>,
        http_headers: &[String],
        poll_interval_secs: f32,
        proxy: &str,
    ) -> Arc<Self> {
        let default_headers = parse_headers(http_headers);
        let mut builder = reqwest::Client::builder()
            .timeout(MEDIA_FETCH_TIMEOUT)
            .default_headers(default_headers);
        builder = apply_proxy(builder, proxy, "poller");
        let poll_interval_override = if poll_interval_secs > 0.0 {
            Some(Duration::from_secs_f32(poll_interval_secs))
        } else {
            None
        };
        Arc::new(Poller {
            client: builder.build().expect("failed to build reqwest client"),
            mirror_pool,
            downloader,
            cache,
            cache_window_multiplier,
            origin_urls,
            media_urls: Mutex::new(Vec::new()),
            last_target_duration: Mutex::new(6.0),
            last_max_global_seq: AtomicU64::new(0),
            has_baseline: AtomicBool::new(false),
            poll_interval_override,
        })
    }

    async fn fetch_text(&self, url: &str) -> anyhow::Result<String> {
        let resp = self.client.get(url).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("playlist fetch failed: {}", resp.status());
        }
        Ok(resp.text().await?)
    }

    /// 二级播放列表解析：返回每个镜像 media playlist 所在目录的 base url
    /// （MirrorPool 的 server 标识，与 Downloader::build_url 拼接逻辑对应），
    /// 同时把各镜像的 media playlist 完整地址记入 self.media_urls。
    async fn resolve_master(&self) -> anyhow::Result<Vec<String>> {
        let first_url = self
            .origin_urls
            .first()
            .ok_or_else(|| anyhow::anyhow!("channel has no configured urls"))?;

        let text = self.fetch_text(first_url).await?;
        let parsed = m3u8::parse(&text);

        let variant_path: Option<String> = match parsed {
            ParsedPlaylist::Master(variants) => {
                let best = m3u8::pick_best_variant(&variants)
                    .ok_or_else(|| anyhow::anyhow!("master playlist has no variants"))?;
                Some(best.uri.clone())
            }
            ParsedPlaylist::Media(_) => None, // 单级列表，master 本身即 media playlist
        };

        let mut media_urls = Vec::with_capacity(self.origin_urls.len());
        let mut bases = Vec::with_capacity(self.origin_urls.len());
        for origin in &self.origin_urls {
            let media_url = match &variant_path {
                Some(path) => {
                    let base = Url::parse(origin)?;
                    base.join(path)?.to_string()
                }
                None => origin.clone(),
            };
            let dir = media_url
                .rsplit_once('/')
                .map(|(d, _)| d.to_string())
                .unwrap_or_else(|| media_url.clone());
            bases.push(dir);
            media_urls.push(media_url);
        }

        // 更新 race_host 为 media playlist 所在域名（修复跨域名场景下 race_host 推导错误）
        // 必须在 master 解析完成后才能确定，不能在 Downloader 初始化时静态推导。
        let media_host = media_urls
            .first()
            .and_then(|u| url::Url::parse(u).ok())
            .and_then(|u| u.host_str().map(|s| s.to_string()));
        self.downloader.set_race_host(media_host);

        *self.media_urls.lock().unwrap() = media_urls;
        Ok(bases)
    }

    /// 拉取一次 media playlist：依次尝试已记录的各镜像 media url，任一成功即返回。
    async fn fetch_media_playlist(&self) -> anyhow::Result<m3u8::MediaPlaylist> {
        let urls = self.media_urls.lock().unwrap().clone();
        let mut last_err = anyhow::anyhow!("no media urls resolved yet");
        for url in &urls {
            match self.fetch_text(url).await {
                Ok(text) => match m3u8::parse(&text) {
                    ParsedPlaylist::Media(m) => return Ok(m),
                    ParsedPlaylist::Master(_) => {
                        anyhow::bail!("media url unexpectedly returned a master playlist (token/redirect changed?)")
                    }
                },
                Err(e) => last_err = e,
            }
        }
        Err(last_err)
    }

    /// 主循环：在 channel 任务里 `tokio::spawn` 运行，直到 stop_rx 收到关闭信号。
    pub async fn run(self: Arc<Self>, mut stop_rx: watch::Receiver<bool>) {
        // 初始解析，失败则退避重试，不让整个频道任务直接退出
        loop {
            match self.resolve_master().await {
                Ok(bases) => {
                    self.reinit_mirror_pool(bases);
                    break;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to resolve master playlist, retrying in 5s");
                    if wait_or_stop(&mut stop_rx, Duration::from_secs(5)).await {
                        return;
                    }
                }
            }
        }

        let mut consecutive_failures: u32 = 0;

        loop {
            match self.fetch_media_playlist().await {
                Ok(playlist) => {
                    consecutive_failures = 0;
                    let target_duration = playlist.target_duration;
                    let changed = {
                        let mut last = self.last_target_duration.lock().unwrap();
                        let changed = (*last - target_duration).abs() > 0.01;
                        *last = target_duration;
                        changed
                    };
                    if changed && target_duration > 0.0 {
                        // 用上游真实 target_duration 重新计算淘汰窗口，避免频道启动时
                        // 6s/分片的保守估计跟真实分片时长偏差过大（见 SegmentCache::set_window）。
                        let window = Duration::from_secs_f32(target_duration * self.cache_window_multiplier);
                        self.cache.set_window(window);
                        tracing::info!(target_duration, window_secs = window.as_secs_f32(), "refreshed cache eviction window from real target_duration");
                    }
                    self.process_playlist(playlist);
                }
                Err(e) => {
                    consecutive_failures += 1;
                    tracing::warn!(error = %e, consecutive_failures, "media playlist fetch failed");
                    if consecutive_failures >= TOKEN_INVALID_THRESHOLD {
                        tracing::warn!("media url repeatedly failing, treating as token expiry, re-resolving master playlist");
                        // 无论这次重新解析 master 成功与否都重置计数：
                        // 之前只在 Ok 分支重置，失败时 consecutive_failures 会一直停留在
                        // >= TOKEN_INVALID_THRESHOLD 的状态，导致下一轮只要 media 拉取
                        // 再失败一次（大概率会失败，因为 media_urls 还是旧值），就会立刻
                        // 再次触发重新解析 master——原本"攒够 N 次失败再重试"的退避意图
                        // 被破坏，变成对 master playlist 的密集轮询。这里对齐旧版 Python
                        // 实现（`self._fail_count = 0` 无条件执行）的行为。
                        match self.resolve_master().await {
                            Ok(bases) => {
                                self.reinit_mirror_pool(bases);
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "re-resolving master playlist also failed");
                            }
                        }
                        consecutive_failures = 0;
                    }
                }
            }

            let poll_interval = match self.poll_interval_override {
                Some(fixed) => fixed,
                None => {
                    let secs = (*self.last_target_duration.lock().unwrap() / 2.0).max(0.5);
                    Duration::from_secs_f32(secs)
                }
            };
            if wait_or_stop(&mut stop_rx, poll_interval).await {
                return;
            }
        }
    }

    /// MirrorPool 的 server 列表在 master 重新解析后可能变化（极少见，host 不会变但保留扩展性），
    /// 当前实现里 MirrorPool 在 channel 启动时一次性创建，这里仅做日志提示；
    /// 如需热替换 server 列表，可在 MirrorPool 上扩展 `replace_servers` 方法。
    fn reinit_mirror_pool(&self, bases: Vec<String>) {
        tracing::info!(?bases, "resolved media playlist bases");
        self.mirror_pool.replace_servers(bases);
    }

    fn process_playlist(&self, playlist: m3u8::MediaPlaylist) {
        let base_seq = playlist.media_sequence;
        let seg_count = playlist.segments.len() as u64;
        let current_max = base_seq + seg_count;

        let has_baseline = self.has_baseline.load(Ordering::Relaxed);
        let last_max = self.last_max_global_seq.load(Ordering::Relaxed);

        // 重置阈值改为动态 2×playlist_len（对齐旧版 Python 的
        // `last_seq_in_playlist < seen_seq - len(segments)*2`），而不是固定的 50。
        // 直播 playlist 通常只有 3~5 个分片，固定阈值 50 意味着"真实的上游重启/切源"
        // （比如序号只回退了 10~20）根本触发不了硬重置：baseline 不会被清零，
        // start_from 仍然停留在重启前的旧序号上，之后每一轮新 playlist 里的 global_seq
        // 全都小于 start_from，被当成"已处理过的旧分片"过滤掉——相当于这个频道从此
        // 再也没有新分片被提交下载，表现为频道卡死不动但没有任何报错。
        let last_seq_in_playlist = base_seq + seg_count.saturating_sub(1);
        let rollback_reset_threshold = seg_count.saturating_mul(2);

        if has_baseline
            && seg_count > 0
            && last_seq_in_playlist < last_max.saturating_sub(rollback_reset_threshold)
        {
            tracing::warn!(
                old_max = last_max,
                new_base = base_seq,
                playlist_len = seg_count,
                reset_threshold = rollback_reset_threshold,
                "media sequence rolled back sharply, assuming upstream server restarted; resetting baseline"
            );
            self.has_baseline.store(false, Ordering::Relaxed);
        } else if has_baseline && base_seq < last_max {
            tracing::warn!(
                old_max = last_max,
                new_base = base_seq,
                rollback = last_max - base_seq,
                "media sequence partially rolled back (below threshold), new segments may be skipped until sequence catches up"
            );
        }

        let has_baseline = self.has_baseline.load(Ordering::Relaxed);
        let start_from = if has_baseline { last_max } else { base_seq };

        let mut new_tasks = Vec::new();
        for (i, seg) in playlist.segments.iter().enumerate() {
            let global_seq = base_seq + i as u64;
            if global_seq < start_from {
                continue;
            }
            new_tasks.push(SegmentTask {
                key: format!("{global_seq}:{}", seg.uri),
                rel_path: seg.uri.clone(),
                duration_secs: seg.duration_secs,
            });
        }

        if !new_tasks.is_empty() {
            self.downloader.submit(new_tasks);
        }

        // 关键修复：last_max_global_seq 语义上是"迄今为止见过的最大 seq"，必须单调不减。
        // 之前这里是无条件 store：一旦某次上游播放列表的 base_seq+len(segments) 比历史值小
        // （多镜像/多 CDN 节点数据不完全同步导致的窗口抖动很容易触发），游标会被直接拽回去，
        // 造成已下载过的分片被重新判定为"新分片"再次提交下载，还会污染下面轮的
        // SEQUENCE_ROLLBACK_THRESHOLD 硬重置判断基准。改用 fetch_max 保证只增不减，
        // 与旧版 Python 实现里 `if new_segs: self._seen_seq = new_segs[-1]["seq"]`
        // （只在真正推进时更新，且取当轮最大值）的语义保持一致。
        self.last_max_global_seq.fetch_max(current_max, Ordering::Relaxed);
        self.has_baseline.store(true, Ordering::Relaxed);
    }
}

/// 等待 duration 或直到收到停止信号；返回 true 表示应当停止。
async fn wait_or_stop(stop_rx: &mut watch::Receiver<bool>, duration: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        _ = stop_rx.changed() => true,
    }
}
