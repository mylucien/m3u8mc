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
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use url::Url;

const MEDIA_FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const TOKEN_INVALID_THRESHOLD: u32 = 5;
/// sequence 回退超过该幅度才判定为"上游服务器重启"，而非普通抖动

pub struct Poller {
    client: reqwest::Client,
    downloader: Arc<Downloader>,
    /// 用于在拿到上游真实 target_duration 后动态刷新缓存淘汰窗口（见
    /// `SegmentCache::set_window` 的说明），避免频道启动时基于固定 6s/分片估计出的
    /// 窗口跟真实分片时长偏差过大，导致分片还没被 ffmpeg 取走就被提前淘汰。
    cache: Arc<SegmentCache>,
    /// = 配置里的 cache_window，即窗口时长相对分片时长的倍数
    cache_window_multiplier: f32,
    /// 配置里的原始镜像 url 列表（master/media playlist 地址，每个镜像一条）
    /// 注意：这里只用于拉取列表本身，不再作为下载 base 的来源
    /// （下载 base 完全由 ChannelConfig.urls 独立决定，见 mod.rs 里 MirrorPool 的初始化）。
    origin_urls: Vec<String>,
    /// 解析出的各镜像 media playlist 完整地址，按 origin_urls 顺序对应
    media_urls: Mutex<Vec<String>>,
    last_target_duration: Mutex<f32>,
    /// 序号追踪状态（见 [`SeqTracker`]）
    seq: Mutex<SeqTracker>,
    /// 仅当 url 同时兼任列表源（未单独配置 list_url）时为 Some：
    /// 每次解析出各镜像 media playlist 的真实地址后，用它们刷新下载 base。
    /// 相对路径分片按 RFC 8216 本来就是相对于 media playlist 的地址解析的；
    /// 如果这里仍用配置里的 master 地址当 base，master → variant 子目录的场景
    /// （variant 在 `hi/index.m3u8`，分片是 `seg1.ts`）会拼成 `<master目录>/seg1.ts`，404。
    /// 单独配置了 list_url 时保持“列表/下载完全解耦”的语义，不动 mirror_pool。
    derive_bases: Option<Arc<MirrorPool>>,
    /// 用户显式配置的轮询间隔（秒）。> 0 时固定使用该值；
    /// <= 0（未配置）时沿用旧的自适应逻辑：target_duration/2。
    poll_interval_override: Option<Duration>,
}

impl Poller {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        downloader: Arc<Downloader>,
        cache: Arc<SegmentCache>,
        cache_window_multiplier: f32,
        origin_urls: Vec<String>,
        http_headers: &[String],
        poll_interval_secs: f32,
        proxy: &str,
        derive_bases: Option<Arc<MirrorPool>>,
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
            downloader,
            cache,
            cache_window_multiplier,
            origin_urls,
            media_urls: Mutex::new(Vec::new()),
            last_target_duration: Mutex::new(6.0),
            seq: Mutex::new(SeqTracker::default()),
            derive_bases,
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

    /// 二级播放列表解析：解析出每个镜像 media playlist 的完整地址并记入
    /// self.media_urls，供后续轮询 fetch_media_playlist 使用。
    ///
    /// 下载 base：配置了独立 list_url 时，下载 base 完全由 `url` 决定（与这里解析结果无关）；
    /// 没配 list_url（url 兼任列表源）时，下载 base 跟随这里解析出的 media playlist 地址
    /// （见 `derive_bases`）。赛马模式下的 Host 覆盖始终只由用户显式配置的
    /// race_host / race_ip@host 决定，不做隐式推断。
    async fn resolve_master(&self) -> anyhow::Result<()> {
        if self.origin_urls.is_empty() {
            anyhow::bail!("channel has no configured list url");
        }

        // 依次尝试各个镜像，第一个能拉到的就用来判断 master/media；
        // 之前只试第一个镜像，第一个镜像挂了会导致整个频道起不来（其余镜像白配）。
        let mut parsed = None;
        let mut last_err: Option<anyhow::Error> = None;
        for origin in &self.origin_urls {
            match self.fetch_text(origin).await {
                Ok(text) => {
                    parsed = Some(m3u8::parse(&text));
                    break;
                }
                Err(e) => {
                    tracing::debug!(url = %origin, error = %e, "list url unavailable while resolving master playlist, trying next");
                    last_err = Some(e);
                }
            }
        }
        let parsed = parsed.ok_or_else(|| last_err.unwrap_or_else(|| anyhow::anyhow!("no list url reachable")))?;

        let variant_path: Option<String> = match parsed {
            ParsedPlaylist::Master(variants) => {
                let best = m3u8::pick_best_variant(&variants)
                    .ok_or_else(|| anyhow::anyhow!("master playlist has no variants"))?;
                Some(best.uri.clone())
            }
            ParsedPlaylist::Media(_) => None, // 单级列表，master 本身即 media playlist
        };

        let media_urls = build_media_urls(&self.origin_urls, variant_path.as_deref())?;

        if let Some(pool) = &self.derive_bases {
            pool.replace_servers(media_urls.clone());
        }
        *self.media_urls.lock().unwrap() = media_urls;
        Ok(())
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
                Ok(()) => break,
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
                            Ok(()) => {}
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

    fn process_playlist(&self, playlist: m3u8::MediaPlaylist) {
        let base_seq = playlist.media_sequence;
        let seg_count = playlist.segments.len() as u64;

        let (start_from, event) = self.seq.lock().unwrap().advance(base_seq, seg_count);
        match event {
            Some(SeqEvent::HardReset { old_max }) => {
                tracing::warn!(
                    old_max,
                    new_base = base_seq,
                    playlist_len = seg_count,
                    "media sequence rolled back sharply, assuming upstream server restarted; resetting baseline"
                );
            }
            Some(SeqEvent::PartialRollback { old_max, new_max }) => {
                tracing::warn!(
                    old_max,
                    new_max,
                    new_base = base_seq,
                    "media sequence partially rolled back (below threshold), new segments may be skipped until sequence catches up"
                );
            }
            None => {}
        }

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
    }
}

/// 由各镜像的列表地址 + （可选的）master 里选出的 variant 路径，得到各镜像 media playlist 的完整地址。
/// 单级列表（无 variant）时原样返回；variant 是绝对地址时 join 会原样返回它（各镜像相同）。
fn build_media_urls(origins: &[String], variant_path: Option<&str>) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::with_capacity(origins.len());
    for origin in origins {
        let media_url = match variant_path {
            Some(path) => Url::parse(origin)?.join(path)?.to_string(),
            None => origin.clone(),
        };
        out.push(media_url);
    }
    Ok(out)
}

#[derive(Debug, PartialEq, Eq)]
enum SeqEvent {
    /// 上游序号大幅回退（判定为上游重启/切源），基线已重置
    HardReset { old_max: u64 },
    /// 序号小幅回退（多镜像/多 CDN 节点窗口不同步等），不重置，仅提示
    PartialRollback { old_max: u64, new_max: u64 },
}

/// 记录“迄今为止见过的最大全局序号”，决定每轮 playlist 里哪些分片是新的。
#[derive(Debug, Default)]
struct SeqTracker {
    last_max: u64,
    has_baseline: bool,
}

impl SeqTracker {
    /// 处理一轮 playlist（`base_seq` = EXT-X-MEDIA-SEQUENCE，`seg_count` = 分片数），
    /// 返回本轮应当开始下载的全局序号下限 `start_from`（小于它的分片视为已处理）。
    ///
    /// - 硬重置阈值为动态 2×playlist_len（对齐旧版 Python 实现）：直播 playlist 通常只有
    ///   3~5 个分片，固定阈值（如 50）意味着真实的上游重启（序号只回退 10~20）永远触发不了
    ///   重置，之后所有新分片都被当成“已处理过”过滤掉，频道卡死但没有任何报错。
    /// - 硬重置时必须把 `last_max` 一并清零。之前只清了 has_baseline、没清 last_max，
    ///   而后面是 `fetch_max` 更新：旧的大值保留下来，下一轮 playlist 又满足“大幅回退”条件，
    ///   于是每个轮询周期都重置一次、把 playlist 里全部分片重新下载一遍，直到上游序号追上旧值。
    /// - 小幅回退的判定用 `current_max < last_max`（playlist 末尾落后于已见最大值）。
    ///   之前用的是 `base_seq < last_max`，而正常滑动窗口的新 playlist 的 base 本来就
    ///   小于上一轮的 last_max（窗口有重叠），导致每次轮询都误报一条警告。
    fn advance(&mut self, base_seq: u64, seg_count: u64) -> (u64, Option<SeqEvent>) {
        let current_max = base_seq + seg_count;
        let last_seq_in_playlist = base_seq + seg_count.saturating_sub(1);
        let reset_threshold = seg_count.saturating_mul(2);

        let mut event = None;
        if self.has_baseline
            && seg_count > 0
            && last_seq_in_playlist < self.last_max.saturating_sub(reset_threshold)
        {
            event = Some(SeqEvent::HardReset { old_max: self.last_max });
            self.has_baseline = false;
            self.last_max = 0;
        } else if self.has_baseline && seg_count > 0 && current_max < self.last_max {
            event = Some(SeqEvent::PartialRollback {
                old_max: self.last_max,
                new_max: current_max,
            });
        }

        let start_from = if self.has_baseline { self.last_max } else { base_seq };

        // last_max 只增不减：多镜像窗口抖动时不能把游标拽回去，否则已下载分片会被重复提交。
        self.last_max = self.last_max.max(current_max);
        self.has_baseline = true;
        (start_from, event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_round_downloads_whole_playlist() {
        let mut t = SeqTracker::default();
        let (start, ev) = t.advance(100, 5);
        assert_eq!(start, 100);
        assert_eq!(ev, None);
    }

    #[test]
    fn sliding_window_has_no_warning_and_only_new_segments() {
        let mut t = SeqTracker::default();
        t.advance(100, 5); // 100..=104
        // 窗口前进 1：base=101，与上一轮重叠，这是常态，不应有任何 rollback 事件
        let (start, ev) = t.advance(101, 5); // 101..=105
        assert_eq!(start, 105, "只有 seq 105 是新的");
        assert_eq!(ev, None, "正常滑动窗口不应误报 partial rollback");
        // 同一份 playlist 重复拉取：没有新分片
        let (start, ev) = t.advance(101, 5);
        assert_eq!(start, 106);
        assert_eq!(ev, None);
    }

    #[test]
    fn small_rollback_warns_but_keeps_baseline() {
        let mut t = SeqTracker::default();
        t.advance(100, 5); // last_max=105
        let (start, ev) = t.advance(99, 5); // 末尾 104 < 105，但没到 2×len 阈值
        assert_eq!(start, 105);
        assert_eq!(ev, Some(SeqEvent::PartialRollback { old_max: 105, new_max: 104 }));
        // 游标没有被拽回
        let (start, _) = t.advance(101, 5);
        assert_eq!(start, 105);
    }

    #[test]
    fn hard_reset_happens_once_then_tracks_new_sequence() {
        let mut t = SeqTracker::default();
        t.advance(1000, 5); // last_max=1005
        // 上游重启：序号从 10 重新开始
        let (start, ev) = t.advance(10, 5);
        assert_eq!(ev, Some(SeqEvent::HardReset { old_max: 1005 }));
        assert_eq!(start, 10, "重置后从新 playlist 的 base 开始下载");
        // 关键回归点：下一轮不能再次触发重置（旧实现会，导致每轮重复下载全部分片）
        let (start, ev) = t.advance(11, 5);
        assert_eq!(ev, None);
        assert_eq!(start, 15, "只下载新增的 seq 15");
        let (start, ev) = t.advance(12, 5);
        assert_eq!(ev, None);
        assert_eq!(start, 16);
    }

    #[test]
    fn empty_playlist_does_not_reset() {
        let mut t = SeqTracker::default();
        t.advance(100, 5);
        let (_, ev) = t.advance(100, 0);
        assert_eq!(ev, None);
    }

    #[test]
    fn media_urls_join_variant_relative_to_each_origin() {
        let origins = vec![
            "http://a.example.com/live/index.m3u8".to_string(),
            "http://b.example.com/x/master.m3u8?token=1".to_string(),
        ];
        let urls = build_media_urls(&origins, Some("hi/index.m3u8")).unwrap();
        assert_eq!(urls[0], "http://a.example.com/live/hi/index.m3u8");
        assert_eq!(urls[1], "http://b.example.com/x/hi/index.m3u8");
    }

    #[test]
    fn media_urls_single_level_playlist_unchanged() {
        let origins = vec!["http://a.example.com/live/index.m3u8?t=1".to_string()];
        assert_eq!(build_media_urls(&origins, None).unwrap(), origins);
    }

    #[test]
    fn media_urls_absolute_variant_is_kept() {
        let origins = vec!["http://a.example.com/live/index.m3u8".to_string()];
        let urls = build_media_urls(&origins, Some("https://cdn.example.com/v/hi.m3u8")).unwrap();
        assert_eq!(urls[0], "https://cdn.example.com/v/hi.m3u8");
    }
}
/// 等待 duration 或直到收到停止信号；返回 true 表示应当停止。
async fn wait_or_stop(stop_rx: &mut watch::Receiver<bool>, duration: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        _ = stop_rx.changed() => true,
    }
}
