-- LuCI CBI model: m3u8mc 频道管理（概览列表）
local m = Map("m3u8mc", translate("m3u8mc 频道管理"),
    translate("配置 m3u8 转组播频道列表。保存&应用后执行热加载，未变更频道不受影响。"))

-- 全局设置
local s_global = m:section(NamedSection, "global", "global", translate("全局设置"))
s_global.addremove = false

s_global:option(Flag, "enabled", translate("启用插件"))

local log_opt = s_global:option(Value, "log_level", translate("日志级别"))
log_opt:value("error", "error")
log_opt:value("warning", "warning (默认)")
log_opt:value("info", "info")
log_opt:value("debug", "debug (详细)")

s_global:option(Value, "http_port_base", translate("内部 HTTP 起始端口"))
s_global:option(Value, "control_socket", translate("控制 Socket 路径"))

-- FFmpeg 下载地址
local ffmpeg_s = m:section(NamedSection, "global", "global", translate("FFmpeg 下载地址"))
ffmpeg_s.addremove = false
ffmpeg_s:option(Value, "ffmpeg_download_url_x86_64",  translate("x86_64"))
ffmpeg_s:option(Value, "ffmpeg_download_url_aarch64", translate("aarch64"))
ffmpeg_s:option(Value, "ffmpeg_download_url_armv7l",  translate("armv7l"))
ffmpeg_s:option(Value, "ffmpeg_download_url_i686",    translate("i686"))

-- 频道列表
local s = m:section(TypedSection, "channel", translate("频道列表"),
    translate("点击 Edit 编辑完整参数（源地址、缓存、ffmpeg 参数等）"))
s.template = "cbi/tblsection"
s.addremove = true
s.anonymous = false

s:option(Flag, "enabled", translate("启用"))
s:option(Value, "name", translate("名称"))
s:option(Value, "addr", translate("输出地址"))
s:option(Value, "port", translate("端口"))

local p = s:option(ListValue, "protocol", translate("协议"))
p:value("udp", "UDP")
p:value("rtp", "RTP")

local i = s:option(ListValue, "iface", translate("出口"))
i:value("lan", "lan")
i:value("wan", "wan")
i:value("br-lan", "br-lan")
i:value("eth0", "eth0")
i:value("eth1", "eth1")

s:option(Flag, "use_downloader", translate("缓存"))
s:option(DynamicList, "url", translate("源地址"))

-- 以下字段在 Edit 页自动显示
local ttl = s:option(Value, "ttl", translate("TTL"))
ttl.default = "4"
s:option(Flag, "race_mode", translate("赛马"))
s:option(DynamicList, "race_ip", translate("赛马 IP"))
s:option(Value, "threads", translate("并发数"))
s:option(Value, "cache_window", translate("缓存窗口"))
s:option(Value, "max_segments", translate("缓存上限"))
s:option(Value, "hls_segments", translate("HLS窗口"))
s:option(Value, "pkt_size", translate("UDP包大小"))
s:option(DynamicList, "ffmpeg_input_arg", translate("ffmpeg入参"))
s:option(DynamicList, "ffmpeg_output_arg", translate("ffmpeg出参"))

-- 重载钩子
m.on_after_commit = function(self)
    local sys = require "luci.sys"
    local uci2 = require "luci.model.uci".cursor()
    local sock = uci2:get("m3u8mc", "global", "control_socket") or "/var/run/m3u8mcd.sock"
    local _, code = sys.exec(string.format("/usr/bin/m3u8mcctl --socket %q reload 2>/dev/null", sock))
    if code ~= 0 then sys.call("/etc/init.d/m3u8mc restart") end
end

return m
