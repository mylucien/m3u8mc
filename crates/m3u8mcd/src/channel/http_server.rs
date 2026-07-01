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
}

pub async fn serve(
    cache: Arc<SegmentCache>,
    http_path: String,
    hls_segments: usize,
    port: u16,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let path = if http_path.starts_with('/') {
        http_path.clone()
    } else {
        format!("/{http_path}")
    };
    let state = ServerState {
        cache,
        hls_segments,
    };

    let playlist_route = format!("{}/playlist.m3u8", path.trim_end_matches('/'));
    let segment_route = format!("{}/seg/:seq", path.trim_end_matches('/'));
    let stream_route = format!("{}/stream.ts", path.trim_end_matches('/'));

    let app = Router::new()
        .route(&playlist_route, get(playlist_handler))
        .route(&segment_route, get(segment_handler))
        .route(&stream_route, get(stream_handler))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, %playlist_route, "internal http server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::time::timeout(Duration::from_secs(4), stop_rx.changed()).await;
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
        .filter_map(|s| state.cache.get(*s))
        .map(|s| s.duration_secs)
        .fold(1.0f32, f32::max)
        .ceil() as u32;
    body.push_str(&format!("#EXT-X-TARGETDURATION:{}\n", target_duration.max(1)));
    body.push_str(&format!("#EXT-X-MEDIA-SEQUENCE:{media_sequence}\n"));
    for seq in recent {
        if let Some(seg) = state.cache.get(*seq) {
            body.push_str(&format!("#EXTINF:{:.3},\n", seg.duration_secs));
            body.push_str(&format!("seg/{}.ts\n", seg.seq));
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
    match state.cache.get(seq) {
        Some(seg) => ([(header::CONTENT_TYPE, "video/mp2t")], seg.data.to_vec()).into_response(),
        None => (StatusCode::NOT_FOUND, "segment not found or evicted").into_response(),
    }
}

/// TS 连续字节流：从当前最新分片开始，按 188 字节 TS 包对齐持续推送，
/// 避免因切片边界不对齐导致花屏。
async fn stream_handler(State(state): State<ServerState>) -> Response {
    let cache = state.cache.clone();

    // 先拿快照，再订阅——避免中间插入新分片导致重复推送
    let initial: Vec<u64> = cache.sorted_seqs();
    let mut rx = cache.subscribe();
    let initial_set: std::collections::HashSet<u64> = initial.iter().copied().collect();

    let body_stream = async_stream::stream! {
        for seq in initial {
            if let Some(seg) = cache.get(seq) {
                yield Ok::<_, std::io::Error>(axum::body::Bytes::from(seg.data));
            }
        }
        loop {
            match rx.recv().await {
                Ok(seq) => {
                    // 跳过在 initial 快照中已推送的分片
                    if initial_set.contains(&seq) {
                        continue;
                    }
                    if let Some(seg) = cache.get(seq) {
                        yield Ok(axum::body::Bytes::from(seg.data));
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // 客户端消费太慢，跳过中间丢失的部分继续，不中断连接
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    };

    Response::builder()
        .header(header::CONTENT_TYPE, "video/mp2t")
        .body(Body::from_stream(body_stream))
        .unwrap()
}
