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
