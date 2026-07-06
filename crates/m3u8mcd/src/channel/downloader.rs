//! Downloader Pool（设计文档 1.8）。
//!
//! 标准模式：单分片按"HEAD探测可分块性→多Range并发→失败回退整体下载→
//! 同服务器重试耗尽换台"流程下载（1.8.2）。
//! 赛马模式：并发对所有候选节点发起整体下载，取第一个成功结果（1.8.3）。

use super::cache::SegmentCache;
use super::mirror_pool::MirrorPool;
use bytes::{Bytes, BytesMut};
use dashmap::DashSet;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use std::sync::{Arc, RwLock};
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
    /// 解析后的赛马候选：(IP/地址, 该候选专属的 Host 覆盖，未指定时为 None 走全局回退)。
    race_targets: Vec<(String, Option<String>)>,
    /// 全局默认赛马 Host（用户显式配置的 race_host，优先级低于候选自身指定的 host）。
    configured_race_host: Option<String>,
    /// media playlist 解析完成后由 Poller 动态写入的自动推断值，仅在
    /// 候选没有指定专属 host、且 configured_race_host 为空时，才会作为最终兜底。
    /// 使用 RwLock 以便 Poller 和 Downloader 并发访问。
    race_host: RwLock<Option<String>>,
    // Bug 7 fix: 已删除 default_headers 死字段。
    // client 构造时已通过 .default_headers(default_headers.clone()) 持有该 HeaderMap，
    // struct 里的副本只写不读，属于死代码。
    overloaded: std::sync::atomic::AtomicBool,
}

impl Downloader {
    pub fn new(
        mirror_pool: Arc<MirrorPool>,
        cache: Arc<SegmentCache>,
        strategy: DownloadStrategy,
        threads: usize,
        race_ips: Vec<String>,
        race_host: &str,
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
        let configured_race_host = if race_host.trim().is_empty() {
            None
        } else {
            Some(race_host.trim().to_string())
        };
        let race_targets = race_ips.iter().map(|entry| parse_race_ip_entry(entry)).collect();
        Arc::new(Downloader {
            client,
            mirror_pool,
            cache,
            semaphore: Arc::new(Semaphore::new(permits)),
            in_progress: Arc::new(DashSet::new()),
            strategy,
            threads: threads.max(1),
            race_targets,
            configured_race_host,
            race_host: RwLock::new(None),
            overloaded: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Poller 解析完 master playlist 后调用，更新自动推断的 race_host 为 media URL 的 host。
    /// 仅当候选没有专属 host、且配置里也未显式指定 race_host 时，这个自动推断值才会被使用
    /// （统一逻辑：标准模式与赛马模式都是"候选自带 > 显式配置 > 自动推断"）。
    pub fn set_race_host(&self, host: Option<String>) {
        *self.race_host.write().unwrap() = host;
    }

    /// 某个赛马候选实际使用的 Host：候选自带 host（`ip@host` 写法）优先，
    /// 其次是全局显式配置的 race_host，最后兜底自动推断值。
    fn effective_race_host(&self, per_target_host: &Option<String>) -> Option<String> {
        if let Some(h) = per_target_host {
            return Some(h.clone());
        }
        if let Some(h) = &self.configured_race_host {
            return Some(h.clone());
        }
        self.race_host.read().unwrap().clone()
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
                let _permit = this.semaphore.acquire().await;
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
    /// 按 HLS 规范（RFC 8216 §4.1）处理两种相对路径形式：
    /// - 根路径相对（以 `/` 开头，如 `/api/amty/2283368`）：
    ///   相对于 origin（scheme://host），而非 media playlist 目录。
    ///   示例：base="https://cdn.hi.hn/163189" + rel="/api/amty/1"
    ///         → "https://cdn.hi.hn/api/amty/1"
    /// - 目录相对（不以 `/` 开头，如 `seg100.ts`）：
    ///   拼在 base 目录后，与原行为一致。
    fn build_url(server_base: &str, rel_path: &str) -> String {
        // 尝试用 url::Url::join 做规范解析（最准确）
        // server_base 可能是目录形式（无尾 /）需补上 / 才能让 join 在正确目录下工作
        let base_with_slash = if server_base.ends_with('/') {
            server_base.to_string()
        } else {
            format!("{}/", server_base)
        };
        if let Ok(base_url) = url::Url::parse(&base_with_slash) {
            if let Ok(joined) = base_url.join(rel_path) {
                return joined.to_string();
            }
        }
        // 回退：原始字符串拼接（仅用于 server_base 本身不是合法 URL 的边缘情况）
        let base = server_base.trim_end_matches('/');
        let rel = rel_path.trim_start_matches('/');
        format!("{base}/{rel}")
    }

    /// 1.8.2 标准模式单分片下载决策流程
    async fn download_round_robin(&self, task: &SegmentTask) -> anyhow::Result<Bytes> {
        let primary = self
            .mirror_pool
            .next_server()
            .ok_or_else(|| anyhow::anyhow!("no mirror server available"))?;
        let mut candidates = vec![primary.clone()];
        for s in self.mirror_pool.available_servers() {
            if s != primary {
                candidates.push(s);
            }
        }

        let mut last_err = anyhow::anyhow!("no candidates");
        for server in candidates {
            let url = Self::build_url(&server, &task.rel_path);
            match self.download_from_server(&url, self.threads).await {
                Ok(data) => {
                    self.mirror_pool.report_success(&server);
                    return Ok(data);
                }
                Err(e) => {
                    last_err = e;
                    self.mirror_pool.report_task_failed(&server);
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
                if !resp.status().is_success() {
                    anyhow::bail!("range request failed: {}", resp.status());
                }
                let bytes = resp.bytes().await?;
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
    async fn download_race(&self, task: &SegmentTask) -> anyhow::Result<Bytes> {
        let mut candidates: Vec<(String, Option<String>)> = self
            .mirror_pool
            .available_servers()
            .into_iter()
            .map(|s| (s, None))
            .collect();
        for (ip, per_target_host) in &self.race_targets {
            let base = if ip.starts_with("http://") || ip.starts_with("https://") {
                ip.clone()
            } else {
                format!("http://{ip}")
            };
            let host = self.effective_race_host(per_target_host);
            candidates.push((base, host));
        }

        if candidates.is_empty() {
            anyhow::bail!("no race candidates available");
        }

        let mut futs = FuturesUnordered::new();
        for (server, host_override) in &candidates {
            let url = Self::build_url(server, &task.rel_path);
            let server = server.clone();
            let host_override = host_override.clone();
            let this_client = self.client.clone();
            futs.push(async move {
                let mut req = this_client.get(&url);
                if let Some(h) = &host_override {
                    req = req.header(reqwest::header::HOST, h.clone());
                }
                let result = async {
                    let resp = req.send().await?;
                    if !resp.status().is_success() {
                        anyhow::bail!("status {}", resp.status());
                    }
                    let bytes = resp.bytes().await?;
                    Ok::<Bytes, anyhow::Error>(bytes)
                }
                .await;
                (server, result)
            });
        }

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
    }
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

/// 解析单条 race_ip 配置项，支持两种写法：
/// - `1.2.3.4`：纯地址，不携带专属 host，后续按"全局 race_host → 自动推断"回退。
/// - `1.2.3.4@mirror2.cdn.com`：地址携带专属 host，用于标准模式镜像池存在
///   多个不同域名时，让每个赛马候选 IP 精确对应自己的域名，避免多域名场景下
///   全部候选被套用同一个 Host 导致目标服务器认错域名。
/// 地址部分本身若已经是 `http://` / `https://` 开头的完整 URL，`@host` 依然按最后一个
/// `@` 切分（IPv6 裸地址不含 `@`，不会误切）。
fn parse_race_ip_entry(entry: &str) -> (String, Option<String>) {
    match entry.rsplit_once('@') {
        Some((addr, host)) if !host.trim().is_empty() => {
            (addr.trim().to_string(), Some(host.trim().to_string()))
        }
        // '@' 存在但 host 为空/空白（如 "1.2.3.4@" 或 "1.2.3.4@ "）：
        // 取 '@' 前的部分作为 addr，忽略空 host。
        Some((addr, _)) => (addr.trim().to_string(), None),
        // 没有 '@'：整体就是 addr。
        None => (entry.trim().to_string(), None),
    }
}

/// 把 UCI list http_header 的字符串列表解析为 reqwest HeaderMap。
/// 每条格式为 "Header-Name: value"，解析失败的条目记录 warn 后跳过。
pub fn parse_headers(raw: &[String]) -> HeaderMap {
    let mut map = HeaderMap::new();
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
        // 普通相对路径（不以 / 开头）：拼在 base 目录后
        assert_eq!(
            Downloader::build_url("https://cdn.example.com/live", "seg100.ts"),
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
}
