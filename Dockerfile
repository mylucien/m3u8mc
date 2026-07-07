# 配合 .github/workflows/build.yml 里的 docker job 使用。
# 二进制在 CI 里已经用 cargo-zigbuild 交叉编译好并合并进 bin/<target-triple>/，
# 这里只是按 buildx 传入的 TARGETPLATFORM 挑对应架构的产物塞进 alpine 镜像，
# 不在容器里现场编译。
#
# ffmpeg 策略：
#   linux/amd64  → 使用 Release 里备份的 ffmpeg-bundle.tar.gz（musl 版，排除 ld-musl）
#   其他架构     → apk add ffmpeg

FROM alpine:3.22
ARG TARGETPLATFORM

RUN set -eu; \
    apk add --no-cache ca-certificates wget; \
    case "$TARGETPLATFORM" in \
        "linux/amd64") \
            wget -q -O /tmp/ffmpeg-bundle.tar.gz \
                "https://github.com/mylucien/lucien/releases/download/v1.0.0/ffmpeg-bundle.tar.gz"; \
            mkdir -p /tmp/ffmpeg-bundle; \
            tar -xzf /tmp/ffmpeg-bundle.tar.gz -C /tmp/ffmpeg-bundle; \
            cp /tmp/ffmpeg-bundle/bin/ffmpeg  /usr/local/bin/ffmpeg; \
            cp /tmp/ffmpeg-bundle/bin/ffprobe /usr/local/bin/ffprobe; \
            chmod +x /usr/local/bin/ffmpeg /usr/local/bin/ffprobe; \
            for so in /tmp/ffmpeg-bundle/lib/*.so*; do \
                case "$so" in *ld-musl-*) continue ;; esac; \
                cp "$so" /usr/local/lib/; \
            done; \
            ldconfig /usr/local/lib 2>/dev/null || true; \
            ;; \
        *) \
            apk add --no-cache ffmpeg; \
            ;; \
    esac; \
    rm -rf /tmp/ffmpeg-bundle.tar.gz /tmp/ffmpeg-bundle

COPY bin/ /tmp/bin/

RUN set -eu; \
    case "$TARGETPLATFORM" in \
        "linux/amd64")  triple=x86_64-unknown-linux-musl ;; \
        "linux/arm64")  triple=aarch64-unknown-linux-musl ;; \
        "linux/arm/v7") triple=armv7-unknown-linux-musleabihf ;; \
        "linux/386")    triple=i686-unknown-linux-musl ;; \
        *) echo "unsupported TARGETPLATFORM: $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    cp "/tmp/bin/${triple}/m3u8mcd"   /usr/local/bin/m3u8mcd; \
    cp "/tmp/bin/${triple}/m3u8mcctl" /usr/local/bin/m3u8mcctl; \
    chmod +x /usr/local/bin/m3u8mcd /usr/local/bin/m3u8mcctl; \
    rm -rf /tmp/bin

RUN mkdir -p /etc/config /var/run
VOLUME ["/etc/config"]

EXPOSE 18080-18090/tcp
ENTRYPOINT ["/usr/local/bin/m3u8mcd"]
CMD ["--config", "/etc/config/m3u8mc"]
