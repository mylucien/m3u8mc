# m3u8mc Docker 容器部署与使用手册

适用镜像：`ghcr.io/mylucien/m3u8mc:latest`

---

## 1. 项目简介

m3u8mc 是一个将 HLS（m3u8）直播源转换为组播/UDP 流的守护进程，主要用于 OpenWrt 路由器或 Linux 服务器场景。容器镜像内置 ffmpeg，无需在宿主机单独安装，开箱即用。

核心组件：

- **m3u8mcd**：主守护进程，负责拉取 m3u8 列表、缓存 TS 分片、通过 ffmpeg 转推组播/UDP，同时提供内置 HTTP 服务供 HLS 回放
- **m3u8mcctl**：命令行控制工具，通过 Unix Socket 与 daemon 通信，用于查看状态、热重载配置、启停频道

---

## 2. 快速开始

### 2.1 准备配置文件

在宿主机创建配置目录和配置文件：

```bash
mkdir -p /etc/m3u8mc
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
```

### 2.2 启动容器

```bash
docker run -d \
  --name m3u8mc \
  --restart unless-stopped \
  --network host \
  -v /etc/m3u8mc:/etc/config \
  ghcr.io/mylucien/m3u8mc:latest \
  --config /etc/config/m3u8mc
```

> **注意**：`--network host` 是必须的，组播/UDP 发包需要直接访问宿主机网络接口，bridge 网络无法正确绑定组播地址。

### 2.3 验证运行状态

```bash
# 查看日志
docker logs -f m3u8mc

# 查看频道状态
docker exec m3u8mc m3u8mcctl status
```

---

## 3. 配置文件详解

配置文件采用 OpenWrt UCI 格式，分为 `global` 和 `channel` 两类 section。

### 3.1 global 配置

| 参数 | 默认值 | 说明 |
|---|---|---|
| `enabled` | `1` | 全局开关，`0` 时 daemon 启动但不拉起任何频道 |
| `log_level` | `warning` | 日志级别：`error` / `warning` / `info` / `debug` / `trace` |
| `control_socket` | `/var/run/m3u8mcd.sock` | Unix Domain Socket 路径，m3u8mcctl 通过此路径与 daemon 通信 |
| `http_port_base` | `18080` | 内置 HTTP 服务端口起始值，各频道依次递增自动分配 |
| `ffmpeg_bin` | （从 PATH 查找） | 指定 ffmpeg 可执行文件的完整路径，留空则使用系统 PATH 中的 ffmpeg |

### 3.2 channel 配置

#### 必填参数

| 参数 | 默认值 | 说明 |
|---|---|---|
| `url`（list） | — | HLS 源地址，可配置多个（仅 `use_downloader=1` 时允许多源） |
| `addr` | — | 组播目标 IP 地址，如 `239.1.1.1` |
| `port` | — | 组播目标端口，如 `5000` |

#### 网络参数

| 参数 | 默认值 | 说明 |
|---|---|---|
| `iface` | `lan` | 发送组播包的网络接口名，容器中通常为 `eth0` |
| `ttl` | `4` | 组播 TTL 值 |
| `pkt_size` | `1316` | UDP 包大小（字节） |
| `protocol` | `rtp` | 输出协议：`rtp` 或 `udp` |

#### 缓存与性能参数

| 参数 | 默认值 | 说明 |
|---|---|---|
| `cache_window` | `3` | 缓存时间窗口（以 target_duration 为单位） |
| `max_segments` | `5` | 内存中最多保留的 TS 分片数量 |
| `hls_segments` | `3` | 内置 HTTP playlist 中包含的分片数量 |
| `buffer_size` | `1048576` | 下载缓冲区大小（字节，默认 1MB） |
| `threads` | `4` | 下载并发线程数 |

#### 内置 HTTP 服务参数

| 参数 | 默认值 | 说明 |
|---|---|---|
| `use_downloader` | `1` | `1`=标准模式（缓存+HTTP服务），`0`=直通模式（直接透传给 ffmpeg） |
| `http_port` | 自动分配 | 内置 HTTP 服务端口，不填则从 `http_port_base` 自动递增分配 |
| `http_path` | `/live` | HTTP 路径前缀，播放地址为 `http://host:port/live/playlist.m3u8` |

#### 赛马模式（race_mode）

赛马模式适用于源站有多个 IP 但共用一个域名的场景，对所有 IP 同时发起请求，取最快响应的数据。

| 参数 | 默认值 | 说明 |
|---|---|---|
| `race_mode` | `0` | 启用赛马模式 |
| `race_ip`（list） | — | 候选 IP，格式为 `1.2.3.4` 或 `1.2.3.4@host.example.com` |
| `race_host` | （自动推断） | 全局默认赛马 Host，优先级低于 race_ip 中的 `@host` |

#### 代理与 HTTP 头参数

| 参数 | 默认值 | 说明 |
|---|---|---|
| `list_proxy` | （空） | 拉取 m3u8 列表时使用的代理，格式 `http://host:port` |
| `download_proxy` | （空） | 下载 TS 分片时使用的代理 |
| `http_header`（list） | — | 通用 HTTP 请求头，格式：`Key: Value` |
| `poller_http_header`（list） | — | 仅用于拉列表的 HTTP 头，未配置时继承 `http_header` |
| `downloader_http_header`（list） | — | 仅用于拉分片的 HTTP 头，未配置时继承 `http_header` |
| `list_url`（list） | — | 独立的列表拉取地址，未配置时使用 `url` |
| `list_poll_interval` | `0` | 列表刷新间隔（秒），`0` 表示跟随 m3u8 中的 target_duration |

#### ffmpeg 参数透传

| 参数 | 默认值 | 说明 |
|---|---|---|
| `ffmpeg_input_arg`（list） | — | 追加到 ffmpeg `-i` 之前的输入参数 |
| `ffmpeg_output_arg`（list） | — | 追加到 ffmpeg 输出部分的参数 |

---

## 4. 完整配置示例

### 4.1 标准模式（多频道）

```
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
```

### 4.2 赛马模式

```
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
```

### 4.3 直通模式（passthrough）

```
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
```

---

## 5. m3u8mcctl 控制命令

### 查看状态

```bash
# 人类可读格式
docker exec m3u8mc m3u8mcctl status

# JSON 格式（适合脚本解析）
docker exec m3u8mc m3u8mcctl status --json
```

状态字段说明：

| 字段 | 说明 |
|---|---|
| `CHANNEL` | 频道 ID（section 名称） |
| `ENABLED` | 配置中是否启用 |
| `RUNNING` | 当前是否正在运行 |
| `MODE` | `standard`（标准）或 `passthrough`（直通） |
| `CACHE` | 当前内存中缓存的 TS 分片数量 |
| `RESTARTS` | ffmpeg 进程累计重启次数 |
| `OVERLOAD` | 是否处于过载状态（下载速度跟不上播放速度） |
| `PLAY_URL` | 内置 HTTP 服务的 HLS 播放地址 |

### 热重载配置

```bash
# 修改宿主机配置文件后，无需重启容器
docker exec m3u8mc m3u8mcctl reload
```

> reload 只对有变更的频道做增量处理，未变更的频道不受影响，播放不中断。

### 频道操作

```bash
# 启动指定频道
docker exec m3u8mc m3u8mcctl channel start --name cctv1

# 停止指定频道
docker exec m3u8mc m3u8mcctl channel stop --name cctv1

# 重启指定频道
docker exec m3u8mc m3u8mcctl channel restart --name cctv1

# 停止所有频道（daemon 进程保持存活）
docker exec m3u8mc m3u8mcctl stop-all
```

---

## 6. 内置 HTTP 服务（HLS 回放）

每个 `use_downloader=1` 的频道都会启动一个独立的 HTTP 服务，提供以下接口：

| 接口 | 路径 | 说明 |
|---|---|---|
| HLS 播放列表 | `GET /live/playlist.m3u8` | 包含最近 `hls_segments` 个分片 |
| TS 分片 | `GET /live/seg/:seq` | 按序号获取单个 TS 分片 |
| 连续字节流 | `GET /live/stream.ts` | 持续推送 TS 字节流，适合 VLC 等播放器直接拉流 |

播放示例（假设 `http_port_base=18080`，第一个频道自动分配 18080 端口）：

```bash
# HLS 播放（VLC / ffplay / mpv）
ffplay http://宿主机IP:18080/live/playlist.m3u8

# TS 字节流
ffplay http://宿主机IP:18080/live/stream.ts

# 第二个频道端口自动为 18081
ffplay http://宿主机IP:18081/live/playlist.m3u8
```

---

## 7. docker-compose 部署

```yaml
services:
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
```

---

# `list_url` / `url` / 赛马模式配置指南

本文档说明频道配置里 `list_url`、`url`、`race_mode`、`race_ip`、`race_host` 这几个参数的职责与组合行为，适用于标准模式（`use_downloader=1`）下的频道配置。

## 一、基本概念

| 参数 | 职责 |
|---|---|
| `list_url` | 只负责拉取播放列表（master/media playlist），周期性轮询获取分片列表。**不参与分片下载**。 |
| `url` | 分片下载的 base 地址来源。标准模式下用于拼接/覆盖分片下载地址；赛马模式下用于在没有配置 `race_ip` 时提供默认竞速候选。 |
| `race_mode` | 是否开启赛马模式：`0` 为标准轮询模式，`1` 为赛马模式（多候选并发请求，取最快返回）。 |
| `race_ip` | 赛马模式下的候选连接目标，可配置多条，格式为 `地址` 或 `地址@host`。 |
| `race_host` | 赛马模式下全局默认的 HTTP `Host` 请求头覆盖值。 |

`list_url` 与 `url` 都可以配置多条（多镜像/多备份源）。

## 二、`list_url` 与 `url` 的组合规则

- **只配 `list_url`，不配 `url`**：`list_url` 承担"拉列表"的职责。是否需要额外配置 `url` 取决于分片地址形式：
  - 播放列表里的分片是**绝对地址**（如 `https://cdn6.cc.cd/api/amty/2283368`）：不需要配 `url`，直接使用分片自带的完整地址下载。
  - 播放列表里的分片是**相对路径**（如 `seg100.ts`）：**必须**配置 `url` 提供下载 base，否则下载会报错（无法拼出完整下载地址）。
- **只配 `url`，不配 `list_url`**：`url` 同时承担"拉列表"和"下载 base"两个职责，等价于把同一个地址填两遍。
- **`list_url` 与 `url` 都配置**：`list_url` 专职拉列表，`url` 专职作下载 base。此时无论分片是相对路径还是绝对地址都能正常下载：
  - 相对路径：`url` 作为 join 的 base，分片路径拼接在其后。
  - 绝对地址：`url` 的域名/协议/端口会**覆盖**分片自带的地址（分片的 path/query 保留不变）。

### 组合速查表（标准模式）

| 配置 | 分片相对路径 | 分片绝对地址 |
|---|---|---|
| 只有 `list_url` | ❌ 报错（无下载 base） | ✅ 直接用分片自带地址下载 |
| 只有 `url` | ✅（`url` 同时充当列表源和下载 base） | ✅（同上，url 覆盖生效） |
| `list_url` + `url` | ✅ 用 `url` 拼接 | ✅ `url` 覆盖分片自带地址 |

> 注意：`url` 若配置为覆盖绝对地址分片，只能是 `scheme://host[:port]` 或 `host[:port]` 形式（不能带路径）。如果 `url` 里带了路径（比如 `https://cdn.com/live`），这条 `url` 只能用于相对路径场景的 join，用来覆盖绝对地址分片时会被跳过并记录警告日志，请为绝对地址场景单独配置不带路径的 `url`。

## 三、赛马模式（`race_mode=1`）

赛马模式下，同一个分片会**同时向多个候选发起请求，取最快返回的结果**，用于绕开 DNS/CDN 调度、强制连接到指定地址。候选来源和优先级如下：

### 3.1 候选来源优先级：`race_ip` 优先于 `url`

- **配置了 `race_ip`（一条或多条）**：只用 `race_ip` 候选竞速，`url` 完全不参与（即使配置了 `list_url`+`url`，`url` 也会被忽略）。
- **没有配置 `race_ip`**：退化为用 `url`（即 `list_url`+`url` 组合里的 `url`，或只配了 `url` 时的那个值）作为候选竞速，此时行为接近标准模式的多镜像轮询，只是改为并发请求。
- **`race_ip` 和 `url` 都没配置**（只有 `list_url`）：赛马模式没有任何候选可用，下载会报错。

### 3.2 `race_ip` 的写法

```
race_ip = 地址
race_ip = 地址@host
```

- **地址**部分（`@` 前面，或整条无 `@`）：赛马候选的实际连接目标，支持：
  - 纯 IP：`1.2.3.4`、`1.2.3.4:8443`
  - 域名：`cdn.example.com`、`cdn.example.com:522`
  - 带协议前缀：`https://1.2.3.4:8443`、`https://cdn.example.com:522`
  - 不带协议前缀时，默认使用 `http`。
- **`@host` 部分**（可选）：这条候选专属的 HTTP `Host` 请求头覆盖值，**不允许带协议**（只能是 `host[:port]`，带了协议会被忽略并记录警告日志）。

可以配置多条 `race_ip`，每条都会作为独立候选参与竞速。

### 3.3 Host 请求头的确定规则

赛马模式下，每个 `race_ip` 候选实际发送请求时使用的 `Host` 请求头，按以下优先级确定：

1. 该候选自带的 `@host`（最高优先级）；
2. 全局配置的 `race_host`（`@host` 未指定时使用）；
3. 都没有配置：**不发送 Host 覆盖**，即视为纯 IP/域名直连请求。

`race_host` 同样**不允许带协议**，只能是 `host[:port]` 形式，带协议的配置会被忽略并记录警告日志。

### 3.4 没有配置 `race_ip`、只有 `url` 时，`race_host` 的作用

这种情况下，`race_host` 会**替换掉 `url` 候选的域名**（协议和端口沿用 `url` 自身，除非 `race_host` 自带端口）：

- `race_host` 不带端口：只换域名，端口沿用 `url` 原有端口（或该协议的默认端口）；
- `race_host` 带端口（如 `mirror2.cdn.com:9999`）：域名和端口一起替换。

### 3.5 组合速查表（赛马模式）

| `race_ip` | `race_host` | 相对路径分片 | 绝对路径分片 |
|---|---|---|---|
| 未配置 | 未配置 | 用 `url` 候选，Host 头不设 | 用 `url` 候选覆盖分片地址，Host 头不设 |
| 未配置 | 已配置 | 用 `url` 候选，但域名被 `race_host` 替换 | 同左 |
| 已配置 | 未配置 | 只用 race_ip 候选，Host 头不设（除非 `@host` 指定） | 同左 |
| 已配置 | 已配置 | 只用 race_ip 候选，Host 头取 `@host`（无则取 `race_host`） | 同左 |
| 未配置 | 未配置，且 `url` 也未配置 | ❌ 报错（无候选） | ❌ 报错（无候选） |

## 四、常见配置示例

### 示例 1：CDN 分片给的是绝对地址，用赛马模式直连指定 IP

```
list_url = https://real-cdn.example.com/live/master.m3u8
race_mode = 1
race_ip = 1.2.3.4@real-cdn.example.com
race_ip = 5.6.7.8@real-cdn.example.com
```

说明：`list_url` 只用来拉取播放列表；不需要配置 `url`（因为分片本身就是绝对地址）；两个 `race_ip` 会并发连接 `1.2.3.4` 和 `5.6.7.8`，同时都带上 `Host: real-cdn.example.com` 请求头，绕开 DNS 解析和 CDN 调度。

### 示例 2：分片是相对路径，标准模式，多镜像轮询

```
list_url = https://origin.example.com/live/master.m3u8
url = https://mirror1.example.com/live
url = https://mirror2.example.com/live
```

说明：`list_url` 拉列表；实际下载会在 `mirror1`/`mirror2` 之间轮询，用 `url` 的值作为 base 和分片相对路径拼接。

### 示例 3：分片是绝对地址，标准模式，希望强制走自己的中转域名

```
list_url = https://origin.example.com/live/master.m3u8
url = https://relay.example.com
```

说明：`url` 不带路径，播放列表里分片自带的绝对地址会被 `relay.example.com` 覆盖域名（协议/端口也一并覆盖），path/query 保留分片原有的部分。

### 示例 4：只有一个地址，最简配置

```
url = https://origin.example.com/live/master.m3u8
```

说明：不配 `list_url` 时，`url` 自动兼职列表源，一份配置两用。

## 五、排错提示

- 频道日志中出现 **"no download base configured for relative-path segment"**：分片是相对路径，但没有配置 `url`，请补充 `url`。
- 频道日志中出现 **"skip url candidate: cannot override absolute segment host with a base that has a path"**：`url` 配置里带了路径，无法用于覆盖绝对地址分片，请为该场景单独配置不带路径的 `url`。
- 频道日志中出现 **"must not include a protocol (scheme)"**：`race_host` 或 `race_ip` 的 `@host` 部分带了 `http://`/`https://` 前缀，这两处都只允许 `host[:port]`，协议前缀只能写在 `race_ip` 的地址部分。
- 赛马模式下候选完全不生效、日志无报错：检查是否同时配置了 `race_ip`；只要 `race_ip` 非空，`url` 就不会参与竞速。
