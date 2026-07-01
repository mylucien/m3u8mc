#!/bin/sh
# network-isolation.sh
# 根据 UCI m3u8mc.vlan_isolation 配置生成/撤销 OpenWrt VLAN 子接口与防火墙隔离规则。
# 由 LuCI 后端或管理员手动调用；init 脚本不调用本脚本（网络隔离为可选特性）。
#
# 用法:
#   network-isolation.sh apply   — 应用隔离（生成 UCI 片段、执行 uci commit、ubus call）
#   network-isolation.sh revert  — 撤销（删除生成的 UCI 条目，重载 network 和 firewall）

set -e

IFACE_NAME="m3u8mc_vlan"
FW_ZONE_NAME="m3u8mc_multicast"

config_load_values() {
    . /lib/functions.sh
    config_load m3u8mc
    config_get    BASE_DEVICE vlan_isolation base_device  "br-lan"
    config_get    VLAN_ID     vlan_isolation vlan_id      "100"
    config_get    IPADDR      vlan_isolation ipaddr       "10.10.100.1"
    config_get    NETMASK     vlan_isolation netmask      "255.255.255.0"
    config_get_bool ENABLED   vlan_isolation enabled      0
}

apply() {
    config_load_values
    if [ "$ENABLED" != "1" ]; then
        echo "vlan_isolation disabled in UCI, nothing to do"
        exit 0
    fi

    echo "==> 创建 VLAN 子接口 ${BASE_DEVICE}.${VLAN_ID} → UCI 接口 ${IFACE_NAME}"

    # 删除已有的同名接口条目（幂等）
    uci -q delete network.${IFACE_NAME} 2>/dev/null || true

    uci set network.${IFACE_NAME}=interface
    uci set network.${IFACE_NAME}.proto=static
    uci set network.${IFACE_NAME}.ifname="${BASE_DEVICE}.${VLAN_ID}"
    uci set network.${IFACE_NAME}.ipaddr="${IPADDR}"
    uci set network.${IFACE_NAME}.netmask="${NETMASK}"
    uci commit network

    echo "==> 创建防火墙 zone ${FW_ZONE_NAME}"
    uci -q delete firewall.${FW_ZONE_NAME} 2>/dev/null || true
    uci set firewall.${FW_ZONE_NAME}=zone
    uci set firewall.${FW_ZONE_NAME}.name="${FW_ZONE_NAME}"
    uci set firewall.${FW_ZONE_NAME}.input=ACCEPT
    uci set firewall.${FW_ZONE_NAME}.output=ACCEPT
    uci set firewall.${FW_ZONE_NAME}.forward=REJECT
    uci add_list firewall.${FW_ZONE_NAME}.network="${IFACE_NAME}"

    # 阻止组播 VLAN 与 WAN 互通
    FW_RULE="${FW_ZONE_NAME}_block_wan"
    uci -q delete firewall.${FW_RULE} 2>/dev/null || true
    uci set firewall.${FW_RULE}=rule
    uci set firewall.${FW_RULE}.name="Block m3u8mc multicast to WAN"
    uci set firewall.${FW_RULE}.src="${FW_ZONE_NAME}"
    uci set firewall.${FW_RULE}.dest=wan
    uci set firewall.${FW_RULE}.target=REJECT
    uci commit firewall

    echo "==> 重载 network 与 firewall"
    ubus call network reload 2>/dev/null || /etc/init.d/network reload
    /etc/init.d/firewall reload

    echo "==> 完成。组播流量将通过 ${BASE_DEVICE}.${VLAN_ID} (${IPADDR}/${NETMASK}) 隔离转发"
}

revert() {
    config_load_values

    echo "==> 撤销网络隔离"
    uci -q delete network.${IFACE_NAME}       2>/dev/null || true
    uci -q delete firewall.${FW_ZONE_NAME}    2>/dev/null || true
    uci -q delete firewall.${FW_ZONE_NAME}_block_wan 2>/dev/null || true
    uci commit network
    uci commit firewall

    ubus call network reload 2>/dev/null || /etc/init.d/network reload
    /etc/init.d/firewall reload
    echo "==> 完成"
}

case "${1:-}" in
    apply)  apply  ;;
    revert) revert ;;
    *)
        echo "用法: $0 apply | revert"
        exit 1
        ;;
esac
