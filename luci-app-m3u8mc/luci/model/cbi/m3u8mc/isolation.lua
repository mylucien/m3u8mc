-- LuCI CBI model: m3u8mc 网络隔离
-- 对应 VlanIsolationConfig（config.rs），并在页面底部动态展示生成的 UCI 配置预览

local m = Map("m3u8mc", translate("网络隔离"),
    translate("为组播流量创建独立 VLAN 子接口，并生成防火墙隔离规则，防止组播流量泄露到其他网络。"))

-- ── 1. 总开关 ────────────────────────────────────────────────
local sg = m:section(NamedSection, "vlan_isolation", "vlan_isolation",
    translate("VLAN 隔离设置"))
sg.addremove = false

local enabled = sg:option(Flag, "enabled", translate("启用网络隔离"))
enabled.rmempty = false

-- ── 2. VLAN 基础参数 ─────────────────────────────────────────
local base_dev = sg:option(Value, "base_device", translate("父设备"),
    translate("VLAN 子接口的父设备；通常为 br-lan（共享 LAN 网桥，三层隔离）"))
base_dev.placeholder = "br-lan"
base_dev.rmempty     = false
-- 从系统设备列表动态填入候选项
local netm = require "luci.model.network"
local nt   = netm.init()
local devs = nt and nt:get_interfaces() or {}
if #devs == 0 then
    -- 回退：手动列举常见父设备
    for _, d in ipairs({"br-lan", "eth0", "eth1"}) do
        base_dev:value(d, d)
    end
else
    for _, dev in ipairs(devs) do
        local n = dev:name()
        if n and not n:match("^lo") then
            base_dev:value(n, n)
        end
    end
end
base_dev:depends("enabled", "1")

local vlan_id = sg:option(Value, "vlan_id", translate("VLAN ID"),
    translate("取值 1–4094；需与交换机/上联端口的 VLAN 配置一致"))
vlan_id.placeholder = "100"
vlan_id.datatype    = "range(1,4094)"
vlan_id.default     = "100"
vlan_id:depends("enabled", "1")

local ipaddr = sg:option(Value, "ipaddr", translate("子接口 IP 地址"),
    translate("VLAN 子接口的本地 IP，同时作为频道 iface 选择该接口时 ffmpeg 的 localaddr"))
ipaddr.placeholder = "10.10.100.1"
ipaddr.datatype    = "ip4addr"
ipaddr:depends("enabled", "1")

local netmask = sg:option(Value, "netmask", translate("子网掩码"))
netmask.placeholder = "255.255.255.0"
netmask.datatype    = "ip4addr"
netmask.default     = "255.255.255.0"
netmask:depends("enabled", "1")

-- ── 3. 生成的配置预览（只读，JS 动态渲染） ──────────────────
local preview_sec = m:section(SimpleSection, translate("生成的 UCI 配置预览"),
    translate("保存并应用后，以下配置将写入 /etc/config/network 和 /etc/config/firewall（标注 managed_by=m3u8mc）"))

function preview_sec.render(self, ...)
    local tpl = require "luci.template"
    tpl.render_string([[
<div class="cbi-section" style="margin-top:0">
  <h3 class="cbi-section-title"><%:生成的 UCI 配置预览%></h3>
  <p class="cbi-section-descr">
    <%:保存后点击「应用隔离」，以下配置将写入 /etc/config/network 和 /etc/config/firewall。标注 managed_by=m3u8mc，卸载插件时可精确清理。%>
  </p>

  <div id="iso-preview-wrap" style="display:none">
    <!-- /etc/config/network 预览 -->
    <h4 style="font-size:0.85em;font-weight:600;margin:12px 0 4px;color:var(--cl-primary,#337ab7)">/etc/config/network（追加）</h4>
    <pre id="iso-preview-network"
         style="background:#1e1e1e;color:#d4d4d4;padding:12px;border-radius:6px;
                font-size:0.82em;overflow-x:auto;white-space:pre;line-height:1.5"></pre>

    <!-- /etc/config/firewall 预览 -->
    <h4 style="font-size:0.85em;font-weight:600;margin:12px 0 4px;color:var(--cl-primary,#337ab7)">/etc/config/firewall（追加）</h4>
    <pre id="iso-preview-firewall"
         style="background:#1e1e1e;color:#d4d4d4;padding:12px;border-radius:6px;
                font-size:0.82em;overflow-x:auto;white-space:pre;line-height:1.5"></pre>
  </div>
  <div id="iso-preview-disabled"
       style="color:#888;font-size:0.9em;padding:8px 0">
    <%:（网络隔离未启用，无需生成配置）%>
  </div>

  <!-- 操作按钮 -->
  <div style="display:flex;gap:10px;margin-top:16px;flex-wrap:wrap">
    <button class="btn cbi-button cbi-button-apply" id="btn-apply-iso"
            onclick="isoAction('apply');return false;">
      <%:应用隔离（生成 VLAN / 防火墙规则）%>
    </button>
    <button class="btn cbi-button cbi-button-reset" id="btn-revert-iso"
            onclick="isoAction('revert');return false;">
      <%:撤销隔离（删除 managed_by=m3u8mc 的条目）%>
    </button>
  </div>
  <div id="iso-action-result"
       style="margin-top:8px;font-size:0.88em;min-height:1.4em"></div>
</div>

<script>
(function() {
  // 从页面表单字段实时读取值，生成 UCI 预览文本
  function fv(id) {
    var el = document.getElementById(id);
    return el ? el.value.trim() : '';
  }
  function cb(id) {
    var el = document.getElementById(id);
    return el ? el.checked : false;
  }

  function renderPreview() {
    // 找字段：LuCI CBI 生成的 id 规则为 cbid.m3u8mc.vlan_isolation.<option>
    var pfx    = 'cbid.m3u8mc.vlan_isolation.';
    var isEnabled = document.getElementById(pfx + 'enabled');
    var enabled   = isEnabled ? (isEnabled.checked || isEnabled.value === '1') : false;

    var wrap    = document.getElementById('iso-preview-wrap');
    var noWrap  = document.getElementById('iso-preview-disabled');

    if (!enabled) {
      if (wrap)   wrap.style.display   = 'none';
      if (noWrap) noWrap.style.display = '';
      return;
    }
    if (wrap)   wrap.style.display   = '';
    if (noWrap) noWrap.style.display = 'none';

    var base   = (document.getElementById(pfx + 'base_device') || {}).value || 'br-lan';
    var vid    = (document.getElementById(pfx + 'vlan_id')     || {}).value || '100';
    var ip     = (document.getElementById(pfx + 'ipaddr')      || {}).value || '10.10.100.1';
    var mask   = (document.getElementById(pfx + 'netmask')     || {}).value || '255.255.255.0';
    var ifname = 'iptv_vlan';   // 逻辑接口名固定
    var devname = base + '.' + vid;

    // /etc/config/network 预览
    var net = [
      "# --- m3u8mc managed_by=m3u8mc ---",
      "config device",
      "\toption name '" + devname + "'",
      "\toption type '8021q'",
      "\toption ifname '" + base + "'",
      "\toption vid '" + vid + "'",
      "",
      "config interface '" + ifname + "'",
      "\toption device '" + devname + "'",
      "\toption proto 'static'",
      "\toption ipaddr '" + ip + "'",
      "\toption netmask '" + mask + "'",
      "\toption defaultroute '0'",
      "\toption managed_by 'm3u8mc'",
    ].join('\n');

    // /etc/config/firewall 预览
    var fw = [
      "# --- m3u8mc managed_by=m3u8mc ---",
      "config zone",
      "\toption name 'iptv'",
      "\tlist network '" + ifname + "'",
      "\toption input 'REJECT'",
      "\toption output 'ACCEPT'",
      "\toption forward 'REJECT'",
      "\toption managed_by 'm3u8mc'",
      "",
      "config rule",
      "\toption name 'm3u8mc-Block-Multicast-Leak'",
      "\toption src 'iptv'",
      "\toption dest '*'",
      "\toption dest_ip '224.0.0.0/4'",
      "\toption target 'REJECT'",
      "\toption enabled '1'",
      "\toption managed_by 'm3u8mc'",
    ].join('\n');

    var pNet = document.getElementById('iso-preview-network');
    var pFw  = document.getElementById('iso-preview-firewall');
    if (pNet) pNet.textContent = net;
    if (pFw)  pFw.textContent  = fw;
  }

  // 监听所有相关字段变化
  function bindFields() {
    var pfx = 'cbid.m3u8mc.vlan_isolation.';
    ['enabled','base_device','vlan_id','ipaddr','netmask'].forEach(function(f) {
      var el = document.getElementById(pfx + f);
      if (el) {
        el.addEventListener('change', renderPreview);
        el.addEventListener('input',  renderPreview);
      }
    });
  }

  // DOM ready
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', function() { bindFields(); renderPreview(); });
  } else {
    bindFields(); renderPreview();
  }

  // 应用 / 撤销按钮
  window.isoAction = function(action) {
    var res = document.getElementById('iso-action-result');
    res.style.color = '#555';
    res.textContent = '执行中…';
    var fd = new FormData();
    fd.append('action', action);
    fetch('/cgi-bin/luci/admin/services/m3u8mc/api/isolation_apply', {
      method: 'POST', body: fd
    }).then(function(r) { return r.json(); })
      .then(function(d) {
        if (d.ok) {
          res.style.color = '#3c763d';
          res.textContent = action === 'apply'
            ? '隔离规则已应用，network/firewall 已 reload'
            : '隔离配置已撤销';
        } else {
          res.style.color = '#a94442';
          res.textContent = '失败：' + (d.error || '未知错误');
        }
      })
      .catch(function(e) {
        res.style.color = '#a94442';
        res.textContent = '请求失败：' + e;
      });
  };
})();
</script>
]])
end

-- ── 保存后热重载 ─────────────────────────────────────────────
m.on_after_commit = function(self)
    -- 隔离配置变更只触发 daemon reload（不触发 network reload，那是"应用"按钮的职责）
    local sys  = require "luci.sys"
    local uci2 = require("luci.model.uci").cursor()
    local sock = uci2:get("m3u8mc", "global", "control_socket")
                 or "/var/run/m3u8mcd.sock"
    local _, code = sys.exec(
        string.format("/usr/bin/m3u8mcctl --socket %q reload 2>/dev/null", sock)
    )
    if code ~= 0 then
        sys.call("/etc/init.d/m3u8mc restart")
    end
end

return m
