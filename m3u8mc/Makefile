include $(TOPDIR)/rules.mk

PKG_NAME:=m3u8mc
PKG_VERSION:=1.1.1
PKG_RELEASE:=1
PKG_MAINTAINER:=wupengyan
PKG_LICENSE:=MIT

# OpenWrt $(ARCH) -> Release 中的 Rust triple + 对应压缩包 sha256 (v1.1.1)
ifeq ($(ARCH),x86_64)
  RUST_TRIPLE:=x86_64-unknown-linux-musl
  PKG_HASH:=1140c4882e5c5b5a44d24b41b4816de175e8f5d4de358dba41fedc9cc9661fa1
else ifeq ($(ARCH),aarch64)
  RUST_TRIPLE:=aarch64-unknown-linux-musl
  PKG_HASH:=a09d1013fa417300255c6c770d5c8dd9b6d1cdd0505dfb0dcf3dad3b73ca09fa
else ifeq ($(ARCH),arm)
  RUST_TRIPLE:=armv7-unknown-linux-musleabihf
  PKG_HASH:=cb1fce3447b7b6a7fa30fabd9b2af77cda11773e29816d119b4662577b0cc7dc
else ifeq ($(ARCH),i386)
  RUST_TRIPLE:=i686-unknown-linux-musl
  PKG_HASH:=b8e21439571234ed3db7b8e1d5e2f9d8596557c967af99a937988f60b50339ad
endif

PKG_SOURCE:=m3u8mc-$(RUST_TRIPLE).tar.gz
PKG_SOURCE_URL:=https://github.com/mylucien/m3u8mc/releases/download/v$(PKG_VERSION)/

include $(INCLUDE_DIR)/package.mk

define Package/m3u8mc
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=Streaming
  TITLE:=m3u8 转组播转发服务
  DEPENDS:=+ffmpeg
endef

define Package/m3u8mc/description
  将 HTTP 单播 m3u8 直播源（支持多镜像服务器）拉流、缓存，转换为 UDP/RTP 组播推送。
  Rust 实现的核心转发引擎 m3u8mcd + procd 服务管理。
endef

# 压缩包顶层目录是 bin-<triple>/，去掉这一层，解压后 m3u8mcd / m3u8mcctl 直接在 PKG_BUILD_DIR 下
define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	tar -xzf $(DL_DIR)/$(PKG_SOURCE) -C $(PKG_BUILD_DIR) --strip-components=1
endef

# 预编译二进制，无需编译
define Build/Compile
endef

define Package/m3u8mc/install
	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/m3u8mcd  $(1)/usr/bin/m3u8mcd
	$(INSTALL_BIN) $(PKG_BUILD_DIR)/m3u8mcctl $(1)/usr/bin/m3u8mcctl

	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/m3u8mc.init $(1)/etc/init.d/m3u8mc

	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) ./files/m3u8mc.config $(1)/etc/config/m3u8mc
endef

$(eval $(call BuildPackage,m3u8mc))
