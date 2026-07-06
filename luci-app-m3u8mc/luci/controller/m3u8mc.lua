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

    entry({"admin", "services", "m3u8mc", "status"},
          template("m3u8mc/status"), _("运行状态"), 20)

    entry({"admin", "services", "m3u8mc", "log"},
          template("m3u8mc/log"), _("实时日志"), 30)

    -- Ajax 接口
    entry({"admin", "services", "m3u8mc", "api", "status"},
          call("api_status")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "channel_action"},
          call("api_channel_action")).leaf = true

    entry({"admin", "services", "m3u8mc", "api", "log"},
          call("api_log")).leaf = true
end

-- ── API handlers ────────────────────────────────────────────

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

function api_log()
    local http = require "luci.http"
    local sys  = require "luci.sys"
    local uci  = require("luci.model.uci").cursor()

    -- 行数参数，限制 100–2000 防止请求过大
    local lines_raw = http.formvalue("lines") or "200"
    local lines = tonumber(lines_raw) or 200
    if lines < 100  then lines = 100  end
    if lines > 2000 then lines = 2000 end

    -- 优先从 m3u8mcd 的 systemd/procd journal 读取；
    -- immortalwrt 使用 logd，通过 logread 获取，再用 grep 过滤 m3u8mc 相关行。
    -- 若 logread 不可用则 fallback 到 journalctl。
    local out = ""

    -- 尝试 logread（procd / OpenWrt / immortalwrt 默认）
    local logread_out = sys.exec(
        string.format("logread 2>/dev/null | grep -i 'm3u8m' | tail -n %d", lines)
    )
    if logread_out and #logread_out > 10 then
        out = logread_out
    else
        -- fallback: journalctl
        local journal_out = sys.exec(
            string.format(
                "journalctl -u m3u8mcd --no-pager -n %d --output=short-iso 2>/dev/null",
                lines
            )
        )
        if journal_out and #journal_out > 10 then
            out = journal_out
        else
            -- 最终 fallback：直接读 /var/log/m3u8mcd.log（若有）
            local log_file = uci:get("m3u8mc", "global", "log_file") or ""
            if log_file ~= "" and nixio.fs.access(log_file) then
                out = sys.exec(string.format("tail -n %d %q 2>/dev/null", lines, log_file)) or ""
            end
        end
    end

    http.prepare_content("text/plain; charset=utf-8")
    http.write(out or "")
end
