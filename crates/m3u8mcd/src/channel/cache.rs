//! SegmentCache：按 `seq` 编号存储分片数据，存储位置可选内存或本地目录。
//!
//! 设计文档 1.2：分片在内部统一按 seq 编号路由（而非沿用上游原始文件名），
//! 与上游文件名格式解耦；按时间窗口 + 数量上限双重淘汰；向 TS 流客户端广播新分片。
//!
//! 存储后端（`cache_storage`）：
//! - `memory`：分片字节数据放在进程内存（`Bytes`，克隆零拷贝）；
//! - `disk`：每个分片写成 `<cache_dir>/<实例id>-<seq>.seg`，内存里只留元数据。
//!   读取时按需从文件读出。目录里只会创建/删除本模块自己命名的 `*.seg` /
//!   `*.seg.tmp` 文件，不会递归删除目录，即使用户把 cache_dir 配到已有目录也不会误删其他内容。

use bytes::Bytes;
use dashmap::DashMap;
use m3u8mc_common::config::CacheStorage;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

/// 分片文件后缀。清理残留文件时只认这两种后缀。
const SEG_SUFFIX: &str = ".seg";
const TMP_SUFFIX: &str = ".seg.tmp";

enum Store {
    Memory(Bytes),
    File(PathBuf),
}

struct Entry {
    store: Store,
    inserted_at: Instant,
    duration_secs: f32,
    size: usize,
}

/// 分片元数据（不含字节内容，读取开销为 O(1)，用于生成 playlist 等只需要时长的场景）。
#[derive(Debug, Clone, Copy)]
pub struct SegmentInfo {
    pub duration_secs: f32,
    #[allow(dead_code)]
    pub size: usize,
}

/// 缓存后端配置。
#[derive(Debug, Clone)]
pub enum CacheBackend {
    Memory,
    Disk(PathBuf),
}

impl CacheBackend {
    /// 由频道配置得到后端：disk 但目录为空时回退内存。
    pub fn from_config(storage: CacheStorage, dir: &str) -> Self {
        match storage {
            CacheStorage::Memory => CacheBackend::Memory,
            CacheStorage::Disk if dir.trim().is_empty() => {
                tracing::warn!("cache_storage=disk but cache_dir is empty, falling back to memory");
                CacheBackend::Memory
            }
            CacheStorage::Disk => CacheBackend::Disk(PathBuf::from(dir)),
        }
    }
}

pub struct SegmentCache {
    segments: DashMap<u64, Entry>,
    max_segments: usize,
    /// 时间窗口淘汰时长 = 分片时长 × cache_window（见设计文档 2.2 关键参数取值依据）。
    /// 用纳秒存成 AtomicU64，允许 Poller 在拿到上游真实 target_duration 后动态刷新，
    /// 而不是永远锁死在频道启动时的保守估计值上（见 `set_window` 的说明）。
    window_nanos: AtomicU64,
    next_seq: AtomicU64,
    /// 广播新分片 seq，供 InternalHttpServer 的 TS 连续流模式订阅
    notify: broadcast::Sender<u64>,
    /// 磁盘目录（None = 内存模式）
    dir: Option<PathBuf>,
    /// 本缓存实例的唯一标识，嵌入文件名。频道重启/reload 时新旧实例的文件互不相同，
    /// 旧实例残留的下载任务晚到的写入/清理不会误伤新实例的文件。
    instance: String,
    /// 频道停止后置位：之后到达的 insert 一律丢弃（并清理自己刚写的文件），
    /// 避免旧下载任务在频道停止后继续往磁盘写孤儿文件。
    closed: AtomicBool,
}

impl SegmentCache {
    pub fn new(max_segments: usize, window: Duration, backend: CacheBackend) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(256);

        let dir = match backend {
            CacheBackend::Memory => None,
            CacheBackend::Disk(dir) => match prepare_dir(&dir) {
                Ok(()) => {
                    tracing::info!(dir = %dir.display(), "segment cache: using disk storage");
                    warn_if_memory_backed(&dir);
                    Some(dir)
                }
                Err(e) => {
                    // 目录不可用（不存在且建不出来、只读文件系统等）：宁可退回内存继续工作，
                    // 也不要让频道起不来。
                    tracing::error!(
                        dir = %dir.display(),
                        error = %e,
                        "segment cache: cannot use cache_dir, falling back to memory storage"
                    );
                    None
                }
            },
        };

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);

        Arc::new(SegmentCache {
            segments: DashMap::new(),
            max_segments,
            window_nanos: AtomicU64::new(window.as_nanos().min(u64::MAX as u128) as u64),
            next_seq: AtomicU64::new(0),
            notify: tx,
            dir,
            instance: format!("{nanos:x}"),
            closed: AtomicBool::new(false),
        })
    }

    /// 实际生效的存储类型（磁盘目录不可用而回退内存时返回 "memory"）。
    pub fn storage_name(&self) -> &'static str {
        if self.dir.is_some() {
            "disk"
        } else {
            "memory"
        }
    }

    fn window(&self) -> Duration {
        Duration::from_nanos(self.window_nanos.load(Ordering::Relaxed))
    }

    /// 用上游真实 target_duration 刷新淘汰窗口。频道启动时 window 只是按 6s/分片的
    /// 保守估计算出来的（见 mod.rs），如果实际分片时长跟这个估计偏差较大（比如
    /// 上游是 2s 或 10s 一片），或者下游 ffmpeg 因为限速参数（如 -readrate）暂时落后
    /// 于实时进度，固定不变的窗口要么淘汰得太快（分片还没被 ffmpeg 取走就被清掉，
    /// 表现为 ffmpeg 日志里的 "skipping N segments ahead, expired from playlists" /
    /// 时间戳跳变），要么白白占用内存。这里允许 Poller 每次拿到最新 target_duration
    /// 时调用本方法刷新窗口，让淘汰策略贴近真实分片节奏。
    pub fn set_window(&self, window: Duration) {
        self.window_nanos
            .store(window.as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
    }

    /// 分配下一个内部 seq 号（与上游原始分片序号无关，由 daemon 自行编号）。
    pub fn alloc_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed)
    }

    fn seg_path(&self, dir: &Path, seq: u64) -> PathBuf {
        dir.join(format!("{}-{seq}{SEG_SUFFIX}", self.instance))
    }

    /// 写入一个分片。磁盘模式下先写 `.tmp` 再原子 rename，读取方永远看不到写了一半的文件。
    /// 写入失败（磁盘满、目录被卸载等）返回 Err，分片不会进入缓存。
    pub async fn insert(&self, seq: u64, data: Bytes, duration_secs: f32) -> std::io::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        let size = data.len();

        let store = match &self.dir {
            None => Store::Memory(data),
            Some(dir) => {
                let final_path = self.seg_path(dir, seq);
                let tmp_path = final_path.with_extension("seg.tmp");
                if let Err(e) = tokio::fs::write(&tmp_path, &data).await {
                    let _ = tokio::fs::remove_file(&tmp_path).await;
                    return Err(e);
                }
                if let Err(e) = tokio::fs::rename(&tmp_path, &final_path).await {
                    let _ = tokio::fs::remove_file(&tmp_path).await;
                    return Err(e);
                }
                // 写文件期间频道可能已被停止：把刚写的文件清掉，不留孤儿
                if self.closed.load(Ordering::Acquire) {
                    let _ = tokio::fs::remove_file(&final_path).await;
                    return Ok(());
                }
                Store::File(final_path)
            }
        };

        self.segments.insert(
            seq,
            Entry {
                store,
                inserted_at: Instant::now(),
                duration_secs,
                size,
            },
        );
        self.evict().await;
        let _ = self.notify.send(seq);
        Ok(())
    }

    async fn evict(&self) {
        let now = Instant::now();
        let window = self.window();

        // 先选出要淘汰的 seq：超出时间窗口的 + 超出数量上限的最旧部分
        let mut victims: Vec<u64> = self
            .segments
            .iter()
            .filter(|e| now.duration_since(e.inserted_at) > window)
            .map(|e| *e.key())
            .collect();

        let remaining = self.segments.len().saturating_sub(victims.len());
        if remaining > self.max_segments {
            let mut alive: Vec<u64> = self
                .segments
                .iter()
                .map(|e| *e.key())
                .filter(|k| !victims.contains(k))
                .collect();
            alive.sort_unstable();
            let overflow = alive.len() - self.max_segments;
            victims.extend(alive.into_iter().take(overflow));
        }

        for seq in victims {
            if let Some((_, entry)) = self.segments.remove(&seq) {
                if let Store::File(path) = entry.store {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }
        }
    }

    /// 读取分片内容。内存模式零拷贝；磁盘模式从文件读出
    /// （分片刚好在读取瞬间被淘汰、文件已删除时返回 None，调用方按 404 处理）。
    pub async fn read(&self, seq: u64) -> Option<Bytes> {
        enum Src {
            Mem(Bytes),
            Path(PathBuf),
        }
        let src = {
            let e = self.segments.get(&seq)?;
            match &e.store {
                Store::Memory(b) => Src::Mem(b.clone()),
                Store::File(p) => Src::Path(p.clone()),
            }
        }; // DashMap 的读锁在这里释放，之后才做 await
        match src {
            Src::Mem(b) => Some(b),
            Src::Path(p) => match tokio::fs::read(&p).await {
                Ok(v) => Some(Bytes::from(v)),
                Err(e) => {
                    tracing::debug!(seq, path = %p.display(), error = %e, "cached segment file unavailable");
                    None
                }
            },
        }
    }

    /// 只取元数据（时长、大小），不读分片内容。
    pub fn info(&self, seq: u64) -> Option<SegmentInfo> {
        self.segments.get(&seq).map(|e| SegmentInfo {
            duration_secs: e.duration_secs,
            size: e.size,
        })
    }

    /// 当前缓存中的全部 seq，按升序排列（用于生成 m3u8 播放列表）。
    pub fn sorted_seqs(&self) -> Vec<u64> {
        let mut seqs: Vec<u64> = self.segments.iter().map(|e| *e.key()).collect();
        seqs.sort_unstable();
        seqs
    }

    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// 当前缓存占用的总字节数（内存模式是内存占用，磁盘模式是磁盘占用）。
    pub fn total_bytes(&self) -> u64 {
        self.segments.iter().map(|e| e.size as u64).sum()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<u64> {
        self.notify.subscribe()
    }

    /// 频道停止时调用：拒绝后续写入，并清空缓存（磁盘模式下删除分片文件）。
    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        let seqs: Vec<u64> = self.segments.iter().map(|e| *e.key()).collect();
        for seq in seqs {
            if let Some((_, entry)) = self.segments.remove(&seq) {
                if let Store::File(path) = entry.store {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }
        }
        if let Some(dir) = &self.dir {
            // 目录若已空则顺手删掉（非空时 remove_dir 会失败，忽略即可，绝不递归删除）
            let _ = tokio::fs::remove_dir(dir).await;
        }
    }
}

/// 在 `/proc/mounts` 内容里找出 `path` 所在文件系统的类型（取最长的挂载点前缀）。
/// 纯函数，便于测试；`/proc/mounts` 里挂载点的空格被转义成 `\040`。
fn fstype_for_path(mounts: &str, path: &Path) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for line in mounts.lines() {
        let mut it = line.split_whitespace();
        let (Some(_dev), Some(mnt), Some(fstype)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let mnt = mnt.replace("\\040", " ");
        let mnt_path = Path::new(&mnt);
        if path.starts_with(mnt_path) {
            let depth = mnt_path.components().count();
            if best.as_ref().map_or(true, |(d, _)| depth >= *d) {
                best = Some((depth, fstype.to_string()));
            }
        }
    }
    best.map(|(_, t)| t)
}

/// 磁盘缓存实际落在 tmpfs/ramfs 上时，占用的仍然是内存，起不到“省内存”的作用。
/// OpenWrt 的 /tmp（含 x86）默认就是 tmpfs，但这取决于具体系统/容器，
/// 所以这里按实际挂载情况检测，而不是靠文档里的假设。
fn warn_if_memory_backed(dir: &Path) {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else { return };
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if let Some(t) = fstype_for_path(&mounts, &real) {
        if t == "tmpfs" || t == "ramfs" {
            tracing::warn!(
                dir = %dir.display(),
                fstype = %t,
                "segment cache: cache_dir is on a memory-backed filesystem, disk cache will still use RAM; \
                 point cache_dir to a USB/SD/HDD mount to actually save memory"
            );
        }
    }
}

/// 创建缓存目录，并清掉上次进程异常退出残留的分片文件（只认 `*.seg` / `*.seg.tmp`）。
fn prepare_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;

    // 可写性检查：提前暴露只读文件系统 / 权限问题，而不是等到第一个分片写失败
    let probe = dir.join(".m3u8mc-write-test");
    std::fs::write(&probe, b"ok")?;
    let _ = std::fs::remove_file(&probe);

    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(SEG_SUFFIX) || name.ends_with(TMP_SUFFIX) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fstype_detection_uses_longest_mount_prefix() {
        let mounts = "rootfs / rootfs rw 0 0\n\
                      /dev/sda2 / ext4 rw 0 0\n\
                      tmpfs /tmp tmpfs rw,nosuid 0 0\n\
                      /dev/sdb1 /tmp/usb ext4 rw 0 0\n\
                      /dev/sdc1 /mnt/my\\040disk ext4 rw 0 0\n";
        let f = |p: &str| fstype_for_path(mounts, Path::new(p));
        assert_eq!(f("/tmp/m3u8mc/ch1").as_deref(), Some("tmpfs"));
        assert_eq!(f("/tmp/usb/m3u8mc").as_deref(), Some("ext4"), "更深的挂载点优先");
        assert_eq!(f("/mnt/sda1/m3u8mc").as_deref(), Some("ext4"));
        assert_eq!(f("/mnt/my disk/x").as_deref(), Some("ext4"), "转义的空格要还原");
        assert_eq!(f("/tmpx/a").as_deref(), Some("ext4"), "/tmpx 不是 /tmp 的子目录");
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "m3u8mc-cache-test-{tag}-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        d
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    #[tokio::test]
    async fn memory_insert_read_and_count_eviction() {
        let c = SegmentCache::new(2, Duration::from_secs(60), CacheBackend::Memory);
        for i in 0..4u64 {
            let seq = c.alloc_seq();
            c.insert(seq, Bytes::from(vec![i as u8; 10]), 6.0).await.unwrap();
        }
        assert_eq!(c.sorted_seqs(), vec![2, 3]);
        assert_eq!(c.read(3).await.unwrap().as_ref(), &[3u8; 10]);
        assert!(c.read(0).await.is_none());
        assert_eq!(c.total_bytes(), 20);
        assert_eq!(c.storage_name(), "memory");
    }

    #[tokio::test]
    async fn disk_insert_read_and_eviction_removes_files() {
        let dir = temp_dir("evict");
        let c = SegmentCache::new(2, Duration::from_secs(60), CacheBackend::Disk(dir.clone()));
        assert_eq!(c.storage_name(), "disk");
        for i in 0..4u64 {
            let seq = c.alloc_seq();
            c.insert(seq, Bytes::from(vec![i as u8; 16]), 6.0).await.unwrap();
        }
        assert_eq!(c.sorted_seqs(), vec![2, 3]);
        assert_eq!(c.read(2).await.unwrap().as_ref(), &[2u8; 16]);
        assert!(c.read(1).await.is_none());
        // 被淘汰的分片文件必须真的从磁盘删除，磁盘上只剩 2 个
        assert_eq!(files_in(&dir).len(), 2);
        c.shutdown().await;
        // shutdown 后文件清空、空目录被删除
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn disk_time_window_eviction() {
        let dir = temp_dir("window");
        let c = SegmentCache::new(10, Duration::from_millis(50), CacheBackend::Disk(dir.clone()));
        c.insert(c.alloc_seq(), Bytes::from_static(b"old"), 1.0).await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        c.insert(c.alloc_seq(), Bytes::from_static(b"new"), 1.0).await.unwrap();
        assert_eq!(c.sorted_seqs(), vec![1]);
        assert_eq!(files_in(&dir).len(), 1);
        c.shutdown().await;
    }

    #[tokio::test]
    async fn disk_startup_cleans_stale_seg_files_only() {
        let dir = temp_dir("stale");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("abc-1.seg"), b"stale").unwrap();
        std::fs::write(dir.join("abc-2.seg.tmp"), b"stale").unwrap();
        std::fs::write(dir.join("keep-me.txt"), b"user file").unwrap();
        let c = SegmentCache::new(2, Duration::from_secs(60), CacheBackend::Disk(dir.clone()));
        assert_eq!(files_in(&dir), vec!["keep-me.txt".to_string()]);
        c.shutdown().await;
        // 目录里还有用户自己的文件，shutdown 不得删除目录和该文件
        assert_eq!(files_in(&dir), vec!["keep-me.txt".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn insert_after_shutdown_is_dropped_and_leaves_no_file() {
        let dir = temp_dir("closed");
        let c = SegmentCache::new(2, Duration::from_secs(60), CacheBackend::Disk(dir.clone()));
        c.shutdown().await;
        c.insert(0, Bytes::from_static(b"late"), 1.0).await.unwrap();
        assert!(c.sorted_seqs().is_empty());
        assert!(files_in(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unusable_dir_falls_back_to_memory() {
        // /proc 下无法创建目录 → 应回退到内存而不是 panic
        let c = SegmentCache::new(2, Duration::from_secs(60), CacheBackend::Disk(PathBuf::from("/proc/m3u8mc-nope")));
        assert_eq!(c.storage_name(), "memory");
        c.insert(0, Bytes::from_static(b"x"), 1.0).await.unwrap();
        assert_eq!(c.read(0).await.unwrap().as_ref(), b"x");
    }
}
