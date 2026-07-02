-- LuCI CBI model: m3u8mc 频道管理
-- 对应设计文档 2.2 节全部 channel 配置项

local m = Map("m3u8mc", translate("m3u8mc 频道管理"),
    translate("配置 m3u8 转组播频道列表。保存&应用后执行热加载，未变更频道不受影响。"))

-- ── 全局配置 ────────────────────────────────────────────────
local s_global = m:section(NamedSection, "global", "global", translate("全局设置"))
s_global.addremove = false

s_global:option(Flag,  "enabled",       translate("启用插件"))
local log_level = s_global:option(Value, "log_level",     translate("日志级别"))
log_level:value("error",   "error")
log_level:value("warning", "warning (默认)")
log_level:value("info",    "info")
log_level:value("debug",   "debug (详细)")
s_global:option(Value, "http_port_base", translate("内部 HTTP 起始端口"),
    translate("各频道按此递增自动分配，范围 1024–65000"))
s_global:option(Value, "control_socket", translate("控制 Socket 路径"))

-- ffmpeg 下载地址（按架构）
local ffmpeg_s = m:section(NamedSection, "global", "global", translate("FFmpeg 下载地址"))
ffmpeg_s.addremove = false
ffmpeg_s:option(Value, "ffmpeg_download_url_x86_64",  translate("x86_64 下载地址"))
ffmpeg_s:option(Value, "ffmpeg_download_url_aarch64", translate("aarch64 下载地址"))
ffmpeg_s:option(Value, "ffmpeg_download_url_armv7l",  translate("armv7l 下载地址"))
ffmpeg_s:option(Value, "ffmpeg_download_url_i686",    translate("i686 下载地址"))

-- ── 频道列表 ────────────────────────────────────────────────
local s = m:section(TypedSection, "channel", translate("频道列表"),
    translate("每个频道是一个配置块，展开后编辑全部参数。"))
s.addremove = true
s.anonymous = false

-- 基本
s:option(Flag,  "enabled", translate("启用"))
s:option(Value, "name",    translate("频道名称"))

-- 源地址
s:option(DynamicList, "url", translate("m3u8 地址"))

-- 组播输出
local proto_opt = s:option(ListValue, "protocol", translate("推流协议"))
proto_opt:value("udp", "UDP")
proto_opt:value("rtp", "RTP")
s:option(Value, "addr", translate("组播地址"))
s:option(Value, "port", translate("组播端口"))
s:option(Value, "iface", translate("出口接口"))
s:option(Value, "ttl",  translate("TTL"))

-- 下载/转发模式
local use_dl = s:option(Flag, "use_downloader", translate("标准模式（拉取→缓存→转发）"))
use_dl.default = "1"
local race = s:option(Flag, "race_mode", translate("赛马模式（多源并发取最快）"))
race:depends("use_downloader", "1")
local race_ips = s:option(DynamicList, "race_ip", translate("赛马候选 IP"))
race_ips:depends("race_mode", "1")

-- 调优
s:option(Value, "threads", translate("下载并发数")):depends("use_downloader", "1")
s:option(Value, "cache_window",  translate("缓存窗口倍数")):depends("use_downloader", "1")
s:option(Value, "max_segments",  translate("缓存分片上限")):depends("use_downloader", "1")
s:option(Value, "hls_segments",  translate("HLS 窗口大小")):depends("use_downloader", "1")
s:option(Value, "pkt_size",      translate("UDP 包大小"))

-- ffmpeg 高级参数
s:option(DynamicList, "ffmpeg_input_arg",  translate("ffmpeg 输入参数"))
s:option(DynamicList, "ffmpeg_output_arg", translate("ffmpeg 输出参数"))

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
