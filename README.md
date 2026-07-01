# m3u8mc — m3u8 转组播 OpenWrt 插件

将 HTTP 单播 m3u8 直播源（支持多镜像）拉流、缓存，转换为 UDP/RTP 组播推送至局域网设备。

## 架构

```
上游 m3u8 源 (多镜像)
      │
      ▼
  Poller (轮询检测新分片)
      │
      ▼
Downloader Pool
  ├─ RoundRobin 标准模式 (HEAD探测→Range分块→失败回退整体，跨镜像换台)
  └─ Race 赛马模式 (多镜像并发竞速取最快)
      │
      ▼
  SegmentCache (内存分片缓存，双重淘汰策略)
      │
      ├──► InternalHttpServer (HLS + TS 字节流，对 ffmpeg 内部可见)
      │         │
      │         ▼
      │     FfmpegSupervisor (监控/重启 ffmpeg)
      │         │
      │         ▼
      │    UDP/RTP 组播 (239.x.x.x)
      │
      └──► [直通模式] ffmpeg 直接拉上游 m3u8

控制面：Unix Domain Socket ← m3u8mcctl / LuCI / init 脚本
```

## 目录结构

```
m3u8mc/
├── Cargo.toml                    # workspace 根
├── build.sh                      # 交叉编译脚本
├── crates/
│   ├── common/                   # 共享库：UCI 解析、类型化配置、控制协议
│   │   └── src/
│   │       ├── uci.rs            # 轻量 UCI 文本解析器（无 libuci 依赖）
│   │       ├── config.rs         # 强类型配置结构 + 从 UCI 翻译 + diff
│   │       └── protocol.rs       # daemon ↔ ctl 控制协议（ndjson over UDS）
│   │
│   ├── m3u8mcd/                  # 核心 daemon
│   │   └── src/
│   │       ├── main.rs           # 入口：加载配置、启动子系统、SIGTERM
│   │       ├── manager.rs        # ChannelManager：增删/重启/热重载
│   │       ├── control.rs        # UDS 控制接口服务端
│   │       └── channel/
│   │           ├── mod.rs        # ChannelHandle：标准/直通模式任务编排
│   │           ├── mirror_pool.rs# MirrorPool：镜像健康度+指数退避
│   │           ├── cache.rs      # SegmentCache：内存分片缓存+广播
│   │           ├── downloader.rs # Downloader：RoundRobin+Race 下载策略
│   │           ├── poller.rs     # Poller：media playlist 轮询+去重
│   │           ├── m3u8.rs       # 极简 m3u8 解析器
│   │           ├── http_server.rs# InternalHttpServer：HLS+TS字节流
│   │           ├── ffmpeg_supervisor.rs # ffmpeg 监控/重启
│   │           └── iface.rs      # 接口名→IP 解析（ubus/ip/直接IP）
│   │
│   └── m3u8mcctl/                # CLI 控制工具
│       └── src/main.rs           # status/reload/channel start|stop|restart
│
├── openwrt-package/              # OpenWrt 打包骨架
│   ├── Makefile                  # ipk 打包规则（预编译二进制安装模式）
│   ├── files/
│   │   ├── m3u8mc.init           # procd init 脚本
│   │   ├── m3u8mc.config         # UCI 默认配置（含注释示例）
│   │   └── network-isolation.sh  # VLAN+防火墙隔离脚本
│   └── luci/
│       ├── controller/m3u8mc.lua # 菜单路由 + Ajax API
│       ├── model/cbi/m3u8mc/
│       │   ├── channels.lua      # 频道管理 CBI 表单
│       │   └── isolation.lua     # 网络隔离 CBI 表单
│       ├── view/m3u8mc/
│       │   ├── status.htm        # 运行状态页模板
│       │   └── ffmpeg.htm        # FFmpeg 管理页模板（下载/检测/恢复默认）
│       └── htdocs/
│           └── m3u8mc-status.js  # 状态页前端（纯 ES5，无依赖）
│
└── smoketest/                    # 本机冒烟测试辅助文件
```

## 快速开始（本机验证）

```bash
# 1. 编译
cargo build -p m3u8mcd -p m3u8mcctl

# 2. 起一个模拟 m3u8 源（任意目录下有 playlist.m3u8 + seg*.ts）
cd smoketest/fixtures && python3 -m http.server 19999 &

# 3. 启动 daemon
./target/debug/m3u8mcd --config smoketest/m3u8mc.conf &

# 4. 查看状态
./target/debug/m3u8mcctl --socket smoketest/m3u8mcd.sock status

# 5. 热重载（修改 smoketest/m3u8mc.conf 后）
./target/debug/m3u8mcctl --socket smoketest/m3u8mcd.sock reload
```

## 交叉编译（上路由器）

```bash
cargo install cargo-zigbuild
pip install ziglang

# AArch64（IPQ807x 等）
./build.sh aarch64-unknown-linux-musl

# MIPSEL
./build.sh mipsel-unknown-linux-musl
```

产物在 `bin/<target-triple>/`，静态链接 musl libc，无运行时依赖。

## OpenWrt 打包

```bash
# 在 OpenWrt SDK 的 package/ 下：
RUST_TARGET_TRIPLE=aarch64-unknown-linux-musl \
  make package/m3u8mc/compile V=s
```

## UCI 配置快速参考

```uci
config global
    option enabled      '1'
    option log_level    'warning'

config channel 'cctv5'
    option enabled      '1'
    option name         'CCTV-5 体育'
    list   url          'http://mirror1.example.com/cctv5/playlist.m3u8'
    list   url          'http://mirror2.example.com/cctv5/playlist.m3u8'
    option protocol     'udp'
    option addr         '239.1.1.1'
    option port         '5001'
    option iface        'lan'
    option use_downloader '1'
    option race_mode    '0'
```

## FFmpeg 管理（LuCI）

OpenWrt 官方 ffmpeg 包通常编译精简（可能缺 h264 比特流过滤器、部分 muxer），
「服务 → m3u8mc 转播 → FFmpeg 管理」页面提供：

- **能力检测**：当前 ffmpeg 路径/版本/关键能力（mpegts muxer / rtp muxer /
  h264_mp4toannexb bsf / hls demuxer）一目了然
- **一键下载**：按 CPU 架构自动填入推荐的静态编译版本下载地址（x86_64/aarch64/armv7/i686
  来自 johnvansickle.com 的长期维护构建；MIPS 系架构暂无公开静态源，需手动填写）
- **自定义地址**：支持任意 URL，兼容 tar.xz/tar.gz/tar.bz2 压缩包或裸二进制直链
- **后台异步执行**：下载解压走后台进程+进度文件轮询，不阻塞 LuCI 请求，避免路由器慢速网络下超时
- **自动生效**：安装成功后自动写入 `m3u8mc.global.ffmpeg_bin` 并热重载，无需手动重启
- **一键恢复**：随时可清空自定义路径，回退到系统 PATH 中的 ffmpeg

对应后端脚本：`openwrt-package/files/ffmpeg-install.sh`（check / download / set-uci / reset-uci 四个子命令）。

## 已知限制 / 后续可做

- 网络隔离脚本目前只做单 VLAN，未覆盖多频道各自独立 VLAN 的场景
- MIPS 架构没有默认下载源，需用户自行编译并提供直链
