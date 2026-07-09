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

impl ChannelConfig {
    /// 实际用于拉取列表（Poller）的源地址：
    /// 优先使用独立配置的 list_urls，未配置时回退到 urls（向后兼容旧配置）。
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

        if !use_downloader && urls.len() > 1 {
            return Err(ConfigError::PassthroughMultipleUrls(id));
        }

        let protocol = match sec.get_or("protocol", "rtp").parse::<Protocol>() {
            Ok(p) => p,
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
            addr: sec
                .get("addr")
                .ok_or_else(|| ConfigError::MissingField(id.clone(), "addr".into()))?
                .to_string(),
            port: sec
                .get("port")
                .and_then(|p| p.parse::<u16>().ok())
                .ok_or_else(|| ConfigError::MissingField(id.clone(), "port".into()))?,
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
