-- LuCI controller for m3u8mc
module("luci.controller.m3u8mc", package.seeall)

function index()
    if not nixio.fs.access("/etc/config/m3u8mc") then
        return
    end

    local e = entry({"admin", "services", "m3u8mc"},
                    firstchild(), _("m3u8mc 转播"), 60)
    e.dependent = false
    e.acl_depends = { "luci-app-m3u8mc" }

    entry({"admin", "services", "m3u8mc", "channels"},
          cbi("m3u8mc/channels"), _("频道管理"), 10)

    -- 频道编辑页（独立路由，不出现在菜单）
    -- leaf=true  : dispatcher 不再向下匹配子路径
    -- i18n=false : 避免 ucode bridge 因找不到 po 目录而抛 "Model not found"
    local ce = entry({"admin", "services", "m3u8mc", "channel_edit"},
          cbi("m3u8mc/channel_edit"), nil, nil)
    ce.leaf = true
    ce.i18n = false

    -- Bug 3 fix: 新增频道 —— 纯 GET 跳转的 controller 动作（call()），
    -- 不经过 CBI Map/TypedSection 的 create 流程。
    -- 原因：新版 LuCI 的 TypedSection 添加按钮是纯前端 AJAX 局部刷新，
    -- 在 Lua 的 create() 里调用 http.redirect() 不会触发浏览器整页跳转；
    -- 而 call() 是一次普通的 HTTP 请求/响应，redirect 一定生效。
    local ca = entry({"admin", "services", "m3u8mc", "channel_add"},
          call("channel_add"), nil, nil)
    ca.leaf  = true
    ca.i18n  = false

    -- Bug 3 fix: 删除频道 —— 同样用普通 controller 动作实现，
    -- 替代 TypedSection.addremove 自带的删除按钮（同一个开关会连
    -- 添加框一起带出来，索性都不用，改成显式链接更可控）。
    local cd = entry({"admin", "services", "m3u8mc", "channel_del"},
          call("channel_del"), nil, nil)
    cd.leaf  = true
    cd.i18n  = false

    entry({"admin", "services", "m3u8mc", "isolation"},
          cbi("m3u8mc/isolation"), _("网络隔离"), 20)

    entry({"admin", "services", "m3u8mc", "ffmpeg"},
          template("m3u8mc/ffmpeg"), _("FFmpeg 管理"), 25)

    entry({"admin", "services", "m3u8mc", "status"},
          template("m3u8mc/status"), _("运行状态"), 30)

    -- Ajax 接口
    entry({"admin", "services", "m3u8mc", "api", "status"},
          call("api_status")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "channel_action"},
          call("api_channel_action")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "isolation_apply"},
          call("api_isolation_apply")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "ffmpeg_check"},
          call("api_ffmpeg_check")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "ffmpeg_install"},
          call("api_ffmpeg_install")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "ffmpeg_progress"},
          call("api_ffmpeg_progress")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "ffmpeg_reset"},
          call("api_ffmpeg_reset")).leaf = true
end

-- ── 频道新增 / 删除（Bug 3 fix）───────────────────────────────
-- 两个函数都是普通 HTTP 请求处理（call()），不是 cbi()，
-- 所以里面的 luci.http.redirect() 是真正的 302 整页跳转，
-- 不会被前端 AJAX 吞掉。

function channel_add()
    local http = require "luci.http"
    local uci  = require("luci.model.uci").cursor()

    -- uci:add() 生成匿名段名（形如 cfgXXXXXX），并写入内存态；
    -- 需要 save+commit 落盘，否则下一次请求 channel_edit 时
    -- uci:get("m3u8mc", section) 会读不到该段而被打回频道列表。
    local name = uci:add("m3u8mc", "channel")
    if name then
        uci:set("m3u8mc", name, "enabled", "1")
        uci:save("m3u8mc")
        uci:commit("m3u8mc")
        http.redirect(
            luci.dispatcher.build_url("admin/services/m3u8mc/channel_edit")
            .. "?section=" .. name
        )
    else
        http.redirect(luci.dispatcher.build_url("admin/services/m3u8mc/channels"))
    end
end

function channel_del()
    local http = require "luci.http"
    local sys  = require "luci.sys"
    local uci  = require("luci.model.uci").cursor()
    local section = http.formvalue("section")

    -- uci:get(config, section) 不带 option 参数时返回该段的类型（字符串），
    -- 用来确认这确实是一个 channel 段，防止误删别的配置段。
    if section and section ~= "" and uci:get("m3u8mc", section) == "channel" then
        uci:delete("m3u8mc", section)
        uci:save("m3u8mc")
        uci:commit("m3u8mc")

        local sock_path = uci:get("m3u8mc", "global", "control_socket")
                           or "/var/run/m3u8mcd.sock"
        local _, code = sys.exec(
            string.format("/usr/bin/m3u8mcctl --socket %q reload 2>/dev/null", sock_path)
        )
        if not code or code ~= 0 then
            sys.call("/etc/init.d/m3u8mc restart")
        end
    end
    http.redirect(luci.dispatcher.build_url("admin/services/m3u8mc/channels"))
end

-- ── API handlers（原样保留）────────────────────────────────────

function api_status()
    local http = require "luci.http"
    local sys  = require "luci.sys"
    local uci  = require("luci.model.uci").cursor()
    local sock = uci:get("m3u8mc", "global", "control_socket")
                 or "/var/run/m3u8mcd.sock"
    local out, code = sys.exec(
        string.format("/usr/bin/m3u8mcctl --socket %q status --json 2>&1", sock)
    )
    http.prepare_content("application/json")
    if code ~= 0 or not out or out == "" then
        http.write('{"error":"daemon not running or m3u8mcctl failed"}')
    else
        http.write(out)
    end
end

function api_channel_action()
    local http   = require "luci.http"
    local sys    = require "luci.sys"
    local uci    = require("luci.model.uci").cursor()
    local action = http.formvalue("action") or ""
    local name   = http.formvalue("name")   or ""
    if action ~= "start" and action ~= "stop" and action ~= "restart" then
        http.prepare_content("application/json")
        http.write('{"error":"invalid action"}')
        return
    end
    if name == "" then
        http.prepare_content("application/json")
        http.write('{"error":"missing channel name"}')
        return
    end
    local sock = uci:get("m3u8mc", "global", "control_socket")
                 or "/var/run/m3u8mcd.sock"
    local cmd = string.format(
        "/usr/bin/m3u8mcctl --socket %q channel %s --name %q 2>&1",
        sock, action, name
    )
    local out, code = sys.exec(cmd)
    http.prepare_content("application/json")
    if code == 0 then
        http.write('{"ok":true}')
    else
        http.write(string.format('{"error":%q}', tostring(out)))
    end
end

function api_isolation_apply()
    local http   = require "luci.http"
    local sys    = require "luci.sys"
    local action = http.formvalue("action") or ""
    if action ~= "apply" and action ~= "revert" then
        http.prepare_content("application/json")
        http.write('{"error":"invalid action"}')
        return
    end
    local out, code = sys.exec(
        "/usr/share/m3u8mc/network-isolation.sh " .. action .. " 2>&1"
    )
    http.prepare_content("application/json")
    if code == 0 then
        http.write('{"ok":true}')
    else
        http.write(string.format('{"error":%q}', tostring(out)))
    end
end

local FFMPEG_SCRIPT = "/usr/share/m3u8mc/ffmpeg-install.sh"
local PROGRESS_FILE = "/tmp/m3u8mc-ffmpeg-install.progress"
local LOCK_FILE     = "/tmp/m3u8mc-ffmpeg-install.lock"

local ARCH_DEFAULTS_RAW = {
    x86_64  = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz",
    aarch64 = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz",
    armv7l  = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-armhf-static.tar.xz",
    i686    = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-i686-static.tar.xz",
}

local function get_download_url(arch, uci_cursor)
    local uci_val = uci_cursor:get("m3u8mc", "global", "ffmpeg_download_url_" .. arch)
    if uci_val and uci_val ~= "" then return uci_val end
    return ARCH_DEFAULTS_RAW[arch] or ""
end

function api_ffmpeg_check()
    local http = require "luci.http"
    local sys  = require "luci.sys"
    local uci  = require("luci.model.uci").cursor()
    local custom_bin  = uci:get("m3u8mc", "global", "ffmpeg_bin") or ""
    local check_target = custom_bin ~= "" and custom_bin or "ffmpeg"
    local out  = sys.exec(string.format("%s check %q 2>&1", FFMPEG_SCRIPT, check_target))
    local arch = (sys.exec("uname -m 2>/dev/null") or ""):gsub("%s+$", "")
    local default_url = get_download_url(arch, uci)
    http.prepare_content("application/json")
    local body = (out or "{}"):gsub("%s+$", "")
    if body:match("^{") and body:match("}$") then
        body = body:sub(1, -2) ..
               string.format(',"custom_bin":%q,"arch":%q,"default_url":%q}',
                              custom_bin, arch, default_url)
    else
        body = string.format(
            '{"found":false,"error":%q,"custom_bin":%q,"arch":%q,"default_url":%q}',
            out or "check failed", custom_bin, arch, default_url)
    end
    http.write(body)
end

function api_ffmpeg_install()
    local http = require "luci.http"
    local sys  = require "luci.sys"
    local url  = http.formvalue("url") or ""
    http.prepare_content("application/json")
    if url == "" then
        http.write('{"error":"缺少下载地址"}')
        return
    end
    if nixio.fs.access(LOCK_FILE) then
        http.write('{"error":"已有安装任务在进行中，请稍候"}')
        return
    end
    local f = io.open(PROGRESS_FILE, "w")
    if f then f:write('{"progress":0,"msg":"排队中"}\n'); f:close() end
    local cmd = string.format(
        "( touch %q; " ..
        "  if %s download %q >%q 2>&1; then %s set-uci /usr/local/bin/ffmpeg >>%q 2>&1; fi; " ..
        "  rm -f %q " ..
        ") &",
        LOCK_FILE, FFMPEG_SCRIPT, url, PROGRESS_FILE,
        FFMPEG_SCRIPT, PROGRESS_FILE, LOCK_FILE
    )
    sys.call(cmd)
    http.write('{"ok":true,"msg":"已开始下载安装"}')
end

function api_ffmpeg_progress()
    local http = require "luci.http"
    local fs   = nixio.fs
    local running  = fs.access(LOCK_FILE)
    local last_line = "{}"
    if fs.access(PROGRESS_FILE) then
        local content = fs.readfile(PROGRESS_FILE) or ""
        for line in content:gmatch("[^\r\n]+") do
            if line:match("^{") then last_line = line end
        end
    end
    http.prepare_content("application/json")
    if last_line:match("^{") and last_line:match("}$") then
        http.write(last_line:sub(1, -2) ..
                    string.format(',"running":%s}', running and "true" or "false"))
    else
        http.write(string.format('{"running":%s}', running and "true" or "false"))
    end
end

function api_ffmpeg_reset()
    local http = require "luci.http"
    local sys  = require "luci.sys"
    local out, code = sys.exec(string.format("%s reset-uci 2>&1", FFMPEG_SCRIPT))
    http.prepare_content("application/json")
    if code == 0 then
        http.write('{"ok":true}')
    else
        http.write(string.format('{"error":%q}', tostring(out)))
    end
end
