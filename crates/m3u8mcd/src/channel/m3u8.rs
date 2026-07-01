//! 极简 m3u8 解析器，只覆盖本插件需要的标签：
//! `#EXT-X-STREAM-INF`（主列表变体）、`#EXTINF`/分片 URI、`#EXT-X-MEDIA-SEQUENCE`、
//! `#EXT-X-TARGETDURATION`。不追求覆盖 HLS 全部规范。

#[derive(Debug, Clone)]
pub struct Variant {
    pub bandwidth: u64,
    pub uri: String,
}

#[derive(Debug, Clone)]
pub struct MediaSegment {
    pub duration_secs: f32,
    pub uri: String,
}

#[derive(Debug, Clone, Default)]
pub struct MediaPlaylist {
    pub media_sequence: u64,
    pub target_duration: f32,
    pub segments: Vec<MediaSegment>,
}

pub enum ParsedPlaylist {
    Master(Vec<Variant>),
    Media(MediaPlaylist),
}

pub fn parse(text: &str) -> ParsedPlaylist {
    let lines: Vec<&str> = text.lines().map(|l| l.trim()).collect();

    let has_stream_inf = lines.iter().any(|l| l.starts_with("#EXT-X-STREAM-INF"));
    if has_stream_inf {
        let mut variants = Vec::new();
        let mut pending_bandwidth: Option<u64> = None;
        for line in &lines {
            if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
                pending_bandwidth = parse_attr_u64(rest, "BANDWIDTH");
                if pending_bandwidth.is_none() {
                    tracing::warn!("master playlist variant missing BANDWIDTH attribute, skipping: {rest}");
                }
            } else if !line.is_empty() && !line.starts_with('#') {
                if let Some(bw) = pending_bandwidth.take() {
                    variants.push(Variant {
                        bandwidth: bw,
                        uri: line.to_string(),
                    });
                }
            }
        }
        return ParsedPlaylist::Master(variants);
    }

    let mut media_sequence = 0u64;
    let mut target_duration = 6.0f32;
    let mut segments = Vec::new();
    let mut pending_duration: Option<f32> = None;

    for line in &lines {
        if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            target_duration = rest.trim().parse().unwrap_or(6.0);
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            let dur_str = rest.split(',').next().unwrap_or("0");
            pending_duration = dur_str.trim().parse().ok();
        } else if !line.is_empty() && !line.starts_with('#') {
            segments.push(MediaSegment {
                duration_secs: pending_duration.take().unwrap_or(target_duration),
                uri: line.to_string(),
            });
        }
    }

    ParsedPlaylist::Media(MediaPlaylist {
        media_sequence,
        target_duration,
        segments,
    })
}

/// 选择带宽最优（最高）的变体，对应设计文档 1.2 "按带宽选最优子列表"。
pub fn pick_best_variant(variants: &[Variant]) -> Option<&Variant> {
    variants.iter().max_by_key(|v| v.bandwidth)
}

fn parse_attr_u64(attrs: &str, key: &str) -> Option<u64> {
    for part in attrs.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(key) {
            let v = v.trim_start_matches('=').trim_matches('"');
            if let Ok(n) = v.parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_media_playlist() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:100\n#EXTINF:5.940,\nseg100.ts\n#EXTINF:6.000,\nseg101.ts\n";
        match parse(text) {
            ParsedPlaylist::Media(m) => {
                assert_eq!(m.media_sequence, 100);
                assert_eq!(m.segments.len(), 2);
                assert_eq!(m.segments[0].uri, "seg100.ts");
            }
            _ => panic!("expected media playlist"),
        }
    }

    #[test]
    fn parses_master_playlist_and_picks_best() {
        let text = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000\nlow.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=3000000\nhigh.m3u8\n";
        match parse(text) {
            ParsedPlaylist::Master(variants) => {
                let best = pick_best_variant(&variants).unwrap();
                assert_eq!(best.uri, "high.m3u8");
            }
            _ => panic!("expected master playlist"),
        }
    }
}
