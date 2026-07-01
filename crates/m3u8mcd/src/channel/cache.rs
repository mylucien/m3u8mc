//! SegmentCache：按 `seq` 编号存储分片字节数据。
//!
//! 设计文档 1.2：分片在内部统一按 seq 编号路由（而非沿用上游原始文件名），
//! 与上游文件名格式解耦；按时间窗口 + 数量上限双重淘汰；向 TS 流客户端广播新分片。

use bytes::Bytes;
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct CachedSegment {
    pub seq: u64,
    pub data: Bytes,
    pub inserted_at: Instant,
    pub duration_secs: f32,
}

pub struct SegmentCache {
    segments: DashMap<u64, CachedSegment>,
    max_segments: usize,
    /// 时间窗口淘汰时长 = 分片时长 × cache_window（见设计文档 2.2 关键参数取值依据）
    window: Duration,
    next_seq: AtomicU64,
    /// 广播新分片 seq，供 InternalHttpServer 的 TS 连续流模式订阅
    notify: broadcast::Sender<u64>,
}

impl SegmentCache {
    pub fn new(max_segments: usize, window: Duration) -> Arc<Self> {
        let (tx, _rx) = broadcast::channel(256);
        Arc::new(SegmentCache {
            segments: DashMap::new(),
            max_segments,
            window,
            next_seq: AtomicU64::new(0),
            notify: tx,
        })
    }

    /// 分配下一个内部 seq 号（与上游原始分片序号无关，由 daemon 自行编号）。
    pub fn alloc_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::Relaxed)
    }

    pub fn insert(&self, seq: u64, data: Bytes, duration_secs: f32) {
        self.segments.insert(
            seq,
            CachedSegment {
                seq,
                data,
                inserted_at: Instant::now(),
                duration_secs,
            },
        );
        self.evict();
        let _ = self.notify.send(seq);
    }

    fn evict(&self) {
        let now = Instant::now();
        // 时间窗口淘汰
        self.segments
            .retain(|_, seg| now.duration_since(seg.inserted_at) <= self.window);
        // 数量上限淘汰：超出部分淘汰最旧的 seq
        if self.segments.len() > self.max_segments {
            let mut seqs: Vec<u64> = self.segments.iter().map(|e| *e.key()).collect();
            seqs.sort_unstable();
            let overflow = self.segments.len() - self.max_segments;
            for seq in seqs.into_iter().take(overflow) {
                self.segments.remove(&seq);
            }
        }
    }

    pub fn get(&self, seq: u64) -> Option<CachedSegment> {
        self.segments.get(&seq).map(|e| e.clone())
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

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<u64> {
        self.notify.subscribe()
    }
}
