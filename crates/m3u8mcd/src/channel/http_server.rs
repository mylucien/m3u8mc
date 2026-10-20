//! InternalHttpServer（设计文档 1.2）。
//!
//! 提供两种接口形态：
//! 1. HLS 模式：动态生成 m3u8 播放列表 + 按 seq 提供 ts 分片，ffmpeg 与外部客户端共用同一入口；
//! 2. TS 连续字节流模式：面向直接拉流播放的客户端（如 VLC），按分片顺序持续推送字节流。
//!
//! 每个频道各自监听独立端口，避免多频道并行时端口冲突。

use super::cache::SegmentCache;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
struct ServerState {
    cache: Arc<SegmentCache>,
    hls_segments: usize,
    /// 归一化后的 http_path（如 "/live"），用于生成 m3u8 里的绝对分片路径
    http_path: String,
}

pub async fn serve(
    cache: Arc<SegmentCache>,
    http_path: String,
    hls_segments: usize,
    port: u16,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // 与 channel/mod.rs 里 normalize_path 保持完全一致的归一化逻辑：
    // 保证有且仅有一个前导 '/'，且没有尾部 '/'，避免 ffmpeg 实际请求的 URL
    // 与这里注册的路由因为斜杠数量不一致而对不上（例如 http_path 配了 '/live/'）。
    let path = format!("/{}", http_path.trim_matches('/'));
    let state = ServerState {
        cache,
        hls_segments,
        http_path: path.clone(),
    };

    let playlist_route = format!("{path}/playlist.m3u8");
    // 路由用子目录格式（axum 只支持 /:param，不支持前缀拼接如 seg_:seq）。
    // playlist 里写绝对路径，兼容 ffmpeg / VLC / 浏览器等各类客户端，
    // 避免相对路径被部分播放器丢失 http_path 前缀而 404。
    let segment_route = format!("{path}/seg/:seq");
    let stream_route = format!("{path}/stream.ts");

    let app = Router::new()
        .route(&playlist_route, get(playlist_handler))
        .route(&segment_route, get(segment_handler))
        .route(&stream_route, get(stream_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, %playlist_route, "internal http server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = stop_rx.changed().await;
            // axum 收到信号后 drain 已有连接，然后 serve() 返回
        })
        .await?;
    Ok(())
}

async fn playlist_handler(State(state): State<ServerState>) -> Response {
    let seqs = state.cache.sorted_seqs();
    if seqs.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "no segments cached yet").into_response();
    }
    let take_n = state.hls_segments.min(seqs.len());
    let recent = &seqs[seqs.len() - take_n..];
    let media_sequence = recent[0];

    let mut body = String::new();
    body.push_str("#EXTM3U\n");
    body.push_str("#EXT-X-VERSION:3\n");
    let target_duration = recent
        .iter()
        .filter_map(|s| state.cache.info(*s))
        .map(|s| s.duration_secs)
        .fold(1.0f32, f32::max)
        .ceil() as u32;
    body.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", target_duration.max(1)));
    body.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n"));
    for seq in recent {
        if let Some(info) = state.cache.info(*seq) {
            body.push_str(&format!("#EXTINF:{:.3},\n", info.duration_secs));
            body.push_str(&format!("{}/seg/{}.ts\n", state.http_path, seq));
        }
    }

    (
        [(header::CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        body,
    )
        .into_response()
}

async fn segment_handler(State(state): State<ServerState>, Path(seq_str): Path<String>) -> Response {
    let seq_str = seq_str.strip_suffix(".ts").unwrap_or(&seq_str);
    let seq: u64 = match seq_str.parse() {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid segment id").into_response(),
    };
    // Bytes 直接作为响应体：内存模式零拷贝（之前 to_vec() 每次请求都会整片复制一份），
    // 磁盘模式下是一次文件读取。
    match state.cache.read(seq).await {
        Some(data) => ([(header::CONTENT_TYPE, "video/mp2t")], Body::from(data)).into_response(),
        None => (StatusCode::NOT_FOUND, "segment not found or evicted").into_response(),
    }
}

/// 等待缺口分片补齐的最长时间：超过后强制跳过空缺，避免下载失败/长时间乱序
/// 导致播放卡死。取值参考典型分片时长的若干倍，兼顾容错与实时性。
const REORDER_GAP_TIMEOUT: Duration = Duration::from_secs(5);

/// TS 连续字节流：从当前最新分片开始，按 seq 严格顺序持续推送（188 字节 TS 包
/// 对齐，避免因切片边界不对齐导致花屏）。
///
/// 下载是并发进行的，不同分片的完成时间可能与 seq 顺序不一致（标准模式重试换台、
/// 赛马模式竞速耗时不同都会导致这种情况）。这里维护一个"下一个期望 seq"游标 +
/// 乱序缓冲：broadcast 通知到达后，只有等到期望的 seq 真正到齐才会连续 yield 出去；
/// 提前到达的后续 seq 先记入 pending，等前面缺口补上后再按序吐出。
/// 若缺口长时间未补齐（下载失败/分片被清理），则强制跳到当前已知的最小 pending
/// seq，避免播放流被单个坏分片卡死。
async fn stream_handler(State(state): State<ServerState>) -> Response {
    let cache = state.cache.clone();

    // 先拿快照，再订阅——避免中间插入新分片导致重复推送
    let initial: Vec<u64> = cache.sorted_seqs();
    let mut rx = cache.subscribe();
    let initial_set: std::collections::HashSet<u64> = initial.iter().copied().collect();
    // 起始游标：从快照里最大的 seq 之后开始接力；快照为空则以第一个到达的 seq 为起点。
    let mut expected_next: Option<u64> = initial.last().map(|s| s + 1);
    // 乱序到达但还不能吐出去的 seq，等待前面缺口补齐；BTreeSet 保证按序遍历。
    let mut pending: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();

    let body_stream = async_stream::stream! {
        for seq in initial {
            if let Some(data) = cache.read(seq).await {
                yield Ok::<_, std::io::Error>(data);
            }
        }

        loop {
            let recv_result = if pending.is_empty() {
                // 没有挂起的缺口，无需超时，正常等待下一个通知
                match rx.recv().await {
                    Ok(seq) => Some(Ok(seq)),
                    Err(e) => Some(Err(e)),
                }
            } else {
                // 有缺口在等待补齐，加超时保护，避免单个坏分片卡死整条流
                match tokio::time::timeout(REORDER_GAP_TIMEOUT, rx.recv()).await {
                    Ok(Ok(seq)) => Some(Ok(seq)),
                    Ok(Err(e)) => Some(Err(e)),
                    Err(_) => None, // 超时：走下面的强制跳跃逻辑
                }
            };

            match recv_result {
                Some(Ok(seq)) => {
                    // 跳过在 initial 快照中已推送的分片
                    if initial_set.contains(&seq) {
                        continue;
                    }
                    if expected_next.is_none() {
                        expected_next = Some(seq);
                    }
                    let exp = expected_next.unwrap();
                    if seq < exp {
                        // 早于当前游标（重复通知/陈旧数据），丢弃
                        continue;
                    }
                    pending.insert(seq);
                }
                Some(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                    // 客户端消费太慢导致 broadcast 缓冲溢出：直接从缓存里按 seq
                    // 重新拉取所有 >= 当前游标且仍在缓存中的分片，补进 pending，
                    // 而不是简单跳过（否则会把还没吐出去的分片直接丢失）。
                    if let Some(exp) = expected_next {
                        for s in cache.sorted_seqs() {
                            if s >= exp {
                                pending.insert(s);
                            }
                        }
                    }
                }
                Some(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                None => {
                    // 等待缺口超时：跳到当前已知最小的 pending seq，放弃缺失的分片，
                    // 避免因某个分片下载失败/被淘汰导致整条流卡死。
                    if let Some(&min_pending) = pending.iter().next() {
                        tracing::warn!(
                            expected = ?expected_next,
                            skip_to = min_pending,
                            "stream reorder gap timed out, skipping missing segment(s)"
                        );
                        expected_next = Some(min_pending);
                    }
                }
            }

            // 按序尽可能多地吐出已到齐的连续分片
            while let Some(&front) = pending.iter().next() {
                let exp = expected_next.unwrap_or(front);
                if front < exp {
                    pending.remove(&front);
                } else if front == exp {
                    pending.remove(&front);
                    if let Some(data) = cache.read(front).await {
                        yield Ok(data);
                    }
                    expected_next = Some(front + 1);
                } else {
                    break; // 前面还有缺口，等待
                }
            }
        }
    };

    Response::builder()
        .header(header::CONTENT_TYPE, "video/mp2t")
        .body(Body::from_stream(body_stream))
        .unwrap()
}
