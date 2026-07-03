-- LuCI CBI model: m3u8mc 频道列表页
-- 只展示关键列；编辑跳转到独立的 channel_edit.lua

local m = Map("m3u8mc", translate("m3u8mc 转播"))

-- ── 全局设置 ──────────────────────────────────────────────────
local sg = m:section(NamedSection, "global", "global", translate("全局设置"))
sg.addremove = false

sg:option(Flag, "enabled", translate("启用插件"))

local log_opt = sg:option(ListValue, "log_level", translate("日志级别"))
log_opt:value("error",   "error")
log_opt:value("warning", "warning（默认）")
log_opt:value("info",    "info")
log_opt:value("m3u8mcd=debug,info", "debug（详细）")
log_opt.default = "warning"

local pb = sg:option(Value, "http_port_base", translate("HTTP 起始端口"),
    translate("各频道未指定端口时从此值起递增自动分配"))
pb.placeholder = "18080"
pb.datatype    = "port"

local sock = sg:option(Value, "control_socket", translate("控制 Socket"),
    translate("m3u8mcctl 与 daemon 通信的 Unix Socket 路径"))
sock.placeholder = "/var/run/m3u8mcd.sock"

-- ── 频道列表 ──────────────────────────────────────────────────
local s = m:section(TypedSection, "channel", translate("频道列表"))
s.template  = "cbi/tblsection"
s.addremove = true
s.anonymous = false
-- Bug 1 fix: 使用标准 extedit 替代自定义 Button.write；
-- LuCI tblsection 会在每行渲染一个「编辑」链接，自动拼入 section name
s.extedit = luci.dispatcher.build_url("admin/services/m3u8mc/channel_edit") .. "?section=%s"

-- Bug 1 fix: 新建时跳转到编辑页（create 留在 TypedSection 标准流程中，
-- 点击「添加」先创建空 section 再由 extedit 跳转过去）
s.create = function(self, section)
    local stat, err = TypedSection.create(self, section)
    if stat then
        luci.http.redirect(
            luci.dispatcher.build_url("admin/services/m3u8mc/channel_edit") ..
            "?section=" .. section
        )
    end
    return stat, err
end

-- Bug 1 fix: 列表页只读展示，改用 DummyValue（不可内联编辑）
s:option(Flag,       "enabled",  translate("启用"))
s:option(DummyValue, "name",     translate("频道名称"))
s:option(DummyValue, "addr",     translate("输出地址"))
s:option(DummyValue, "port",     translate("端口"))

local proto_col = s:option(DummyValue, "protocol", translate("协议"))
proto_col.cfgvalue = function(self, section)
    local v = self.map:get(section, "protocol") or "rtp"
    return v:upper()
end

-- ── 保存后热重载 ──────────────────────────────────────────────
m.on_after_commit = function(self)
    local sys  = require "luci.sys"
    local uci2 = require("luci.model.uci").cursor()
    local sock_path = uci2:get("m3u8mc", "global", "control_socket")
                      or "/var/run/m3u8mcd.sock"
    local _, code = sys.exec(
        string.format("/usr/bin/m3u8mcctl --socket %q reload 2>/dev/null", sock_path)
    )
    -- Bug 2 fix: sys.exec 在命令不存在时返回 nil，需先判断 nil 再比较值
    if not code or code ~= 0 then
        sys.call("/etc/init.d/m3u8mc restart")
    end
end

return m
