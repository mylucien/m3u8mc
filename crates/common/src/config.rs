//! 内部配置模型：从 [`crate::uci::UciConfig`] 翻译而来的强类型结构。
//!
//! 对应设计文档 2.2 节 schema：global / channel 两类 section。

use crate::uci::UciConfig;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Rtp,
    Udp,
}

impl Default for Protocol {
    fn default() -> Self {
        Protocol::Rtp
    }
}

impl std::str::FromStr for Protocol {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "rtp" => Ok(Protocol::Rtp),
            "udp" => Ok(Protocol::Udp),
            other => Err(format!("unknown protocol: {other}")),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GlobalConfig {
    pub enabled: bool,
    pub log_level: String,
    pub control_socket: String,
    pub http_port_base: u16,
    /// 优先使用的 ffmpeg 可执行文件路径。
    /// 空字符串 → 从 PATH 查找系统 ffmpeg；
    /// 非空 → 直接使用该路径（通常是 /usr/local/bin/ffmpeg 下载的全功能版本）。
    pub ffmpeg_bin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub id: String, // section name，作为频道唯一标识
    pub enabled: bool,
    pub name: String,
    pub urls: Vec<String>,
    pub protocol: Protocol,
    pub addr: String,
    pub port: u16,
    pub iface: String,
    pub ttl: u8,
    pub pkt_size: u32,
    pub buffer_size: u32,
    pub threads: usize,
    pub cache_window: u32,
    pub max_segments: usize,
    pub hls_segments: usize,
    pub http_port: Option<u16>,
    pub http_path: String,
    pub use_downloader: bool,
    /// 纯代理加速模式（不拉起 ffmpeg、不组播推流）：只对外提供内部 HTTP server
    /// 代理转发出的 m3u8 播放列表 + 分片（多线程/分块/多源加速下载后的缓存），
    /// 客户端直接用播放器拉取该 http 地址播放，不经过 ffmpeg 重新封装。
    /// 只在 use_downloader=1（标准模式）下有意义，proxy_only=1 但 use_downloader=0
    /// 是无效组合（直通模式本身就没有下载/缓存管线，也没有可代理的内容）。
    /// UCI: option proxy_only '1'
    pub proxy_only: bool,
    pub race_mode: bool,
    /// 赛马模式候选地址。每条支持两种写法：
    /// - `1.2.3.4`：纯 IP/地址，不带专属域名，实际 Host 走 race_host 配置或自动推断兜底；
    /// - `1.2.3.4@mirror2.cdn.com`：显式指定该地址对应的域名（Host 覆盖），
    ///   用于标准模式镜像池本身存在多个不同域名镜像时，让每个赛马 IP 精确匹配自己的域名。
    /// UCI: list race_ip '1.2.3.4' / list race_ip '5.6.7.8@mirror2.cdn.com'
    pub race_ips: Vec<String>,
    /// 全局默认赛马 Host，当某个 race_ip 没有用 @host 指定专属域名时使用；
    /// 若也未配置本项，则回退到自动推断（media playlist 第一个镜像的域名，旧行为）。
    /// 优先级：race_ip 自带的 @host > 本项 > 自动推断。
    /// UCI: option race_host 'cdn.example.com'
    pub race_host: String,
    pub http_headers: Vec<String>,
    pub poller_http_headers: Vec<String>,
    pub downloader_http_headers: Vec<String>,
    pub list_urls: Vec<String>,
    pub list_poll_interval_secs: f32,
    pub list_proxy: String,
    pub download_proxy: String,
    pub ffmpeg_input_args: Vec<String>,
    pub ffmpeg_output_args: Vec<String>,
}

/// 校验一个"候选覆盖"字符串是否为合法的 `scheme://host[:port]` 或 `host[:port]` 形式
/// （不允许带 path/query/fragment）。
///
/// 归一化/校验逻辑：从 `downloader.rs` 搬迁到这里（配置层），职责是"拿到用户配置的
/// 原始字符串，做格式校验"，与运行时下载逻辑分离——下载器只应拿到已经校验干净的值。
pub fn validate_host_only(entry: &str) -> Result<(), String> {
    let has_scheme = entry.starts_with("http://") || entry.starts_with("https://");
    let host_port_part = if has_scheme {
        entry.split_once("://").map(|(_, rest)| rest).unwrap_or(entry)
    } else {
        entry
    };
    if host_port_part.contains('?') || host_port_part.contains('#') {
        return Err("must not contain a query/fragment; only scheme/host/port are allowed".to_string());
    }
    // url::Url 序列化带 scheme 的地址时，即使 path 为空也会保留末尾的单个 "/"
    // （strip_path 的输出就是这种形式，如 "https://6.cdn.hi.hn/"）。
    // 这种"只有一个尾部 /、没有其它路径内容"的情况视为合法的纯 host，
    // 只有出现真正的路径内容（"/"后面还有字符，或"/"出现在中间）才判定为带 path。
    let trimmed = host_port_part.strip_suffix('/').unwrap_or(host_port_part);
    if trimmed.is_empty() {
        return Err("empty host".to_string());
    }
    if trimmed.contains('/') {
        return Err("must not contain a path; only scheme/host/port are allowed".to_string());
    }
    Ok(())
}

/// 只把 base 的 scheme://host[:port] 部分保留，丢弃 path/query/fragment。
/// 用于赛马模式下把 mirror_pool 自动派生的、带 path 的 base（如
/// "https://6.cdn.hi.hn/163189"）统一收窄成"纯连接目标"。
/// 从 `downloader.rs` 搬迁到配置层。
pub fn strip_path(base: &str) -> Result<String, String> {
    let mut url = url::Url::parse(base).map_err(|e| format!("race candidate base '{base}' is not a valid url: {e}"))?;
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

/// 校验 race_host（全局配置）或 race_ip 上 `@host` 部分的值：不允许带协议（scheme），
/// HTTP Host 请求头本身不支持 scheme，带了协议的配置属于误用。
/// 校验不通过时记录 warn 并返回 None（等同于没有配置这个值，不做静默接受也不 panic）。
/// 从 `downloader.rs` 搬迁到配置层，在配置加载阶段就完成校验/归一化。
pub fn validate_race_host_value(raw: &str, field_name: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        tracing::warn!(field_name, value = trimmed, "must not include a protocol (scheme); only host[:port] is allowed for a Host header value, ignoring this value");
        return None;
    }
    Some(trimmed.to_string())
}

/// 解析单条 race_ip 配置项，支持两种写法：
/// - `1.2.3.4`：纯地址，不携带专属 host。
/// - `1.2.3.4@mirror2.cdn.com`：地址携带专属 host（Host 覆盖）。
/// 从 `downloader.rs` 搬迁到配置层。
pub fn parse_race_ip_entry(entry: &str) -> (String, Option<String>) {
    match entry.rsplit_once('@') {
        Some((addr, host)) if !host.trim().is_empty() => {
            (addr.trim().to_string(), Some(host.trim().to_string()))
        }
        Some((addr, _)) => (addr.trim().to_string(), None),
        None => (entry.trim().to_string(), None),
    }
}

impl ChannelConfig {
    /// 实际用于拉取列表（Poller）的源地址：
    /// 优先使用独立配置的 list_urls，未配置时回退到 urls（向后兼容旧配置，
    /// 即"只填 url 无 list_url"时 url 同时承担列表源的职责）。
    ///
    /// 注意：这个方法只服务于"拉列表"这一件事。下载 base 完全由 `urls` 字段
    /// 独立决定（见 MirrorPool 的初始化，直接使用 `self.urls`），不再由这里
    /// 派生，也不再随 Poller 解析出的 media playlist 目录变化——
    /// list_url 与 url 各司其职：list_url 只管拉列表，url 只管下载分片。
    pub fn effective_list_urls(&self) -> &[String] {
        if self.list_urls.is_empty() {
            &self.urls
        } else {
            &self.list_urls
        }
    }

    /// 实际用于 Poller 请求的 HTTP 头：未单独配置 poller_http_headers 时回退到公共 http_headers。
    pub fn effective_poller_headers(&self) -> &[String] {
        if self.poller_http_headers.is_empty() {
            &self.http_headers
        } else {
            &self.poller_http_headers
        }
    }

    /// 实际用于 Downloader 请求的 HTTP 头：未单独配置 downloader_http_headers 时回退到公共 http_headers。
    pub fn effective_downloader_headers(&self) -> &[String] {
        if self.downloader_http_headers.is_empty() {
            &self.http_headers
        } else {
            &self.downloader_http_headers
        }
    }

    /// 归一化后的 race_host：校验/归一化在配置层一次性完成
    /// （不允许带协议，HTTP Host 请求头本身不支持 scheme），
    /// 不合法的配置在这里就被过滤掉并记录 warn，Downloader 直接拿到干净值，
    /// 不需要在运行时（Downloader::new）重复做同样的校验。
    pub fn effective_race_host(&self) -> Option<String> {
        validate_race_host_value(&self.race_host, "race_host")
    }

    /// 归一化后的 race_targets：解析每条 race_ip 的 `地址[@host]` 写法，
    /// 并校验 `@host` 部分不带协议。同样是配置层一次性完成的归一化，
    /// 从 `downloader.rs` 的构造逻辑中搬迁过来。
    pub fn effective_race_targets(&self) -> Vec<(String, Option<String>)> {
        self.race_ips
            .iter()
            .map(|entry| {
                let (addr, host) = parse_race_ip_entry(entry);
                let host = host.and_then(|h| validate_race_host_value(&h, "race_ip @host"));
                (addr, host)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub global: GlobalConfig,
    pub channels: Vec<ChannelConfig>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("uci parse error: {0}")]
    Uci(#[from] crate::uci::UciParseError),
    #[error("channel '{0}' missing required field: {1}")]
    MissingField(String, String),
    #[error("channel '{0}': use_downloader=0 (直通模式) 但配置了多个 url，直通模式只允许单一源")]
    PassthroughMultipleUrls(String),
    #[error("channel '{0}': proxy_only=1 (纯代理加速模式) 要求 use_downloader=1，直通模式没有下载/缓存管线，无法提供代理服务")]
    ProxyOnlyRequiresDownloader(String),
    #[error("channel '{0}': invalid protocol")]
    InvalidProtocol(String),
}

pub fn parse_str(input: &str) -> Result<Config, ConfigError> {
    let uci = crate::uci::parse(input)?;
    from_uci(&uci)
}

pub fn from_uci(uci: &UciConfig) -> Result<Config, ConfigError> {
    let global_section = uci.first_of("global");
    let global = GlobalConfig {
        enabled: global_section.map(|s| s.get_bool("enabled", true)).unwrap_or(true),
        log_level: global_section
            .map(|s| s.get_or("log_level", "warning"))
            .unwrap_or_else(|| "warning".to_string()),
        control_socket: global_section
            .map(|s| s.get_or("control_socket", "/var/run/m3u8mcd.sock"))
            .unwrap_or_else(|| "/var/run/m3u8mcd.sock".to_string()),
        http_port_base: global_section
            .map(|s| s.get_parsed("http_port_base", 18080u16))
            .unwrap_or(18080),
        ffmpeg_bin: global_section
            .map(|s| s.get_or("ffmpeg_bin", ""))
            .unwrap_or_default(),
    };

    let mut channels = Vec::new();
    let mut next_auto_port = global.http_port_base;
    let mut used_ports: std::collections::HashSet<u16> = std::collections::HashSet::new();

    // 先收集已手动指定的端口，避免自动分配时冲突。
    // 与第二遍的 filter(|s| !s.is_empty()) 保持一致，跳过空字符串。
    for sec in uci.sections_of("channel") {
        if let Some(p) = sec.get("http_port").filter(|s| !s.is_empty()) {
            if let Ok(p) = p.parse::<u16>() {
                used_ports.insert(p);
            }
        }
    }

    for sec in uci.sections_of("channel") {
        let id = sec
            .name
            .clone()
            .ok_or_else(|| ConfigError::MissingField("<anonymous>".into(), "name (section name)".into()))?;

        let urls: Vec<String> = sec.list("url").to_vec();
        let use_downloader = sec.get_bool("use_downloader", true);
        let proxy_only = sec.get_bool("proxy_only", false);

        if !use_downloader && urls.len() > 1 {
            return Err(ConfigError::PassthroughMultipleUrls(id));
        }
        if proxy_only && !use_downloader {
            return Err(ConfigError::ProxyOnlyRequiresDownloader(id));
        }

        // proxy_only 模式不组播推流，protocol 字段完全用不上；此时即便用户手滑填了
        // 非法值，也不应该因为一个无关字段导致整个频道解析失败——直接忽略、退回默认值。
        // 非 proxy_only 场景（真正需要组播）仍然严格校验，行为不变。
        let protocol = match sec.get_or("protocol", "rtp").parse::<Protocol>() {
            Ok(p) => p,
            Err(_) if proxy_only => Protocol::Rtp,
            Err(_) => return Err(ConfigError::InvalidProtocol(id)),
        };

        let http_port = if use_downloader {
            match sec.get("http_port").filter(|s| !s.is_empty()) {
                Some(p) => Some(p.parse::<u16>().map_err(|_| {
                    ConfigError::MissingField(id.clone(), "http_port (invalid number)".into())
                })?),
                None => {
                    // 自动分配：跳过已被手动占用的端口
                    while used_ports.contains(&next_auto_port) {
                        next_auto_port += 1;
                    }
                    let assigned = next_auto_port;
                    used_ports.insert(assigned);
                    next_auto_port += 1;
                    Some(assigned)
                }
            }
        } else {
            None
        };

        let channel = ChannelConfig {
            id: id.clone(),
            enabled: sec.get_bool("enabled", true),
            name: sec.get_or("name", &id),
            urls,
            protocol,
            addr: if proxy_only {
                // 纯代理加速模式不组播推流，不需要组播地址；未填时留空，
                // play_url() 会返回内部 http 代理地址而不是 udp/rtp 地址。
                sec.get("addr").unwrap_or_default().to_string()
            } else {
                sec.get("addr")
                    .ok_or_else(|| ConfigError::MissingField(id.clone(), "addr".into()))?
                    .to_string()
            },
            port: if proxy_only {
                sec.get("port").and_then(|p| p.parse::<u16>().ok()).unwrap_or(0)
            } else {
                sec.get("port")
                    .and_then(|p| p.parse::<u16>().ok())
                    .ok_or_else(|| ConfigError::MissingField(id.clone(), "port".into()))?
            },
            iface: sec.get_or("iface", "lan"),
            ttl: sec.get_parsed("ttl", 4u8),
            pkt_size: sec.get_parsed("pkt_size", 1316u32),
            buffer_size: sec.get_parsed("buffer_size", 1_048_576u32),
            threads: sec.get_parsed("threads", 4usize),
            cache_window: sec.get_parsed("cache_window", 3u32),
            max_segments: sec.get_parsed("max_segments", 5usize),
            hls_segments: sec.get_parsed("hls_segments", 3usize),
            http_port,
            http_path: sec.get("http_path").filter(|s| !s.is_empty()).map(|s| s.to_string()).unwrap_or_else(|| "/live".to_string()),
            use_downloader,
            proxy_only,
            race_mode: sec.get_bool("race_mode", false),
            race_ips: sec.list("race_ip").iter().filter(|s| !s.is_empty()).cloned().collect(),
            race_host: sec.get_or("race_host", ""),
            http_headers: sec.list("http_header").iter().filter(|s| !s.is_empty()).cloned().collect(),
            poller_http_headers: sec.list("poller_http_header").iter().filter(|s| !s.is_empty()).cloned().collect(),
            downloader_http_headers: sec.list("downloader_http_header").iter().filter(|s| !s.is_empty()).cloned().collect(),
            list_urls: sec.list("list_url").iter().filter(|s| !s.is_empty()).cloned().collect(),
            list_poll_interval_secs: sec.get_parsed("list_poll_interval", 0.0f32),
            list_proxy: sec.get_or("list_proxy", ""),
            download_proxy: sec.get_or("download_proxy", ""),
            ffmpeg_input_args: sec.list("ffmpeg_input_arg").iter().filter(|s| !s.is_empty()).cloned().collect(),
            ffmpeg_output_args: sec.list("ffmpeg_output_arg").iter().filter(|s| !s.is_empty()).cloned().collect(),
        };
        channels.push(channel);
    }

    Ok(Config {
        global,
        channels,
    })
}

/// 计算两份配置之间频道级别的差异，供 reload 时做增量处理（见设计文档 3.2）。
#[derive(Debug, Default)]
pub struct ConfigDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
    pub unchanged: Vec<String>,
}

pub fn diff_channels(old: &Config, new: &Config) -> ConfigDiff {
    let old_map: HashMap<&str, &ChannelConfig> = old.channels.iter().map(|c| (c.id.as_str(), c)).collect();
    let new_map: HashMap<&str, &ChannelConfig> = new.channels.iter().map(|c| (c.id.as_str(), c)).collect();

    let mut diff = ConfigDiff::default();
    for (id, new_ch) in &new_map {
        match old_map.get(id) {
            None => diff.added.push(id.to_string()),
            Some(old_ch) => {
                // 用 JSON 序列化做简单的结构相等比较，避免手写逐字段对比
                let old_json = serde_json::to_string(old_ch).unwrap_or_default();
                let new_json = serde_json::to_string(new_ch).unwrap_or_default();
                if old_json == new_json {
                    diff.unchanged.push(id.to_string());
                } else {
                    diff.changed.push(id.to_string());
                }
            }
        }
    }
    for id in old_map.keys() {
        if !new_map.contains_key(id) {
            diff.removed.push(id.to_string());
        }
    }
    diff
}

#[cfg(test)]
mod normalization_tests {
    use super::*;

    #[test]
    fn validate_host_only_rejects_path() {
        assert!(validate_host_only("cdn2.example.com/live").is_err());
        assert!(validate_host_only("https://cdn2.example.com/live").is_err());
        assert!(validate_host_only("cdn2.example.com").is_ok());
        assert!(validate_host_only("https://cdn2.example.com:8443").is_ok());
    }

    #[test]
    fn strip_path_removes_path_query_fragment() {
        assert_eq!(
            strip_path("https://6.cdn.hi.hn/163189?x=1#frag").unwrap(),
            "https://6.cdn.hi.hn/"
        );
    }

    #[test]
    fn validate_race_host_rejects_scheme() {
        assert_eq!(validate_race_host_value("mirror2.cdn.com", "race_host"), Some("mirror2.cdn.com".to_string()));
        assert_eq!(validate_race_host_value("mirror2.cdn.com:8443", "race_host"), Some("mirror2.cdn.com:8443".to_string()));
        assert_eq!(validate_race_host_value("https://mirror2.cdn.com", "race_host"), None);
        assert_eq!(validate_race_host_value("", "race_host"), None);
        assert_eq!(validate_race_host_value("   ", "race_host"), None);
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
        assert_eq!(parse_race_ip_entry("1.2.3.4@ "), ("1.2.3.4".to_string(), None));
    }

    #[test]
    fn channel_config_effective_race_host_filters_scheme() {
        let mut ch = default_test_channel();
        ch.race_host = "https://bad.example.com".to_string();
        assert_eq!(ch.effective_race_host(), None);
        ch.race_host = "good.example.com:8443".to_string();
        assert_eq!(ch.effective_race_host(), Some("good.example.com:8443".to_string()));
    }

    #[test]
    fn channel_config_effective_race_targets_parses_and_validates() {
        let mut ch = default_test_channel();
        ch.race_ips = vec!["1.2.3.4".to_string(), "5.6.7.8@mirror.cdn.com".to_string()];
        let targets = ch.effective_race_targets();
        assert_eq!(targets, vec![
            ("1.2.3.4".to_string(), None),
            ("5.6.7.8".to_string(), Some("mirror.cdn.com".to_string())),
        ]);
    }

    #[test]
    fn proxy_only_conflicts_with_passthrough() {
        let input = r#"
config channel 'ch1'
    option enabled '1'
    option use_downloader '0'
    option proxy_only '1'
    list url 'http://a/playlist.m3u8'
"#;
        let err = parse_str(input).unwrap_err();
        assert!(matches!(err, ConfigError::ProxyOnlyRequiresDownloader(id) if id == "ch1"));
    }

    #[test]
    fn proxy_only_does_not_require_addr_or_port() {
        // 纯代理加速模式下不需要 addr/port（没有组播目标），不应报 MissingField。
        let input = r#"
config channel 'ch1'
    option enabled '1'
    option use_downloader '1'
    option proxy_only '1'
    list url 'http://a/playlist.m3u8'
"#;
        let cfg = parse_str(input).unwrap();
        let ch = &cfg.channels[0];
        assert!(ch.proxy_only);
        assert_eq!(ch.addr, "");
        assert_eq!(ch.port, 0);
        assert!(ch.http_port.is_some());
    }

    #[test]
    fn standard_mode_still_requires_addr_and_port() {
        // 非 proxy_only 的标准模式（需要组播推流）依然要求 addr/port 必填，
        // 确认这条限制没有被这次改动放宽。
        let input = r#"
config channel 'ch1'
    option enabled '1'
    option use_downloader '1'
    list url 'http://a/playlist.m3u8'
"#;
        let err = parse_str(input).unwrap_err();
        assert!(matches!(err, ConfigError::MissingField(id, field) if id == "ch1" && field == "addr"));
    }

    #[test]
    fn proxy_only_defaults_to_false() {
        let input = r#"
config channel 'ch1'
    option enabled '1'
    option use_downloader '1'
    option addr '239.0.0.1'
    option port '1234'
    list url 'http://a/playlist.m3u8'
"#;
        let cfg = parse_str(input).unwrap();
        assert!(!cfg.channels[0].proxy_only);
    }

    #[test]
    fn proxy_only_tolerates_invalid_protocol() {
        // proxy_only 模式下 protocol 字段用不上，非法值不应导致解析失败，
        // 而是静默回退到默认的 Rtp（该值本身在这个模式下也不会被使用）。
        let input = r#"
config channel 'ch1'
    option enabled '1'
    option use_downloader '1'
    option proxy_only '1'
    option protocol 'not-a-real-protocol'
    list url 'http://a/playlist.m3u8'
"#;
        let cfg = parse_str(input).unwrap();
        assert_eq!(cfg.channels[0].protocol, Protocol::Rtp);
    }

    #[test]
    fn non_proxy_only_still_rejects_invalid_protocol() {
        // 非 proxy_only 场景（真正需要组播）依然严格校验 protocol，
        // 确认这条限制没有被上面那条放宽规则误伤。
        let input = r#"
config channel 'ch1'
    option enabled '1'
    option use_downloader '1'
    option addr '239.0.0.1'
    option port '1234'
    option protocol 'not-a-real-protocol'
    list url 'http://a/playlist.m3u8'
"#;
        let err = parse_str(input).unwrap_err();
        assert!(matches!(err, ConfigError::InvalidProtocol(id) if id == "ch1"));
    }

    fn default_test_channel() -> ChannelConfig {
        ChannelConfig {
            id: "test".to_string(),
            enabled: true,
            name: "test".to_string(),
            urls: vec![],
            protocol: Protocol::Rtp,
            addr: "239.0.0.1".to_string(),
            port: 1234,
            iface: "lan".to_string(),
            ttl: 4,
            pkt_size: 1316,
            buffer_size: 1_048_576,
            threads: 4,
            cache_window: 3,
            max_segments: 5,
            hls_segments: 3,
            http_port: None,
            http_path: "/live".to_string(),
            use_downloader: true,
            proxy_only: false,
            race_mode: false,
            race_ips: vec![],
            race_host: String::new(),
            http_headers: vec![],
            poller_http_headers: vec![],
            downloader_http_headers: vec![],
            list_urls: vec![],
            list_poll_interval_secs: 0.0,
            list_proxy: String::new(),
            download_proxy: String::new(),
            ffmpeg_input_args: vec![],
            ffmpeg_output_args: vec![],
        }
    }
}
