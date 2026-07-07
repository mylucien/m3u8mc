//! ChannelManager：持有所有频道运行时句柄，负责增/删/重启单个频道任务（设计文档 1.1）。
//!
//! reload 不等于 restart：收到新配置后 diff 出变更的频道，只对变更频道做增删/重启，
//! 未变更频道的缓存、ffmpeg 进程、镜像服务器健康度状态全部保留（设计文档 3.2）。

use crate::channel::ChannelHandle;
use m3u8mc_common::config::{self, Config};
use m3u8mc_common::protocol::{ChannelStatus, DaemonStatus};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

pub struct ChannelManager {
    channels: RwLock<HashMap<String, ChannelHandle>>,
    current_config: RwLock<Config>,
    config_path: String,
    started_at: Instant,
}

impl ChannelManager {
    pub fn new(config_path: String) -> Arc<Self> {
        Arc::new(ChannelManager {
            channels: RwLock::new(HashMap::new()),
            current_config: RwLock::new(Config::default()),
            config_path,
            started_at: Instant::now(),
        })
    }

    fn ffmpeg_bin(config: &Config) -> String {
        config.global.ffmpeg_bin.clone()
    }

    /// 加载启动时的初始配置。`start_channels` 对应 `global.enabled`：
    /// 为 false 时（daemon 整体idle）仍然把 config 写入 current_config，
    /// 只是不拉起任何频道任务——这样即使 global.enabled=0，
    /// 通过控制 socket 手动 `channel_start` 单个频道时也能在 current_config
    /// 里正确找到它，不需要先手动 reload 一次。
    pub async fn load_initial(&self, config: Config, start_channels: bool) {
        let ffmpeg_bin = Self::ffmpeg_bin(&config);
        if start_channels {
            let mut channels = self.channels.write().await;
            for ch in config.channels.iter().filter(|c| c.enabled) {
                tracing::info!(channel = %ch.id, ffmpeg_bin = %ffmpeg_bin, "starting channel");
                channels.insert(
                    ch.id.clone(),
                    ChannelHandle::spawn(ch.clone(), ffmpeg_bin.clone()),
                );
            }
        }
        *self.current_config.write().await = config;
    }

    fn read_config_from_disk(&self) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(&self.config_path)?;
        Ok(config::parse_str(&text)?)
    }

    pub async fn reload(&self) -> anyhow::Result<()> {
        let new_config = self.read_config_from_disk()?;
        let old_config = self.current_config.read().await.clone();
        let diff = config::diff_channels(&old_config, &new_config);
        let ffmpeg_bin = Self::ffmpeg_bin(&new_config);

        tracing::info!(
            added = diff.added.len(),
            removed = diff.removed.len(),
            changed = diff.changed.len(),
            unchanged = diff.unchanged.len(),
            ffmpeg_bin = %ffmpeg_bin,
            "reload: config diff computed"
        );

        // 第一步：从 map 中取出需要停止的旧 handle，不持有锁等待
        let mut channels = self.channels.write().await;
        let mut to_stop = Vec::new();
        for id in &diff.removed {
            if let Some(handle) = channels.remove(id) {
                tracing::info!(channel = %id, "stopping removed channel");
                to_stop.push(handle);
            }
        }
        for id in diff.added.iter().chain(diff.changed.iter()) {
            if let Some(old_handle) = channels.remove(id) {
                to_stop.push(old_handle);
            }
        }
        drop(channels);

        // 第二步：等待所有旧 handle 停止（不持有 lock）
        for mut handle in to_stop {
            handle.stop_and_wait().await;
        }

        // 第三步：重新加锁，插入新建的 handle
        let mut channels = self.channels.write().await;
        for id in diff.added.iter().chain(diff.changed.iter()) {
            let Some(ch_cfg) = new_config.channels.iter().find(|c| &c.id == id) else {
                continue;
            };
            if ch_cfg.enabled {
                tracing::info!(channel = %id, "starting added/changed channel");
                channels.insert(
                    id.clone(),
                    ChannelHandle::spawn(ch_cfg.clone(), ffmpeg_bin.clone()),
                );
            }
        }

        // unchanged 频道：保持现有 handle 不动，缓存/ffmpeg/健康度状态全部保留
        *self.current_config.write().await = new_config;
        Ok(())
    }

    pub async fn channel_start(&self, name: &str) -> anyhow::Result<()> {
        let cfg = self.current_config.read().await;
        let ch_cfg = cfg
            .channels
            .iter()
            .find(|c| c.id == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("channel '{name}' not found in config"))?;
        if !ch_cfg.enabled {
            anyhow::bail!(
                "channel '{name}' is disabled in config (enabled=0); \
                 set 'option enabled 1' and reload before starting it"
            );
        }
        let ffmpeg_bin = Self::ffmpeg_bin(&cfg);
        drop(cfg);

        // 取出旧 handle（如果有），立刻释放锁再 await
        let old_handle = {
            let mut channels = self.channels.write().await;
            channels.remove(name)
        };
        if let Some(mut old) = old_handle {
            old.stop_and_wait().await;
        }

        let mut channels = self.channels.write().await;
        channels.insert(name.to_string(), ChannelHandle::spawn(ch_cfg, ffmpeg_bin));
        Ok(())
    }

    pub async fn channel_stop(&self, name: &str) -> anyhow::Result<()> {
        let handle = {
            let mut channels = self.channels.write().await;
            channels.remove(name)
        };
        match handle {
            Some(mut h) => {
                h.stop_and_wait().await;
                Ok(())
            }
            None => anyhow::bail!("channel '{name}' is not running"),
        }
    }

    pub async fn channel_restart(&self, name: &str) -> anyhow::Result<()> {
        self.channel_stop(name).await.ok();
        self.channel_start(name).await
    }

    /// 停止所有正在运行的频道（daemon 退出时调用）
    pub async fn shutdown_all(&self) {
        let handles: Vec<ChannelHandle> = {
            let mut channels = self.channels.write().await;
            channels.drain().map(|(_, h)| h).collect()
        };
        for mut handle in handles {
            handle.stop_and_wait().await;
        }
    }

    pub async fn status(&self) -> DaemonStatus {
        let channels = self.channels.read().await;
        let mut list: Vec<ChannelStatus> = channels.values().map(|h| h.status()).collect();
        list.sort_by(|a, b| a.id.cmp(&b.id));
        DaemonStatus {
            uptime_secs: self.started_at.elapsed().as_secs(),
            channels: list,
        }
    }
}
