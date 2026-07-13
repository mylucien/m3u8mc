//! Downloader Pool（设计文档 1.8）。
//!
//! 标准模式：单分片按"HEAD探测可分块性→多Range并发→失败回退整体下载→
//! 同服务器重试耗尽换台"流程下载（1.8.2）。
//! 赛马模式：并发对所有候选节点发起整体下载，取第一个成功结果（1.8.3）。

use super::cache::SegmentCache;
use super::mirror_pool::MirrorPool;
use bytes::{Bytes, BytesMut};
// 配置校验/归一化逻辑（validate_race_host_value、validate_host_only、strip_path、
// parse_race_ip_entry）已按分层原则搬迁到 common::config，这里只是使用方，
// 不再维护重复实现。strip_path/validate_host_only 仍在运行时路径中使用
// （赛马候选净化、绝对路径 host 覆盖校验）；parse_race_ip_entry/validate_race_host_value
// 的运行时调用已随 Downloader::new 签名变更移到调用方（ChannelConfig::effective_race_*），
// 这里仅在单测中直接引用以验证行为。
#[allow(unused_imports)]
use m3u8mc_common::config::{parse_race_ip_entry, strip_path, validate_host_only, validate_race_host_value};
use dashmap::DashSet;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

const RETRY_PER_SERVER: u32 = 2;
const READ_STALL_TIMEOUT: Duration = Duration::from_secs(8);
const HEAD_TIMEOUT: Duration = Duration::from_secs(4);
const WHOLE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStrategy {
    RoundRobin,
    Race,
}

#[derive(Debug, Clone)]
pub struct SegmentTask {
    /// 用于跨轮次去重的 key（通常是分片在媒体列表内的相对路径）
    pub key: String,
    /// 相对路径（拼到各镜像服务器 base url 后即为完整下载地址）
    pub rel_path: String,
    pub duration_secs: f32,
}

/// 软上限背压（1.8.4）：进行中任务数超过该阈值时，丢弃本轮多余的新分片提交
const MAX_INFLIGHT_SOFT_LIMIT: usize = 64;

pub struct Downloader {
    client: reqwest::Client,
    mirror_pool: Arc<MirrorPool>,
    cache: Arc<SegmentCache>,
    semaphore: Arc<Semaphore>,
    in_progress: Arc<DashSet<String>>,
    strategy: DownloadStrategy,
    threads: usize,
    /// 解析后的赛马候选：(IP/地址, 该候选专属的 Host 覆盖，未指定时为 None)。
    /// 只要 race_targets 非空，赛马模式就只用这一组候选竞速，mirror_pool（url 配置）
    /// 派生的候选会被完全让位（"race_ip 优先级高于 url"）。
    race_targets: Vec<(String, Option<String>)>,
    /// 全局默认赛马 Host（用户显式配置的 race_host）。
    /// - 当 race_targets 非空时：作为没有专属 `@host` 的候选的 Host 请求头兜底；
    /// - 当 race_targets 为空、mirror_pool（url）非空时：作为 mirror_pool 候选的
    ///   连接域名覆盖（只换 host，scheme/port 沿用 url 自身，除非 race_host 自带端口）。
    /// 不允许带协议（scheme），构造时会校验，带协议的配置会被丢弃并记录 warn。
    configured_race_host: Option<String>,
    // Bug 7 fix: 已删除 default_headers 死字段。
    // client 构造时已通过 .default_headers(default_headers.clone()) 持有该 HeaderMap，
    // struct 里的副本只写不读，属于死代码。
    overloaded: std::sync::atomic::AtomicBool,
}

impl Downloader {
    /// `race_targets` 与 `race_host` 由调用方通过
    /// [`m3u8mc_common::config::ChannelConfig::effective_race_targets`] /
    /// [`m3u8mc_common::config::ChannelConfig::effective_race_host`] 在配置加载阶段
    /// 完成解析与校验（不允许带协议等），这里不再重复做同样的字符串校验/解析——
    /// Downloader 只消费已经归一化干净的值。
    pub fn new(
        mirror_pool: Arc<MirrorPool>,
        cache: Arc<SegmentCache>,
        strategy: DownloadStrategy,
        threads: usize,
        race_targets: Vec<(String, Option<String>)>,
        race_host: Option<String>,
        http_headers: &[String],
        proxy: &str,
    ) -> Arc<Self> {
        let n_servers = mirror_pool.server_count().max(1);
        let permits = (n_servers * 3).max(6);
        let default_headers = parse_headers(http_headers);
        let mut builder = reqwest::Client::builder()
            .timeout(WHOLE_DOWNLOAD_TIMEOUT)
            .default_headers(default_headers.clone());
        builder = apply_proxy(builder, proxy, "downloader");
        let client = builder.build().expect("failed to build reqwest client");
        Arc::new(Downloader {
            client,
            mirror_pool,
            cache,
            semaphore: Arc::new(Semaphore::new(permits)),
            in_progress: Arc::new(DashSet::new()),
            strategy,
            threads: threads.max(1),
            race_targets,
            configured_race_host: race_host,
            overloaded: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// 某个赛马候选实际使用的 Host 请求头，按级联优先级确定：
    /// 1. 候选自带 `@host`（最高优先级）；
    /// 2. 全局显式配置的 race_host；
    /// 3. 最后一级默认值，按分片路径类型分两种情况：
    ///    - 分片是绝对地址（如 `https://cdn6.cc.cd/api/amty/234028X`）：
    ///      默认值是**分片自己带的这个绝对地址的域名**（如 `cdn6.cc.cd`），
    ///      而不是 `url` 的域名——`url` 往往是转发/中转服务，跟分片真正的源站是两个
    ///      不同的服务，拿 `url` 的域名当 Host 语义上对不上、大概率连不通。
    ///      `url`（若也配置了）在这里是"优先级更高的覆盖"，而不是"默认值本身"：
    ///      默认值永远是分片自带地址；只有当 race_host/race_ip@host 更高优先级的值缺失、
    ///      且用户显式希望走自己的中转 `url` 时，才需要用 race_host/race_ip@host 顶上去，
    ///      而不是指望这里静默换成 url 的域名。
    ///    - 分片是相对路径：分片本身不带任何域名信息，没有"分片自带地址"可用，
    ///      这种情况下退回用 `url`（mirror_pool）的域名兜底。
    /// 4. 都没有：返回 None，视为纯 IP/域名请求，不发送 Host 覆盖。
    fn effective_race_host(&self, per_target_host: &Option<String>, rel_path: &str) -> Option<String> {
        per_target_host
            .clone()
            .or_else(|| self.configured_race_host.clone())
            .or_else(|| {
                if Self::is_absolute_url(rel_path) {
                    Self::host_of(rel_path)
                } else {
                    self.url_domain_default()
                }
            })
    }

    /// 从一个绝对 URL 里取出 host[:port] 部分（Host 请求头不允许带协议）。
    fn host_of(abs_url: &str) -> Option<String> {
        let u = url::Url::parse(abs_url).ok()?;
        let host = u.host_str()?;
        Some(match u.port() {
            Some(p) => format!("{host}:{p}"),
            None => host.to_string(),
        })
    }

    /// 从 mirror_pool（即 `url` 配置）的第一个候选里取出纯域名部分，
    /// 作为赛马模式下"相对路径分片、url 的域名作为默认值"这条规则的来源。
    /// mirror_pool 为空（只填了 list_url，没配 url）时返回 None。
    fn url_domain_default(&self) -> Option<String> {
        let first = self.mirror_pool.available_servers().into_iter().next()?;
        let host_only = strip_path(&first).ok()?;
        Self::host_of(&host_only)
    }

    /// race_ip 不带协议时使用的默认协议，设计理念是"尽量保持直播源原来的信息"，
    /// 与 [`Self::effective_race_host`] 的默认值来源完全对称，按 rel_path 是否
    /// 绝对路径分两种情况：
    /// - 分片是绝对地址：用分片自带地址的 scheme（该地址是真正的下载源）；
    /// - 分片是相对路径：分片本身不带协议信息，退回用 `url`（mirror_pool）
    ///   第一个候选的 scheme 兜底。
    /// 两者都取不到时（理论上不会真正走到这里：相对路径 + 没配 url 的组合下，
    /// build_race_candidates 会在没有任何候选时提前返回空列表，见 `race_candidates_empty_when_nothing_configured`），
    /// 兜底为 "http"。
    fn default_race_scheme(&self, rel_path: &str) -> &'static str {
        let scheme = if Self::is_absolute_url(rel_path) {
            url::Url::parse(rel_path).ok().map(|u| u.scheme().to_string())
        } else {
            self.mirror_pool
                .available_servers()
                .into_iter()
                .next()
                .and_then(|s| url::Url::parse(&s).ok())
                .map(|u| u.scheme().to_string())
        };
        match scheme.as_deref() {
            Some("https") => "https",
            _ => "http",
        }
    }

    pub fn is_overloaded(&self) -> bool {
        self.overloaded.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 提交一批新发现的分片任务（1.8.1）。
    pub fn submit(self: &Arc<Self>, tasks: Vec<SegmentTask>) {
        let inflight = self.in_progress.len();
        let mut tasks = tasks;
        if inflight > MAX_INFLIGHT_SOFT_LIMIT {
            self.overloaded.store(true, std::sync::atomic::Ordering::Relaxed);
            let keep = tasks.len().min(4);
            tracing::warn!(
                inflight,
                dropped = tasks.len().saturating_sub(keep),
                "downloader overloaded, dropping older pending segments in this batch"
            );
            tasks = tasks.split_off(tasks.len() - keep);
        } else {
            self.overloaded.store(false, std::sync::atomic::Ordering::Relaxed);
        }

        for task in tasks {
            if !self.in_progress.insert(task.key.clone()) {
                continue; // 已在下载中，跳过（去重）
            }
            let this = Arc::clone(self);
            let seq = this.cache.alloc_seq();
            tokio::spawn(async move {
                let _permit = this.semaphore.acquire().await.unwrap();
                let result = match this.strategy {
                    DownloadStrategy::RoundRobin => this.download_round_robin(&task).await,
                    DownloadStrategy::Race => this.download_race(&task).await,
                };
                match result {
                    Ok(data) => {
                        this.cache.insert(seq, data, task.duration_secs);
                    }
                    Err(e) => {
                        tracing::warn!(rel_path = %task.rel_path, error = %e, "segment download failed on all candidates");
                    }
                }
                this.in_progress.remove(&task.key);
            });
        }
    }

    /// 将 server_base 与 rel_path 拼接为完整 URL。
    ///
    /// 严格按 RFC 3986 §5.3 的标准 URL merge 语义（与 url::Url::join、
    /// ffmpeg 的 ff_make_absolute_url()、浏览器行为完全一致），不做任何
    /// 额外的"补斜杠 / 强制当目录"归一化：
    /// - 根路径相对（以 `/` 开头，如 `/api/amty/2283368`）：
    ///   相对于 origin（scheme://host），而非 media playlist 目录。
    ///   示例：base="https://cdn.hi.hn/163189" + rel="/api/amty/1"
    ///         → "https://cdn.hi.hn/api/amty/1"
    /// - 普通相对路径（不以 `/` 开头，如 `seg100.ts`）：
    ///   取 base 路径中最后一个 `/` 之前的部分（即把最后一段当"文件名"丢弃），
    ///   再拼上相对路径。base 是否以 `/` 结尾，决定最后一段是被当成"目录"
    ///   保留还是被当成"文件"替换掉——这是标准语义，也是唯一判断依据：
    ///     base="https://cdn.example.com/live"（无尾斜杠，视为文件）
    ///       + "seg100.ts" → "https://cdn.example.com/seg100.ts"（"live"被替换）
    ///     base="https://cdn.example.com/live/"（有尾斜杠，视为目录）
    ///       + "seg100.ts" → "https://cdn.example.com/live/seg100.ts"
    ///   若用户希望 base 被当作目录，需要自己在配置里给 url 补上尾斜杠；
    ///   这是数据层面的写法问题，代码不再替用户做归一化猜测。
    fn build_url(server_base: &str, rel_path: &str) -> String {
        if let Ok(base_url) = url::Url::parse(server_base) {
            if let Ok(joined) = base_url.join(rel_path) {
                return joined.to_string();
            }
        }
        // 回退：原始字符串拼接（仅用于 server_base 本身不是合法 URL 的边缘情况）
        let base = server_base.trim_end_matches('/');
        let rel = rel_path.trim_start_matches('/');
        format!("{base}/{rel}")
    }

    /// 判断 rel_path 是否为绝对 URL（带 scheme）。
    fn is_absolute_url(rel_path: &str) -> bool {
        url::Url::parse(rel_path)
            .map(|u| !u.scheme().is_empty())
            .unwrap_or(false)
    }

    /// 1.8.2 标准模式单分片下载决策流程。
    ///
    /// list_url 与 url 职责彻底分离：
    /// - mirror_pool 的内容始终且只来自 ChannelConfig.urls（即配置里的 `url`），
    ///   不再由 Poller 解析 media playlist 目录动态派生——list_url 只负责拉列表。
    /// - 相对路径分片：必须有 mirror_pool（即必须配置了 `url`）才能拼出下载地址；
    ///   只填 list_url 不填 url 时,mirror_pool 为空，直接报错（"无下载分片路径参数"）。
    /// - 绝对路径分片：不管 mirror_pool（url）是否配置，一律直接使用分片自带的
    ///   绝对地址，不做任何 scheme/host/port 覆盖——源既然已经返回了绝对地址，
    ///   标准模式下没有理由去改它；`url` 在标准模式下只服务于相对路径分片的拼接。
    ///   如果需要主动更换连接目标（例如绕开某个不稳定节点），应使用赛马模式
    ///   （race_ip/race_host），那才是覆盖分片地址这件事该发生的地方。
    async fn download_round_robin(&self, task: &SegmentTask) -> anyhow::Result<Bytes> {
        // 标准模式下，源既然已经返回绝对路径分片地址，就说明这条分片本该走这个地址，
        // 直接使用，不受 url（mirror_pool）是否配置影响，也不覆盖 scheme/host/port——
        // 覆盖分片自带的绝对地址在“最普通模式”下没有道理，只有赛马模式才需要主动
        // 换连接目标（那是赛马模式存在的意义），标准模式应尊重源本身。
        if Self::is_absolute_url(&task.rel_path) {
            return self.download_from_server(&task.rel_path, self.threads).await;
        }

        // 相对路径分片本身不带域名信息，必须依赖 url（mirror_pool）才能拼出完整地址；
        // 只填 list_url 不填 url 时 mirror_pool 为空，直接报错。
        let bases = self.mirror_pool.available_servers();
        if bases.is_empty() {
            anyhow::bail!(
                "no download base configured for relative-path segment: \
                 list_url only fetches the playlist, please configure url as the download base"
            );
        }

        let primary = self
            .mirror_pool
            .next_server()
            .ok_or_else(|| anyhow::anyhow!("no mirror server available"))?;
        let mut candidates: Vec<String> = vec![primary.clone()];
        for s in bases {
            if s != primary {
                candidates.push(s);
            }
        }

        let mut last_err = anyhow::anyhow!("no candidates");
        for server in &candidates {
            // 沿用 build_url（Url::join）拼接，url 里的 path 会被保留、参与拼接。
            let url = Self::build_url(server, &task.rel_path);
            match self.download_from_server(&url, self.threads).await {
                Ok(data) => {
                    self.mirror_pool.report_success(server);
                    return Ok(data);
                }
                Err(e) => {
                    last_err = e;
                    self.mirror_pool.report_task_failed(server);
                }
            }
        }

        Err(last_err)
    }

    /// 单台服务器：HEAD 探测 → 按需多线程分块 → 失败回退整体下载（含重试）
    async fn download_from_server(&self, url: &str, threads: usize) -> anyhow::Result<Bytes> {
        if threads > 1 {
            if let Some(size) = self.probe_size(url).await {
                match self.download_ranged(url, size, threads).await {
                    Ok(data) => return Ok(data),
                    Err(e) => {
                        tracing::debug!(url, error = %e, "ranged download failed, falling back to whole download");
                    }
                }
            }
        }
        self.download_whole_with_retry(url, None).await
    }

    async fn probe_size(&self, url: &str) -> Option<u64> {
        let resp = tokio::time::timeout(HEAD_TIMEOUT, self.client.head(url).send())
            .await
            .ok()?
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let accepts_ranges = resp
            .headers()
            .get(reqwest::header::ACCEPT_RANGES)
            .map(|v| v.as_bytes() == b"bytes")
            .unwrap_or(false);
        if !accepts_ranges {
            return None;
        }
        resp.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
    }

    async fn download_ranged(&self, url: &str, size: u64, threads: usize) -> anyhow::Result<Bytes> {
        let chunk = (size + threads as u64 - 1) / threads as u64;
        let mut futs = FuturesUnordered::new();
        for i in 0..threads {
            let start = i as u64 * chunk;
            if start >= size {
                break;
            }
            let end = ((start + chunk).min(size)) - 1;
            let url = url.to_string();
            let client = self.client.clone();
            futs.push(async move {
                let range = format!("bytes={start}-{end}");
                let resp = tokio::time::timeout(
                    WHOLE_DOWNLOAD_TIMEOUT,
                    client.get(&url).header(reqwest::header::RANGE, range).send(),
                )
                .await??;
                // Range 请求成功必须是 206 Partial Content；
                // 若服务器返回 200 说明不支持 Range，数据是完整内容而非分块，
                // 此时继续拼接会产生 n_threads 倍的重复数据，应当报错回退整体下载。
                if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                    anyhow::bail!("range request returned {} (expected 206)", resp.status());
                }
                let bytes = tokio::time::timeout(WHOLE_DOWNLOAD_TIMEOUT, resp.bytes()).await??;
                Ok::<(u64, Bytes), anyhow::Error>((start, bytes))
            });
        }

        let mut parts: Vec<(u64, Bytes)> = Vec::new();
        while let Some(res) = futs.next().await {
            parts.push(res?); // 任一分块失败即整体判定失败（不做缺块补传）
        }
        parts.sort_by_key(|(start, _)| *start);
        let mut buf = BytesMut::with_capacity(size as usize);
        for (_, b) in parts {
            buf.extend_from_slice(&b);
        }
        Ok(buf.freeze())
    }

    async fn download_whole_with_retry(&self, url: &str, host_override: Option<&str>) -> anyhow::Result<Bytes> {
        let mut last_err = anyhow::anyhow!("unreachable");
        for attempt in 0..=RETRY_PER_SERVER {
            match self.download_whole_once(url, host_override).await {
                Ok(data) => return Ok(data),
                Err(e) => {
                    last_err = e;
                    if attempt < RETRY_PER_SERVER {
                        tokio::time::sleep(Duration::from_millis(200 * (attempt as u64 + 1))).await;
                    }
                }
            }
        }
        Err(last_err)
    }

    async fn download_whole_once(&self, url: &str, host_override: Option<&str>) -> anyhow::Result<Bytes> {
        let mut req = self.client.get(url);
        if let Some(host) = host_override {
            req = req.header(reqwest::header::HOST, host.to_string());
        }
        let resp = tokio::time::timeout(READ_STALL_TIMEOUT, req.send()).await??;
        if !resp.status().is_success() {
            anyhow::bail!("download failed: {}", resp.status());
        }
        let bytes = tokio::time::timeout(WHOLE_DOWNLOAD_TIMEOUT, resp.bytes()).await??;
        Ok(bytes)
    }

    /// 1.8.3 赛马模式：并发对所有候选节点发起整体下载请求，取第一个成功结果。
    ///
    /// 问题1 修复：赛马候选的全部意义在于强制连接到用户指定的地址，绕开 DNS/CDN 调度。
    /// 之前直接用 build_url（本质是 url::Url::join）拼接候选与分片路径：当分片本身是
    /// 绝对 URL 时，join 会原样返回该绝对 URL、完全无视候选，导致所有候选（不管配了
    /// 几个 race_ip）最终都请求同一个真实 CDN 地址，赛马形同虚设且没有任何报错。
    ///
    /// 修复方式：赛马模式下所有候选（不管来自 mirror_pool 自动派生 base 还是显式配置的
    /// race_ip）统一视为"纯连接目标"，进入竞速前先 strip_path 剥离掉候选自身可能带的
    /// path（比如镜像池 base 形如 https://6.cdn.hi.hn/163189），只保留 scheme/host/port；
    /// 再用 resolve_segment_url 统一处理：分片相对路径时按 join 拼接（行为不变），
    /// 分片是绝对 URL 时只替换 scheme/host/port，不动分片原本的 path/query，
    /// 这样候选 IP/host 才会真正参与 TCP 连接，而不是被 join 悄悄吞掉。
    /// 构建赛马候选列表：`(连接目标 scheme://host[:port], 该候选的 Host 请求头覆盖)`。
    ///
    /// 优先级（"race_ip 优先级高于 url"，配置了 race_ip 就完全不再用 url 兜底）：
    /// - `race_ips` 非空：只用这一组候选竞速。每个候选的 Host 头 = 自带 `@host` 优先，
    ///   否则用全局 `race_host` 兜底，都没有则不设 Host 头（纯 IP 请求）。
    /// - `race_ips` 为空：退化为用 mirror_pool（即 `url` 配置）里的候选竞速——
    ///   这就是"url 的域名/协议/端口作为默认值"；如果同时配置了全局 `race_host`，
    ///   则用它替换掉每个候选的域名（scheme/port 仍沿用 url 自身，除非 race_host
    ///   自带端口），这就是"race_host 会覆盖 url 的域名"。
    /// - 两者都为空：返回空列表，调用方会报错。
    ///
    /// 赛马候选统一是"纯连接目标"，不带 path（mirror_pool 里的 url 候选先用
    /// strip_path 剥离掉可能带的路径），与相对/绝对路径无关，和 mirror_pool 里
    /// 候选来源也无关——这是问题1修复时就统一好的设计，这里延续。
    fn build_race_candidates(&self, rel_path: &str) -> Vec<(String, Option<String>)> {
        if !self.race_targets.is_empty() {
            return self
                .race_targets
                .iter()
                .map(|(ip, per_target_host)| {
                    let base = if ip.starts_with("http://") || ip.starts_with("https://") {
                        ip.clone()
                    } else {
                        format!("{}://{ip}", self.default_race_scheme(rel_path))
                    };
                    let host = self.effective_race_host(per_target_host, rel_path);
                    (base, host)
                })
                .collect();
        }

        let mut candidates = Vec::new();
        for s in self.mirror_pool.available_servers() {
            let host_only = match strip_path(&s) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!(server = %s, error = %e, "skip invalid url candidate for race mode");
                    continue;
                }
            };
            match &self.configured_race_host {
                Some(rh) => match override_domain_keep_scheme_port(&host_only, rh) {
                    Ok(overridden) => candidates.push((overridden, None)),
                    Err(e) => tracing::warn!(server = %s, race_host = %rh, error = %e, "skip: race_host cannot override this url candidate's domain"),
                },
                None => candidates.push((host_only, None)),
            }
        }
        candidates
    }

    async fn download_race(&self, task: &SegmentTask) -> anyhow::Result<Bytes> {
        let candidates = self.build_race_candidates(&task.rel_path);

        if candidates.is_empty() {
            anyhow::bail!("no race candidates available (configure race_ip, or url as a fallback base)");
        }

        let mut futs = FuturesUnordered::new();
        for (server, host_override) in &candidates {
            let url = match resolve_segment_url(server, &task.rel_path) {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(server, rel_path = %task.rel_path, error = %e, "skip invalid race candidate for this segment");
                    continue;
                }
            };
            let server = server.clone();
            let host_override = host_override.clone();
            let this_client = self.client.clone();
            futs.push(async move {
                let mut req = this_client.get(&url);
                if let Some(h) = &host_override {
                    req = req.header(reqwest::header::HOST, h.clone());
                }
                let result = async {
                    let resp = tokio::time::timeout(READ_STALL_TIMEOUT, req.send()).await??;
                    if !resp.status().is_success() {
                        anyhow::bail!("status {}", resp.status());
                    }
                    // 对响应体读取单独设超时，防止服务器慢速响应导致 race 整体卡死
                    let bytes = tokio::time::timeout(WHOLE_DOWNLOAD_TIMEOUT, resp.bytes()).await??;
                    Ok::<Bytes, anyhow::Error>(bytes)
                }
                .await;
                (server, result)
            });
        }

        // 兜底超时：所有候选的内部 timeout 均已覆盖，此处作为最后防线，
        // 与 Python 版 winner_event.wait(timeout=self.timeout + 5) 对齐。
        let race_timeout = READ_STALL_TIMEOUT + WHOLE_DOWNLOAD_TIMEOUT + Duration::from_secs(5);
        let race_result = tokio::time::timeout(race_timeout, async {
            let mut last_err: Option<anyhow::Error> = None;
            while let Some((server, result)) = futs.next().await {
                match result {
                    Ok(data) => {
                        self.mirror_pool.report_success(&server);
                        // 其余仍在进行的请求随 futs 被 drop 时自然 abort（FuturesUnordered 析构会取消未完成 future）
                        return Ok(data);
                    }
                    Err(e) => {
                        self.mirror_pool.report_task_failed(&server);
                        last_err = Some(e);
                    }
                }
            }
            Err(last_err.unwrap_or_else(|| anyhow::anyhow!("all race candidates failed")))
        })
        .await;

        match race_result {
            Ok(inner) => inner,
            Err(_) => anyhow::bail!(
                "race mode timed out after {:.0}s for {}",
                race_timeout.as_secs_f32(),
                task.rel_path
            ),
        }
    }
}

/// 用 override_cfg（"scheme://host[:port]" 或 "host[:port]"）只替换 orig_abs_url 的
/// scheme/host/port，path/query/fragment 原样保留。
///
/// 仅赛马模式（race_ip 分支）使用：赛马模式的意义就是主动换连接目标，覆盖分片
/// 自带地址是它存在的理由。标准模式不再调用本函数——标准模式下绝对路径分片
/// 直接使用源地址，不做任何覆盖（源既然给了绝对地址，普通模式没理由去动它）。
///
/// 规则：
/// - override_cfg 不带 scheme（纯域名/IP，可选端口）→ 只换 host[:port]，协议沿用
///   orig_abs_url 原有 scheme；
/// - override_cfg 带 `http://` 或 `https://` 前缀 → scheme/host 一并替换；
/// - override_cfg 未显式带端口 → 保留 orig_abs_url 原有端口（没设置就沿用源自带
///   端口，与 override_domain_keep_scheme_port 语义一致，不再清空）；
/// - override_cfg 若带 path/query/fragment（即出现 `/`、`?`、`#`），视为配置错误，
///   直接返回 Err，不做静默降级或忽略，避免用户配置了却在运行时被悄悄丢弃。
fn apply_host_override(orig_abs_url: &str, override_cfg: &str) -> anyhow::Result<String> {
    validate_host_only(override_cfg).map_err(|e| anyhow::anyhow!(e))?;

    let mut url = url::Url::parse(orig_abs_url)
        .map_err(|e| anyhow::anyhow!("segment url is not a valid absolute url: {orig_abs_url}: {e}"))?;

    let has_scheme = override_cfg.starts_with("http://") || override_cfg.starts_with("https://");
    let (scheme_to_set, host_port_raw): (Option<&str>, &str) = if has_scheme {
        let (scheme, rest) = override_cfg.split_once("://").unwrap();
        (Some(scheme), rest)
    } else {
        (None, override_cfg)
    };
    // strip_path 产生的候选可能带一个孤立的尾部 "/"（如 "https://6.cdn.hi.hn/"），
    // 已在 validate_host_only 里判定为合法，这里同样要去掉才能正确拆 host:port。
    let host_port_part = host_port_raw.strip_suffix('/').unwrap_or(host_port_raw);

    // 拆分 host 和 port（尾部若为纯数字端口才当作端口切分，避免误切 IPv6 裸地址；
    // 如需 IPv6，要求用户按标准 `[::1]:8080` 写法配置，url::Url 能正确处理）。
    let (host, port_opt) = match host_port_part.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            let port: u16 = p
                .parse()
                .map_err(|_| anyhow::anyhow!("url override '{override_cfg}' has invalid port"))?;
            (h, Some(port))
        }
        _ => (host_port_part, None),
    };

    if host.is_empty() {
        anyhow::bail!("url override '{override_cfg}' has empty host");
    }

    if let Some(scheme) = scheme_to_set {
        url.set_scheme(scheme)
            .map_err(|_| anyhow::anyhow!("url override '{override_cfg}' has invalid scheme"))?;
    }
    url.set_host(Some(host))
        .map_err(|e| anyhow::anyhow!("url override '{override_cfg}' has invalid host: {e}"))?;
    // override_cfg 显式带端口才替换；未带端口时保留原地址（分片自身）的端口，
    // 而不是清空——没设置就沿用源自带端口，是最小惊讶原则，与
    // override_domain_keep_scheme_port 的语义保持一致。
    if let Some(port) = port_opt {
        url.set_port(Some(port))
            .map_err(|_| anyhow::anyhow!("url override '{override_cfg}' port not allowed for this scheme"))?;
    }

    Ok(url.to_string())
}

/// 统一入口：根据分片路径是相对还是绝对，决定走 join 还是 host 覆盖。
/// 仅赛马模式（race_ip 分支）调用；标准模式绝对路径分片不经过这里，直接用源地址。
///
/// - rel_path 为相对路径：走 Downloader::build_url（Url::join）拼接。
/// - rel_path 为绝对 URL：调用 apply_host_override，只替换 scheme/host/port，
///   candidate_base 必须是不带 path 的纯连接目标（调用方需保证，赛马模式在调用前
///   已用 strip_path 剥离过路径）。
fn resolve_segment_url(candidate_base: &str, rel_path: &str) -> anyhow::Result<String> {
    if Downloader::is_absolute_url(rel_path) {
        apply_host_override(rel_path, candidate_base)
    } else {
        Ok(Downloader::build_url(candidate_base, rel_path))
    }
}

/// 只替换 base（"scheme://host[:port]"，不带 path，通常是 strip_path 的输出）的域名部分，
/// scheme 和 port 保持不变——除非 host_override 自己带了端口，这种情况下端口才会被替换。
/// host_override 不允许带协议（由调用方或这里校验），也不允许带 path。
/// 用于赛马模式下 "race_host 覆盖 url 候选的域名" 这条规则：端口缺省时保留 base 原有端口，
/// 与 apply_host_override 缺省时保留 orig_abs_url 原有端口的语义现已一致。
fn override_domain_keep_scheme_port(base: &str, host_override: &str) -> anyhow::Result<String> {
    if host_override.starts_with("http://") || host_override.starts_with("https://") {
        anyhow::bail!("race_host must not include a protocol (scheme); only host[:port] is allowed");
    }
    if host_override.contains('/') || host_override.contains('?') || host_override.contains('#') {
        anyhow::bail!("race_host must not contain a path/query/fragment; only host[:port] is allowed");
    }
    let mut url = url::Url::parse(base)
        .map_err(|e| anyhow::anyhow!("race candidate base '{base}' is not a valid url: {e}"))?;

    let (host, port_opt) = match host_override.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => {
            let port: u16 = p.parse().map_err(|_| anyhow::anyhow!("race_host '{host_override}' has invalid port"))?;
            (h, Some(port))
        }
        _ => (host_override, None),
    };
    if host.is_empty() {
        anyhow::bail!("race_host has empty host");
    }
    url.set_host(Some(host))
        .map_err(|e| anyhow::anyhow!("race_host '{host_override}' has invalid host: {e}"))?;
    if let Some(p) = port_opt {
        // race_host 自带端口：替换掉 base 原有端口
        url.set_port(Some(p))
            .map_err(|_| anyhow::anyhow!("race_host '{host_override}' port not allowed for this scheme"))?;
    }
    // race_host 没带端口：不动 base 原有端口（与 apply_host_override 的"清空端口"语义不同）

    Ok(url.to_string())
}

/// 若 proxy 非空，将其应用到 reqwest ClientBuilder 上（同时用于 http 和 https 请求）；
/// 解析失败时记录 warn 并忽略，不影响客户端正常构建（回退为直连）。
/// `label` 仅用于日志区分是 poller 还是 downloader 的代理配置。
pub fn apply_proxy(builder: reqwest::ClientBuilder, proxy: &str, label: &str) -> reqwest::ClientBuilder {
    let proxy = proxy.trim();
    if proxy.is_empty() {
        return builder;
    }
    match reqwest::Proxy::all(proxy) {
        Ok(p) => builder.proxy(p),
        Err(e) => {
            tracing::warn!(proxy, label, error = %e, "invalid proxy url, falling back to direct connection");
            builder
        }
    }
}

/// 把 UCI list http_header 的字符串列表解析为 reqwest HeaderMap。
/// 每条格式为 "Header-Name: value"，解析失败的条目记录 warn 后跳过。
///
/// 默认注入 `User-Agent: Mozilla/5.0`，与 Python 版本行为一致。
/// 用户配置的同名 header 会覆盖该默认值（insert 语义：后写覆盖先写）。
pub fn parse_headers(raw: &[String]) -> HeaderMap {
    let mut map = HeaderMap::new();
    // 先插入默认 UA，用户配置的同名 header 会在后续循环中覆盖它。
    // Python 版本在 Poller session 和 Downloader._headers_for 里都硬编码了
    // "User-Agent: Mozilla/5.0"；不设置时 reqwest 默认发送 "reqwest/x.x.x"，
    // 许多 CDN 会直接以 403 拒绝非浏览器 UA。
    map.insert(
        reqwest::header::USER_AGENT,
        HeaderValue::from_static("Mozilla/5.0 (Linux; Android 9; ATV1200 Build/PPR1.180610.011; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/91.0.4472.114 Safari/537.36"),
    );
    for entry in raw {
        match entry.splitn(2, ':').collect::<Vec<_>>().as_slice() {
            [name, value] => {
                let name = name.trim();
                let value = value.trim();
                match (HeaderName::from_bytes(name.as_bytes()), HeaderValue::from_str(value)) {
                    (Ok(n), Ok(v)) => { map.insert(n, v); }
                    _ => tracing::warn!(entry, "invalid http_header entry, skipping"),
                }
            }
            _ => tracing::warn!(entry, "http_header entry missing ':', skipping"),
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_url_root_relative() {
        // 根路径相对（以 / 开头）：应相对于 origin，而非 media playlist 目录
        // 场景：media url = https://6.cdn.hi.hn/163189/amty
        //       poller base = https://6.cdn.hi.hn/163189
        //       分片路径 = /api/amty/2283368
        assert_eq!(
            Downloader::build_url("https://6.cdn.hi.hn/163189", "/api/amty/2283368"),
            "https://6.cdn.hi.hn/api/amty/2283368"
        );
    }

    #[test]
    fn build_url_relative_no_slash() {
        // 方案A：base 无尾斜杠时按标准 RFC 3986 merge 语义，最后一段被当"文件名"替换掉，
        // 而不是无脑当目录拼接（"live" 被 "seg100.ts" 替换）。
        assert_eq!(
            Downloader::build_url("https://cdn.example.com/live", "seg100.ts"),
            "https://cdn.example.com/seg100.ts"
        );
    }

    #[test]
    fn build_url_relative_with_trailing_slash_is_directory() {
        // base 显式以 / 结尾时才被当作目录，分片拼在其后
        assert_eq!(
            Downloader::build_url("https://cdn.example.com/live/", "seg100.ts"),
            "https://cdn.example.com/live/seg100.ts"
        );
    }

    #[test]
    fn build_url_already_absolute() {
        // 分片路径本身是绝对 URL（部分 CDN 会这样）
        let abs = "https://other.cdn.com/seg100.ts";
        let result = Downloader::build_url("https://cdn.example.com/live", abs);
        assert_eq!(result, abs);
    }

    #[test]
    fn parse_race_ip_entry_plain() {
        assert_eq!(parse_race_ip_entry("1.2.3.4"), ("1.2.3.4".to_string(), None));
    }

    #[test]
    fn parse_race_ip_entry_with_host() {
        assert_eq!(
            parse_race_ip_entry("1.2.3.4@mirror2.cdn.com"),
            ("1.2.3.4".to_string(), Some("mirror2.cdn.com".to_string()))
        );
    }

    #[test]
    fn parse_race_ip_entry_trailing_at_ignored() {
        // '@' 后面为空白，视为没有指定 host
        assert_eq!(parse_race_ip_entry("1.2.3.4@ "), ("1.2.3.4".to_string(), None));
    }

    // ---- 问题1/问题2 修复：resolve_segment_url / apply_host_override / strip_path ----

    #[test]
    fn resolve_relative_path_unaffected() {
        // 相对路径：沿用 build_url 的标准 join 语义（方案A）
        assert_eq!(
            resolve_segment_url("https://cdn.example.com/live", "seg100.ts").unwrap(),
            "https://cdn.example.com/seg100.ts"
        );
        assert_eq!(
            resolve_segment_url("https://6.cdn.hi.hn/163189", "/api/amty/2283368").unwrap(),
            "https://6.cdn.hi.hn/api/amty/2283368"
        );
    }

    #[test]
    fn resolve_absolute_path_host_only_override_no_scheme() {
        // 候选没有协议：只换 host，协议沿用分片原 scheme，path/query 保留
        let result = resolve_segment_url(
            "1.2.3.4",
            "https://cdn6.cc.cd/api/amty/234028X?token=abc",
        )
        .unwrap();
        assert_eq!(result, "https://1.2.3.4/api/amty/234028X?token=abc");
    }

    #[test]
    fn resolve_absolute_path_override_with_scheme_and_port() {
        // 候选带协议+端口：scheme/host/port 一并替换
        let result = resolve_segment_url(
            "https://5.6.7.8:8443",
            "http://cdn6.cc.cd/api/amty/234028X",
        )
        .unwrap();
        assert_eq!(result, "https://5.6.7.8:8443/api/amty/234028X");
    }

    #[test]
    fn resolve_absolute_path_override_no_port_keeps_original_port() {
        // Bug 修复验证：候选（race_ip）没带端口时，应保留分片原有端口，
        // 而不是静默清空成默认端口。
        let result = resolve_segment_url(
            "1.2.3.4",
            "https://cdn6.cc.cd:8443/api/amty/234028X",
        )
        .unwrap();
        assert_eq!(result, "https://1.2.3.4:8443/api/amty/234028X");
    }

    #[test]
    fn resolve_absolute_path_override_with_path_errors() {
        // 候选带 path：直接报错，不做静默降级
        let err = resolve_segment_url(
            "https://5.6.7.8/somepath",
            "https://cdn6.cc.cd/api/amty/234028X",
        );
        assert!(err.is_err());
    }

    #[test]
    fn strip_path_removes_path_query_fragment() {
        assert_eq!(
            strip_path("https://6.cdn.hi.hn/163189?x=1#frag").unwrap(),
            "https://6.cdn.hi.hn/"
        );
    }

    #[test]
    fn race_mode_uses_candidate_host_not_absolute_segment_host() {
        // 复现问题1 的核心场景：mirror_pool base 带 path，赛马候选是纯 IP，
        // 分片是绝对 URL。strip_path 后再 resolve_segment_url，
        // 最终结果的 host 必须是候选 IP，而不是分片自带的真实 CDN 域名。
        let mirror_base = strip_path("https://6.cdn.hi.hn/163189").unwrap();
        let mirror_result = resolve_segment_url(&mirror_base, "https://cdn6.cc.cd/api/amty/234028X").unwrap();
        assert!(mirror_result.starts_with("https://6.cdn.hi.hn/"), "{mirror_result}");

        let race_ip_result = resolve_segment_url("1.2.3.4", "https://cdn6.cc.cd/api/amty/234028X").unwrap();
        assert_eq!(race_ip_result, "https://1.2.3.4/api/amty/234028X");
    }

    #[test]
    fn validate_host_only_rejects_path() {
        assert!(validate_host_only("cdn2.example.com/live").is_err());
        assert!(validate_host_only("https://cdn2.example.com/live").is_err());
        assert!(validate_host_only("cdn2.example.com").is_ok());
        assert!(validate_host_only("https://cdn2.example.com:8443").is_ok());
    }

    // ---- 本轮修复：list_url/url 职责分离 + 赛马候选优先级 ----

    #[test]
    fn validate_race_host_rejects_scheme() {
        assert_eq!(validate_race_host_value("mirror2.cdn.com", "race_host"), Some("mirror2.cdn.com".to_string()));
        assert_eq!(validate_race_host_value("mirror2.cdn.com:8443", "race_host"), Some("mirror2.cdn.com:8443".to_string()));
        assert_eq!(validate_race_host_value("https://mirror2.cdn.com", "race_host"), None);
        assert_eq!(validate_race_host_value("", "race_host"), None);
        assert_eq!(validate_race_host_value("   ", "race_host"), None);
    }

    #[test]
    fn override_domain_keeps_port_when_override_has_none() {
        // base 自带端口，race_host 没指定端口：保留 base 原有端口
        let result = override_domain_keep_scheme_port("https://cdn.example.com:8443", "1.2.3.4").unwrap();
        assert_eq!(result, "https://1.2.3.4:8443/");
    }

    #[test]
    fn override_domain_replaces_port_when_override_has_one() {
        // race_host 自带端口：替换 base 原有端口
        let result = override_domain_keep_scheme_port("https://cdn.example.com:8443", "1.2.3.4:9999").unwrap();
        assert_eq!(result, "https://1.2.3.4:9999/");
    }

    #[test]
    fn override_domain_rejects_scheme_in_override() {
        assert!(override_domain_keep_scheme_port("https://cdn.example.com", "https://1.2.3.4").is_err());
    }

    /// 构造一个仅用于纯逻辑测试的 Downloader（不会真的发起网络请求）。
    /// `race_ips`/`race_host` 仍按原始 UCI 字符串写法传入，这里在测试内部
    /// 用与 `ChannelConfig::effective_race_targets`/`effective_race_host` 相同的
    /// 归一化函数解析，模拟真实调用方（config 层）的行为。
    fn make_downloader(url_bases: Vec<String>, race_ips: Vec<String>, race_host: &str) -> Arc<Downloader> {
        let mirror_pool = MirrorPool::new(url_bases);
        let cache = SegmentCache::new(8, Duration::from_secs(30));
        let race_targets: Vec<(String, Option<String>)> = race_ips
            .iter()
            .map(|entry| {
                let (addr, host) = parse_race_ip_entry(entry);
                let host = host.and_then(|h| validate_race_host_value(&h, "race_ip @host"));
                (addr, host)
            })
            .collect();
        let race_host = validate_race_host_value(race_host, "race_host");
        Downloader::new(
            mirror_pool,
            cache,
            DownloadStrategy::Race,
            1,
            race_targets,
            race_host,
            &[],
            "",
        )
    }

    #[test]
    fn race_candidates_prefer_race_ip_over_url() {
        // 同时配置了 url（mirror_pool）和 race_ip：race_ip 优先级更高，
        // url 候选完全让位，只有 race_ip 参与竞速。
        let d = make_downloader(
            vec!["https://cdn.example.com/live".to_string()],
            vec!["9.9.9.9".to_string()],
            "",
        );
        let candidates = d.build_race_candidates("seg1.ts");
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].0.contains("9.9.9.9"));
    }

    #[test]
    fn race_candidates_fallback_to_url_when_no_race_ip() {
        // 只填了 list_url + url，没填 race_ip：退化为用 url（mirror_pool）候选竞速。
        let d = make_downloader(
            vec!["https://cdn.example.com/live".to_string()],
            vec![],
            "",
        );
        let candidates = d.build_race_candidates("seg1.ts");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "https://cdn.example.com/");
        assert_eq!(candidates[0].1, None);
    }

    #[test]
    fn race_candidates_race_host_overrides_url_domain_when_no_race_ip() {
        // 只填了 url + race_host（没填 race_ip）：race_host 覆盖 url 的域名，
        // 协议/端口沿用 url 自身。
        let d = make_downloader(
            vec!["https://cdn.example.com:8443/live".to_string()],
            vec![],
            "override.host.com",
        );
        let candidates = d.build_race_candidates("seg1.ts");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "https://override.host.com:8443/");
    }

    #[test]
    fn race_candidates_pure_ip_no_host_when_nothing_configured() {
        // 只填了 race_ip，没填 race_host、没填 @host、也没配 url，分片还是相对路径：
        // 没有任何域名可以兜底（分片本身也不带域名信息），视为纯 IP 请求，Host 头为 None。
        let d = make_downloader(vec![], vec!["1.2.3.4".to_string()], "");
        let candidates = d.build_race_candidates("seg1.ts");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].1, None);
    }

    #[test]
    fn race_candidates_absolute_segment_falls_back_to_its_own_domain_not_url() {
        // 复现真实场景并修正：配置了 race_ip（作为连接目标）+ url（转发/中转服务，
        // 跟分片真正的源站是两个不同域名），race_ip 没带 @host、也没配全局 race_host，
        // 分片本身是绝对地址（如 CDN 直接返回 https://cdn6.cc.cd/api/amty/234028X）。
        // 兜底 Host 应该是分片自己带的域名（cdn6.cc.cd），而不是 url 的域名
        // （ts.321288.xyz，这是转发服务自己的域名，跟分片真正的源站语义对不上）。
        let d = make_downloader(
            vec!["https://ts.321288.xyz/?url=https://cdn.qd.je/163189.php".to_string()],
            vec!["https://172.64.53.54".to_string(), "https://cdn6.cc.cd".to_string()],
            "",
        );
        let candidates = d.build_race_candidates("https://cdn6.cc.cd/api/amty/234028X");
        assert_eq!(candidates.len(), 2);
        // 连接目标依然是各自的 race_ip 地址
        assert!(candidates[0].0.contains("172.64.53.54"));
        assert!(candidates[1].0.contains("cdn6.cc.cd"));
        // 两个候选的 Host 头都兜底成了分片自带地址的域名，不是 url 的域名
        assert_eq!(candidates[0].1, Some("cdn6.cc.cd".to_string()));
        assert_eq!(candidates[1].1, Some("cdn6.cc.cd".to_string()));
    }

    #[test]
    fn race_candidates_relative_segment_falls_back_to_url_domain() {
        // 分片是相对路径时，分片本身不带任何域名信息，没有"分片自带地址"可用，
        // 这种情况下才应该退回用 url 的域名兜底（相对路径场景专属的规则，与绝对路径不同）。
        let d = make_downloader(
            vec!["https://ts.321288.xyz/list.m3u8".to_string()],
            vec!["https://172.64.53.54".to_string()],
            "",
        );
        let candidates = d.build_race_candidates("seg1.ts");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].1, Some("ts.321288.xyz".to_string()));
    }

    #[test]
    fn race_candidates_per_target_host_still_wins_over_segment_own_domain() {
        // race_ip 自带 @host 时，优先级依然最高，不会被"分片自带域名"这层兜底覆盖掉。
        let d = make_downloader(
            vec!["https://ts.321288.xyz/list.m3u8".to_string()],
            vec!["https://172.64.53.54@mirror.cdn.com".to_string()],
            "",
        );
        let candidates = d.build_race_candidates("https://cdn6.cc.cd/api/amty/234028X");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].1, Some("mirror.cdn.com".to_string()));
    }

    #[test]
    fn race_candidates_scheme_defaults_to_absolute_segment_scheme() {
        // race_ip 不带协议 + 分片是绝对地址（https）：候选协议应跟随分片自身的
        // scheme，而不是写死 http。
        let d = make_downloader(vec![], vec!["1.2.3.4".to_string()], "");
        let candidates = d.build_race_candidates("https://cdn6.cc.cd/api/amty/234028X");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "https://1.2.3.4");
    }

    #[test]
    fn race_candidates_scheme_defaults_to_url_scheme_for_relative_segment() {
        // race_ip 不带协议 + 分片是相对路径 + 配置了 url（https）：候选协议
        // 跟随 url 的 scheme 兜底，而不是写死 http。
        let d = make_downloader(
            vec!["https://ts.321288.xyz/list.m3u8".to_string()],
            vec!["1.2.3.4".to_string()],
            "",
        );
        let candidates = d.build_race_candidates("seg1.ts");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "https://1.2.3.4");
    }

    #[test]
    fn race_candidates_scheme_falls_back_to_http_when_segment_is_plain_http() {
        // 分片本身就是 http 的绝对地址时，候选协议也应是 http（沿用分片信息，
        // 而不是意外升级成 https）。
        let d = make_downloader(vec![], vec!["1.2.3.4".to_string()], "");
        let candidates = d.build_race_candidates("http://cdn6.cc.cd/api/amty/234028X");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].0, "http://1.2.3.4");
    }

    #[test]
    fn race_candidates_empty_when_nothing_configured() {
        // 只填了 list_url，url 和 race_ip 都没填：没有任何候选可竞速。
        let d = make_downloader(vec![], vec![], "");
        assert!(d.build_race_candidates("seg1.ts").is_empty());
    }
}
