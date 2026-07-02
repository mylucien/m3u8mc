-- LuCI CBI model: m3u8mc 频道管理（概览表格）
local m = Map("m3u8mc", translate("m3u8mc 频道管理"),
    translate("概览频道输出信息。完整参数请在 UCI 配置文件 /etc/config/m3u8mc 中编辑。"))

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
local s = m:section(TypedSection, "channel", translate("频道列表"))
s.template = "cbi/tblsection"
s.addremove = true
s.anonymous = false

s:option(Flag, "enabled", translate("启用"))
s:option(Value, "name", translate("频道名称"))
s:option(Value, "addr", translate("输出地址"))
s:option(Value, "port", translate("输出端口"))
local p = s:option(ListValue, "protocol", translate("协议"))
p:value("udp", "UDP")
p:value("rtp", "RTP")

-- 重载钩子
m.on_after_commit = function(self)
    local sys = require "luci.sys"
    local uci2 = require "luci.model.uci".cursor()
    local sock = uci2:get("m3u8mc", "global", "control_socket") or "/var/run/m3u8mcd.sock"
    local _, code = sys.exec(string.format("/usr/bin/m3u8mcctl --socket %q reload 2>/dev/null", sock))
    if code ~= 0 then sys.call("/etc/init.d/m3u8mc restart") end
end

return m
