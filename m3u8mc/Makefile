include $(TOPDIR)/rules.mk

PKG_NAME:=m3u8mc
PKG_VERSION:=0.1.0
PKG_RELEASE:=1
PKG_MAINTAINER:=wupengyan
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

define Package/m3u8mc/description
  将 HTTP 单播 m3u8 直播源（支持多镜像服务器）拉流、缓存，转换为 UDP/RTP 组播推送。
  Rust 实现的核心转发引擎 m3u8mcd + procd 服务管理 + 可选网络隔离（VLAN/防火墙）。
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

$(eval $(call BuildPackage,m3u8mc))
