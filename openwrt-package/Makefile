#
# m3u8mc：m3u8 转组播插件
#
# 构建策略：Rust 部分在开发机用 cross/cargo-zigbuild 交叉编译为 musl 静态二进制
# (详见仓库根目录 build.sh)，本 Makefile 只负责把产物和静态资源打进 ipk。
#
include $(TOPDIR)/rules.mk

PKG_NAME:=m3u8mc
PKG_VERSION:=0.1.0
PKG_RELEASE:=1z
PKG_MAINTAINER:=yz
PKG_LICENSE:=MIT

PREBUILT_BIN_DIR:=$(CURDIR)/bin/$(RUST_TARGET_TRIPLE)

include $(INCLUDE_DIR)/package.mk

define Package/m3u8mc
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=Streaming
  TITLE:=m3u8 转组播转发服务
  DEPENDS:=+ffmpeg +curl +file +tar +liblzma
endef

# curl 用于 FFmpeg 管理页的下载功能；file 用于识别压缩包格式；
# tar/liblzma 用于解压 .tar.xz 静态构建（busybox 自带 tar 的 xz 支持不完整）


define Package/m3u8mc/description
  将 HTTP 单播 m3u8 直播源（支持多镜像服务器）拉流、缓存，转换为 UDP/RTP 组播推送。
  Rust 实现的核心转发引擎 m3u8mcd + procd 服务管理 + 可选网络隔离（VLAN/防火墙）。
endef

define Package/luci-app-m3u8mc
  SECTION:=luci
  CATEGORY:=LuCI
  SUBMENU:=3. Applications
  TITLE:=LuCI support for m3u8mc
  DEPENDS:=+m3u8mc +luci-base
  PKGARCH:=all
endef

define Build/Compile
	@if [ ! -x "$(PREBUILT_BIN_DIR)/m3u8mcd" ]; then \
		echo "WARNING: prebuilt binary not found at $(PREBUILT_BIN_DIR)/m3u8mcd"; \
		echo "Run ./build.sh <target-triple> on the dev machine first."; \
	fi
	@true
endef

define Package/m3u8mc/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(PREBUILT_BIN_DIR)/m3u8mcd  $(1)/usr/bin/m3u8mcd
	$(INSTALL_BIN) $(PREBUILT_BIN_DIR)/m3u8mcctl $(1)/usr/bin/m3u8mcctl

	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/m3u8mc.init $(1)/etc/init.d/m3u8mc

	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) ./files/m3u8mc.config $(1)/etc/config/m3u8mc

	$(INSTALL_DIR) $(1)/usr/share/m3u8mc
	$(INSTALL_DATA) ./files/network-isolation.sh \
	    $(1)/usr/share/m3u8mc/network-isolation.sh
	$(INSTALL_BIN) ./files/ffmpeg-install.sh \
	    $(1)/usr/share/m3u8mc/ffmpeg-install.sh
endef

define Package/luci-app-m3u8mc/install
	$(INSTALL_DIR) $(1)/usr/lib/lua/luci/controller
	$(INSTALL_DATA) ./luci/controller/m3u8mc.lua \
	    $(1)/usr/lib/lua/luci/controller/m3u8mc.lua

	$(INSTALL_DIR) $(1)/usr/lib/lua/luci/model/cbi/m3u8mc
	$(INSTALL_DATA) ./luci/model/cbi/m3u8mc/channels.lua \
	    $(1)/usr/lib/lua/luci/model/cbi/m3u8mc/channels.lua
	$(INSTALL_DATA) ./luci/model/cbi/m3u8mc/isolation.lua \
	    $(1)/usr/lib/lua/luci/model/cbi/m3u8mc/isolation.lua

	$(INSTALL_DIR) $(1)/usr/lib/lua/luci/view/m3u8mc
	$(INSTALL_DATA) ./luci/view/m3u8mc/status.htm \
	    $(1)/usr/lib/lua/luci/view/m3u8mc/status.htm
	$(INSTALL_DATA) ./luci/view/m3u8mc/ffmpeg.htm \
	    $(1)/usr/lib/lua/luci/view/m3u8mc/ffmpeg.htm

	$(INSTALL_DIR) $(1)/www/luci-static/resources
	$(INSTALL_DATA) ./luci/htdocs/m3u8mc-status.js \
	    $(1)/www/luci-static/resources/m3u8mc-status.js
endef

$(eval $(call BuildPackage,m3u8mc))
$(eval $(call BuildPackage,luci-app-m3u8mc))
