-- LuCI CBI model: m3u8mc 频道编辑页
-- 覆盖 ChannelConfig 的全部 UCI 字段（见 config.rs）
-- 通过 ?section=<name> 参数确定编辑目标

local http    = require "luci.http"
local uci_r   = require("luci.model.uci").cursor()
local sys     = require "luci.sys"

-- Bug fix: 原表达式 "or http.formvalue('cbi.submit') and nil" 因运算符优先级
-- 会将右侧解析为 (http.formvalue('cbi.submit') and nil)，使 section 永远可能为 nil。
-- 直接取 "section" 参数即可。
local section = http.formvalue("section")

-- 若 URL 里没有 section 参数，回到列表页
if not section or section == "" then
    http.redirect(luci.dispatcher.build_url("admin/services/m3u8mc/channels"))
    return
end

-- 确保该 section 存在（新建时 create 已建好空 section）
local sec_type = uci_r:get("m3u8mc", section)
if not sec_type then
    http.redirect(luci.dispatcher.build_url("admin/services/m3u8mc/channels"))
    return
end

local m = Map("m3u8mc",
    translate("编辑频道"),
    translate("修改后点击「保存并应用」，daemon 将热重载受影响的频道。"))

-- ── Bug fix: POST 提交时 query string 丢失导致 section=nil 保存失败 ──────────
-- LuCI CBI 表单的 action 不带 query string，因此 ?section=xxx 在 POST 时丢失。
-- 将 section 注入为隐藏字段，使 POST body 中始终携带该值。
m.hidden = { section = section }

-- 锁定到指定 section
local s = m:section(NamedSection, section, "channel")
s.addremove = false
s.anonymous = false

-- ── 基础信息 ──────────────────────────────────────────────────
local name = s:option(Value, "name", translate("频道名称"))
name.rmempty = false
name.placeholder = "如：澳视体育"

-- 镜像源地址（list url，多行动态列表）
local url = s:option(DynamicList, "url", translate("镜像源地址"),
    translate("内容相同的多个镜像 URL，逐行填写；直通模式下只允许一条"))
url.placeholder = "http://..."
url.rmempty = false

-- ── 输出参数 ──────────────────────────────────────────────────
local proto = s:option(ListValue, "protocol", translate("协议"))
proto:value("rtp", "RTP")
proto:value("udp", "UDP")
proto.default = "rtp"

local addr = s:option(Value, "addr", translate("组播地址"))
addr.placeholder = "239.255.1.1"
addr.datatype    = "ip4addr"
addr.rmempty     = false

local port = s:option(Value, "port", translate("组播端口"))
port.placeholder = "1234"
port.datatype    = "port"
port.rmempty     = false

-- 出口接口：从 network.interface 动态读取，同时允许手填 IP
local iface = s:option(Value, "iface", translate("出口接口"),
    translate("ffmpeg 推流绑定的本地接口，运行时解析为 IP；也可直接填 IP 地址"))
iface.placeholder = "lan"
uci_r:foreach("network", "interface", function(sec)
    local iname = sec[".name"]
    if iname and iname ~= "loopback" then
        iface:value(iname, iname)
    end
end)

-- ── 运行模式 ──────────────────────────────────────────────────
local use_dl = s:option(Flag, "use_downloader", translate("启用缓存下载器（标准模式）"),
    translate("关闭后切换为直通模式：ffmpeg 直接拉取原始 m3u8，不支持多镜像故障切换"))
use_dl.default  = "1"
use_dl.rmempty  = false

-- ── 高级选项：下载策略（标准模式下可见）────────────────────
local race = s:option(Flag, "race_mode", translate("启用赛马下载策略"),
    translate("并发向所有节点请求同一分片，取响应最快者；会成倍增加带宽消耗"))
race.default = "0"
race:depends("use_downloader", "1")

local race_ips = s:option(DynamicList, "race_ip", translate("赛马节点 IP"),
    translate("支持两种格式：\n" ..
              "  纯 IP：1.2.3.4（Host 使用全局 race_host 或自动推断）\n" ..
              "  IP@域名：5.6.7.8@mirror2.cdn.com（为该节点单独指定 Host）\n" ..
              "不同格式可在同一频道混用"))
race_ips.placeholder = "1.2.3.4 或 5.6.7.8@mirror2.cdn.com"
race_ips:depends("race_mode", "1")

-- 全局赛马 Host（可选，不填则自动推断自第一个镜像域名）
local race_host = s:option(Value, "race_host", translate("赛马全局 Host"),
    translate("所有未用 IP@域名 格式指定专属 Host 的赛马节点，统一使用此域名作为 HTTP Host 头。\n" ..
              "留空则自动推断自第一个镜像 URL 的域名（原有行为）。\n" ..
              "配置了此项后可避免多节点共用同一域名时的推断歧义。"))
race_host.placeholder = "cdn.example.com"
race_host.rmempty = true
race_host:depends("race_mode", "1")

local threads = s:option(Value, "threads", translate("分块下载线程数"),
    translate("单分片并发 Range 下载数；开启赛马模式时建议设为 1"))
threads.placeholder = "4"
threads.datatype    = "uinteger"
threads.default     = "4"
threads:depends("use_downloader", "1")

-- ── 高级选项：独立列表拉取配置 ────────────────────────────────
local list_url = s:option(DynamicList, "list_url", translate("列表专用地址（可选）"),
    translate("仅当'获取 m3u8 列表'与'下载 ts 分片'需要走不同源时才配置。\n" ..
              "留空则用上方镜像源地址来拉取列表（原有行为）。\n" ..
              "填写后，列表拉取走这里配置的地址，分片下载走上方镜像源地址。"))
list_url.placeholder = "http://list.origin.com/index.m3u8"
list_url.rmempty = true
list_url:depends("use_downloader", "1")

local list_poll = s:option(Value, "list_poll_interval", translate("列表轮询间隔（秒）"),
    translate("固定拉取间隔（秒）。填 0 或留空则按 HLS target_duration/2 自适应（默认推荐）。\n" ..
              "仅在需要强制固定频率时配置，否则保持默认自适应。"))
list_poll.placeholder = "0（自适应）"
list_poll.datatype    = "uinteger"
list_poll.default     = "0"
list_poll.rmempty     = true
list_poll:depends("use_downloader", "1")

-- ── 高级选项：代理配置 ────────────────────────────────────────
local list_proxy = s:option(Value, "list_proxy", translate("列表拉取代理"),
    translate("拉取 m3u8 列表所用的代理地址。留空 = 直连。\n" ..
              "支持格式：http://host:port、https://host:port、socks5://host:port\n" ..
              "地址非法时自动回退直连，不会导致程序崩溃。"))
list_proxy.placeholder = "http://127.0.0.1:7890"
list_proxy.rmempty = true
list_proxy:depends("use_downloader", "1")

local dl_proxy = s:option(Value, "download_proxy", translate("分片下载代理"),
    translate("下载 ts 分片所用的代理地址。留空 = 直连。\n" ..
              "与列表代理相互独立，可只配置其中一个。\n" ..
              "支持格式：http://host:port、https://host:port、socks5://host:port"))
dl_proxy.placeholder = "socks5://127.0.0.1:1080"
dl_proxy.rmempty = true
dl_proxy:depends("use_downloader", "1")

-- ── 高级选项：缓存参数 ────────────────────────────────────────
local cache_w = s:option(Value, "cache_window", translate("缓存窗口倍数"),
    translate("分片保留时长 = 分片时长 × 此值（秒）；应 ≥ hls_segments 覆盖的时间跨度"))
cache_w.placeholder = "3"
cache_w.datatype    = "uinteger"
cache_w.default     = "3"
cache_w:depends("use_downloader", "1")

local max_seg = s:option(Value, "max_segments", translate("最大缓存分片数"),
    translate("超出时淘汰最旧分片；建议略大于 hls_segments"))
max_seg.placeholder = "5"
max_seg.datatype    = "uinteger"
max_seg.default     = "5"
max_seg:depends("use_downloader", "1")

local hls_seg = s:option(Value, "hls_segments", translate("HLS 预热分片数"),
    translate("内部 m3u8 返回的分片数；同时作为 ffmpeg 启动前的预热阈值"))
hls_seg.placeholder = "3"
hls_seg.datatype    = "uinteger"
hls_seg.default     = "3"
hls_seg:depends("use_downloader", "1")

local http_port = s:option(Value, "http_port", translate("内部 HTTP 端口"),
    translate("留空则从全局 http_port_base 自动分配；直通模式下忽略此项"))
http_port.placeholder = "留空自动分配"
-- Bug fix: LuCI datatype 不支持 "or(port, \"\")" 复合语法，改用自定义 validate。
-- 空值直接放行（由 Rust 侧处理自动分配），非空时做 1–65535 范围校验。
http_port.validate = function(self, value, section)
    if not value or value == "" then return value end
    local n = tonumber(value)
    if n and n >= 1 and n <= 65535 then return value end
    return nil, translate("端口号须为 1–65535 之间的整数")
end
http_port:depends("use_downloader", "1")

local http_path = s:option(Value, "http_path", translate("内部 HTTP 路径"))
http_path.placeholder = "/live"
http_path.default     = "/live"
http_path:depends("use_downloader", "1")

-- ── 高级选项：推流参数 ────────────────────────────────────────
local ttl = s:option(Value, "ttl", translate("TTL"),
    translate("组播 TTL，控制数据包可跨越的路由器跳数"))
ttl.placeholder = "4"
ttl.datatype    = "range(1,255)"
ttl.default     = "4"

local pkt_size = s:option(Value, "pkt_size", translate("UDP 包大小（pkt_size）"),
    translate("推荐 1316 = 7 × 188 字节（单个 MPEG-TS 包），避免包边界跨越导致接收端拆包错误"))
pkt_size.placeholder = "1316"
pkt_size.datatype    = "uinteger"
pkt_size.default     = "1316"

local buf = s:option(Value, "buffer_size", translate("推流缓冲区（buffer_size，字节）"),
    translate("经验公式：目标缓冲秒数 × 码率(bps) ÷ 8；如缓冲 2s@4Mbps ≈ 1048576"))
buf.placeholder = "1048576"
buf.datatype    = "uinteger"
buf.default     = "1048576"

-- ── 自定义请求头 ────────────────────────────────────────────────
local hdr = s:option(DynamicList, "http_header", translate("公共 HTTP 请求头"),
    translate("每行格式：Header-Name: value。常用：User-Agent、Referer、Origin。\n" ..
              "Poller（列表拉取）和 Downloader（分片下载）的专属头未配置时均回退到此处。"))
hdr.placeholder = "User-Agent: Mozilla/5.0 (Linux; Android 10)"

local poller_hdr = s:option(DynamicList, "poller_http_header", translate("列表请求专属头（可选）"),
    translate("仅列表拉取请求（Poller）携带的额外头。\n" ..
              "留空则使用上方公共 HTTP 请求头。\n" ..
              "填写后完全覆盖公共头（针对 Poller），常用于列表源需要不同 Referer 的场景。"))
poller_hdr.placeholder = "Referer: https://list.example.com/"
poller_hdr.rmempty = true
poller_hdr:depends("use_downloader", "1")

local dl_hdr = s:option(DynamicList, "downloader_http_header", translate("分片下载专属头（可选）"),
    translate("仅分片下载请求（Downloader）携带的额外头。\n" ..
              "留空则使用上方公共 HTTP 请求头。\n" ..
              "填写后完全覆盖公共头（针对 Downloader），常用于 CDN 需要不同 Referer 的场景。"))
dl_hdr.placeholder = "Referer: https://cdn.example.com/"
dl_hdr.rmempty = true
dl_hdr:depends("use_downloader", "1")

-- ── ffmpeg 自定义参数 ─────────────────────────────────────────
local fin = s:option(DynamicList, "ffmpeg_input_arg", translate("ffmpeg 额外输入参数"),
    translate("追加在 -i 之前；留空时 daemon 自动加 -analyzeduration/-probesize 5000000"))
fin.placeholder = "-re"

local fout = s:option(DynamicList, "ffmpeg_output_arg", translate("ffmpeg 额外输出参数"),
    translate("追加在 -i 之后；留空时 daemon 自动应用兼容性默认组合（pcr_period/muxdelay/bsf:v 等）；非空则完全覆盖默认值"))
fout.placeholder = "-pcr_period"

-- ── 保存后热重载 ──────────────────────────────────────────────
m.on_after_commit = function(self)
    local uci2     = require("luci.model.uci").cursor()
    local sock_path = uci2:get("m3u8mc", "global", "control_socket")
                      or "/var/run/m3u8mcd.sock"
    local _, code = sys.exec(
        string.format("/usr/bin/m3u8mcctl --socket %q reload 2>/dev/null", sock_path)
    )
    -- Bug fix: sys.exec 命令不存在时 code 为 nil，需先判 nil 再比较
    if not code or code ~= 0 then
        sys.call("/etc/init.d/m3u8mc restart")
    end
    -- 保存后跳回列表页
    http.redirect(luci.dispatcher.build_url("admin/services/m3u8mc/channels"))
end

return m
