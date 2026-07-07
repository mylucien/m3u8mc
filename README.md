1. 项目简介

m3u8mc 是一个将 HLS（m3u8）直播源转换为组播/UDP 流的守护进程，主要用于 OpenWrt 路由器或 Linux 服务器场景。容器镜像内置 ffmpeg，无需在宿主机单独安装，开箱即用。

核心组件：


m3u8mcd：主守护进程，负责拉取 m3u8 列表、缓存 TS 分片、通过 ffmpeg 转推组播/UDP，同时提供内置 HTTP 服务供 HLS 回放
m3u8mcctl：命令行控制工具，通过 Unix Socket 与 daemon 通信，用于查看状态、热重载配置、启停频道



2. 快速开始

2.1 准备配置文件

在宿主机创建配置目录和配置文件：

bashmkdir -p /etc/m3u8mc
cat > /etc/m3u8mc/m3u8mc << 'EOF'
config global
    option enabled        1
    option log_level      info
    option http_port_base 18080

config channel 'cctv1'
    option enabled  1
    option name     'CCTV-1'
    list   url      'http://your-source.example.com/cctv1/playlist.m3u8'
    option addr     '239.1.1.1'
    option port     5000
    option iface    'eth0'
EOF

2.2 启动容器

bashdocker run -d \
  --name m3u8mc \
  --restart unless-stopped \
  --network host \
  -v /etc/m3u8mc:/etc/config \
  ghcr.io/mylucien/m3u8mc:latest \
  --config /etc/config/m3u8mc


注意：--network host 是必须的，组播/UDP 发包需要直接访问宿主机网络接口，bridge 网络无法正确绑定组播地址。



2.3 验证运行状态

bash# 查看日志
docker logs -f m3u8mc

# 查看频道状态
docker exec m3u8mc m3u8mcctl status


3. 配置文件详解

配置文件采用 OpenWrt UCI 格式，分为 global 和 channel 两类 section。

3.1 global 配置

参数默认值说明enabled1全局开关，0 时 daemon 启动但不拉起任何频道log_levelwarning日志级别：error / warning / info / debug / tracecontrol_socket/var/run/m3u8mcd.sockUnix Domain Socket 路径，m3u8mcctl 通过此路径与 daemon 通信http_port_base18080内置 HTTP 服务端口起始值，各频道依次递增自动分配ffmpeg_bin（从 PATH 查找）指定 ffmpeg 可执行文件的完整路径，留空则使用系统 PATH 中的 ffmpeg

3.2 channel 配置

必填参数

参数默认值说明url（list）—HLS 源地址，可配置多个（仅 use_downloader=1 时允许多源）addr—组播目标 IP 地址，如 239.1.1.1port—组播目标端口，如 5000

网络参数

参数默认值说明ifacelan发送组播包的网络接口名，容器中通常为 eth0ttl4组播 TTL 值pkt_size1316UDP 包大小（字节）protocolrtp输出协议：rtp 或 udp

缓存与性能参数

参数默认值说明cache_window3缓存时间窗口（以 target_duration 为单位）max_segments5内存中最多保留的 TS 分片数量hls_segments3内置 HTTP playlist 中包含的分片数量buffer_size1048576下载缓冲区大小（字节，默认 1MB）threads4下载并发线程数

内置 HTTP 服务参数

参数默认值说明use_downloader11=标准模式（缓存+HTTP服务），0=直通模式（直接透传给 ffmpeg）http_port自动分配内置 HTTP 服务端口，不填则从 http_port_base 自动递增分配http_path/liveHTTP 路径前缀，播放地址为 http://host:port/live/playlist.m3u8

赛马模式（race_mode）

赛马模式适用于源站有多个 IP 但共用一个域名的场景，对所有 IP 同时发起请求，取最快响应的数据。

参数默认值说明race_mode0启用赛马模式race_ip（list）—候选 IP，格式为 1.2.3.4 或 1.2.3.4@host.example.comrace_host（自动推断）全局默认赛马 Host，优先级低于 race_ip 中的 @host

代理与 HTTP 头参数

参数默认值说明list_proxy（空）拉取 m3u8 列表时使用的代理，格式 http://host:portdownload_proxy（空）下载 TS 分片时使用的代理http_header（list）—通用 HTTP 请求头，格式：Key: Valuepoller_http_header（list）—仅用于拉列表的 HTTP 头，未配置时继承 http_headerdownloader_http_header（list）—仅用于拉分片的 HTTP 头，未配置时继承 http_headerlist_url（list）—独立的列表拉取地址，未配置时使用 urllist_poll_interval0列表刷新间隔（秒），0 表示跟随 m3u8 中的 target_duration

ffmpeg 参数透传

参数默认值说明ffmpeg_input_arg（list）—追加到 ffmpeg -i 之前的输入参数ffmpeg_output_arg（list）—追加到 ffmpeg 输出部分的参数


4. 完整配置示例

4.1 标准模式（多频道）

config global
    option enabled        1
    option log_level      info
    option http_port_base 18080

config channel 'cctv1'
    option enabled        1
    option name           'CCTV-1'
    list   url            'http://source1.example.com/cctv1/playlist.m3u8'
    list   url            'http://source2.example.com/cctv1/playlist.m3u8'
    option addr           '239.1.1.1'
    option port           5000
    option iface          'eth0'
    option cache_window   3
    option max_segments   5

config channel 'cctv5'
    option enabled        1
    option name           'CCTV-5'
    list   url            'http://source1.example.com/cctv5/playlist.m3u8'
    option addr           '239.1.1.2'
    option port           5000
    option iface          'eth0'
    list   http_header    'User-Agent: Mozilla/5.0'
    list   http_header    'Referer: http://example.com'

4.2 赛马模式

config channel 'iptv_race'
    option enabled    1
    option name       '赛马频道'
    list   url        'http://cdn.example.com/live/playlist.m3u8'
    option addr       '239.1.1.10'
    option port       5000
    option iface      'eth0'
    option race_mode  1
    list   race_ip    '1.2.3.4'
    list   race_ip    '5.6.7.8@mirror2.cdn.com'
    option race_host  'cdn.example.com'

4.3 直通模式（passthrough）

config channel 'direct'
    option enabled        1
    option name           '直通频道'
    option use_downloader 0
    list   url            'http://source.example.com/live/playlist.m3u8'
    option addr           '239.1.1.20'
    option port           5000
    option iface          'eth0'
    list   ffmpeg_input_arg  '-timeout'
    list   ffmpeg_input_arg  '5000000'


5. m3u8mcctl 控制命令

查看状态

bash# 人类可读格式
docker exec m3u8mc m3u8mcctl status

# JSON 格式（适合脚本解析）
docker exec m3u8mc m3u8mcctl status --json

状态字段说明：

字段说明CHANNEL频道 ID（section 名称）ENABLED配置中是否启用RUNNING当前是否正在运行MODEstandard（标准）或 passthrough（直通）CACHE当前内存中缓存的 TS 分片数量RESTARTSffmpeg 进程累计重启次数OVERLOAD是否处于过载状态（下载速度跟不上播放速度）PLAY_URL内置 HTTP 服务的 HLS 播放地址

热重载配置

bash# 修改宿主机配置文件后，无需重启容器
docker exec m3u8mc m3u8mcctl reload


reload 只对有变更的频道做增量处理，未变更的频道不受影响，播放不中断。



频道操作

bash# 启动指定频道
docker exec m3u8mc m3u8mcctl channel start --name cctv1

# 停止指定频道
docker exec m3u8mc m3u8mcctl channel stop --name cctv1

# 重启指定频道
docker exec m3u8mc m3u8mcctl channel restart --name cctv1

# 停止所有频道（daemon 进程保持存活）
docker exec m3u8mc m3u8mcctl stop-all


6. 内置 HTTP 服务（HLS 回放）

每个 use_downloader=1 的频道都会启动一个独立的 HTTP 服务，提供以下接口：

接口路径说明HLS 播放列表GET /live/playlist.m3u8包含最近 hls_segments 个分片TS 分片GET /live/seg/:seq按序号获取单个 TS 分片连续字节流GET /live/stream.ts持续推送 TS 字节流，适合 VLC 等播放器直接拉流

播放示例（假设 http_port_base=18080，第一个频道自动分配 18080 端口）：

bash# HLS 播放（VLC / ffplay / mpv）
ffplay http://宿主机IP:18080/live/playlist.m3u8

# TS 字节流
ffplay http://宿主机IP:18080/live/stream.ts

# 第二个频道端口自动为 18081
ffplay http://宿主机IP:18081/live/playlist.m3u8


7. docker-compose 部署

yamlservices:
  m3u8mc:
    image: ghcr.io/mylucien/m3u8mc:latest
    container_name: m3u8mc
    restart: unless-stopped
    network_mode: host
    volumes:
      - /etc/m3u8mc:/etc/config
    command: ["--config", "/etc/config/m3u8mc"]
    logging:
      driver: json-file
      options:
        max-size: "10m"
        max-file: "3"


8. 常见问题

组播包发出去了但收不到


确认使用了 --network host，bridge 模式无法正确处理组播
确认 iface 配置的网卡名与宿主机实际网卡名一致（容器内是 eth0，宿主机可能是 ens3 等）
确认路由器/交换机开启了 IGMP Snooping


m3u8mcctl: daemon not running


control_socket 默认为 /var/run/m3u8mcd.sock，确认容器内外路径一致
确认 daemon 进程正在运行：docker exec m3u8mc ps aux | grep m3u8mcd


频道 RUNNING=false，RESTARTS 不断增加


查看日志：docker logs m3u8mc，找 ffmpeg 报错信息
确认源地址可访问：docker exec m3u8mc wget -q -O- 'http://your-source/playlist.m3u8' | head
如需代理，配置 list_proxy 和 download_proxy


日志输出太多


将 log_level 改为 warning 或 error
配置 docker logging driver 限制日志文件大小（见 docker-compose 示例）
