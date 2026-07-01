#!/bin/sh
# /usr/share/m3u8mc/ffmpeg-install.sh
#
# 下载并安装全功能静态 ffmpeg 二进制。
# 由 LuCI 后端 API 调用；也可以在 SSH 里手动运行。
#
# 用法：
#   ffmpeg-install.sh check                   — 检测当前 ffmpeg 能力
#   ffmpeg-install.sh download <url> [dest]   — 下载并安装到 dest（默认 /usr/local/bin/ffmpeg）
#   ffmpeg-install.sh set-uci <path>          — 把路径写入 UCI ffmpeg_bin 并热重载
#   ffmpeg-install.sh reset-uci               — 清空 UCI ffmpeg_bin（回到系统 PATH）
#
# 输出格式：每行都是 JSON，最后一行以 {"done":true} 或 {"error":"..."} 结尾，
# 便于 LuCI 前端逐行解析流式进度。

set -e

DEST_DEFAULT="/usr/local/bin/ffmpeg"
VERIFY_TIMEOUT=8

# ── 工具函数 ──────────────────────────────────────────────────

json_progress() { printf '{"progress":%s,"msg":%s}\n' "$1" "\"$2\""; }
json_done()     { printf '{"done":true,"msg":%s}\n' "\"$1\""; }
json_error()    { printf '{"error":%s}\n' "\"$1\""; exit 1; }

# 检测可用下载工具，返回下载命令前缀（后接 url 和目标文件）
detect_downloader() {
    if command -v curl >/dev/null 2>&1; then
        echo "curl -fsSL --connect-timeout 10 -o"
    elif command -v wget >/dev/null 2>&1; then
        echo "wget -q --timeout=10 -O"
    else
        json_error "未找到 curl 或 wget，请先安装其中之一"
    fi
}

# ── check ─────────────────────────────────────────────────────
cmd_check() {
    local bin="${1:-ffmpeg}"
    [ -z "$bin" ] && bin="ffmpeg"

    if ! command -v "$bin" >/dev/null 2>&1 && [ ! -x "$bin" ]; then
        printf '{"found":false,"path":"","version":"","features":{}}\n'
        return
    fi

    # 版本
    local ver
    ver=$("$bin" -version 2>&1 | head -1 | sed 's/ffmpeg version //' | awk '{print $1}')

    # 注意：grep -c 在 0 匹配时输出 "0" 但退出码为 1。
    # 在 set -e 下，普通变量赋值（非 local x=$(...) 合并写法）会把这个非零
    # 退出码原样传给当前语句，导致脚本提前退出且不输出任何 JSON；
    # 用 `|| true` 兜底退出码，同时不能再用 `|| echo 0` 追加输出（会拼成两行）。
    local has_mpegts has_rtp has_bsf_h264 has_hls_demux
    has_mpegts=$("$bin"  -muxers   2>/dev/null | grep -c '\bmpegts\b') || true
    has_rtp=$("$bin"     -muxers   2>/dev/null | grep -c '\brtp\b') || true
    has_bsf_h264=$("$bin" -bsfs    2>/dev/null | grep -c 'h264_mp4toannexb') || true
    has_hls_demux=$("$bin" -demuxers 2>/dev/null | grep -c '\bhls\b') || true

    local path
    path=$(command -v "$bin" 2>/dev/null || echo "$bin")

    printf '{"found":true,"path":"%s","version":"%s","features":{"mpegts":%s,"rtp":%s,"h264_mp4toannexb":%s,"hls_demux":%s}}\n' \
        "$path" "$ver" \
        "$([ "$has_mpegts"   -gt 0 ] && echo true || echo false)" \
        "$([ "$has_rtp"      -gt 0 ] && echo true || echo false)" \
        "$([ "$has_bsf_h264" -gt 0 ] && echo true || echo false)" \
        "$([ "$has_hls_demux" -gt 0 ] && echo true || echo false)"
}

# ── download ──────────────────────────────────────────────────
cmd_download() {
    local url="$1"
    local dest="${2:-$DEST_DEFAULT}"

    [ -z "$url" ] && json_error "缺少下载 URL 参数"

    local dl
    dl=$(detect_downloader)

    json_progress 5 "开始下载 $url"

    local tmpfile
    tmpfile=$(mktemp /tmp/ffmpeg_dl_XXXXXX)
    # shellcheck disable=SC2064
    trap "rm -f '$tmpfile'" EXIT

    # 实际下载（不显示进度条，输出交给进度 JSON）
    if ! $dl "$tmpfile" "$url" 2>/tmp/ffmpeg_dl_err; then
        local err
        err=$(cat /tmp/ffmpeg_dl_err 2>/dev/null | head -3 | tr '\n' ' ')
        json_error "下载失败: $err"
    fi

    json_progress 60 "下载完成，检测文件格式"

    # 检测是否压缩包（tar.xz / tar.gz / tar.bz2）
    local mime
    mime=$(file -b --mime-type "$tmpfile" 2>/dev/null || echo "application/octet-stream")

    case "$mime" in
        application/x-xz|application/x-bzip2|application/gzip|application/x-tar)
            json_progress 65 "检测到压缩包，解压并提取 ffmpeg 二进制"
            local unpackdir
            unpackdir=$(mktemp -d /tmp/ffmpeg_unpack_XXXXXX)
            # shellcheck disable=SC2064
            trap "rm -rf '$unpackdir' '$tmpfile'" EXIT

            case "$mime" in
                application/x-xz)  tar -xJf "$tmpfile" -C "$unpackdir" 2>/dev/null ;;
                application/gzip)   tar -xzf "$tmpfile" -C "$unpackdir" 2>/dev/null ;;
                application/x-bzip2) tar -xjf "$tmpfile" -C "$unpackdir" 2>/dev/null ;;
                application/x-tar) tar -xf "$tmpfile" -C "$unpackdir" 2>/dev/null ;;
            esac

            # 在解压目录里找第一个名为 ffmpeg 的可执行文件
            local found
            found=$(find "$unpackdir" -type f -name "ffmpeg" | head -1)
            [ -z "$found" ] && json_error "压缩包内未找到 ffmpeg 可执行文件"
            cp "$found" "$tmpfile"
            rm -rf "$unpackdir"
            ;;
        *)
            # 直接是二进制
            ;;
    esac

    json_progress 75 "验证二进制可执行性"
    chmod +x "$tmpfile"

    # 简单验证：能运行 -version 且输出包含 ffmpeg
    if ! timeout "$VERIFY_TIMEOUT" "$tmpfile" -version 2>&1 | grep -qi ffmpeg; then
        json_error "验证失败：文件不是有效的 ffmpeg 二进制（或架构不匹配）"
    fi

    json_progress 85 "安装到 $dest"
    local dest_dir
    dest_dir=$(dirname "$dest")
    mkdir -p "$dest_dir"

    # 先写到 .tmp 再 mv，避免覆盖过程中 daemon 读到损坏文件
    cp "$tmpfile" "${dest}.tmp"
    chmod +x "${dest}.tmp"
    mv "${dest}.tmp" "$dest"

    json_progress 100 "安装完成：$dest"
    cmd_check "$dest"
    json_done "ffmpeg 安装成功：$dest"
}

# ── set-uci ───────────────────────────────────────────────────
cmd_set_uci() {
    local path="$1"
    [ -z "$path" ] && json_error "缺少路径参数"
    [ ! -x "$path" ] && json_error "路径 $path 不存在或不可执行"

    uci set m3u8mc.global.ffmpeg_bin="$path"
    uci commit m3u8mc

    local sock
    sock=$(uci -q get m3u8mc.global.control_socket 2>/dev/null || echo "/var/run/m3u8mcd.sock")
    if [ -S "$sock" ]; then
        /usr/bin/m3u8mcctl --socket "$sock" reload 2>/dev/null && \
            json_done "UCI 已更新并热重载" || \
            json_done "UCI 已更新，热重载失败（daemon 可能未运行），请手动 reload"
    else
        json_done "UCI 已更新，daemon 未运行，下次启动生效"
    fi
}

# ── reset-uci ─────────────────────────────────────────────────
cmd_reset_uci() {
    uci -q delete m3u8mc.global.ffmpeg_bin 2>/dev/null || true
    uci commit m3u8mc

    local sock
    sock=$(uci -q get m3u8mc.global.control_socket 2>/dev/null || echo "/var/run/m3u8mcd.sock")
    [ -S "$sock" ] && /usr/bin/m3u8mcctl --socket "$sock" reload 2>/dev/null || true
    json_done "已清空 ffmpeg_bin，daemon 将使用系统 PATH 中的 ffmpeg"
}

# ── 入口 ──────────────────────────────────────────────────────
case "${1:-}" in
    check)    cmd_check    "${2:-}" ;;
    download) cmd_download "${2:-}" "${3:-}" ;;
    set-uci)  cmd_set_uci  "${2:-}" ;;
    reset-uci) cmd_reset_uci ;;
    *)
        printf 'Usage: %s check [bin] | download <url> [dest] | set-uci <path> | reset-uci\n' "$0" >&2
        exit 1
        ;;
esac
