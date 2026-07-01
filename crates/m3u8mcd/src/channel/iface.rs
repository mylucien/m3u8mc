//! 接口名 → IP 地址解析（设计文档 2.3 配置翻译层）。
//!
//! 优先通过 `ubus call network.interface.<name> status` 查询 OpenWrt 运行时真实 IP
//! （而非配置时固化的值）；非 OpenWrt 环境（本机调试）下回退到 `ip` 命令，
//! 如果 iface 字段本身就是一个合法 IP 地址（测试/手动覆盖场景）则直接使用。

use serde_json::Value;
use tokio::process::Command;

pub async fn resolve_iface_ip(iface: &str) -> Option<String> {
    if iface.parse::<std::net::IpAddr>().is_ok() {
        return Some(iface.to_string());
    }

    if let Some(ip) = resolve_via_ubus(iface).await {
        return Some(ip);
    }

    if let Some(ip) = resolve_via_ip_command(iface).await {
        return Some(ip);
    }

    tracing::warn!(iface, "failed to resolve interface IP via ubus or ip command, ffmpeg localaddr will be omitted");
    None
}

async fn resolve_via_ubus(iface: &str) -> Option<String> {
    let output = Command::new("ubus")
        .arg("call")
        .arg(format!("network.interface.{iface}"))
        .arg("status")
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: Value = serde_json::from_slice(&output.stdout).ok()?;
    json.get("ipv4-address")
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|entry| entry.get("address"))
        .and_then(|a| a.as_str())
        .map(|s| s.to_string())
}

async fn resolve_via_ip_command(iface: &str) -> Option<String> {
    let output = Command::new("ip")
        .arg("-4")
        .arg("-j")
        .arg("addr")
        .arg("show")
        .arg(iface)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: Value = serde_json::from_slice(&output.stdout).ok()?;
    json.as_array()?
        .first()?
        .get("addr_info")?
        .as_array()?
        .first()?
        .get("local")?
        .as_str()
        .map(|s| s.to_string())
}
