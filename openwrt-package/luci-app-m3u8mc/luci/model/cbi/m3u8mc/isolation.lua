-- LuCI CBI model: m3u8mc 网络隔离
-- 对应设计文档 2.4 节 vlan_isolation 配置项

local m = Map("m3u8mc", translate("m3u8mc 网络隔离"),
    translate([[将组播流量隔离到独立 VLAN 子接口，避免在 LAN 侧广播泛洪。
    "应用隔离"按钮会自动生成 VLAN 子接口与防火墙规则（幂等，可重复执行）。]]))

local s = m:section(NamedSection, "vlan_isolation", "vlan_isolation",
    translate("VLAN 隔离设置"))
s.addremove = false

local enabled = s:option(Flag, "enabled", translate("启用网络隔离"))

local dev = s:option(Value, "base_device", translate("基础设备"),
    translate("组播 VLAN 子接口的父设备，通常为 br-lan"))
dev.placeholder = "br-lan"
dev:depends("enabled", "1")

s:option(Value, "vlan_id",  translate("VLAN ID"),
    translate("1–4094，与机架/交换机端口 VLAN 配置一致")):depends("enabled", "1")

s:option(Value, "ipaddr",   translate("本地 IP"),
    translate("VLAN 子接口地址，用于 ffmpeg localaddr 参数")
):depends("enabled", "1")

s:option(Value, "netmask",  translate("子网掩码")):depends("enabled", "1")

-- 应用/撤销按钮区（Button 不影响 UCI，点击后走 Ajax 调用 api_isolation_apply）
local btn_section = m:section(SimpleSection)

function btn_section.render(self, ...)
    local tpl = require "luci.template"
    tpl.render_string([[
<div class="cbi-section-node">
  <div style="padding:6px 0;display:flex;gap:8px;">
    <button class="btn cbi-button cbi-button-apply"
            onclick="m3u8mc_isolation_action('apply');return false;">
      <%:应用隔离（生成 VLAN / 防火墙规则）%>
    </button>
    <button class="btn cbi-button cbi-button-reset"
            onclick="m3u8mc_isolation_action('revert');return false;">
      <%:撤销隔离%>
    </button>
  </div>
  <div id="isolation-result" style="font-size:0.9em;color:#555;margin-top:4px;"></div>
  <script>
  function m3u8mc_isolation_action(action) {
    var el = document.getElementById('isolation-result');
    el.textContent = '执行中…';
    var fd = new FormData();
    fd.append('action', action);
    fetch('/cgi-bin/luci/admin/services/m3u8mc/api/isolation_apply', {
      method: 'POST', body: fd
    }).then(function(r){ return r.json(); }).then(function(d){
      el.textContent = d.ok ? '完成' : ('失败：' + (d.error || '未知错误'));
    }).catch(function(e){ el.textContent = '请求失败：' + e; });
  }
  </script>
</div>
]])
end

return m
