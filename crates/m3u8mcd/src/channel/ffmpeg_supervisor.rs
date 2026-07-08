//! FfmpegSupervisor（设计文档 1.2 / 1.9）。
//!
//! 拉起 ffmpeg 进程，独立 task 扫描其输出日志：命中致命错误关键字主动 kill；
//! 日志静默超过阈值（卡死）同样主动重启；异常退出或判定卡死后，
//! 先轮询等待内部 m3u8 重新可用（有超时兜底）再重新拉起，避免缓存未就绪时空转重启。

use m3u8mc_common::config::{ChannelConfig, Protocol};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;

const LOG_SILENCE_TIMEOUT: Duration = Duration::from_secs(30);
const RESTART_DELAY: Duration = Duration::from_secs(2);
const WARMUP_POLL_TIMEOUT: Duration = Duration::from_secs(60);
const WARMUP_POLL_INTERVAL: Duration = Duration::from_millis(500);

const FATAL_LOG_KEYWORDS: &[&str] = &[
    "Connection refused",
    "Invalid data found",
    "Server returned 404",
    "Server returned 403",
    "End of file",
    "I/O error",
    "Conversion failed",
    // 以下几个是之前遗漏、但确实应该触发主动重启的连接类致命错误
    // （对齐 Python 版本 _FATAL_PATTERNS，之前只能靠 30s 静默超时兜底，响应滞后）
    "Error opening input",
    "Connection timed out",
    "Network unreachable",
    "moov atom not found",
    "No route to host",
    "broken pipe",
];

/// 良性/预期内的日志片段：命中这些就直接跳过 fatal 判断，不做其他处理。
/// 对齐 Python 版本的 _IGNORE_PATTERNS。目的有两个：
/// 1) 避免把 ffmpeg 正常重连/追赶时间线过程中的噪声（DTS 抖动、重复行折叠提示等）
///    误判为异常并触发不必要的重启；
/// 2) 防止未来给 FATAL_LOG_KEYWORDS 加词时，恰好和这些良性噪声行产生子串碰撞。
const IGNORE_LOG_PATTERNS: &[&str] = &[
    "Last message repeated",
    "non monotonous",
    "DTS out of order",
    "max_analyze_duration",
];

#[derive(Default)]
pub struct FfmpegStatus {
    pub pid: AtomicU32,
    pub running: AtomicBool,
    pub restarts: AtomicU64,
}

pub struct FfmpegSupervisor {
    channel: ChannelConfig,
    /// ffmpeg 应该拉取的输入地址：标准模式下是内部 InternalHttpServer 的 m3u8 url，
    /// 直通模式下是上游原始 url。
    input_url: String,
    /// 直通模式下不需要等待内部缓存预热
    is_passthrough: bool,
    /// 优先使用的 ffmpeg 路径（空字符串 → 从 PATH 查找）
    ffmpeg_bin: String,
    pub status: Arc<FfmpegStatus>,
}

impl FfmpegSupervisor {
    pub fn new(
        channel: ChannelConfig,
        input_url: String,
        is_passthrough: bool,
        ffmpeg_bin: String,
    ) -> Arc<Self> {
        Arc::new(FfmpegSupervisor {
            channel,
            input_url,
            is_passthrough,
            ffmpeg_bin,
            status: Arc::new(FfmpegStatus::default()),
        })
    }

    fn output_url(&self, local_ip: Option<&str>) -> String {
        let scheme = match self.channel.protocol {
            Protocol::Rtp => "rtp",
            Protocol::Udp => "udp",
        };
        let mut url = format!("{scheme}://{}:{}", self.channel.addr, self.channel.port);
        let mut params = Vec::new();
        params.push(format!("ttl={}", self.channel.ttl));
        params.push(format!("pkt_size={}", self.channel.pkt_size));
        params.push(format!("buffer_size={}", self.channel.buffer_size));
        if let Some(ip) = local_ip {
            params.push(format!("localaddr={ip}"));
        }
        if !params.is_empty() {
            url.push('?');
            url.push_str(&params.join("&"));
        }
        url
    }

    fn build_command(&self, local_ip: Option<&str>) -> Command {
        let bin = if self.ffmpeg_bin.is_empty() {
            "ffmpeg".to_string()
        } else {
            self.ffmpeg_bin.clone()
        };
        let mut cmd = Command::new(&bin);
        cmd.arg("-hide_banner").arg("-loglevel").arg("warning").arg("-nostats");

        // 输入侧参数。注意：-extension_picky 在部分 ffmpeg 发行版/版本里并不存在
        // （命中则会报 Unrecognized option 导致进程直接退出），且 InternalHttpServer
        // 统一用 seg/{seq}.ts 命名，本身不存在扩展名嗅探歧义，因此默认不再附加该参数；
        // 如目标固件的 ffmpeg 支持该选项，可通过 ffmpeg_input_arg 自行追加。
        cmd.arg("-analyzeduration").arg("5000000");
        cmd.arg("-probesize").arg("5000000");
        for arg in &self.channel.ffmpeg_input_args {
            cmd.arg(arg);
        }
        cmd.arg("-i").arg(&self.input_url);

        // 输出侧参数（1.9 节兼容性调优组合，作为高级选项默认值，由 LuCI 预填，
        // 这里同样兜底应用一份，避免空配置时完全没有这些关键参数）
        if self.channel.ffmpeg_output_args.is_empty() {
            cmd.arg("-pcr_period").arg("20");
            cmd.arg("-muxdelay").arg("0.7");
            cmd.arg("-bsf:v").arg("h264_mp4toannexb");
            cmd.arg("-pat_period").arg("0.5");
            cmd.arg("-sdt_period").arg("0.5");
            cmd.arg("-mpegts_service_type").arg("digital_tv");
            cmd.arg("-mpegts_flags").arg("+initial_discontinuity+resend_headers");
        } else {
            for arg in &self.channel.ffmpeg_output_args {
                cmd.arg(arg);
            }
        }

        // muxer 必须跟协议匹配：
        // - udp:// 输出用裸 mpegts（IPTV 组播的标准做法，不需要额外封装）
        // - rtp:// 输出必须用 rtp_mpegts（专门给每个 RTP 包加 12 字节标准头、
        //   按 7 个 TS 包一组打包），否则 ffmpeg 会把裸 TS 数据直接怼给 rtp://，
        //   收不到合法 RTP 头，报 "Data doesn't look like RTP packets" 警告，
        //   下游按标准 RTP 解析的设备/播放器会解析失败。
        let output_format = match self.channel.protocol {
            Protocol::Rtp => "rtp_mpegts",
            Protocol::Udp => "mpegts",
        };
        cmd.arg("-f").arg(output_format);
        cmd.arg(self.output_url(local_ip));

        cmd.stdin(Stdio::null());
        // stdout 从不被读取（只有 supervise_child 读 stderr）。之前用 Stdio::piped() 会创建
        // 一个从未被消费的管道：一旦 ffmpeg 在某些边界情况下往 stdout 写入超过管道缓冲区
        // （Linux 默认 64KB）的内容，写入方会永久阻塞，进而挂起整个 ffmpeg 进程。既然
        // 代码设计上就没打算使用 stdout，直接丢弃更安全。
        cmd.stdout(Stdio::null());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        cmd
    }

    async fn resolve_local_ip(&self) -> Option<String> {
        super::iface::resolve_iface_ip(&self.channel.iface).await
    }

    /// 等待内部 InternalHttpServer 的 m3u8 出现至少一个分片，避免缓存未就绪时空转重启。
    async fn wait_for_playlist_ready(&self) {
        if self.is_passthrough {
            return;
        }
        let client = reqwest::Client::new();
        let deadline = tokio::time::Instant::now() + WARMUP_POLL_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if let Ok(resp) = client.get(&self.input_url).send().await {
                if resp.status().is_success() {
                    return;
                }
            }
            tokio::time::sleep(WARMUP_POLL_INTERVAL).await;
        }
        tracing::warn!(url = %self.input_url, "warmup wait timed out, starting ffmpeg anyway");
    }

    pub async fn run(self: Arc<Self>, mut stop_rx: watch::Receiver<bool>) {
        loop {
            if *stop_rx.borrow() {
                return;
            }

            self.wait_for_playlist_ready().await;
            if *stop_rx.borrow() {
                return;
            }

            let local_ip = self.resolve_local_ip().await;
            let mut cmd = self.build_command(local_ip.as_deref());

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "failed to spawn ffmpeg, retrying in 2s");
                    if wait_or_stop(&mut stop_rx, RESTART_DELAY).await {
                        return;
                    }
                    continue;
                }
            };

            if let Some(pid) = child.id() {
                self.status.pid.store(pid, Ordering::Relaxed);
            }
            self.status.running.store(true, Ordering::Relaxed);
            tracing::info!(channel = %self.channel.id, "ffmpeg started");

            let exit_reason = self.supervise_child(&mut child, &mut stop_rx).await;
            self.status.running.store(false, Ordering::Relaxed);
            self.status.restarts.fetch_add(1, Ordering::Relaxed);

            let _ = child.kill().await;

            if *stop_rx.borrow() {
                return;
            }

            tracing::warn!(channel = %self.channel.id, reason = ?exit_reason, "ffmpeg stopped, restarting after delay");
            if wait_or_stop(&mut stop_rx, RESTART_DELAY).await {
                return;
            }
        }
    }

    /// 监控子进程：扫描日志关键字、检测静默超时、等待进程退出。
    /// 返回值仅用于日志展示。
    async fn supervise_child(&self, child: &mut Child, stop_rx: &mut watch::Receiver<bool>) -> &'static str {
        let stderr = match child.stderr.take() {
            Some(s) => s,
            None => return "no-stderr",
        };
        let mut lines = BufReader::new(stderr).lines();
        let (activity_tx, mut activity_rx) = watch::channel(());
        let (fatal_tx, mut fatal_rx) = watch::channel(false);

        let log_task = async {
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let _ = activity_tx.send(());
                        tracing::debug!(channel = %self.channel_id(), "ffmpeg: {line}");
                        if IGNORE_LOG_PATTERNS.iter().any(|kw| line.contains(kw)) {
                            // 良性噪声（DTS 抖动、重复行折叠提示等），跳过 fatal 判断
                        } else if FATAL_LOG_KEYWORDS.iter().any(|kw| line.contains(kw)) {
                            tracing::warn!(channel = %self.channel_id(), "ffmpeg fatal keyword matched: {line}");
                            let _ = fatal_tx.send(true);
                            // 不 break：让 log_task 继续读到 EOF，避免与 fatal_rx.changed()
                            // 分支同时就绪导致 select! 随机选择，误导排障日志
                        }
                    }
                    Ok(None) => break, // stderr 关闭，通常意味着进程已退出
                    Err(_) => break,
                }
            }
        };

        tokio::pin!(log_task);

        loop {
            tokio::select! {
                _ = &mut log_task => {
                    return "log-stream-ended";
                }
                _ = fatal_rx.changed() => {
                    if *fatal_rx.borrow() {
                        return "fatal-keyword";
                    }
                }
                status = child.wait() => {
                    return match status {
                        Ok(s) if s.success() => "exited-ok",
                        Ok(_) => "exited-error",
                        Err(_) => "wait-error",
                    };
                }
                _ = tokio::time::sleep(LOG_SILENCE_TIMEOUT) => {
                    if activity_rx.has_changed().unwrap_or(false) {
                        let _ = activity_rx.borrow_and_update();
                        continue;
                    }
                    tracing::warn!(channel = %self.channel_id(), "ffmpeg log silent for {:?}, assuming stuck", LOG_SILENCE_TIMEOUT);
                    return "log-silent-stuck";
                }
                _ = stop_rx.changed() => {
                    if *stop_rx.borrow() {
                        return "stop-requested";
                    }
                }
            }
        }
    }

    fn channel_id(&self) -> &str {
        &self.channel.id
    }
}

async fn wait_or_stop(stop_rx: &mut watch::Receiver<bool>, duration: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(duration) => false,
        _ = stop_rx.changed() => true,
    }
}
