-- LuCI controller for m3u8mc
-- 注册四个菜单项：频道管理 / 网络隔离 / FFmpeg 管理 / 运行状态
module("luci.controller.m3u8mc", package.seeall)

local nixio = require "nixio"
local dispatcher = require "luci.dispatcher"
local entry = dispatcher.entry
local firstchild = dispatcher.firstchild

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

    entry({"admin", "services", "m3u8mc", "isolation"},
          cbi("m3u8mc/isolation"), _("网络隔离"), 20)

    entry({"admin", "services", "m3u8mc", "ffmpeg"},
          template("m3u8mc/ffmpeg"), _("FFmpeg 管理"), 25)

    entry({"admin", "services", "m3u8mc", "status"},
          template("m3u8mc/status"), _("运行状态"), 30)

    -- Ajax 接口（状态页轮询）
    entry({"admin", "services", "m3u8mc", "api", "status"},
          call("api_status")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "channel_action"},
          call("api_channel_action")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "isolation_apply"},
          call("api_isolation_apply")).leaf = true

    -- FFmpeg 管理 API
    entry({"admin", "services", "m3u8mc", "api", "ffmpeg_check"},
          call("api_ffmpeg_check")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "ffmpeg_install"},
          call("api_ffmpeg_install")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "ffmpeg_progress"},
          call("api_ffmpeg_progress")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "ffmpeg_reset"},
          call("api_ffmpeg_reset")).leaf = true
end

-- GET /admin/services/m3u8mc/api/status → m3u8mcctl status --json
function api_status()
    local http = require "luci.http"
    local sys  = require "luci.sys"
    local uci  = require "luci.model.uci".cursor()

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

-- POST /admin/services/m3u8mc/api/channel_action
-- body: action=start|stop|restart&name=<channel_id>
function api_channel_action()
    local http   = require "luci.http"
    local sys    = require "luci.sys"
    local uci    = require "luci.model.uci".cursor()
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

-- POST /admin/services/m3u8mc/api/isolation_apply
-- body: action=apply|revert
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

-- ══════════════════════════════════════════════════════════════
-- FFmpeg 管理
-- ══════════════════════════════════════════════════════════════

local FFMPEG_SCRIPT   = "/usr/share/m3u8mc/ffmpeg-install.sh"
local PROGRESS_FILE   = "/tmp/m3u8mc-ffmpeg-install.progress"
local LOCK_FILE       = "/tmp/m3u8mc-ffmpeg-install.lock"

-- 默认 ffmpeg 静态构建下载地址（按 CPU 架构）。
-- 来源：johnvansickle.com 的 ffmpeg 静态构建（长期维护，覆盖主流架构）。
-- MIPS 系架构目前没有稳定的公开静态构建源，需用户手动提供 URL。
-- 可通过 UCI global 下的 ffmpeg_download_url_<arch> 覆盖（LuCI 全局设置页可配置）。
local ARCH_DEFAULTS_RAW = {
    x86_64  = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz",
    aarch64 = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz",
    armv7l  = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-armhf-static.tar.xz",
    i686    = "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-i686-static.tar.xz",
}

--- 读取指定架构的 ffmpeg 下载地址：优先使用 UCI 配置值，其次内置默认值。
local function get_download_url(arch, uci_cursor)
    local uci_val = uci_cursor:get("m3u8mc", "global", "ffmpeg_download_url_" .. arch)
    if uci_val and uci_val ~= "" then
        return uci_val
    end
    return ARCH_DEFAULTS_RAW[arch] or ""
end

-- GET /admin/services/m3u8mc/api/ffmpeg_check
-- 返回：当前生效 ffmpeg 的路径/版本/关键能力，以及按本机架构推荐的默认下载地址
function api_ffmpeg_check()
    local http = require "luci.http"
    local sys  = require "luci.sys"
    local uci  = require "luci.model.uci".cursor()

    local custom_bin = uci:get("m3u8mc", "global", "ffmpeg_bin") or ""
    local check_target = custom_bin ~= "" and custom_bin or "ffmpeg"

    local out = sys.exec(
        string.format("%s check %q 2>&1", FFMPEG_SCRIPT, check_target)
    )

    local arch = (sys.exec("uname -m 2>/dev/null") or ""):gsub("%s+$", "")
    local default_url = get_download_url(arch, uci)

    http.prepare_content("application/json")
    -- ffmpeg-install.sh check 本身已输出合法 JSON，这里拼接额外字段
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

-- POST /admin/services/m3u8mc/api/ffmpeg_install
-- body: url=<下载地址>
-- 后台异步执行，立即返回，进度写入 PROGRESS_FILE，前端轮询 ffmpeg_progress
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

    -- 写空进度文件，避免前端第一次轮询时读到上次残留内容
    local f = io.open(PROGRESS_FILE, "w")
    if f then f:write('{"progress":0,"msg":"排队中"}\n'); f:close() end

    -- 后台执行：下载安装成功后自动写入 UCI 并热重载。
    -- 拆成独立语句而非依赖跨命令 $?，避免 shell 差异导致误判。
    local cmd = string.format(
        "( touch %q; " ..
        "  if %s download %q >%q 2>&1; then %s set-uci /usr/local/bin/ffmpeg >>%q 2>&1; fi; " ..
        "  rm -f %q " ..
        ") &",
        LOCK_FILE, FFMPEG_SCRIPT, url, PROGRESS_FILE, FFMPEG_SCRIPT, PROGRESS_FILE, LOCK_FILE
    )
    sys.call(cmd)

    http.write('{"ok":true,"msg":"已开始下载安装"}')
end

-- GET /admin/services/m3u8mc/api/ffmpeg_progress
-- 返回 PROGRESS_FILE 最新一行 JSON + 是否仍在运行
function api_ffmpeg_progress()
    local http = require "luci.http"
    local fs   = nixio.fs

    local running = fs.access(LOCK_FILE)
    local last_line = "{}"

    if fs.access(PROGRESS_FILE) then
        local content = fs.readfile(PROGRESS_FILE) or ""
        for line in content:gmatch("[^\r\n]+") do
            if line:match("^{") then last_line = line end
        end
    end

    http.prepare_content("application/json")
    -- 拼接 running 字段（last_line 保证是脚本吐出的合法 JSON 对象）
    if last_line:match("^{") and last_line:match("}$") then
        http.write(last_line:sub(1, -2) ..
                    string.format(',"running":%s}', running and "true" or "false"))
    else
        http.write(string.format('{"running":%s}', running and "true" or "false"))
    end
end

-- POST /admin/services/m3u8mc/api/ffmpeg_reset
-- 清空自定义 ffmpeg_bin，恢复使用系统 PATH 中的 ffmpeg
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

