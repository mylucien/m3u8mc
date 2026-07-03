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
-- Bug 3 fix: 不再使用 TypedSection 自带的 addremove。
-- 原因：addremove=true 时，LuCI 会在表格下方弹出一个
-- 「文本框（输入新段名）+ Add 按钮」的组合（因为 anonymous 默认为 false），
-- 这就是你看到的"只有两个东西"；而且这个 Add 走的是前端 AJAX 局部刷新，
-- 不会整页跳转到编辑页。所以这里关掉它，改用下面的自定义链接。
s.addremove = false
s.anonymous = false

-- Bug 1 fix: 使用标准 extedit 替代自定义 Button.write；
-- LuCI tblsection 会在每行渲染一个「编辑」链接，自动拼入 section name
s.extedit = luci.dispatcher.build_url("admin/services/m3u8mc/channel_edit") .. "?section=%s"

-- Bug 1 fix: 新建时跳转到编辑页——见 Bug 3 fix：新增已改为
-- 独立的 channel_add controller 动作（controller/m3u8mc.lua），
-- 不再依赖 TypedSection 的 create 流程。

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

-- Bug 3 fix: 删除按钮改为链接到独立的 channel_del controller 动作，
-- 原来 TypedSection.addremove 自带的删除按钮已经关闭（见上）。
local del_col = s:option(DummyValue, "_del", translate("操作"))
del_col.rawhtml = true
del_col.cfgvalue = function(self, section)
    local url = luci.dispatcher.build_url("admin/services/m3u8mc/channel_del")
                .. "?section=" .. section
    local confirm_msg = translate("确定要删除该频道吗？此操作不可撤销。")
    return string.format(
        '<a class="btn cbi-button cbi-button-remove" href="%s" ' ..
        'onclick="return confirm(\'%s\');">%s</a>',
        url, confirm_msg, translate("删除")
    )
end

-- ── 新增频道按钮（Bug 3 fix）────────────────────────────────────
-- 用普通 <a> 链接触发整页 GET 跳转到 channel_add，
-- 由 channel_add 建好一个空的 channel 段后再 302 到 channel_edit，
-- 用户填完全部参数一次性提交，不会先经过阉割版的“添加框”。
local addsec = m:section(SimpleSection)
local addbtn = addsec:option(DummyValue, "_add")
addbtn.rawhtml = true
addbtn.cfgvalue = function()
    local url = luci.dispatcher.build_url("admin/services/m3u8mc/channel_add")
    return string.format(
        '<a class="btn cbi-button cbi-button-add" href="%s">%s</a>',
        url, translate("新增频道")
    )
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
