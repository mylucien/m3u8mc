mod channel;
mod control;
mod logfmt;
mod manager;

use clap::Parser;
use m3u8mc_common::config;
use manager::ChannelManager;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

/// 编译期由 build.rs 注入（见 build.rs 顶部说明）。拼成完整版本串，
/// `m3u8mcd --version` 直接就能看到是哪个 commit、什么时候编译的，
/// 避免部署后没法确认跑的是不是预期的那次修复。
const VERSION_INFO: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (commit ",
    env!("M3U8MC_GIT_SHA"),
    ", built ",
    env!("M3U8MC_BUILD_TIME"),
    ")"
);

#[derive(Parser, Debug)]
#[command(name = "m3u8mcd", about = "m3u8 转组播 OpenWrt 插件 daemon", version = VERSION_INFO)]
struct Args {
    /// UCI 配置文件路径，对应 /etc/config/m3u8mc
    #[arg(long, default_value = "/etc/config/m3u8mc")]
    config: PathBuf,

    /// 日志行首加时间戳。默认不加：procd/logread 与 docker logs 本身已带时间。
    #[arg(long)]
    log_timestamp: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let config_text = std::fs::read_to_string(&args.config)
        .map_err(|e| anyhow::anyhow!("failed to read config file {:?}: {e}", args.config))?;
    let parsed = config::parse_str(&config_text)?;

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(logfmt::normalize_log_filter(&parsed.global.log_level)))
        .with_ansi(false)
        .event_format(logfmt::LineFormat { with_time: args.log_timestamp })
        .init();

    if !parsed.global.enabled {
        tracing::warn!("global.enabled=0, daemon will idle without starting any channel");
    }

    tracing::info!(version = VERSION_INFO, channels = parsed.channels.len(), "loaded configuration");

    let manager = ChannelManager::new(args.config.to_string_lossy().to_string());
    // 无论 global.enabled 是否为 true，都要把解析好的配置写入 current_config，
    // 否则 global.enabled=0 时手动 channel_start 会因为 current_config 是空的
    // Config::default() 而报"channel not found in config"。
    manager.load_initial(parsed.clone(), parsed.global.enabled).await;

    let control_socket = parsed.global.control_socket.clone();
    let manager_for_control = manager.clone();
    let control_task = tokio::spawn(async move {
        if let Err(e) = control::serve(&control_socket, manager_for_control).await {
            tracing::error!(error = %e, "control socket server exited with error");
        }
    });

    shutdown_signal().await;
    tracing::info!("shutdown signal received, exiting");
    // 先优雅停止所有频道（等待 ffmpeg 子进程退出），再关 control socket
    manager.shutdown_all().await;
    control_task.abort();
    Ok(())
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::select! {
        _ = ctrl_c => {}
        _ = sigterm.recv() => {}
    }
}
