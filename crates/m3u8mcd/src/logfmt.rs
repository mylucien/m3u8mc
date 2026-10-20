//! 日志格式：一行一条，便于在 logread / docker logs / LuCI 日志页里阅读。
//!
//! ```text
//! INFO  [cctv1] poller: refreshed cache eviction window target_duration=2.0
//! WARN  [cctv1] downloader: segment download failed on all candidates rel_path="seg1.ts"
//! INFO  m3u8mcd: loaded configuration channels=3
//! ```
//!
//! 与默认格式相比：
//! - 没有 ANSI 颜色转义（OpenWrt 的 logread 里会显示成 `\x1b[2m` 乱码）；
//! - 默认不带时间戳（procd/syslog、docker 本身已带），需要时用 `--log-timestamp`；
//! - 每条日志前带 `[频道ID]`（来自各频道任务的 `ch` span），多频道时一眼看出是哪个频道；
//! - 模块名只保留最后一段（`poller` 而不是 `m3u8mcd::channel::poller`）。

use std::fmt;
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields};
use tracing_subscriber::registry::LookupSpan;

/// 把配置里写的 log_level 规范成 EnvFilter 能识别的指令。
///
/// 配置模板/LuCI 里的默认值是 `warning`，但 tracing 的级别名是 `warn`：
/// EnvFilter 会把 `warning` 当成一个叫 "warning" 的 target，于是所有日志都被过滤掉，
/// 出厂默认配置下什么日志都看不到。这里对逗号分隔的每一段，
/// 把单独出现的 `warning`（以及大小写变体）和 `err` 换成标准写法；
/// 带 `target=level` 的写法同样处理等号右侧。
pub fn normalize_log_filter(spec: &str) -> String {
    fn fix_level(l: &str) -> String {
        match l.trim().to_ascii_lowercase().as_str() {
            "warning" => "warn".to_string(),
            "err" => "error".to_string(),
            _ => l.trim().to_string(),
        }
    }
    let out: Vec<String> = spec
        .split(',')
        .filter(|p| !p.trim().is_empty())
        .map(|part| match part.split_once('=') {
            Some((target, level)) => format!("{}={}", target.trim(), fix_level(level)),
            None => fix_level(part),
        })
        .collect();
    if out.is_empty() { "warn".to_string() } else { out.join(",") }
}

pub struct LineFormat {
    pub with_time: bool,
}

/// 从 span 的已格式化字段（形如 `id="cctv1"`）里取出频道 id。
fn parse_channel_id(fields: &str) -> Option<String> {
    let rest = fields.split("id=").nth(1)?;
    let token = rest.split_whitespace().next()?;
    Some(token.trim_matches('"').to_string())
}

impl<S, N> FormatEvent<S, N> for LineFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let meta = event.metadata();

        if self.with_time {
            SystemTime.format_time(&mut writer)?;
            writer.write_str(" ")?;
        }

        write!(writer, "{:<5} ", meta.level())?;

        // 频道 id：沿 span 链找名为 "ch" 的 span
        let mut channel: Option<String> = None;
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                if span.name() == "ch" {
                    if let Some(f) = span.extensions().get::<FormattedFields<N>>() {
                        channel = parse_channel_id(f.fields.as_str());
                    }
                }
            }
        }
        if let Some(ch) = channel {
            write!(writer, "[{ch}] ")?;
        }

        let module = meta.target().rsplit("::").next().unwrap_or(meta.target());
        write!(writer, "{module}: ")?;

        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_warning_and_keeps_directives() {
        assert_eq!(normalize_log_filter("warning"), "warn");
        assert_eq!(normalize_log_filter("WARNING"), "warn");
        assert_eq!(normalize_log_filter("info"), "info");
        assert_eq!(normalize_log_filter("m3u8mcd=debug,warning"), "m3u8mcd=debug,warn");
        assert_eq!(normalize_log_filter("m3u8mcd=warning"), "m3u8mcd=warn");
        assert_eq!(normalize_log_filter(""), "warn");
    }

    #[test]
    fn parses_quoted_and_plain_ids() {
        assert_eq!(parse_channel_id("id=\"cctv1\"").as_deref(), Some("cctv1"));
        assert_eq!(parse_channel_id("id=cctv1 other=1").as_deref(), Some("cctv1"));
        assert_eq!(parse_channel_id("nothing"), None);
    }
}
