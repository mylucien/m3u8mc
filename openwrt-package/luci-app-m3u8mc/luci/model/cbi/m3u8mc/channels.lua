-- LuCI CBI model: m3u8mc 频道管理
-- 对应设计文档 2.2 节全部 channel 配置项

local m = Map("m3u8mc", translate("m3u8mc 频道管理"),
    translate("配置 m3u8 转组播频道列表。保存&应用后执行热加载，未变更频道不受影响。"))

-- ── 全局配置 ────────────────────────────────────────────────
local s_global = m:section(NamedSection, "global", "global", translate("全局设置"))
s_global.addremove = false

s_global:option(Flag,  "enabled",       translate("启用插件"))
s_global:option(Value, "log_level",     translate("日志级别"))
             :value("error",   "error")
             :value("warning", "warning (默认)")
             :value("info",    "info")
             :value("debug",   "debug (详细)")
s_global:option(Value, "http_port_base", translate("内部 HTTP 起始端口"),
    translate("各频道按此递增自动分配，范围 1024–65000"))
s_global:option(Value, "control_socket", translate("控制 Socket 路径"))

-- ffmpeg 下载地址（按架构）
local ffmpeg_s = m:section(NamedSection, "global", "global", translate("FFmpeg 下载地址"))
ffmpeg_s.addremove = false
ffmpeg_s:option(Value, "ffmpeg_download_url_x86_64",  translate("x86_64 下载地址"))
         :placeholder("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz")
ffmpeg_s:option(Value, "ffmpeg_download_url_aarch64", translate("aarch64 下载地址"))
         :placeholder("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz")
ffmpeg_s:option(Value, "ffmpeg_download_url_armv7l",  translate("armv7l 下载地址"))
         :placeholder("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-armhf-static.tar.xz")
ffmpeg_s:option(Value, "ffmpeg_download_url_i686",    translate("i686 下载地址"))
         :placeholder("https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-i686-static.tar.xz")

-- ── 频道列表 ────────────────────────────────────────────────
local s = m:section(TypedSection, "channel", translate("频道列表"))
s.template = "cbi/tblsection"
s.addremove = true
s.anonymous = false   -- 允许用户指定 section name 作为频道 ID

-- 基本
s:option(Flag,  "enabled", translate("启用"))
s:option(Value, "name",    translate("显示名称"))

-- 源地址
local o_urls = s:option(DynamicList, "url", translate("m3u8 地址"),
    translate("多个地址为镜像服务器，可启用轮询或赛马模式选取最快源"))
o_urls.placeholder = "http://mirror.example.com/live/playlist.m3u8"

-- 组播输出
local proto_opt = s:option(ListValue, "protocol", translate("推流协议"))
proto_opt:value("udp", "UDP")
proto_opt:value("rtp", "RTP")

s:option(Value, "addr", translate("组播地址"),
    translate("224.0.0.0 – 239.255.255.255"))
s:option(Value, "port", translate("组播端口"))
s:option(Value, "iface", translate("出口接口"),
    translate("OpenWrt 接口名（如 lan、br-lan）或直接填本机 IP"))
s:option(Value, "ttl",  translate("TTL"), translate("默认 4"))

-- 下载/转发模式
local use_dl = s:option(Flag, "use_downloader", translate("标准模式"),
    translate("开启：拉取→缓存→内部 HTTP→ffmpeg；关闭：直通（ffmpeg 直接拉上游，仅支持单源）"))
use_dl.default = "1"

local race = s:option(Flag, "race_mode", translate("赛马模式"),
    translate("对所有镜像服务器并发下载，取最先完成者，自动跟随最快节点"))
race:depends("use_downloader", "1")

local race_ips = s:option(DynamicList, "race_ip", translate("赛马候选 IP"),
    translate("可选：补充填写边缘节点 IP（格式 x.x.x.x 或 http://x.x.x.x），配合 Host 头绕过 DNS"))
race_ips:depends("race_mode", "1")

-- 调优
local adv = s:option(Value, "threads", translate("分块下载并发数"),
    translate("仅在服务器支持 Range 请求且 use_downloader=1 时有效，默认 4"))
adv:depends("use_downloader", "1")

s:option(Value, "cache_window",  translate("缓存时间窗口倍数"),
    translate("缓存时长 = 分片时长 × 该值，默认 3")):depends("use_downloader", "1")

s:option(Value, "max_segments",  translate("缓存分片数上限"),
    translate("超出时按时间淘汰最旧条目，默认 5")):depends("use_downloader", "1")

s:option(Value, "hls_segments",  translate("内部 m3u8 滑动窗口"),
    translate("内部播放列表保留最近 N 个分片，供 ffmpeg 拉取，默认 3")):depends("use_downloader", "1")

s:option(Value, "pkt_size",      translate("UDP 包大小"), translate("默认 1316 字节，188×7"))

-- ffmpeg 高级参数（多值），LuCI 展示为可增删的输入列表
local fi = s:option(DynamicList, "ffmpeg_input_arg",  translate("ffmpeg 输入参数"),
    translate("追加在 -i 之前，如 -timeout 5000000"))
local fo = s:option(DynamicList, "ffmpeg_output_arg", translate("ffmpeg 输出参数"),
    translate("覆盖默认输出参数组合（含 -f mpegts 之前的全部参数）；留空则使用内置默认值"))

-- 重载钩子：保存后触发 reload 而非完整重启
m.on_after_commit = function(self)
    local sys  = require "luci.sys"
    local uci2 = require "luci.model.uci".cursor()
    local sock = uci2:get("m3u8mc", "global", "control_socket")
                 or "/var/run/m3u8mcd.sock"
    -- 先尝试热重载；若 daemon 未运行（exit ≠ 0），触发服务重启
    local _, code = sys.exec(
        string.format("/usr/bin/m3u8mcctl --socket %q reload 2>/dev/null", sock)
    )
    if code ~= 0 then
        sys.call("/etc/init.d/m3u8mc restart")
    end
end

return m
