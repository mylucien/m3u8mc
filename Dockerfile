# 配合 .github/workflows/build.yml 里的 docker job 使用。
# 二进制在 CI 里已经用 cargo-zigbuild 交叉编译好并合并进 bin/<target-triple>/，
# 这里只是按 buildx 传入的 TARGETPLATFORM 挑对应架构的产物塞进 alpine 镜像，
# 不在容器里现场编译。

FROM alpine:3.22

ARG TARGETPLATFORM

# m3u8mcd 需要 ffmpeg 来做转码/推流
RUN apk add --no-cache ffmpeg ca-certificates

COPY bin/ /tmp/bin/

RUN set -eu; \
    case "$TARGETPLATFORM" in \
        "linux/amd64")  triple=x86_64-unknown-linux-musl ;; \
        "linux/arm64")  triple=aarch64-unknown-linux-musl ;; \
        "linux/arm/v7") triple=armv7-unknown-linux-musleabihf ;; \
        "linux/386")    triple=i686-unknown-linux-musl ;; \
        *) echo "unsupported TARGETPLATFORM: $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    cp "/tmp/bin/${triple}/m3u8mcd" /usr/local/bin/m3u8mcd; \
    cp "/tmp/bin/${triple}/m3u8mcctl" /usr/local/bin/m3u8mcctl; \
    chmod +x /usr/local/bin/m3u8mcd /usr/local/bin/m3u8mcctl; \
    rm -rf /tmp/bin

# 配置文件、control socket 目录
RUN mkdir -p /etc/config /var/run

VOLUME ["/etc/config"]

# HTTP 缓存端口段的起始端口，方便 docker run -p 映射；具体范围看你的
# global.http_port_base 配置，这里只是给个默认示例，不影响实际监听端口数量
EXPOSE 18080-18090/tcp

ENTRYPOINT ["/usr/local/bin/m3u8mcd"]
CMD ["--config", "/etc/config/m3u8mc"]
